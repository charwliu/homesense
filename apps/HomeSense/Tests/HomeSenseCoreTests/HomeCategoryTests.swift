import Foundation
import Testing
@testable import HomeSenseCore

@Suite @MainActor
struct HomeCategoryTests {
    private func device(_ id: String, _ kind: String, room: String? = "study", on: Bool? = nil,
                        quality: String = "Confirmed", visible: Bool = true) -> HomeRecord {
        HomeRecord(.object(["id": .string(id), "name": .string("相同名称"), "kind": .string(kind),
            "room": room.map(HomeJSON.string) ?? .null, "catalog_visible": .bool(visible),
            "quality": .string(quality), "can_control": .bool(true),
            "observed": .object(["on": on.map(HomeJSON.bool) ?? .null])]))
    }

    @Test func climateContainsCurtainsAndLightsKeepDistinctIDsWithoutPanelsOrHiddenAliases() {
        let rows = [device("curtain", "Motor"), device("lamp", "Switch"), device("dimmer", "Dimmer"),
            device("panel", "Keypad"), device("hidden", "Dimmer", visible: false),
            device("sensor", "Sensor"), device("lamp", "Switch"), device("", "Switch")]
        #expect(HomeDeviceCategory.climate.devices(from: rows).map(\.id) == ["curtain"])
        #expect(HomeDeviceCategory.lights.devices(from: rows).map(\.id) == ["lamp", "dimmer"])
        #expect(HomeDeviceCategory.climate.summary(devices: rows, current: true) == "1 扇窗帘")
    }

    @Test func roomGroupingUsesAuthorizedActiveIDsAndKeepsUnknownRoomsSeparateFromDrafts() {
        let rows = [device("one", "Dimmer"), device("two", "Switch", room: "other"),
            device("three", "Switch", room: nil), device("four", "Switch", room: "draft-only")]
        let rooms = [HomeRecord(.object(["id": .string("study"), "name": .string("书房"), "floor": .string("二层")])),
            HomeRecord(.object(["id": .string("other"), "name": .string("画室")])),
            HomeRecord(.object(["id": .string("study"), "name": .string("重复房间")]))]
        let groups = HomeDeviceCategory.lights.rooms(devices: rows, rooms: rooms)
        #expect(groups.map(\.name) == ["书房", "画室", "未分配房间"])
        #expect(groups[0].floor == "二层")
        #expect(groups.map { $0.devices.map(\.id) } == [["one"], ["two"], ["three", "four"]])
        #expect(groups.flatMap(\.devices).count == 4)
    }

    @Test func summaryUsesConfirmedActualFeedbackAndNeverTurnsMissingOrOfflineStateIntoAllOff() {
        let light = HomeDeviceCategory.lights
        let off = device("off", "Switch", on: false), on = device("on", "Dimmer", on: true)
        let stale = device("stale", "Dimmer", on: true, quality: "Stale")
        #expect(light.summary(devices: [], current: true) == "暂无灯")
        #expect(light.summary(devices: [off], current: true) == "全部关闭")
        #expect(light.summary(devices: [on, off], current: true) == "1 盏开启")
        #expect(light.summary(devices: [on, off, stale], current: true) == "1 盏开启 · 1 盏待同步")
        #expect(light.summary(devices: [off, stale], current: true) == "1 盏待同步")
        #expect(light.summary(devices: [off], current: false) == "1 盏灯 · 状态未同步")
        var unavailable = on.raw
        unavailable["action_gate"] = .object(["available": .bool(false)])
        #expect(light.summary(devices: [HomeRecord(unavailable)], current: true) == "1 盏待同步")
        var pending = off.raw
        pending["state_authority"] = .string("pending_target")
        pending["reported"] = .object(["on": .bool(true)])
        #expect(light.summary(devices: [HomeRecord(pending)], current: true) == "全部关闭")
    }

    @Test func browsingCategoriesAddsNoRequestsAndRetainsOriginalCatalogAndControlGates() async throws {
        let server = HomeTestServer()
        let rows = [device("curtain", "Motor"), device("lamp", "Switch", on: true),
            device("hidden", "Switch", visible: false)]
        await server.setPresentedDevices(rows.map(\.raw))
        let home = HomeServerStore(transport: server, defaults: nil, credentialStore: InMemoryCredentials())
        home.pause(); home.endpointText = "https://home.example:8444"; await home.connect(token: "test-token")
        defer { home.disconnect() }
        #expect(home.connected)
        let fullCatalog = home.devices, count = await server.captured().count
        for category in HomeDeviceCategory.allCases {
            let groups = category.rooms(devices: home.dailyDevices, rooms: home.rooms)
            #expect(groups.count == 1 && groups.first?.name == "书房")
            #expect(groups.flatMap(\.devices).count == 1)
        }
        home.pause()
        #expect(HomeDeviceCategory.lights.devices(from: home.dailyDevices).map(\.id) == ["lamp"])
        #expect(!home.canControlDevice("lamp") && !home.canControlDevice("curtain"))
        #expect(home.devices == fullCatalog)
        #expect(await server.captured().count == count)
        #expect(await server.captured().allSatisfy { $0.httpMethod == "GET" })
    }
}
