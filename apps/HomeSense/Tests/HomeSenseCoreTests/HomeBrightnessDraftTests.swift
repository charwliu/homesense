import Testing
@testable import HomeSenseCore

@Suite struct HomeBrightnessDraftTests {
    @Test func delayedRampFeedbackAfterTimeoutDoesNotPullTheTargetBack() throws {
        var draft = HomeBrightnessDraft()
        draft.observe(level: 100, confirmed: true, timestamp: 100)
        draft.beginEditing(); draft.setValue(30)
        let released = draft.endEditing(feedbackTimestamp: 100)
        let request = try #require(released)
        draft.complete(request.id, accepted: true)
        draft.observe(level: 70, confirmed: true, timestamp: 110)
        draft.expire(request.id)
        draft.observe(level: 70, confirmed: true, timestamp: 110)
        #expect(draft.value == 30 && draft.unconfirmedTarget == 30)
        draft.observe(level: 30, confirmed: true, timestamp: 100)
        #expect(draft.unconfirmedTarget == 30)
        draft.observe(level: 30, confirmed: true, timestamp: 115)
        #expect(draft.value == 30 && draft.unconfirmedTarget == nil)
        draft.observe(level: 45, confirmed: true, timestamp: 120)
        #expect(draft.value == 45)
    }
    @Test func dragAndReleaseProduceExactlyOneFinalSubmission() throws {
        var draft = HomeBrightnessDraft()
        draft.observe(level: 20, confirmed: true, timestamp: 100)
        draft.beginEditing()
        for value in [25.0, 45, 70, 83] { draft.setValue(value) }
        let released1 = draft.endEditing(feedbackTimestamp: 100)
        let submission = try #require(released1)
        #expect(submission.level == 83 && draft.value == 83)
        let released2 = draft.endEditing(feedbackTimestamp: 100)
        #expect(released2 == nil)
    }
    @Test func oldPushAndAcceptedReceiptNeverOverwriteAnEditedOrPendingTarget() throws {
        var draft = HomeBrightnessDraft()
        draft.observe(level: 20, confirmed: true, timestamp: 100)
        draft.beginEditing(); draft.setValue(80)
        draft.observe(level: 30, confirmed: true, timestamp: 110)
        #expect(draft.value == 80)
        let released3 = draft.endEditing(feedbackTimestamp: 110)
        let request = try #require(released3)
        draft.complete(request.id, accepted: true)
        draft.observe(level: 20, confirmed: true, timestamp: 120)
        draft.observe(level: 80, confirmed: true, timestamp: 110)
        draft.observe(level: 80, confirmed: false, timestamp: 130)
        #expect(draft.submission?.id == request.id && draft.value == 80)
        draft.observe(level: 80, confirmed: true, timestamp: 131)
        #expect(draft.submission == nil && draft.value == 80)
    }
    @Test func anOldCompletionOrDeadlineCannotClearANewerGesture() throws {
        var draft = HomeBrightnessDraft()
        draft.beginEditing(); draft.setValue(40)
        let released4 = draft.endEditing(feedbackTimestamp: 1)
        let old = try #require(released4)
        draft.beginEditing(); draft.setValue(90)
        draft.complete(old.id, accepted: false); draft.expire(old.id)
        #expect(draft.isEditing && draft.value == 90 && draft.unconfirmedTarget == nil)
        let released5 = draft.endEditing(feedbackTimestamp: 2)
        let new = try #require(released5)
        draft.complete(old.id, accepted: false); draft.expire(old.id)
        #expect(draft.submission?.id == new.id)
    }
    @Test func feedbackBeforeTheHTTPCompletionDoesNotResurrectPendingState() throws {
        var draft = HomeBrightnessDraft()
        draft.beginEditing(); draft.setValue(60)
        let released6 = draft.endEditing(feedbackTimestamp: 100)
        let request = try #require(released6)
        draft.observe(level: 60, confirmed: true, timestamp: 101)
        draft.complete(request.id, accepted: true)
        #expect(draft.submission == nil && draft.value == 60)
    }
    @Test func timeoutAndFailureReleaseTheDraftWithoutRetryingAndAllowTheNextGesture() throws {
        var draft = HomeBrightnessDraft()
        draft.beginEditing(); draft.setValue(70)
        let released7 = draft.endEditing(feedbackTimestamp: 10)
        let request = try #require(released7)
        draft.expire(request.id)
        #expect(draft.submission == nil && draft.unconfirmedTarget == 70)
        draft.observe(level: 20, confirmed: true, timestamp: 11)
        #expect(draft.value == 70 && draft.unconfirmedTarget == 70)
        draft.beginEditing(); draft.setValue(35)
        let released8 = draft.endEditing(feedbackTimestamp: 11)
        let next = try #require(released8)
        draft.complete(next.id, accepted: false)
        #expect(draft.submission == nil && draft.unconfirmedTarget == 35)
        draft.observe(level: 35, confirmed: true, timestamp: 12)
        #expect(draft.value == 35 && draft.unconfirmedTarget == nil)
    }
    @Test func zeroAndFullLevelRemainExplicitAndCancelDoesNotSubmit() throws {
        var draft = HomeBrightnessDraft()
        draft.setValue(-10); draft.beginEditing()
        let released9 = draft.endEditing(feedbackTimestamp: nil)
        #expect(try #require(released9).level == 0)
        draft.beginEditing(); draft.setValue(500)
        let released10 = draft.endEditing(feedbackTimestamp: nil)
        #expect(try #require(released10).level == 100)
        draft.beginEditing(); draft.setValue(.infinity)
        #expect(draft.value == 100)
        draft.cancel()
        let released11 = draft.endEditing(feedbackTimestamp: nil)
        #expect(!draft.isEditing && draft.submission == nil && released11 == nil)
    }
}
