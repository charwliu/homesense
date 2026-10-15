import Foundation

public enum LHIO1600Failure: LocalizedError, Equatable {
    case invalidTarget, invalidNewAddress, isolationRequired, sameAddress
    case noReply, invalidReply, ambiguousReply, exception(UInt8)
    public var errorDescription: String? {
        switch self {
        case .invalidTarget: "LH-IO1600 当前地址请输入十进制 0–255。"
        case .invalidNewAddress: "新地址请输入十进制 1–247，避免广播及保留地址。"
        case .isolationRequired: "请先确认总线上只连接这一块 LH-IO1600；改址或使用 0／254 查询需要单模块隔离。"
        case .sameAddress: "新地址与当前目标地址相同。"
        case .noReply: "2 秒内未收到回复；未确认地址，不会自动重发。"
        case .invalidReply: "未收到匹配地址、功能码与 CRC 的有效回复；请检查日志、串口参数和接线。"
        case .ambiguousReply: "收到多个匹配回复，无法确认单一结果；请检查总线隔离、回显和重复地址。"
        case .exception(let code): "模块返回 Modbus 异常：\(String(format: "%02X", code))。"
        }
    }
}

/// LH-IO1600 manual §4.3: parameter register 03EA / PLC 41003, FC03 / FC06.
/// These are RTU bytes for a transparent TCP serial gateway, not Modbus TCP.
public struct LHIO1600Request: Sendable {
    public let target: UInt8
    public let newAddress: UInt8?
    public var function: UInt8 { newAddress == nil ? 0x03 : 0x06 }
    public var data: Data {
        Self.withCRC([target, function, 0x03, 0xEA, 0, newAddress ?? 1])
    }
    public var hex: String { data.hex }

    public static func read(target: String, isolated: Bool) throws -> Self {
        let address = try parseTarget(target)
        if address == 0 || address == 254 {
            guard isolated else { throw LHIO1600Failure.isolationRequired }
        }
        return Self(target: address, newAddress: nil)
    }

    public static func write(target: String, newAddress: String, isolated: Bool) throws -> Self {
        guard isolated else { throw LHIO1600Failure.isolationRequired }
        let address = try parseTarget(target)
        let text = newAddress.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, text.allSatisfy({ $0.isASCII && $0.isNumber }),
              let next = UInt8(text), (1...247).contains(next) else { throw LHIO1600Failure.invalidNewAddress }
        guard next != address else { throw LHIO1600Failure.sameAddress }
        return Self(target: address, newAddress: next)
    }

    private static func parseTarget(_ value: String) throws -> UInt8 {
        let text = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, text.allSatisfy({ $0.isASCII && $0.isNumber }), let address = UInt8(text)
        else { throw LHIO1600Failure.invalidTarget }
        return address
    }

    public static func withCRC(_ payload: [UInt8]) -> Data {
        var crc: UInt16 = 0xFFFF
        for byte in payload {
            crc ^= UInt16(byte)
            for _ in 0..<8 { crc = crc & 1 != 0 ? (crc >> 1) ^ 0xA001 : crc >> 1 }
        }
        return Data(payload + [UInt8(crc & 0xFF), UInt8(crc >> 8)])
    }

    public enum Reply: Equatable, Sendable { case address(UInt8), writeEcho }

    /// Inspect the whole bounded receive window, tolerating TCP fragmentation,
    /// concatenation and unrelated traffic. Never interpret a request echo as a read.
    public func reply(in data: Data) throws -> Reply {
        guard !data.isEmpty else { throw LHIO1600Failure.noReply }
        let bytes = Array(data)
        var matches: [Result<Reply, LHIO1600Failure>] = []
        var index = 0
        while index + 5 <= bytes.count {
            let unit = bytes[index], code = bytes[index + 1]
            let allowedUnit = unit == target || (newAddress != nil && unit == newAddress)
                || target == 254
            guard allowedUnit, code == function || code == function | 0x80 else { index += 1; continue }
            let length = code & 0x80 != 0 ? 5 : (function == 0x03 ? 7 : 8)
            guard index + length <= bytes.count else { index += 1; continue }
            let candidate = Array(bytes[index..<(index + length)])
            guard Self.withCRC(Array(candidate.dropLast(2))) == Data(candidate) else { index += 1; continue }
            if code & 0x80 != 0 {
                matches.append(.failure(.exception(candidate[2])))
            } else if function == 0x03 {
                guard candidate[2] == 2, candidate[3] == 0 else { index += length; continue }
                matches.append(.success(.address(candidate[4])))
            } else {
                guard Array(candidate[2..<6]) == [0x03, 0xEA, 0, newAddress!] else { index += length; continue }
                matches.append(.success(.writeEcho))
            }
            index += length
        }
        guard matches.count == 1 else {
            throw matches.isEmpty ? LHIO1600Failure.invalidReply : .ambiguousReply
        }
        return try matches[0].get()
    }
}
