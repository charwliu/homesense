import Foundation

public enum HomeEventMessage: Sendable {
    case state(Data)
    case heartbeat
}

public protocol HomeEventTransport: Sendable {
    func events(_ request: URLRequest) -> AsyncThrowingStream<HomeEventMessage, Error>
}

// URLSession may deliver a ping failure again while its task/session is being
// cancelled. Claim completion under the lock, then resume outside the lock.
final class HomePingCompletion: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Void, Error>?
    private var result: Result<Void, Error>?
    private var installed = false

    func install(_ continuation: CheckedContinuation<Void, Error>) -> Bool {
        lock.lock()
        precondition(!installed)
        installed = true
        if let result {
            lock.unlock()
            continuation.resume(with: result)
            return false
        }
        self.continuation = continuation
        lock.unlock()
        return true
    }

    @discardableResult
    func complete(_ result: Result<Void, Error>) -> Bool {
        lock.lock()
        guard self.result == nil else { lock.unlock(); return false }
        self.result = result
        let continuation = continuation
        self.continuation = nil
        lock.unlock()
        continuation?.resume(with: result)
        return true
    }
}

func waitForHomePing(
    timeout: Duration = .seconds(5),
    onTimeout: @escaping @Sendable () -> Void,
    send: @Sendable (@escaping @Sendable (Error?) -> Void) -> Void
) async throws {
    let completion = HomePingCompletion()
    let deadline = Task {
        do { try await Task.sleep(for: timeout) } catch { return }
        if completion.complete(.failure(URLError(.timedOut))) { onTimeout() }
    }
    defer { deadline.cancel() }
    try await withTaskCancellationHandler {
        try await withCheckedThrowingContinuation { (reply: CheckedContinuation<Void, Error>) in
            if completion.install(reply) {
                send { error in
                    completion.complete(error.map { .failure($0) } ?? .success(()))
                }
            }
        }
    } onCancel: {
        completion.complete(.failure(CancellationError()))
    }
}

/// Uses system TLS trust and the same Bearer/Origin boundary as HTTPS requests.
public struct HomeWebSocketTransport: HomeEventTransport {
    private let delegate: any URLSessionDelegate & Sendable
    public init() { delegate = NoRedirects() }
    init(delegate: any URLSessionDelegate & Sendable) { self.delegate = delegate }
    public func events(_ request: URLRequest) -> AsyncThrowingStream<HomeEventMessage, Error> {
        AsyncThrowingStream(bufferingPolicy: .bufferingOldest(16)) { continuation in
            let configuration = URLSessionConfiguration.ephemeral
            configuration.httpShouldSetCookies = false
            configuration.httpCookieStorage = nil
            configuration.urlCache = nil
            configuration.timeoutIntervalForRequest = 15
            configuration.timeoutIntervalForResource = 24 * 60 * 60
            let session = URLSession(configuration: configuration, delegate: delegate, delegateQueue: nil)
            let socket = session.webSocketTask(with: request)
            socket.maximumMessageSize = 512 * 1024
            socket.resume()
            let initialDeadline = Task {
                do { try await Task.sleep(for: .seconds(15)) } catch { return }
                socket.cancel(with: .goingAway, reason: nil)
            }
            let receiver = Task {
                do {
                    let initial = request.value(forHTTPHeaderField: "X-HomeSense-Module-Events") == "1"
                        ? "{\"cursor\":null,\"modules\":true}" : "{\"cursor\":null}"
                    try await socket.send(.string(initial))
                    while !Task.isCancelled {
                        let message = try await socket.receive()
                        initialDeadline.cancel()
                        let data: Data
                        switch message {
                        case .data(let bytes): data = bytes
                        case .string(let text): data = Data(text.utf8)
                        @unknown default: throw HomeServerError.invalidResponse
                        }
                        guard data.count <= 512 * 1024 else { throw HomeServerError.invalidResponse }
                        if case .dropped = continuation.yield(.state(data)) { throw HomeServerError.invalidResponse }
                    }
                } catch { continuation.finish(throwing: error) }
            }
            let heartbeat = Task {
                while !Task.isCancelled {
                    do { try await Task.sleep(for: .seconds(5)) } catch { return }
                    // A missing pong cancels the connection even on a silent network loss.
                    do {
                        try await waitForHomePing(onTimeout: {
                            socket.cancel(with: .goingAway, reason: nil)
                        }) { reply in
                            socket.sendPing { error in reply(error) }
                        }
                        continuation.yield(.heartbeat)
                    } catch {
                        continuation.finish(throwing: error); return
                    }
                }
            }
            continuation.onTermination = { _ in
                receiver.cancel(); heartbeat.cancel(); initialDeadline.cancel()
                socket.cancel(with: .goingAway, reason: nil)
                session.invalidateAndCancel()
            }
        }
    }
}
