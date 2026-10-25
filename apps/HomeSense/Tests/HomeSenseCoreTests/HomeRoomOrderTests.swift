import Foundation
import Testing
@testable import HomeSenseCore

@Suite struct HomeRoomOrderTests {
    @Test func customOrderSurvivesRefreshAndAppendsNewRooms() {
        let first = UUID(), second = UUID(), added = UUID()
        var value = HomeRoomOrder()
        #expect(value.orderedIDs(in: [first, second]) == [first, second])
        value.setOrder([second, first])
        #expect(value.orderedIDs(in: [first, second]) == [second, first])
        #expect(value.orderedIDs(in: [added, first, second]) == [second, first, added])
        // Removing a room does not lose its preference if HomeKit reports it later.
        #expect(value.orderedIDs(in: [first]) == [first])
        #expect(value.orderedIDs(in: [first, second]) == [second, first])
    }

    @Test func duplicateAndStaleIdentifiersDoNotDuplicateOrInventRooms() {
        let a = UUID(), b = UUID(), missing = UUID()
        var value = HomeRoomOrder()
        value.setOrder([b, b, missing, a])
        #expect(value.roomIDs == [b, missing, a])
        #expect(value.orderedIDs(in: [a, b, a]) == [b, a])
        #expect(value.orderedIDs(in: []) == [])
    }

    @Test func persistenceIsHomeAndDeviceLocalAndResetRestoresDefault() throws {
        let suite = "room-order-test-\(UUID())", otherSuite = "room-order-test-\(UUID())"
        let defaults = try #require(UserDefaults(suiteName: suite))
        let otherDevice = try #require(UserDefaults(suiteName: otherSuite))
        defer {
            defaults.removePersistentDomain(forName: suite)
            otherDevice.removePersistentDomain(forName: otherSuite)
        }
        let home = UUID(), otherHome = UUID(), a = UUID(), b = UUID()
        var value = HomeRoomOrder()
        value.setOrder([b, a]); value.save(homeID: home, defaults: defaults)
        let restored = HomeRoomOrder.load(homeID: home, defaults: defaults)
        // Names and the discovery order are not persisted: renamed rooms keep their IDs.
        #expect(restored.orderedIDs(in: [a, b]) == [b, a])
        #expect(HomeRoomOrder.load(homeID: otherHome, defaults: defaults) == HomeRoomOrder())
        #expect(HomeRoomOrder.load(homeID: home, defaults: otherDevice) == HomeRoomOrder())
        HomeRoomOrder().save(homeID: home, defaults: defaults)
        #expect(HomeRoomOrder.load(homeID: home, defaults: defaults).orderedIDs(in: [a, b]) == [a, b])
        defaults.set(Data("invalid".utf8), forKey: "homekit.daily.roomOrder.\(home)")
        #expect(HomeRoomOrder.load(homeID: home, defaults: defaults) == HomeRoomOrder())
    }
}
