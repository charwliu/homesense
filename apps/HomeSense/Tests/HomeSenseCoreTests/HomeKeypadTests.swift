import Foundation
import Testing
@testable import HomeSenseCore

struct HomeKeypadTests {
    static func record() -> HomeRecord {
        let keys: [HomeJSON] = (1...6).map { number in
            let endpoint: HomeJSON = number <= 2 ? .string("legacy-key-\(number)") : .null
            let written: HomeJSON = number == 1 ? .object(["status": .string("written"), "color": .string("red")]) : .null
            let action: HomeJSON = number == 1 ? .object([
                "kind": .string("scene_toggle"), "scene": .string("reading"),
                "path": .string("/api/v1/scene-relative-actions")]) : .null
            return .object([
                "number": .integer(Int64(number)), "label": .string("按键 \(number)"),
                "configured": .bool(number <= 2), "endpoint_id": endpoint,
                "can_write_led": .bool(number == 1),
                "key_event": .object(["pressed": .bool(true), "quality": .string("Confirmed")]),
                "led": .object(["quality": .string("Unknown"), "observed": .null, "last_written": written]),
                "action": action])
        }
        return HomeRecord(.object([
            "id": .string("panel"), "name": .string("书房面板"), "kind": .string("Keypad"),
            "module_id": .string("panel-module"), "can_control": .bool(false), "quality": .string("Confirmed"),
            "room": .object(["id": .string("study"), "name": .string("书房")]),
            "capabilities": .object(["led_readback": .bool(false), "led_colors": .array([.string("red"), .string("green")])]), "keys": .array(keys),
            "settings": .object(["work_mode": .object(["configured": .string("manual"), "observed": .null])])]))
    }
    @Test func serverProjectionNeverUsesButtonOrParentQualityAsLEDFeedback() throws {
        let panel = try HomeKeypad(record: Self.record())
        #expect(panel.keys.map(\.number) == [1, 2, 3, 4, 5, 6])
        #expect(panel.keys.allSatisfy { $0.led.actual == .unknown })
        #expect(panel.keys[0].led.lastWritten == .red)
        #expect(panel.keys[0].canActivate && panel.keys[0].canWriteLED)
        #expect(!panel.keys[1].canActivate && !panel.keys[1].canWriteLED)
        #expect(panel.keys[2].endpointID == nil)
        var forged = Self.record().raw
        var rows = forged["keys"].values
        rows[0]["action"]["path"] = .string("https://untrusted.example/control")
        rows[0]["led"]["last_written"]["status"] = .string("uncertain")
        rows[0]["led"]["observed"] = .string("red")
        rows[0]["led"]["quality"] = .string("Confirmed")
        forged["keys"] = .array(rows)
        let rejected = try HomeKeypad(record: HomeRecord(forged))
        #expect(!rejected.keys[0].canActivate && rejected.keys[0].led.displayColor == .unknown)
        forged["keys"] = .array(Array(rows.dropLast()))
        #expect(throws: HomeServerError.self) { try HomeKeypad(record: HomeRecord(forged)) }
    }
    @Test func onlyConfirmedLEDFeedbackIsActualAndWritesRemainSeparate() {
        let pending = HomeKeypadLED(observed: .green, confirmed: false, lastWritten: .red)
        #expect(pending.actual == .unknown)
        #expect(pending.displayColor == .red && pending.displayDescription == "最后写出：红")
        #expect(pending.historyDescription == "最后写出：红")
        let confirmed = HomeKeypadLED(observed: .green, confirmed: true, lastWritten: .red)
        #expect(confirmed.actual == .green)
        #expect(confirmed.displayColor == .green && confirmed.displayDescription == "实测：绿")
        #expect(confirmed.historyDescription == "最后写出：红")
        #expect(HomeKeypadLED(observed: .off, confirmed: true).actual == .off)
    }
    @Test func parentHasSixOrderedKeysWithoutInventingMissingStateOrActions() throws {
        let keypad = try HomeKeypad(id: "study-keypad", name: "书房面板", roomName: "书房", moduleID: "panel-module",
            keys: [HomeKeypadKey(number: 6, name: "窗帘停止"), HomeKeypadKey(number: 1, name: "主灯")])
        #expect(keypad.keys.map(\.number) == [1, 2, 3, 4, 5, 6])
        #expect(keypad.keys[5].name == "窗帘停止")
        #expect(keypad.keys.allSatisfy { $0.led.actual == .unknown && !$0.canActivate })
        #expect(throws: HomeServerError.self) {
            try HomeKeypad(id: "duplicate", name: "面板", roomName: nil, moduleID: "m",
                keys: [HomeKeypadKey(number: 1, name: "A"), HomeKeypadKey(number: 1, name: "B")])
        }
        #expect(throws: HomeServerError.self) {
            try HomeKeypad(id: "invalid", name: "面板", roomName: nil, moduleID: "m", keys: [HomeKeypadKey(number: 7, name: "无效")])
        }
    }
}

