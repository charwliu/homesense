import Foundation
import Testing
@testable import HomeSenseCore

@Suite @MainActor struct HomeCallbackCompletionTests {
    @Test func duplicateAndLateSDKRepliesResumeOnce() async throws {
        let completion = HomeCallbackCompletion()
        try await withCheckedThrowingContinuation { (reply: CheckedContinuation<Void, Error>) in
            #expect(completion.install(reply))
            completion.finish(.success(()))
            completion.finish(.failure(URLError(.networkConnectionLost)))
            completion.finish(.success(()))
        }
    }
    @Test func cancellationBeforeInstallPreventsStartingRequest() async {
        let completion = HomeCallbackCompletion()
        completion.finish(.failure(CancellationError()))
        do {
            try await withCheckedThrowingContinuation { (reply: CheckedContinuation<Void, Error>) in
                #expect(!completion.install(reply))
                completion.finish(.success(()))
            }
            Issue.record("Cancellation was lost")
        } catch { #expect(error is CancellationError) }
    }
    @Test func timeoutPreservedAfterLateSuccess() async {
        let completion = HomeCallbackCompletion()
        do {
            try await withCheckedThrowingContinuation { (reply: CheckedContinuation<Void, Error>) in
                #expect(completion.install(reply))
                completion.finish(.failure(URLError(.timedOut)))
                completion.finish(.success(()))
            }
            Issue.record("Timeout was lost")
        } catch { #expect((error as? URLError)?.code == .timedOut) }
    }
}
