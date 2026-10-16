import Foundation
import Testing
@testable import HomeSenseCore

// Synthetic transport for client admission/one-request tests only. Actual
// backend DTO qualification is separate and must retain provenance.
private actor AwayCardHTTP: HomeHTTPTransport {
    let base: GatewayHTTP
    var requests: [URLRequest] = []
    var mode = "accepted"
    var held = false, arrived = false
    var continuation: CheckedContinuation<Void, Never>?
    init(_ state: HomeJSON) { base = GatewayHTTP(state) }
    func configure(_ mode: String) { self.mode = mode }
    func hold() { held = true }
    func waiting() -> Bool { arrived }
    func release() { held = false; continuation?.resume(); continuation = nil }
    func captured() -> [URLRequest] { requests }
    func writes() -> [URLRequest] { requests.filter { $0.httpMethod == "POST" } }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        requests.append(request)
        if request.httpMethod != "POST" { return try await base.send(request) }
        if held { arrived = true; await withCheckedContinuation { continuation = $0 } }
        switch mode {
        case "timeout": throw URLError(.timedOut)
        case "lost": throw URLError(.networkConnectionLost)
        case "denied": return .init(data: Data("{\"error\":\"source_scope_changed\"}".utf8), status: 403)
        case "stale": return .init(data: Data("{\"error\":\"stale_revision\"}".utf8), status: 409)
        case "malformed": return .init(data: Data("not-json".utf8), status: 202)
        case "unknown": return .init(data: Data("{\"outcome\":\"unknown\",\"retry\":false}".utf8), status: 202)
        default: return .init(data: Data("{\"outcome\":\"accepted\",\"commands\":[\"101\",\"102\"]}".utf8), status: 202)
        }
    }
}

