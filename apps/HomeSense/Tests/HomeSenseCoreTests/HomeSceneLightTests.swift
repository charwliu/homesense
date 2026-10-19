import Testing
@testable import HomeSenseCore

struct HomeSceneLightTests {
    private func device(_ id: String = "lamp", on: Bool = true, level: Int64 = 20, quality: String = "Confirmed", pending: Bool = false) -> HomeRecord {
        HomeRecord(.object(["id": .string(id), "name": .string("灯光\(id)"), "kind": .string("Dimmer"),
            "quality": .string(quality), "observed": .object(["on": .bool(on), "level": .integer(level)]),
            "reported": .object(["on": .bool(true), "level": .integer(90)]),
            "state_authority": .string(pending ? "pending_target" : "feedback")]))
    }
    private func scene(_ members: [String], complete: Bool = true) -> HomeRecord {
        HomeRecord(.object(["id": .string("scene"), "room": .object(["id": .string("room")]),
            "lights": .array(members.map { .object(["id": .string($0), "name": .string("成员\($0)")]) }),
            "lights_complete": .bool(complete)]))
    }
    @Test func exactMembersIncludeOtherRoomsAndNeverAppendUnlistedDevices() {
        let lights = scene(["lamp", "other-room", "lamp"]).sceneLightStates(devices: [device(), device("other-room"), device("not-a-member")], current: true)
        #expect(lights.map(\.id) == ["lamp", "other-room"])
        #expect(lights.allSatisfy { $0.isConfirmed && $0.isOn == true })
    }
    @Test func actualReadbackNeverUsesPendingTargetAndUpdatesWithThePushedDeviceValue() {
        let metadata = scene(["lamp"])
        let pending = metadata.sceneLightStates(devices: [device(level: 20, pending: true)], current: true)[0]
        #expect(pending.status == "实测 · 20% · 目标待确认")
        #expect(!pending.status.contains("90%"))
        let confirmed = metadata.sceneLightStates(devices: [device(level: 90)], current: true)[0]
        #expect(confirmed.status == "实测 · 90%")
        #expect(metadata.sceneLightStates(devices: [device(on: false, level: 0)], current: true)[0].status == "实测 · 关闭 · 0%")
    }
    @Test func StaleDisconnectedAndUnavailableValuesNeverPaintCurrentOn() {
        let metadata = scene(["lamp"])
        let stale = metadata.sceneLightStates(devices: [device(quality: "Stale")], current: true)[0]
        #expect(stale.isOn == nil && !stale.isConfirmed && stale.status == "上次 · 20%")
        let offline = metadata.sceneLightStates(devices: [device()], current: false)[0]
        #expect(offline.isOn == nil && offline.status == "离线 · 上次20%")
        var raw = device().raw
        raw["action_gate"] = .object(["available": .bool(false)])
        let unavailable = metadata.sceneLightStates(devices: [HomeRecord(raw)], current: true)[0]
        #expect(unavailable.isOn == nil && !unavailable.isConfirmed)
    }
    @Test func missingMembersPartialGrantsAndOldServersDoNotGuessFromRoom() {
        let partial = scene(["missing"], complete: false).sceneLightStates(devices: [device()], current: true)
        #expect(partial.count == 1 && partial[0].name == "成员missing" && partial[0].status == "等待状态同步")
        #expect(scene([], complete: false).sceneLightStates(devices: [device()], current: true).isEmpty)
        let old = HomeRecord(.object(["id": .string("old"), "room": .object(["id": .string("room")])]))
        #expect(!old.sceneLightMembershipAvailable)
        #expect(old.sceneLightStates(devices: [device()], current: true).isEmpty)
    }
}
