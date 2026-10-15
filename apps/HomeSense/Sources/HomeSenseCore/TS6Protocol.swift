import Foundation

public enum ProtocolFailure: LocalizedError, Equatable {
    case invalidAddress, broadcastRequiresIsolation, invalidKey, invalidValue, sameAddress
    public var errorDescription: String? {
        switch self {
        case .invalidAddress: "地址须为 01–FF 的两位十六进制数。"
        case .broadcastRequiresIsolation: "使用 00 查询或改址前，需要确认总线上仅连接一块 TS6-86 面板。"
        case .invalidKey: "按键编号须为 1–6。"
        case .invalidValue: "指令参数超出 TS6-86 手册范围。"
        case .sameAddress: "新地址与当前目标地址相同。"
        }
    }
}

public struct BusAddress: Hashable, Sendable, CustomStringConvertible {
    public let value: UInt8
    public init(_ value: UInt8) throws {
        guard value != 0 else { throw ProtocolFailure.invalidAddress }
        self.value = value
    }
    public init(hex: String) throws {
        try self.init(Self.parseHex(hex))
    }
    /// Parse a target, including 00. Permission to broadcast is checked by the
    /// commissioning encoder, never by the general nonzero BusAddress type.
    public static func parseHex(_ hex: String) throws -> UInt8 {
        var text = hex.trimmingCharacters(in: .whitespacesAndNewlines).uppercased()
        if text.hasPrefix("0X") { text.removeFirst(2) }
        guard text.count == 2, text.allSatisfy({ $0.isASCII && $0.isHexDigit }),
              let value = UInt8(text, radix: 16) else { throw ProtocolFailure.invalidAddress }
        return value
    }
    public var description: String { String(format: "%02X", value) }
}

public struct Frame: Equatable, Sendable {
    public let address: UInt8
    public let command: UInt8
    public let value: UInt8
    public init(address: UInt8, command: UInt8, value: UInt8) {
        self.address = address; self.command = command; self.value = value
    }
    public var data: Data { Data([0xAE, address, command, value, 0xEE]) }
    public var hex: String { data.hex }

    /// C3 response encoding is not specified by the manual. Preserve its raw
    /// value rather than interpreting it as an address or a write acknowledgement.
    public var summary: String {
        let prefix = String(format: "地址 %02X · ", address)
        if (0xD1...0xD6).contains(command), [0xF1, 0xF2].contains(value) {
            return prefix + "按键 \(command - 0xD0) " + (value == 0xF1 ? "按下" : "释放")
        }
        let names: [UInt8: String] = [0xC1: "按键声参数", 0xC3: "地址参数", 0xC5: "背光参数", 0xC7: "反馈灯模式"]
        if let name = names[command] { return prefix + name + String(format: "原始帧 · 值 %02X", value) }
        return prefix + "原始帧（未解释）"
    }
}

/// Fixed-width framing; AE/EE may occur inside the payload. At most four
/// undecoded bytes survive a feed. Reset for every new transport connection.
public struct FrameDecoder: Sendable {
    private var buffer: [UInt8] = []
    public init() {}
    public mutating func reset() { buffer.removeAll(keepingCapacity: true) }
    public mutating func feed(_ data: Data) -> [Frame] {
        var frames: [Frame] = []
        for byte in data {
            buffer.append(byte)
            while let first = buffer.first, first != 0xAE { buffer.removeFirst() }
            if buffer.count == 5 {
                if buffer[4] == 0xEE {
                    frames.append(Frame(address: buffer[1], command: buffer[2], value: buffer[3]))
                    buffer.removeAll(keepingCapacity: true)
                } else {
                    buffer.removeFirst()
                    while let first = buffer.first, first != 0xAE { buffer.removeFirst() }
                }
            }
        }
        return frames
    }
}

public enum LEDColor: UInt8, CaseIterable, Sendable { case off = 0xF0, red = 0xF1, green = 0xF2 }
public enum TS6Command: Sendable {
    case readAddress, setAddress(BusAddress), firmware
    case led(key: Int, color: LEDColor)
    case readBacklight, setBacklight(Int), readSound, setSound(Bool)
    case readFeedbackMode, setManualFeedback(Bool)

    public func frame(to address: BusAddress) throws -> Frame {
        let pair: (UInt8, UInt8)
        switch self {
        case .readAddress: pair = (0xC3, 0)
        case .setAddress(let next):
            guard next != address else { throw ProtocolFailure.sameAddress }
            pair = (0xC4, next.value)
        case .firmware: pair = (0xCC, 0)
        case .led(let key, let color):
            guard (1...6).contains(key) else { throw ProtocolFailure.invalidKey }
            pair = (UInt8(key), color.rawValue)
        case .readBacklight: pair = (0xC5, 0)
        case .setBacklight(let mode):
            guard (1...3).contains(mode) else { throw ProtocolFailure.invalidValue }
            pair = (0xC6, UInt8(mode))
        case .readSound: pair = (0xC1, 0)
        case .setSound(let enabled): pair = (0xC2, enabled ? 1 : 2)
        case .readFeedbackMode: pair = (0xC7, 0)
        case .setManualFeedback(let manual): pair = (0xC8, manual ? 2 : 1)
        }
        return Frame(address: address.value, command: pair.0, value: pair.1)
    }

    public static func unknownAddressQuery(isolated: Bool) throws -> Frame {
        guard isolated else { throw ProtocolFailure.broadcastRequiresIsolation }
        return Frame(address: 0, command: 0xC3, value: 0)
    }

    public static func addressQuery(targetHex: String, isolated: Bool) throws -> Frame {
        let target = try BusAddress.parseHex(targetHex)
        if target == 0 { return try unknownAddressQuery(isolated: isolated) }
        return try TS6Command.readAddress.frame(to: BusAddress(target))
    }

    /// 00 can be a panel's read-back value as well as a broadcast target. A
    /// zero-address panel can be commissioned only on an explicitly isolated bus.
    public static func addressChange(fromHex: String, to next: BusAddress, isolated: Bool) throws -> Frame {
        let target = try BusAddress.parseHex(fromHex)
        if target == 0 {
            guard isolated else { throw ProtocolFailure.broadcastRequiresIsolation }
            return Frame(address: 0, command: 0xC4, value: next.value)
        }
        return try TS6Command.setAddress(next).frame(to: BusAddress(target))
    }
}

public struct AddressReading: Equatable, Sendable, CustomStringConvertible {
    public let value: UInt8
    public var description: String { String(format: "%02X", value) }
}

/// Field observation: TS6-86/B V4.3 returns one binary address byte to C3.
/// Only interpret an entire serialized read window containing exactly one
/// byte, including 00. Other firmware/traffic stays raw. This is not a transaction ID.
public enum AddressReply {
    public static func candidate(in data: Data, queriedAddress: UInt8) -> AddressReading? {
        guard data.count == 1, let byte = data.first,
              queriedAddress == 0 || queriedAddress == byte else { return nil }
        return AddressReading(value: byte)
    }
}

public extension Data {
    var hex: String { map { String(format: "%02X", $0) }.joined(separator: " ") }
    /// Visible ASCII is a diagnostic stream, not a firmware response parser.
    var printableASCII: String {
        map { (32...126).contains($0) ? String(UnicodeScalar($0)) : "·" }.joined()
    }
}
