import Foundation
import Combine
import Network

public enum ConnectionStatus: Equatable, Sendable {
    case disconnected, connecting, connected
}

public enum TransportFailure: LocalizedError {
    case invalidEndpoint, notConnected, busy, timedOut, disconnected
    public var errorDescription: String? {
        switch self {
        case .invalidEndpoint: "请填写网关主机名 / IP 和 1–65535 的端口。"
        case .notConnected: "请先连接 TCP 串口 网关。"
        case .busy: "上一条指令仍在写出，请稍后再试。"
        case .timedOut: "通信超时；连接已关闭，指令不会自动重发。"
        case .disconnected: "连接已断开；待发送指令已取消。"
        }
    }
}

/// One connection, one in-flight write, no reconnect/replay. All externally
/// visible state and callbacks are serialized on the main actor.
@MainActor
public final class TCPClient: ObservableObject {
    @Published public private(set) var status: ConnectionStatus = .disconnected
    public var onBytes: ((Data) -> Void)?
    public var onDisconnect: (() -> Void)?
    public var onMessage: ((String) -> Void)?
    private var connection: NWConnection?
    private var generation = UUID()
    private var deadline: Task<Void, Never>?
    private var pending: CheckedContinuation<Void, Error>?
    private var sendDeadline: Task<Void, Never>?
    private var lastWrite: Date = .distantPast
    public init() {}

    public func connect(host: String, port: String) throws {
        let host = host.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !host.isEmpty, !host.contains(where: \.isWhitespace),
              let number = UInt16(port), number > 0, let endpointPort = NWEndpoint.Port(rawValue: number)
        else { throw TransportFailure.invalidEndpoint }
        disconnect()
        let token = UUID()
        generation = token
        let active = NWConnection(host: NWEndpoint.Host(host), port: endpointPort, using: .tcp)
        connection = active
        status = .connecting
        active.stateUpdateHandler = { [weak self] state in
            Task { @MainActor [weak self] in
                guard let self, self.generation == token else { return }
                switch state {
                case .ready:
                    self.deadline?.cancel()
                    self.status = .connected
                    self.onMessage?("TCP 已连接；尚未确认面板响应。")
                    self.receive(active, token: token)
                case .failed(let error): self.fail(error)
                case .waiting(let error): self.onMessage?("等待网络：\(error.localizedDescription)")
                case .cancelled: self.disconnect()
                default: break
                }
            }
        }
        deadline = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(8)) } catch { return }
            guard let self, self.generation == token, self.status == .connecting else { return }
            self.fail(TransportFailure.timedOut)
        }
        active.start(queue: .main)
    }

    public func disconnect() {
        let hadConnection = status != .disconnected
        generation = UUID()
        deadline?.cancel(); deadline = nil
        sendDeadline?.cancel(); sendDeadline = nil
        connection?.stateUpdateHandler = nil
        connection?.cancel(); connection = nil
        status = .disconnected
        pending?.resume(throwing: TransportFailure.disconnected); pending = nil
        lastWrite = .distantPast
        if hadConnection { onDisconnect?() }
    }

    private func fail(_ error: Error) {
        onMessage?(error.localizedDescription)
        disconnect()
    }

    public func send(_ frame: Frame) async throws {
        guard pending == nil else { throw TransportFailure.busy }
        guard status == .connected, let active = connection else { throw TransportFailure.notConnected }
        let token = generation
        // The app owns the command queue; enforce spacing here as well.
        let delay = 0.15 - Date().timeIntervalSince(lastWrite)
        if delay > 0 { try await Task.sleep(for: .seconds(delay)) }
        try Task.checkCancellation()
        guard generation == token, status == .connected else { throw TransportFailure.disconnected }
        guard pending == nil else { throw TransportFailure.busy }
        try await withCheckedThrowingContinuation { continuation in
            pending = continuation
            sendDeadline = Task { [weak self] in
                do { try await Task.sleep(for: .seconds(5)) } catch { return }
                guard let self, self.generation == token, self.pending != nil else { return }
                self.fail(TransportFailure.timedOut)
            }
            active.send(content: frame.data, completion: .contentProcessed { [weak self] error in
                Task { @MainActor [weak self] in
                    guard let self, self.generation == token, let pending = self.pending else { return }
                    self.pending = nil
                    self.sendDeadline?.cancel(); self.sendDeadline = nil
                    if let error {
                        pending.resume(throwing: error)
                        self.fail(error)
                    } else {
                        self.lastWrite = Date()
                        pending.resume()
                    }
                }
            })
        }
    }

    private func receive(_ active: NWConnection, token: UUID) {
        active.receive(minimumIncompleteLength: 1, maximumLength: 4096) { [weak self] data, _, complete, error in
            Task { @MainActor [weak self] in
                guard let self, self.generation == token else { return }
                if let data, !data.isEmpty { self.onBytes?(data) }
                if let error { self.fail(error) }
                else if complete { self.onMessage?("网关已关闭连接。"); self.disconnect() }
                else { self.receive(active, token: token) }
            }
        }
    }
}
