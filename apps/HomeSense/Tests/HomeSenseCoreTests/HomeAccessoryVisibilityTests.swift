import Foundation
import Testing
@testable import HomeSenseCore

@Suite struct HomeAccessoryVisibilityTests {
    @Test func lightStripRuleMatchesPrefixAndCanRestoreOneAccessory() {
        let first = UUID(), other = UUID()
        var value = HomeAccessoryVisibility()
        #expect(!value.isHidden(id: first, name: "灯带01"))
        value.setHidesLightStrips(true)
        for name in ["灯带", "灯带01", "灯带-东", " 灯带 02\n"] {
            #expect(value.isHidden(id: first, name: name))
        }
        #expect(!value.isHidden(id: first, name: "主灯"))
        #expect(!value.isHidden(id: first, name: "客厅灯带"))
        value.setHidden(false, id: first)
        #expect(!value.isHidden(id: first, name: "灯带01"))
        #expect(value.isHidden(id: other, name: "灯带02"))
        value.setHidesLightStrips(false)
        value.setHidesLightStrips(true)
        #expect(value.isHidden(id: first, name: "灯带01"))
    }

    @Test func manualHidingSurvivesRenameAndBulkRuleChanges() {
        let id = UUID()
        var value = HomeAccessoryVisibility()
        value.setHidden(true, id: id)
        value.setHidesLightStrips(true)
        value.setHidesLightStrips(false)
        #expect(value.isHidden(id: id, name: "新名称"))
        value.setHidden(false, id: id)
        #expect(!value.isHidden(id: id, name: "新名称"))
    }

    @Test func preferencesPersistPerHomeAndDoNotReplaceFavorites() throws {
        let suite = "visibility-test-\(UUID())"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let home = UUID(), otherHome = UUID(), hidden = UUID(), restored = UUID()
        let favorites = "homekit.daily.favorites.\(home)"
        defaults.set([hidden.uuidString], forKey: favorites)
        var value = HomeAccessoryVisibility()
        value.setHidesLightStrips(true)
        value.setHidden(true, id: hidden)
        value.setHidden(false, id: restored)
        value.save(homeID: home, defaults: defaults)
        #expect(HomeAccessoryVisibility.load(homeID: home, defaults: defaults) == value)
        #expect(HomeAccessoryVisibility.load(homeID: otherHome, defaults: defaults) == HomeAccessoryVisibility())
        #expect(defaults.stringArray(forKey: favorites) == [hidden.uuidString])
        defaults.set(Data("invalid".utf8), forKey: "homekit.daily.visibility.\(otherHome)")
        #expect(HomeAccessoryVisibility.load(homeID: otherHome, defaults: defaults) == HomeAccessoryVisibility())
    }
}
