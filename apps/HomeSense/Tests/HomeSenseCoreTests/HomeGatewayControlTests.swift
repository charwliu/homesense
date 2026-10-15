import Foundation
import Testing
@testable import HomeSenseCore

// Real device WSS rows and scene HTTP rows are frozen unchanged. They were
// captured in separate loopback server runs; this in-memory envelope combines
// them for client tests, and never claims a new real-server WSS observation.
struct GatewayInputs: Sendable {
    let devices: HomeJSON, scenes: HomeJSON
    init() throws {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("test-inputs")
        devices = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("actual-device-availability-contract.json")))
        scenes = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("actual-scene-availability-contract.json")))
    }
    func frame(_ phase: String) -> HomeJSON {
        var frame = devices[phase]
        frame["scenes"] = scenes[phase]["body"]["scenes"]
        return frame
    }
    static func motor(_ id: String, view: HomeJSON) -> HomeJSON {
        .object(["id": .string(id), "name": .string(id), "kind": .string("Motor"),
                 "can_control": .bool(true), "motor_calibrated": .bool(true), "quality": .string("Confirmed"),
                 "observed": .object(["on": .bool(true)]), "availability": view,
                 "action_gate": .object(["available": .bool(view["state"].text == "ready")])])
    }
    static func panel() -> HomeJSON {
        var panel = HomeKeypadTests.record().raw, keys = panel["keys"].values
        for i in keys.indices {
            keys[i]["configured"] = .bool(true); keys[i]["endpoint_id"] = .string("panel-led-\(i+1)")
            keys[i]["can_write_led"] = .bool(true)
        }
        keys[0]["action"] = .object(["kind": .string("motor"), "device": .string("motor-a"), "path": .string("/api/v1/device-commands"), "command": .object(["action": .string("stop")])])
        keys[1]["action"] = .object(["kind": .string("motor"), "device": .string("motor-b"), "path": .string("/api/v1/device-commands"), "command": .object(["action": .string("open")])])
        for (index, scene) in [(2,"later-a"),(3,"later-b")] {
            keys[index]["action"] = .object(["kind": .string("scene_toggle"), "scene": .string(scene), "path": .string("/api/v1/scene-relative-actions")])
        }
        panel["keys"] = .array(keys)
        for field in ["backlight","sound","work_mode"] { panel["settings"][field] = .object(["can_write": .bool(true)]) }
        panel["settings"]["work_mode"]["configured"] = .string("manual")
        return panel
    }
}
actor GatewayHTTP: HomeHTTPTransport {
    var state: HomeJSON
    var requests: [URLRequest] = []
    var dropWrites = false
    init(_ state: HomeJSON) { self.state = state }
    func replace(_ state: HomeJSON) { self.state = state }
    func loseWriteResponse() { dropWrites = true }
    func writes() -> [URLRequest] { requests.filter { $0.httpMethod == "POST" } }
    func captured() -> [URLRequest] { requests }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        precondition(request.url?.host == "127.0.0.1")
        precondition(request.value(forHTTPHeaderField: "Authorization") == "Bearer synthetic-gateway-client-only")
        requests.append(request)
        if request.httpMethod == "POST" {
            if dropWrites { dropWrites = false; throw URLError(.networkConnectionLost) }
            return .init(data: try HomeJSON.object(["outcome": .string("accepted"), "status": .string("pending")]).data(), status: 200)
        }
        let value: HomeJSON
        switch request.url!.path {
        case "/api/v1/session": value = .object(["can_manage_configuration": .bool(true)])
        case "/api/v1/devices": value = .object(["revision": state["revision"], "boot": state["cursor"]["boot"], "devices": state["devices"]])
        case "/api/v1/scenes": value = .object(["revision": state["revision"], "scenes": state["scenes"]])
        case "/api/v1/rooms": value = .object(["revision": state["revision"], "boot": state["cursor"]["boot"], "rooms": .array([]), "floors": .array([])])
        case "/api/v1/config/versions": value = .object(["active_revision": state["revision"], "versions": .array([])])
        case "/api/v1/config/versions/1/export": value = .object(["active_revision": .string("1"), "revision": .string("1"), "devices": .object(["schema_version": .integer(2), "devices": .array([])]), "automation": .object(["schema_version": .integer(1)])])
        default: throw HomeServerError.invalidResponse
        }
        return .init(data: try value.data(), status: 200)
    }
}
final class GatewayEvents: HomeEventTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var streams: [AsyncThrowingStream<HomeEventMessage,Error>.Continuation] = []
    var count: Int { lock.withLock { streams.count } }
    func events(_ request: URLRequest) -> AsyncThrowingStream<HomeEventMessage,Error> {
        precondition(request.url?.host == "127.0.0.1")
        return AsyncThrowingStream { c in lock.withLock { streams.append(c) } }
    }
    func push(_ frame: HomeJSON) throws { lock.withLock { streams.last }?.yield(.state(try frame.data())) }
}
@MainActor func gatewayUntil(_ predicate: () -> Bool) async throws {
    for _ in 0..<200 { if predicate() { return }; try await Task.sleep(for: .milliseconds(10)) }
    throw URLError(.timedOut)
}
@MainActor func gatewayConnect(_ frame: HomeJSON) async throws -> (HomeServerStore,GatewayHTTP,GatewayEvents) {
    let http = GatewayHTTP(frame), events = GatewayEvents()
    let home = HomeServerStore(transport: http, eventTransport: events, defaults: nil, credentialStore: InMemoryCredentials())
    home.endpointText = "https://127.0.0.1:39443"; await home.connect(token: "synthetic-gateway-client-only")
    try await gatewayUntil { events.count == 1 }
    try events.push(frame); try await gatewayUntil { home.receivingPush }
    return (home,http,events)
}

