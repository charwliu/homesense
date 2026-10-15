import Foundation
import Combine

public struct LogEntry: Identifiable, Sendable {
    public enum Kind: String, Sendable { case tx = "TX", rx = "RX", frame = "帧", info = "提示", error = "错误" }
    public let id = UUID()
    public let time = Date()
    public let kind: Kind
    public let text: String
}

public struct KeyObservation: Sendable {
    public let pressed: Bool
    public let time: Date
}

@MainActor
public final class CommissioningSession: ObservableObject {
    @Published public var host: String
    @Published public var port: String
    @Published public var address: String
    @Published public var newAddress = "42"
    @Published public var isolatedPanel = false
    @Published public var holdingKeyOne = false
    @Published public private(set) var status: ConnectionStatus = .disconnected
    @Published public private(set) var busy = false
    @Published public private(set) var logs: [LogEntry] = []
    @Published public private(set) var observedAddresses: [UInt8: Date] = [:]
    @Published public private(set) var keyEvents: [Int: KeyObservation] = [:]
    @Published public private(set) var notice = "连接网关后，先查询面板地址。"
    @Published public var error: String?
    @Published public private(set) var writtenAddress: BusAddress?
    @Published public private(set) var queriedAddress: AddressReading?
    @Published public private(set) var lastReply = ""
    @Published public private(set) var keyLEDLinkEnabled = false
    /// Successfully written requests, not measured LED state.
    @Published public private(set) var lastWrittenLEDColors: [Int: LEDColor] = [:]
    @Published public var lhAddress = "1"
    @Published public var lhNewAddress = "10"
    @Published public var isolatedRelay = false
    @Published public private(set) var lhQueriedAddress: UInt8?
    @Published public private(set) var lhWrittenAddress: UInt8?
    @Published public private(set) var lhVerifiedAddress: UInt8?
    @Published public private(set) var lhNotice = "LH-IO1600 · 地址均为十进制；请先查询。"
    @Published public private(set) var lhLastReply = ""
    private var lhWindow: Data?
    public let client: TCPClient
    private var decoder = FrameDecoder()
    private var subscriptions: Set<AnyCancellable> = []
    private var operation: Task<Void, Never>?
    private var readWindow: Data?
    private var desiredLEDColors: [Int: LEDColor] = [:]
    // At most one pending absolute target per key, preserving press parity
    // without replaying an unbounded backlog after a query or slow connection.
    private var pendingLEDColors: [Int: LEDColor] = [:]
    private var automaticOperation = false
    private let defaults: UserDefaults?

    public init(defaults: UserDefaults? = .standard) {
        self.defaults = defaults
        host = defaults?.string(forKey: "gateway.host") ?? "192.168.3.7"
        port = defaults?.string(forKey: "gateway.port") ?? "8234"
        address = defaults?.string(forKey: "gateway.address") ?? "10"
        client = TCPClient()
        client.$status.sink { [weak self] value in self?.status = value }.store(in: &subscriptions)
        client.onMessage = { [weak self] message in self?.append(.info, message) }
        client.onBytes = { [weak self] bytes in self?.receive(bytes) }
        client.onDisconnect = { [weak self] in
            guard let self else { return }
            self.operation?.cancel(); self.readWindow = nil; self.lhWindow = nil
            self.lhQueriedAddress = nil; self.lhWrittenAddress = nil; self.lhVerifiedAddress = nil
            self.isolatedRelay = false; self.lhLastReply = ""
            self.lhNotice = "已断开；LH-IO1600 本次连接的地址结果已清空。"
            self.stopKeyLEDLink()
            self.lastWrittenLEDColors.removeAll()
            self.decoder.reset(); self.observedAddresses.removeAll(); self.keyEvents.removeAll()
            self.queriedAddress = nil; self.lastReply = ""
            self.holdingKeyOne = false; self.isolatedPanel = false
            self.notice = "已断开；当前连接的地址观测已清空。"
            self.append(.info, self.notice)
        }
        $address.dropFirst().sink { [weak self] _ in
            self?.keyEvents.removeAll()
            self?.stopKeyLEDLink()
            self?.lastWrittenLEDColors.removeAll()
        }.store(in: &subscriptions)
    }

