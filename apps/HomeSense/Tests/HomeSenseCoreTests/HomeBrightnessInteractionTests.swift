import Testing
@testable import HomeSenseCore

@Suite struct HomeBrightnessInteractionTests {
    @Test func tapsSelectEveryTenPercentStopFromBottomToTop() {
        for level in stride(from: 0, through: 100, by: 10) {
            let position = Double(100 - level) * 3
            #expect(HomeBrightnessInteraction.tappedLevel(verticalPosition: position, height: 300) == Double(level))
        }
        #expect(HomeBrightnessInteraction.tappedLevel(verticalPosition: 81, height: 300) == 70)
        #expect(HomeBrightnessInteraction.tappedLevel(verticalPosition: 69, height: 300) == 80)
        #expect(HomeBrightnessInteraction.tappedLevel(verticalPosition: 150, height: 300) == 50)
        #expect(HomeBrightnessInteraction.tappedLevel(verticalPosition: -20, height: 300) == 100)
        #expect(HomeBrightnessInteraction.tappedLevel(verticalPosition: 320, height: 300) == 0)
    }
    @Test func invalidGeometryNeverProducesACommand() {
        for height in [0.0, -1, .infinity, .nan] {
            #expect(HomeBrightnessInteraction.tappedLevel(verticalPosition: 100, height: height) == nil)
        }
        for position in [Double.infinity, -.infinity, .nan] {
            #expect(HomeBrightnessInteraction.tappedLevel(verticalPosition: position, height: 300) == nil)
        }
    }
    @Test func aTapSubmitsItsLevelOnceAndRetainsItThroughOldFeedback() throws {
        var draft = HomeBrightnessDraft()
        draft.observe(level: 100, confirmed: true, timestamp: 100)
        let target = try #require(HomeBrightnessInteraction.tappedLevel(verticalPosition: 210, height: 300))
        draft.beginEditing(); draft.setValue(target)
        let submitted = draft.endEditing(feedbackTimestamp: 100)
        let request = try #require(submitted)
        #expect(request.level == 30)
        #expect(draft.endEditing(feedbackTimestamp: 100) == nil)
        draft.complete(request.id, accepted: true)
        draft.observe(level: 100, confirmed: true, timestamp: 101)
        #expect(draft.value == 30 && draft.submission?.id == request.id)
        draft.observe(level: 30, confirmed: true, timestamp: 102)
        #expect(draft.value == 30 && draft.submission == nil)
        draft.beginEditing(); draft.setValue(37)
        #expect(draft.endEditing(feedbackTimestamp: 102)?.level == 37, "drag targets retain single-percent precision")
    }
}