@Suite @MainActor struct HomeGatewayControlTests {
    @Test func actualDevicePartialWSSBlocksOnlyOfflineTargetAndRecoveryDoesNotReplay() async throws {
        let input = try GatewayInputs(), (home,http,events) = try await gatewayConnect(input.frame("initial"))
        defer { home.disconnect() }
        try events.push(input.frame("partial")); try await gatewayUntil { home.devices.first?.raw["availability"]["state"].text == "offline" }
        #expect(home.connected && home.controlsAvailable && home.devices.allSatisfy { $0.raw["can_control"].flag })
        let cursorBefore = home.revision
        await home.controlDevice("lamp-a", command: .object(["action": .string("brightness"), "level": .integer(5)]))
        #expect(await http.writes().isEmpty, "Offline permission is not readiness")
        await home.controlDevice("lamp-b", command: .object(["action": .string("brightness"), "level": .integer(60)]))
        #expect(await http.writes().count == 1)
        try events.push(input.frame("recovered")); try await gatewayUntil { home.devices.first?.raw["availability"]["state"].text == "ready" }
        try await Task.sleep(for: .milliseconds(100))
        #expect(await http.writes().count == 1, "Recovery never replays rejected actions")
        #expect(events.count == 1 && home.revision == cursorBefore, "Local connection epoch must not reconnect/reset global scope")
        await home.controlDevice("lamp-a", command: .object(["action": .string("brightness"), "level": .integer(10)]))
        #expect(await http.writes().count == 2)
    }
    @Test func actualSceneHTTPGateBlocksOnlyRequiredOfflineGateway() async throws {
        let input = try GatewayInputs(), (home,http,events) = try await gatewayConnect(input.frame("partial").withReset())
        defer { home.disconnect() }
        #expect(home.scenes.allSatisfy { $0.raw["can_control"].flag })
        await home.controlScene("later-a", on: nil)
        #expect(await http.writes().isEmpty)
        await home.controlScene("later-b", on: nil)
        #expect(await http.writes().count == 1)
        try events.push(input.frame("recovered")); try await gatewayUntil { home.scenes.first?.raw["action_gate"]["available"] == .bool(true) }
        #expect(await http.writes().count == 1)
        await home.controlScene("later-a", on: nil)
        #expect(await http.writes().count == 2)
    }
    @Test func keypadWithoutOwnHealthChecksMappedMotorAndSceneTargetsIndependently() async throws {
        let input = try GatewayInputs(); var frame = input.frame("partial").withReset()
        var rows = frame["devices"].values
        rows += [GatewayInputs.motor("motor-a", view: rows[0]["availability"]), GatewayInputs.motor("motor-b", view: rows[1]["availability"]), GatewayInputs.panel()]
        frame["devices"] = .array(rows)
        frame["scenes"] = .array(frame["scenes"].values.map { scene in var scene=scene; scene["selected"] = .bool(false); scene["switch_kind"] = .string("selection"); return scene })
        let (home,http,_) = try await gatewayConnect(frame); defer { home.disconnect() }
        #expect(!home.canActivateKeypad("panel", key: 1))
        #expect(home.canActivateKeypad("panel", key: 2))
        #expect(!home.canActivateKeypad("panel", key: 3))
        #expect(home.canActivateKeypad("panel", key: 4))
        await home.controlDevice("motor-a", command: .object(["action": .string("stop")]))
        await home.activateKeypad("panel", key: 1); await home.activateKeypad("panel", key: 3)
        #expect(await http.writes().isEmpty)
        await home.activateKeypad("panel", key: 2); await home.activateKeypad("panel", key: 4)
        #expect(await http.writes().count == 2)
    }
    @Test func explicitParentHealthGatesLEDAndWholePanelSettingsWithoutBorrowingSceneHealth() async throws {
        let input = try GatewayInputs(); var frame=input.frame("partial").withReset(), panel=GatewayInputs.panel()
        // Synthetic forward-compatible parent metadata; runtime7 has not emitted this field yet.
        panel["availability"] = frame["devices"].values[0]["availability"]
        frame["devices"] = .array(frame["devices"].values + [panel])
        let (home,http,events) = try await gatewayConnect(frame); defer { home.disconnect() }
        await home.writeKeypadLED("panel", key: 1, color: .red)
        await home.writeKeypadSetting("panel", command: .object(["action": .string("read_sound")]))
        #expect(await http.writes().isEmpty)
        panel["availability"] = input.frame("recovered")["devices"].values[0]["availability"]
        frame["devices"] = .array(frame["devices"].values.filter { $0["id"].text != "panel" } + [panel])
        frame["cursor"]["sequence"] = .integer(20)
        try events.push(frame); try await gatewayUntil { home.devices.last?.raw["availability"]["state"].text == "ready" }
        #expect(!home.canActivateKeypad("panel", key: 3), "Ready panel does not override offline scene target")
        await home.writeKeypadLED("panel", key: 1, color: .green)
        await home.writeKeypadSetting("panel", command: .object(["action": .string("read_sound")]))
        #expect(await http.writes().count == 2)
    }
}
private extension HomeJSON {
    func withReset() -> HomeJSON { var value=self; value["reset"] = .bool(true); return value }
}