    public var canSend: Bool { status == .connected && !busy }
    public var addressPreview: String {
        do { return try TS6Command.addressQuery(targetHex: address, isolated: isolatedPanel).hex }
        catch { return "请输入有效目标地址" }
    }
    public var changePreview: String {
        do { return try TS6Command.addressChange(fromHex: address, to: BusAddress(hex: newAddress), isolated: isolatedPanel).hex }
        catch { return error.localizedDescription }
    }
    public var transcript: String {
        let formatter = ISO8601DateFormatter()
        return "HomeSense · TS6-86 / TS6-86-B / LH-IO1600\n" + logs.map {
            "\(formatter.string(from: $0.time)) [\($0.kind.rawValue)] \($0.text)"
        }.joined(separator: "\n")
    }

    public func connect() {
        do {
            try client.connect(host: host, port: port)
            decoder.reset(); observedAddresses.removeAll(); keyEvents.removeAll()
            writtenAddress = nil
            queriedAddress = nil; lastReply = ""
            defaults?.set(host, forKey: "gateway.host"); defaults?.set(port, forKey: "gateway.port")
            append(.info, "连接 \(host):\(port) · TCP 透明串口网关")
        } catch { report(error) }
    }
    public func disconnect() { client.disconnect() }
    public func clearLogs() { logs.removeAll() }

    public func setKeyLEDLinkEnabled(_ enabled: Bool) {
        if !enabled {
            stopKeyLEDLink()
            return
        }
        guard !keyLEDLinkEnabled else { return }
        guard canSend else { report(status == .connected ? TransportFailure.busy : .notConnected); return }
        do {
            let target = try BusAddress(hex: address)
            let mode = try TS6Command.setManualFeedback(true).frame(to: target)
            let green = try (1...6).map { try TS6Command.led(key: $0, color: .green).frame(to: target) }
            keyEvents.removeAll()
            desiredLEDColors = Dictionary(uniqueKeysWithValues: (1...6).map { ($0, .green) })
            pendingLEDColors.removeAll()
            keyLEDLinkEnabled = true
            append(.info, "启用按键联动：目标 \(target)，切换手动灯控并将六键设为绿灯；每次按下切换红／绿。")
            perform(mode, label: "按键联动初始化", additionalFrames: green, automatic: true)
        } catch { report(error) }
    }

    private func stopKeyLEDLink() {
        let wasEnabled = keyLEDLinkEnabled
        keyLEDLinkEnabled = false
        pendingLEDColors.removeAll(); desiredLEDColors.removeAll()
        if automaticOperation { operation?.cancel() }
        if wasEnabled { append(.info, "按键 LED 联动已停止，未写出的灯控已取消。") }
    }

    private func drainLinkedLEDs() {
        guard keyLEDLinkEnabled, canSend,
              let key = pendingLEDColors.keys.sorted().first,
              let color = pendingLEDColors.removeValue(forKey: key) else { return }
        do {
            let frame = try TS6Command.led(key: key, color: color).frame(to: BusAddress(hex: address))
            perform(frame, label: "按键 \(key) 联动 → \(color == .red ? "红灯" : "绿灯")", automatic: true)
        } catch { stopKeyLEDLink(); report(error) }
    }

    public func send(_ command: TS6Command, label: String, expectsReply: Bool = false) {
        do {
            let frame: Frame
            if case .readAddress = command {
                frame = try TS6Command.addressQuery(targetHex: address, isolated: isolatedPanel)
            } else {
                frame = try command.frame(to: BusAddress(hex: address))
            }
            let addressQuery: Bool
            let firmwareQuery: Bool
            if case .readAddress = command { addressQuery = true } else { addressQuery = false }
            if case .firmware = command { firmwareQuery = true } else { firmwareQuery = false }
            // An explicit manual light/mode operation takes over from linking.
            if canSend {
                switch command {
                case .led, .setManualFeedback: stopKeyLEDLink()
                default: break
                }
            }
            perform(frame, label: label, expectsReply: expectsReply || addressQuery || firmwareQuery,
                    addressQuery: addressQuery)
        } catch { report(error) }
    }

    public func queryUnknownAddress() {
        do { perform(try TS6Command.unknownAddressQuery(isolated: isolatedPanel), label: "单面板广播地址查询", expectsReply: true, addressQuery: true) }
        catch { report(error) }
    }

    public func changeAddress() {
        do {
            guard holdingKeyOne else {
                throw SessionFailure.keyNotHeld
            }
            let next = try BusAddress(hex: newAddress)
            let frame = try TS6Command.addressChange(fromHex: address, to: next, isolated: isolatedPanel)
            if canSend { stopKeyLEDLink() }
            perform(frame, label: "硬件改址 \(String(format: "%02X", frame.address)) → \(next)", changedTo: next)
        } catch { report(error) }
    }