@Suite @MainActor struct HomeKeypadAwayTests {
    private func fixture(key number: Int = 6, parentID: String = "future-entry") throws -> HomeJSON {
        var frame = try GatewayInputs().frame("initial"), panel = GatewayInputs.panel(), keys = panel["keys"].values
        panel["id"] = .string(parentID)
        panel["availability"] = frame["devices"].values[0]["availability"]
        panel["action_gate"] = .object(["available": .bool(true)])
        panel["can_control"] = .bool(false) // Parent identity is not an elevated grant.
        for i in keys.indices {
            keys[i]["action"] = .object(["kind": .string("scene_toggle"), "scene": .string("later-b"), "path": .string("/api/v1/scene-actions")])
            keys[i]["card_action"] = .null
        }
        keys[number - 1]["action"] = .object(["kind": .string("away_off"), "path": .string("/api/v1/away-off")])
        keys[number - 1]["card_action"] = .object(["kind": .string("away_off"), "path": .string("/api/v1/away-off"),
            "command": .object(["action": .string("keypad_press"), "keypad": .string(parentID), "key": .integer(Int64(number))])])
        keys[number - 1]["can_activate"] = .bool(true)
        keys[number - 1]["action_gate"] = .object(["available": .bool(true)])
        panel["keys"] = .array(keys)
        frame["devices"] = .array(frame["devices"].values + [panel])
        frame["scenes"] = .array(frame["scenes"].values.map { var row = $0; row["selected"] = .bool(false); return row })
        return frame
    }
    private func changed(_ frame: HomeJSON, key number: Int = 6, _ edit: (inout HomeJSON, inout HomeJSON) -> Void) -> HomeJSON {
        var result = frame, rows = frame["devices"].values, parent = rows.removeLast(), keys = parent["keys"].values, key = keys[number - 1]
        edit(&parent, &key); keys[number - 1] = key; parent["keys"] = .array(keys)
        result["devices"] = .array(rows + [parent]); return result
    }
    private func connect(_ frame: HomeJSON) async throws -> (HomeServerStore, AwayCardHTTP, GatewayEvents) {
        let http = AwayCardHTTP(frame), events = GatewayEvents(), log = HomeDiagnostics(fileURL: nil, emitSystemLog: false)
        let home = HomeServerStore(transport: http, eventTransport: events, defaults: nil, credentialStore: InMemoryCredentials(), diagnostics: log)
        home.endpointText = "https://127.0.0.1:39443"; await home.connect(token: "synthetic-gateway-client-only")
        try await gatewayUntil { events.count == 1 }; try events.push(frame); try await gatewayUntil { home.receivingPush }
        return (home,http,events)
    }
    private func body(_ request: URLRequest) throws -> HomeJSON { try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody)) }
    private func log(_ home: HomeServerStore, operation: HomeControlOperation) throws -> [HomeJSON] {
        try home.diagnostics.exportText().split(separator: "\n").dropFirst().map { try JSONDecoder().decode(HomeJSON.self, from: Data($0.utf8)) }.filter { $0["operation_id"].text == operation.id.uuidString }
    }
    @Test(arguments: [1,6]) func cardSubmitsOneReviewedIntentAndKeepsLEDAndExplicitDetailsIndependent(_ key: Int) async throws {
        let frame = try fixture(key: key), (home,http,_) = try await connect(frame); defer { home.disconnect() }
        let parent = try #require(home.devices.last), parsed = try HomeKeypad(record: parent).keys[key - 1]
        #expect(parsed.cardAction == .awayOff && parsed.canActivateCard && parsed.actionScene == nil && parsed.actionDevice == nil)
        #expect(parsed.actionDescription == nil && parsed.cardActionDescription != nil)
        #expect(home.canActivateKeypad("future-entry", key: key, card: true))
        #expect(!home.canActivateKeypad("future-entry", key: key))
        await home.activateKeypad("future-entry", key: key)
        #expect(await http.writes().isEmpty, "No new absolute Away entry in Details")
        let operation = home.beginKeypadClick("future-entry", key: key, card: true)
        await home.activateKeypadCard("future-entry", key: key, operation: operation)
        let writes = await http.writes(); try #require(writes.count == 1)
        let request = try body(writes[0])
        #expect(writes[0].url?.path == "/api/v1/away-off")
        #expect(writes[0].value(forHTTPHeaderField: "Authorization") == "Bearer synthetic-gateway-client-only")
        #expect(writes[0].value(forHTTPHeaderField: "Origin") == "https://127.0.0.1:39443")
        #expect(request == .object(["revision": .string(home.revision), "request_id": request["request_id"],
            "command": .object(["action": .string("keypad_press"), "keypad": .string("future-entry"), "key": .integer(Int64(key))])]))
        #expect(request["request_id"].text.flatMap(UUID.init(uuidString:)) != nil && request["request_id"].text != operation.id.uuidString)
        #expect(home.devices.last?.raw == parent.raw, "202 cannot optimistically alter LED, loads or parent state")
        #expect(home.message.contains("已受理") && home.message.contains("实际设备反馈"))
        let chain = try log(home, operation: operation)
        #expect(chain.compactMap { $0["event"].text } == ["button.click","operation.begin","gate.check","http.send","http.response","operation.complete"])
        #expect(chain.first { $0["event"].text == "http.send" }?["path"].text == "/api/v1/away-off")
        #expect(chain.first { $0["event"].text == "http.response" }?["request_id"] == request["request_id"])
        for secret in ["synthetic-gateway-client-only","Authorization","future-entry","127.0.0.1"] { #expect(!home.diagnostics.exportText().contains(secret)) }
    }
    @Test(arguments: ["missing_card","wrong_kind","wrong_action_path","wrong_card_path","extra_action","extra_card","extra_command","wrong_parent","wrong_key","string_key","false_card","missing_action","unconfigured","missing_endpoint"])
    func invalidMetadataCannotFallBackOrExpandScope(_ variant: String) async throws {
        let frame = changed(try fixture()) { _, key in
            switch variant {
            case "missing_card": key["card_action"] = .null
            case "wrong_kind": key["card_action"]["kind"] = .string("motor_press")
            case "wrong_action_path": key["action"]["path"] = .string("https://example.invalid/control")
            case "wrong_card_path": key["card_action"]["path"] = .string("/api/v1/device-commands")
            case "extra_action": key["action"]["devices"] = .array([.string("other")])
            case "extra_card": key["card_action"]["devices"] = .array([.string("other")])
            case "extra_command": key["card_action"]["command"]["devices"] = .array([.string("other")])
            case "wrong_parent": key["card_action"]["command"]["keypad"] = .string("other")
            case "wrong_key": key["card_action"]["command"]["key"] = .integer(5)
            case "string_key": key["card_action"]["command"]["key"] = .string("6")
            case "false_card": key["card_action"] = .bool(false)
            case "missing_action": key["action"] = .null
            case "unconfigured": key["configured"] = .bool(false)
            case "missing_endpoint": key["endpoint_id"] = .null
            default: Issue.record("Unknown variant")
            }
        }
        let (home,http,_) = try await connect(frame); defer { home.disconnect() }
        #expect(!home.canActivateKeypad("future-entry", key: 6, card: true))
        await home.activateKeypadCard("future-entry", key: 6); await home.activateKeypad("future-entry", key: 6)
        #expect(await http.writes().isEmpty)
    }
    @Test(arguments: ["denied","permission_missing","permission_string","scope_unavailable","gate_missing","gate_string","parent_offline"])
    func scopeAndParentReadinessFailClosedAndRecoverWithoutReplay(_ variant: String) async throws {
        let initial = try fixture(), blocked = changed(initial) { parent, key in
            switch variant {
            case "denied": key["can_activate"] = .bool(false)
            case "permission_missing": key = key.removing(["can_activate"])
            case "permission_string": key["can_activate"] = .string("true")
            case "scope_unavailable": key["action_gate"]["available"] = .bool(false)
            case "gate_missing": key = key.removing(["action_gate"])
            case "gate_string": key["action_gate"]["available"] = .string("true")
            case "parent_offline": parent["action_gate"]["available"] = .bool(false)
            default: Issue.record("Unknown variant")
            }
        }
        let (home,http,events) = try await connect(blocked); defer { home.disconnect() }
        #expect(!home.canActivateKeypad("future-entry", key: 6, card: true))
        await home.activateKeypadCard("future-entry", key: 6); #expect(await http.writes().isEmpty)
        var recovered = initial; recovered["cursor"]["sequence"] = .integer(100)
        try events.push(recovered); try await gatewayUntil { home.canActivateKeypad("future-entry", key: 6, card: true) }
        #expect(await http.writes().isEmpty)
        await home.activateKeypadCard("future-entry", key: 6); #expect(await http.writes().count == 1)
    }
    @Test(arguments: ["timeout","lost","denied","stale","malformed","unknown"])
    func failedOrUnknownRequestNeverRetriesOrMarksHouseOff(_ mode: String) async throws {
        let frame = try fixture(), (home,http,events) = try await connect(frame); defer { home.disconnect() }
        await http.configure(mode); let before = home.devices.last?.raw
        let operation = home.beginKeypadClick("future-entry", key: 6, card: true)
        await home.activateKeypadCard("future-entry", key: 6, operation: operation)
        #expect(await http.writes().count == 1 && home.devices.last?.raw == before && !home.busy)
        let chain = try log(home, operation: operation)
        #expect(chain.last?["event"].text == "operation.complete")
        #expect(chain.filter { $0["event"].text == "http.send" }.count == 1)
        if mode == "unknown" { #expect(home.message.contains("结果未知") && home.error == nil) }
        else { #expect(home.error != nil) }
        var recovered = frame; recovered["cursor"]["sequence"] = .integer(100)
        try events.push(recovered); try await Task.sleep(for: .milliseconds(30))
        #expect(await http.writes().count == 1)
    }
    @Test func busyAndOfflineSuppressRequestsAndNewClicksUseFreshUUID() async throws {
        let frame = try fixture(), (home,http,events) = try await connect(frame); defer { home.disconnect() }
        await http.hold(); let first = Task { await home.activateKeypadCard("future-entry", key: 6) }
        for _ in 0..<100 { if await http.waiting() { break }; try await Task.sleep(for: .milliseconds(5)) }
        try #require(await http.waiting())
        #expect(!home.canActivateKeypad("future-entry", key: 6, card: true))
        await home.activateKeypadCard("future-entry", key: 6); #expect(await http.writes().count == 1)
        await http.release(); await first.value
        home.pause(); #expect(!home.controlsAvailable)
        await home.activateKeypadCard("future-entry", key: 6); #expect(await http.writes().count == 1)
        home.resume(); try await gatewayUntil { events.count == 2 }
        var recovered = frame; recovered["cursor"]["sequence"] = .integer(100)
        try events.push(recovered); try await gatewayUntil { home.canActivateKeypad("future-entry", key: 6, card: true) }
        #expect(await http.writes().count == 1)
        await home.activateKeypadCard("future-entry", key: 6)
        let writes = await http.writes(); try #require(writes.count == 2)
        #expect(try body(writes[0])["request_id"] != body(writes[1])["request_id"])
    }
    @Test func hiddenDailyRowsRemainAuthorizedSceneFactsAndDefaultVisibilityIsPreserved() async throws {
        var frame = try fixture(), rows = frame["devices"].values
        let hiddenIndex = try #require(rows.firstIndex { $0["id"].text == "lamp-b" })
        rows[hiddenIndex]["catalog_visible"] = .bool(false)
        rows[hiddenIndex]["room"] = .object(["id": .string("test-room")])
        frame["devices"] = .array(rows)
        var scenes = frame["scenes"].values
        let sceneIndex = try #require(scenes.firstIndex { $0["id"].text == "later-b" })
        scenes[sceneIndex]["devices"] = .array([.string("lamp-b")])
        frame["scenes"] = .array(scenes)
        let (home,http,events) = try await connect(frame); defer { home.disconnect() }
        #expect(home.devices.contains { $0.id == "lamp-b" })
        #expect(!home.dailyDevices.contains { $0.id == "lamp-b" })
        #expect(home.dailyDevices.count == home.devices.count - 1)
        #expect(home.dailyDevices.filter { $0.raw["room"]["id"].text == "test-room" }.isEmpty)
        #expect(home.dailyDevices.contains { $0.id == "future-entry" }, "Missing flag retains existing parent card")
        #expect(home.canControlDevice("lamp-b") && home.canActivateKeypad("future-entry", key: 1, card: true))
        await home.activateKeypadCard("future-entry", key: 1)
        #expect(await http.writes().count == 1, "A hidden target remains available to an authorized scene")
        rows[hiddenIndex]["catalog_visible"] = .bool(true)
        frame["devices"] = .array(rows); frame["cursor"]["sequence"] = .integer(100)
        try events.push(frame); try await gatewayUntil { home.dailyDevices.contains { $0.id == "lamp-b" } }
        #expect(home.dailyDevices.count == home.devices.count && home.canControlDevice("lamp-b"))
        #expect(await http.writes().count == 1, "Changing visibility does not send or replay controls")
    }
    @Test func existingFiveSceneKeysKeepExplicitToggleAndLEDProjection() async throws {
        let frame = try fixture(), (home,http,_) = try await connect(frame); defer { home.disconnect() }
        let original = home.devices.last?.raw
        for number in 1...5 {
            #expect(home.canActivateKeypad("future-entry", key: number, card: true))
            await home.activateKeypadCard("future-entry", key: number)
        }
        let writes = await http.writes(); try #require(writes.count == 5)
        for request in writes {
            let requestBody = try body(request)
            #expect(request.url?.path == "/api/v1/scene-actions" && requestBody["on"] == .bool(true))
        }
        #expect(home.devices.last?.raw == original)
    }
}

private actor AwayDraftHTTP: HomeHTTPTransport {
    let base = HomeTestServer()
    static var panel: HomeJSON { .object([
        "id": .string("entry-draft"), "name": .string("Entry"), "module_id": .string("panel-module"), "enabled": .bool(true),
        "keys": .array([.object(["number": .integer(6), "label": .string("离家"),
            "led_endpoint": .object(["id": .string("entry-key-6"), "name": .string("Existing LED"), "enabled": .bool(true)]),
            "action": .object(["kind": .string("away_off"), "review_evidence": .string("Synthetic approved mapping"), "indicator_reset": .string("green")])])])]) }
    func captured() async -> [URLRequest] { await base.captured() }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        let response = try await base.send(request)
        guard request.url?.path.hasSuffix("/export") == true else { return response }
        var value = try JSONDecoder().decode(HomeJSON.self, from: response.data)
        value["devices"]["keypads"] = .array([Self.panel])
        var devices = value["devices"]["devices"].values
        devices[0]["catalog_visible"] = .bool(false)
        value["devices"]["devices"] = .array(devices)
        return .init(data: try value.data(), status: response.status)
    }
}

