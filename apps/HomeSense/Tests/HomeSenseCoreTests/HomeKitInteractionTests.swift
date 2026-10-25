import Foundation
import Testing
@testable import HomeSenseCore

@Suite struct HomeKitInteractionTests {
    @Test func curtainTargetReadbackDoesNotConfirmActualPosition() throws {
        let actual = UUID(), target = UUID(), accessory = UUID()
        var observations = HomeKitObservations()
        observations.reset(members: [actual: accessory, target: accessory])
        let generation = observations.generation
        observations.received(.number(50), for: actual, generation: generation)
        let baseline = try #require(observations.revisions[actual])
        var draft = HomeBrightnessDraft()
        draft.observe(level: 50, confirmed: true, timestamp: baseline)
        draft.beginEditing(); draft.setValue(100)
        let release = draft.endEditing(feedbackTimestamp: baseline)
        let released = try #require(release)
        draft.complete(released.id, accepted: true)
        observations.received(.number(100), for: target, generation: generation)
        draft.observe(level: 50, confirmed: true, timestamp: observations.revisions[actual])
        #expect(draft.submission?.id == released.id && draft.value == 100)
        observations.received(.number(75), for: actual, generation: generation)
        draft.observe(level: 75, confirmed: true, timestamp: observations.revisions[actual])
        #expect(draft.value == 100 && draft.submission != nil)
        observations.received(.number(100), for: actual, generation: generation)
        draft.observe(level: 100, confirmed: true, timestamp: observations.revisions[actual])
        #expect(draft.submission == nil && draft.value == 100)
    }
    @Test func oneGestureSubmitsOnceAndStopClearsOnlyTheLocalTarget() throws {
        var draft = HomeBrightnessDraft()
        draft.observe(level: 35, confirmed: true, timestamp: 1)
        draft.beginEditing(); draft.setValue(80)
        let release = draft.endEditing(feedbackTimestamp: 1)
        let released = try #require(release)
        let repeatedRelease = draft.endEditing(feedbackTimestamp: 1)
        #expect(repeatedRelease == nil)
        draft.expire(released.id)
        #expect(draft.unconfirmedTarget == 80)
        draft.cancel()
        draft.observe(level: 62, confirmed: true, timestamp: 2)
        #expect(draft.value == 62 && draft.submission == nil && draft.unconfirmedTarget == nil)
        let stoppedRelease = draft.endEditing(feedbackTimestamp: 2)
        #expect(stoppedRelease == nil)
    }
    @Test func otherAccessoryFeedbackCannotAdvanceThisConfirmationRevision() throws {
        let one = UUID(), two = UUID(), owner = UUID()
        var state = HomeKitObservations(); state.reset(members: [one: owner, two: owner])
        state.received(.number(50), for: one, generation: state.generation)
        let before = try #require(state.revisions[one])
        state.received(.boolean(true), for: two, generation: state.generation)
        #expect(state.revisions[one] == before)
        state.received(.number(50), for: one, generation: state.generation)
        #expect(try #require(state.revisions[one]) > before)
    }
}