    public func queryWrittenAddress() {
        guard let writtenAddress else { return }
        do { perform(try TS6Command.readAddress.frame(to: writtenAddress), label: "核对新地址 \(writtenAddress)", expectsReply: true, addressQuery: true) }
        catch { report(error) }
    }

    public func useAddress(_ value: UInt8) {
        guard !busy else { return }
        guard value != 0 || isolatedPanel else { report(ProtocolFailure.broadcastRequiresIsolation); return }
        address = String(format: "%02X", value)
        defaults?.set(address, forKey: "gateway.address")
        notice = "当前测试目标为 \(address)。此操作只选择目标，不修改硬件。"
    }

    private func perform(_ frame: Frame, label: String, expectsReply: Bool = false,
                         addressQuery: Bool = false, changedTo: BusAddress? = nil,
                         additionalFrames: [Frame] = [], automatic: Bool = false) {
        guard canSend else { report(status == .connected ? TransportFailure.busy : .notConnected); return }
        busy = true
        automaticOperation = automatic
        error = nil
        if addressQuery { queriedAddress = nil }
        if expectsReply { readWindow = Data(); lastReply = "" }
        operation = Task { [weak self] in
            guard let self else { return }
            defer {
                self.busy = false; self.operation = nil; self.readWindow = nil
                self.automaticOperation = false
                self.drainLinkedLEDs()
            }
            do {
                for outgoing in [frame] + additionalFrames {
                    try Task.checkCancellation()
                    try await self.client.send(outgoing)
                    self.append(.tx, "\(outgoing.hex) · \(label) · 已写出")
                    try Task.checkCancellation()
                    if (1...6).contains(outgoing.command), let color = LEDColor(rawValue: outgoing.value) {
                        self.lastWrittenLEDColors[Int(outgoing.command)] = color
                    }
                }
                self.defaults?.set(self.address, forKey: "gateway.address")
                if let next = changedTo {
                    self.writtenAddress = next
                    self.holdingKeyOne = false
                    self.notice = "改址指令已写出。请核对新地址 \(next)；尚未确认硬件改址成功。"
                } else {
                    self.notice = "\(label)已写出" + (expectsReply ? "；等待原始回复。" : "；请观察面板实际效果。")
                }
                if expectsReply {
                    // Keep the queue occupied for the entire reply window so a
                    // following command cannot contaminate a single-byte read.
                    try await Task.sleep(for: .seconds(2))
                    try Task.checkCancellation()
                    let reply = self.readWindow ?? Data()
                    self.lastReply = reply.isEmpty ? "未收到回复" : "HEX: \(reply.hex)\nASCII: \(reply.printableASCII)"
                    if addressQuery, let candidate = AddressReply.candidate(in: reply, queriedAddress: frame.address) {
                        self.queriedAddress = candidate
                        self.notice = "地址查询读回 \(candidate)（单字节）。请核对面板；总线协议没有事务确认。"
                    } else {
                        self.notice = reply.isEmpty
                            ? "2 秒内未收到回复。请检查地址、接线与网关；不会自动重发。"
                            : "收到原始回复，请核对日志。回复不代表设置成功确认。"
                    }
                    self.append(.info, self.notice)
                }
            } catch is CancellationError {
                self.append(.info, "操作已取消；不自动重发。")
            } catch {
                if automatic { self.stopKeyLEDLink() }
                self.report(error)
            }
        }
    }

    public var lhReadPreview: String {
        (try? LHIO1600Request.read(target: lhAddress, isolated: isolatedRelay).hex) ?? "请输入有效当前地址；0／254 需确认单模块隔离"
    }
    public var lhChangePreview: String {
        do { return try LHIO1600Request.write(target: lhAddress, newAddress: lhNewAddress, isolated: isolatedRelay).hex }
        catch { return error.localizedDescription }
    }
    public var canChangeLHAddress: Bool {
        (try? LHIO1600Request.write(target: lhAddress, newAddress: lhNewAddress, isolated: isolatedRelay)) != nil
    }
    public func readLHAddress(unknown: Bool = false) {
        do {
            let request = try LHIO1600Request.read(target: unknown ? "254" : lhAddress, isolated: isolatedRelay)
            performLH(request)
        } catch { report(error) }
    }
    public func changeLHAddress() {
        do { performLH(try LHIO1600Request.write(target: lhAddress, newAddress: lhNewAddress, isolated: isolatedRelay)) }
        catch { report(error) }
    }
    public func verifyLHAddress() {
        guard let next = lhWrittenAddress else { return }
        do { performLH(try LHIO1600Request.read(target: String(next), isolated: false)) }
        catch { report(error) }
    }
    public func useLHAddress(_ address: UInt8) {
        guard !busy else { return }
        lhAddress = String(address)
    }