@Suite @MainActor
struct HomeKeypadStoreTests {
    private func connected(_ server: HomeTestServer) async -> HomeServerStore {
        await server.setPresentedDevices([HomeKeypadTests.record().raw])
        let home = HomeServerStore(transport: server, defaults: nil, credentialStore: InMemoryCredentials())
        home.pause(); home.endpointText = "https://home.example:8444"
        await home.connect(token: "test-token")
        return home
    }
    @Test func parentActionsUseOnlyReviewedSceneAndExplicitAuthorizedLEDEndpoint() async throws {
        let server = HomeTestServer(), home = await connected(server)
        defer { home.disconnect() }
        #expect(home.devices.map(\.id) == ["panel"])
        let initial = await server.captured().count
        await home.controlDevice("panel", command: .object(["action": .string("power"), "on": .bool(true)]))
        await home.activateKeypad("panel", key: 2)
        await home.writeKeypadLED("panel", key: 2, color: .red)
        await home.writeKeypadLED("panel", key: 1, color: .off)
        #expect(await server.captured().count == initial)
        await home.activateKeypad("panel", key: 1)
        await home.writeKeypadLED("panel", key: 1, color: .green)
        let writes = await server.captured().dropFirst(initial).filter { $0.httpMethod == "POST" }
        #expect(writes.count == 2)
        let action = try #require(writes.first), led = try #require(writes.last)
        #expect(action.url?.path == "/api/v1/scene-relative-actions")
        let actionBody = try JSONDecoder().decode(HomeJSON.self, from: #require(action.httpBody))
        #expect(actionBody["scene"].text == "reading" && actionBody["on"] == .null)
        #expect(actionBody["request_id"].text.flatMap(UUID.init(uuidString:)) != nil)
        let ledBody = try JSONDecoder().decode(HomeJSON.self, from: #require(led.httpBody))
        #expect(led.url?.path == "/api/v1/device-commands" && ledBody["device"].text == "legacy-key-1")
        #expect(ledBody["command"] == .object(["action": .string("panel_led"), "on": .bool(false)]))
        #expect(try HomeKeypad(record: #require(home.devices.first)).keys[0].led.actual == .unknown)
    }
    @Test func lostKeyActionIsNeverRetriedOrOptimisticallyTreatedAsFeedback() async throws {
        let server = HomeTestServer(), home = await connected(server)
        defer { home.disconnect() }
        let before = await server.captured().count
        await server.setDropWrites()
        await home.activateKeypad("panel", key: 1)
        #expect(home.error != nil && home.message.contains("结果未知"))
        #expect(await server.captured().dropFirst(before).filter { $0.httpMethod == "POST" }.count == 1)
        #expect(try HomeKeypad(record: #require(home.devices.first)).keys[0].led.actual == .unknown)
    }
    @Test func knownSceneSelectionUsesExplicitActionAndUnknownNeedsPermission() async throws {
        let server = HomeTestServer(), home = await connected(server)
        defer { home.disconnect() }
        await server.setSceneSelected(.bool(true), relativeAllowed: false); await home.refresh()
        let before = await server.captured().count
        #expect(home.canActivateKeypad("panel", key: 1))
        await home.activateKeypad("panel", key: 1)
        let write = try #require(await server.captured().dropFirst(before).first { $0.httpMethod == "POST" })
        let body = try JSONDecoder().decode(HomeJSON.self, from: #require(write.httpBody))
        #expect(write.url?.path == "/api/v1/scene-actions" && body["on"] == .bool(false))
        await server.setSceneSelected(.null, relativeAllowed: false); await home.refresh()
        let blocked = await server.captured().count
        #expect(!home.canActivateKeypad("panel", key: 1))
        await home.activateKeypad("panel", key: 1)
        #expect(await server.captured().count == blocked)
    }
    @Test func reviewedMotorMappingAndExplicitOffUseDistinctCommands() async throws {
        let server = HomeTestServer(), home = await connected(server)
        defer { home.disconnect() }
        var panel = HomeKeypadTests.record().raw, keys = panel["keys"].values
        keys[0]["action"] = .object(["kind": .string("motor"), "device": .string("curtain"),
            "command": .object(["action": .string("stop")]), "path": .string("/api/v1/device-commands")])
        panel["keys"] = .array(keys)
        panel["capabilities"]["led_colors"] = .array([.string("red"), .string("green"), .string("off")])
        var motor: HomeJSON = .object(["id": .string("curtain"), "name": .string("窗帘"), "kind": .string("Motor"),
            "can_control": .bool(true), "motor_calibrated": .bool(true)])
        await server.setPresentedDevices([panel, motor]); await home.refresh()
        let before = await server.captured().count
        await home.activateKeypad("panel", key: 1)
        await home.writeKeypadLED("panel", key: 1, color: .off)
        let writes = await server.captured().dropFirst(before).filter { $0.httpMethod == "POST" }
        #expect(writes.count == 2)
        let motorBody = try JSONDecoder().decode(HomeJSON.self, from: #require(writes.first?.httpBody))
        let ledBody = try JSONDecoder().decode(HomeJSON.self, from: #require(writes.last?.httpBody))
        #expect(motorBody["device"].text == "curtain" && motorBody["command"]["action"].text == "stop")
        #expect(ledBody["device"].text == "legacy-key-1" && ledBody["command"]["action"].text == "panel_led_off")
        motor["can_control"] = .bool(false)
        await server.setPresentedDevices([panel, motor]); await home.refresh()
        let denied = await server.captured().count
        await home.activateKeypad("panel", key: 1)
        #expect(await server.captured().count == denied)
    }
    @Test func wholePanelSettingsRequireSixKeysAdminAndPublishedMode() async throws {
        let server = HomeTestServer(), home = await connected(server)
        defer { home.disconnect() }
        var panel = HomeKeypadTests.record().raw
        for field in ["backlight", "sound", "work_mode"] { panel["settings"][field] = .object(["can_write": .bool(true)]) }
        panel["settings"]["work_mode"]["configured"] = .string("manual")
        await server.setPresentedDevices([panel]); await home.refresh()
        var count = await server.captured().count
        await home.writeKeypadSetting("panel", command: .object(["action": .string("backlight"), "mode": .string("bright")]))
        #expect(await server.captured().count == count, "A two-key scope cannot gain whole-panel control")
        var keys = panel["keys"].values
        for index in keys.indices { keys[index]["configured"] = .bool(true); keys[index]["endpoint_id"] = .string("legacy-key-\(index + 1)") }
        panel["keys"] = .array(keys)
        await server.setPresentedDevices([panel]); await home.refresh()
        count = await server.captured().count
        await home.writeKeypadSetting("panel", command: .object(["action": .string("work_mode"), "mode": .string("self_reset")]))
        #expect(await server.captured().count == count, "Do not send a hardware mode contradicting published configuration")
        await home.writeKeypadSetting("panel", command: .object(["action": .string("backlight"), "mode": .string("bright")]))
        let write = try #require(await server.captured().dropFirst(count).first { $0.httpMethod == "POST" })
        #expect(write.url?.path == "/api/v1/keypad-settings")
        let body = try JSONDecoder().decode(HomeJSON.self, from: #require(write.httpBody))
        #expect(body["keypad"].text == "panel" && body["revision"].text == home.revision)
        #expect(body["request_id"].text.flatMap(UUID.init(uuidString:)) != nil)
        #expect(home.devices.first?.raw["settings"]["backlight"]["observed"] == .null)
        await server.setAdministrator(false); await home.refresh()
        count = await server.captured().count
        await home.writeKeypadSetting("panel", command: .object(["action": .string("read_sound")]))
        #expect(await server.captured().count == count)
    }
    @Test func keypadConfigurationPreservesLegacyIDsAndOnlyEditsDraft() async throws {
        let server = HomeTestServer(), home = await connected(server)
        defer { home.disconnect() }
        let count = await server.captured().count
        let row: HomeJSON = .object(["id": .string("panel"), "name": .string("面板"), "module_id": .string("panel-module"),
            "room": .string("study"), "enabled": .bool(true), "keys": .array([
                .object(["number": .integer(1), "label": .string("主灯"), "led_endpoint": .object([
                    "id": .string("legacy-key-1"), "name": .string("原LED"), "enabled": .bool(true)])])])])
        try home.upsertKeypad(row)
        var renamed = row; renamed["name"] = .string("新面板名")
        try home.upsertKeypad(renamed, originalID: "panel")
        #expect(home.editableKeypads.count == 1)
        #expect(home.editableKeypads.first?.raw["keys"].values.first?["led_endpoint"]["id"].text == "legacy-key-1")
        #expect(home.editableActionEndpoints.contains { $0.id == "legacy-key-1" })
        #expect(await server.captured().count == count && home.draftChanged)
        #expect(home.devices.first?.name == "书房面板")
        try home.removeKeypad("panel")
        #expect(home.editableKeypads.isEmpty && home.draftChanged)
        #expect(await server.captured().count == count)
    }
}