@Suite @MainActor struct HomeAwayDraftPreservationTests {
    @Test func renamePreservesReviewedAwayAndHiddenCatalogBindingWithoutCreatingScope() async throws {
        let http = AwayDraftHTTP(), home = HomeServerStore(transport: http, defaults: nil, credentialStore: InMemoryCredentials())
        home.pause(); home.endpointText = "https://home.example:8444"; await home.connect(token: "test-token")
        defer { home.disconnect() }
        try #require(home.canManage && home.editableKeypads.count == 1)
        let beforeRequests = await http.captured().count
        let original = try #require(home.editableKeypads.first?.raw)
        var renamed = original; renamed["name"] = .string("Renamed entry")
        try home.upsertKeypad(renamed, originalID: "entry-draft")
        #expect(home.editableKeypads.first?.raw["keys"] == original["keys"])
        var hidden = try #require(home.editableDevices.first?.raw); hidden["name"] = .string("Renamed hidden coverage")
        try home.upsertDevice(hidden, originalID: hidden["id"].text)
        #expect(home.editableDevices.first?.raw["catalog_visible"] == .bool(false))
        #expect(await http.captured().count == beforeRequests, "Draft edits issue no network writes or field actions")
        var newPanel = original; newPanel["id"] = .string("copy")
        #expect(throws: HomeServerError.self) { try home.upsertKeypad(newPanel) }
        var moved = original, keys = moved["keys"].values; keys[0]["number"] = .integer(5); moved["keys"] = .array(keys)
        #expect(throws: HomeServerError.self) { try home.upsertKeypad(moved, originalID: "entry-draft") }
        var altered = original; keys = altered["keys"].values; keys[0]["action"]["devices"] = .array([.string("other")]); altered["keys"] = .array(keys)
        #expect(throws: HomeServerError.self) { try home.upsertKeypad(altered, originalID: "entry-draft") }
        #expect(home.editableKeypads.first?.raw["keys"] == original["keys"])
    }
}
