import Foundation
import Testing
@testable import HomeSenseCore

@Suite @MainActor struct HomeAccessoryNameEditorTests {
    @Test func confirmedRenamePersistsThroughANewEditor() async {
        var appleName = "Temperature Sensor-5475"
        let editor = HomeAccessoryNameEditor(name: appleName)
        editor.name = "  客厅温湿度  "
        let result = await editor.save { requested in appleName = requested; return appleName }
        #expect(result && appleName == "客厅温湿度")
        #expect(HomeAccessoryNameEditor(name: appleName).name == "客厅温湿度")
        #expect(editor.original == appleName && !editor.canSave)
    }
    @Test func rejectedHomeKitSaveKeepsInputAndReportsError() async {
        let editor = HomeAccessoryNameEditor(name: "原名称"); editor.name = "客厅温湿度"
        var calls = 0
        let result = await editor.save { _ in calls += 1; throw NSError(domain: "HMErrorDomain", code: 80) }
        #expect(!result && calls == 1)
        #expect(editor.original == "原名称" && editor.name == "客厅温湿度")
        #expect(editor.error?.contains("HMErrorDomain / 80") == true && !editor.saving)
    }
    @Test func successfulCallbackWithOldNameIsNotSuccess() async {
        let editor = HomeAccessoryNameEditor(name: "原名称"); editor.name = "客厅温湿度"
        let result = await editor.save { _ in "原名称" }
        #expect(!result && editor.original == "原名称")
        #expect(editor.name == "客厅温湿度" && editor.error?.contains("尚未确认保存") == true)
    }
    @Test func emptyOrUnchangedNameNeverInvokesHomeKit() async {
        let editor = HomeAccessoryNameEditor(name: "原名称"); var calls = 0
        for input in ["\n  ", "原名称", " 原名称 "] {
            editor.name = input
            let result = await editor.save { name in calls += 1; return name }
            #expect(!result)
        }
        #expect(calls == 0)
    }
    @Test func duplicateSaveDoesNotIssueASecondRename() async {
        let editor = HomeAccessoryNameEditor(name: "原名称"); editor.name = "客厅温湿度"
        var calls = 0
        let result = await editor.save { name in
            calls += 1
            let duplicate = await editor.save { second in calls += 1; return second }
            #expect(!duplicate && editor.saving)
            return name
        }
        #expect(result && calls == 1)
    }
}