    private func performLH(_ request: LHIO1600Request) {
        guard canSend else { report(status == .connected ? TransportFailure.busy : .notConnected); return }
        stopKeyLEDLink()
        decoder.reset()
        busy = true; error = nil; lhWindow = Data(); lhLastReply = ""
        lhQueriedAddress = nil; lhVerifiedAddress = nil
        if request.newAddress != nil { lhWrittenAddress = nil }
        operation = Task { [weak self] in
            guard let self else { return }
            defer { self.busy = false; self.operation = nil; self.lhWindow = nil; self.decoder.reset() }
            do {
                try Task.checkCancellation()
                try await self.client.send(request.data)
                self.append(.tx, "\(request.hex) · LH-IO1600 \(request.newAddress == nil ? "读地址" : "改址") · 已写出")
                try Task.checkCancellation()
                if let next = request.newAddress {
                    self.lhWrittenAddress = next
                    self.isolatedRelay = false
                    self.lhNotice = "改址指令已写出 → \(next)；尚未确认，请查询新地址核对。"
                } else { self.lhNotice = "读取地址指令已写出；等待 CRC 校验通过的回复。" }
                try await Task.sleep(for: .seconds(2))
                try Task.checkCancellation()
                let bytes = self.lhWindow ?? Data()
                self.lhLastReply = bytes.isEmpty ? "未收到回复" : bytes.hex
                switch try request.reply(in: bytes) {
                case .address(let value):
                    self.lhQueriedAddress = value
                    if request.target == self.lhWrittenAddress, value == request.target {
                        self.lhVerifiedAddress = value
                        self.lhNotice = "已通过新地址 \(value) 读回软件地址 \(value)，CRC 校验通过。"
                    } else {
                        self.lhNotice = "软件地址读回 \(value)（HEX \(String(format: "%02X", value))），CRC 校验通过。"
                    }
                case .writeEcho:
                    self.lhNotice = "收到匹配的写入回显；回显不能证明硬件改址成功，请查询新地址核对。"
                }
                self.append(.info, "LH-IO1600 · \(self.lhNotice)")
            } catch is CancellationError {
                self.append(.info, "LH-IO1600 操作已取消；不自动重发。")
            } catch {
                self.lhNotice = (self.lhWrittenAddress == nil ? "" : "改址尚未确认；可查询新地址核对。") + error.localizedDescription
                self.report(error)
            }
        }
    }

    private func receive(_ data: Data) {
        if let window = lhWindow {
            lhWindow = window + data.prefix(max(0, 65536 - window.count))
            append(.rx, "\(data.hex) · LH-IO1600 RTU")
            return
        }
        if let window = readWindow {
            // Bounded diagnostics even if a gateway continuously streams noise.
            readWindow = window + data.prefix(max(0, 65536 - window.count))
        }
        append(.rx, "\(data.hex) · ASCII: \(data.printableASCII)")
        for frame in decoder.feed(data) {
            append(.frame, "\(frame.hex) · \(frame.summary)")
            if frame.address != 0 { observedAddresses[frame.address] = Date() }
            if let target = try? BusAddress(hex: address), frame.address == target.value,
               (0xD1...0xD6).contains(frame.command), [0xF1, 0xF2].contains(frame.value) {
                let key = Int(frame.command - 0xD0)
                let pressed = frame.value == 0xF1
                let wasPressed = keyEvents[key]?.pressed == true
                keyEvents[key] = KeyObservation(pressed: pressed, time: Date())
                if keyLEDLinkEnabled, pressed, !wasPressed {
                    let next: LEDColor = desiredLEDColors[key] == .red ? .green : .red
                    desiredLEDColors[key] = next
                    pendingLEDColors[key] = next
                }
            }
        }
        drainLinkedLEDs()
    }

    private func append(_ kind: LogEntry.Kind, _ text: String) {
        logs.append(LogEntry(kind: kind, text: text))
        if logs.count > 600 { logs.removeFirst(logs.count - 600) }
    }
    private func report(_ error: Error) {
        self.error = error.localizedDescription
        append(.error, error.localizedDescription)
    }
}

public enum SessionFailure: LocalizedError {
    case keyNotHeld
    public var errorDescription: String? { "改址时需要同时长按面板按键①，请先确认已按住。" }
}
