import Foundation
import Testing
@testable import HomeSenseCore

private final class CapturedPingReply: @unchecked Sendable {
    private let lock = NSLock()
    private var reply: (@Sendable (Error?) -> Void)?
    func store(_ reply: @escaping @Sendable (Error?) -> Void) {
        lock.lock(); self.reply = reply; lock.unlock()
    }
    func finish(_ error: Error?) {
        lock.lock(); let reply = reply; lock.unlock()
        reply?(error)
    }
}

@Test func successfulPingIgnoresDuplicateAbortCallback() async throws {
    try await waitForHomePing(onTimeout: {}) { reply in
        reply(nil)
        reply(NSError(domain: NSPOSIXErrorDomain, code: 53))
        reply(nil)
    }
}

@Test func firstPingErrorSurvivesLaterSuccessAndErrors() async {
    do {
        try await waitForHomePing(onTimeout: {}) { reply in
            reply(NSError(domain: NSPOSIXErrorDomain, code: 53))
            reply(nil)
            reply(URLError(.cancelled))
        }
        Issue.record("The original ping error must propagate")
    } catch {
        #expect((error as NSError).domain == NSPOSIXErrorDomain)
        #expect((error as NSError).code == 53)
    }
}

@Test func concurrentPingCompletionsResumeOnlyOnce() async throws {
    try await waitForHomePing(onTimeout: {}) { reply in
        DispatchQueue.concurrentPerform(iterations: 64) { _ in reply(nil) }
    }
}

@Test func cancelledPingFinishesWithoutCallbackAndIgnoresLateReplies() async {
    let callbacks = CapturedPingReply()
    let ready = AsyncStream<Void>.makeStream()
    let task = Task {
        try await waitForHomePing(onTimeout: {}) { reply in
            callbacks.store(reply)
            ready.continuation.yield(())
            ready.continuation.finish()
        }
    }
    for await _ in ready.stream { break }
    task.cancel()
    do { try await task.value; Issue.record("Cancellation must finish the waiting ping") }
    catch { #expect(error is CancellationError) }
    callbacks.finish(NSError(domain: NSPOSIXErrorDomain, code: 53))
    callbacks.finish(nil)
}

@Test func pingTimeoutDoesNotRequireURLSessionToCallBack() async {
    do {
        try await waitForHomePing(timeout: .milliseconds(10), onTimeout: {}) { _ in }
        Issue.record("A silent ping must time out")
    } catch {
        #expect((error as? URLError)?.code == .timedOut)
    }
}

@Test func cancellationBeforeContinuationInstallationCompletesExactlyOnce() async {
    let completion = HomePingCompletion()
    completion.complete(.failure(CancellationError()))
    do {
        try await withCheckedThrowingContinuation { (reply: CheckedContinuation<Void, Error>) in
            #expect(!completion.install(reply))
            completion.complete(.success(()))
        }
        Issue.record("Cancellation before installation must propagate")
    } catch { #expect(error is CancellationError) }
}
