import Foundation

public enum HomeEventMessage: Sendable {
    case state(Data)
    case heartbeat
}

public protocol HomeEventTransport: Sendable {
    func events(_ request: URLRequest) -> AsyncThrowingStream<HomeEventMessage, Error>
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
                    try await socket.send(.string("{\"cursor\":null}"))
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
                    let timeout = Task {
                        do { try await Task.sleep(for: .seconds(5)) } catch { return }
                        socket.cancel(with: .goingAway, reason: nil)
                    }
                    do {
                        try await withCheckedThrowingContinuation { (reply: CheckedContinuation<Void, Error>) in
                            socket.sendPing { error in
                                if let error { reply.resume(throwing: error) } else { reply.resume() }
                            }
                        }
                        timeout.cancel()
                        continuation.yield(.heartbeat)
                    } catch {
                        timeout.cancel(); continuation.finish(throwing: error); return
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
