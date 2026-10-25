import Foundation

/// Main-actor SDK callbacks may arrive after timeout, cancellation, or twice.
@MainActor public final class HomeCallbackCompletion {
    private var reply: CheckedContinuation<Void, Error>?
    private var installed = false
    private var result: Result<Void, Error>?
    public init() {}
    public func install(_ reply: CheckedContinuation<Void, Error>) -> Bool {
        precondition(!installed); installed = true
        if let result { reply.resume(with: result); return false }
        self.reply = reply; return true
    }
    public func finish(_ result: Result<Void, Error>) {
        guard self.result == nil else { return }
        self.result = result; reply?.resume(with: result); reply = nil
    }
}
