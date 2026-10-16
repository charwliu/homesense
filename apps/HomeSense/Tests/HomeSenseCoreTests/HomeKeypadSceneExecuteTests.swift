import Foundation
import Testing
@testable import HomeSenseCore

private actor SceneExecuteHTTP: HomeHTTPTransport {
    let base = HomeTestServer()
    var selected: HomeJSON = .null
    var relativeAllowed = false
    var sceneGate: HomeJSON = .object(["available": .bool(true)])
    var canControl = true, scenePresent = true
    var mode = "accepted"
    var posts: [URLRequest] = []
    var held: CheckedContinuation<Void, Never>?
    var hold = false
    func prepare(_ panel: HomeJSON, selected: HomeJSON = .null, relativeAllowed: Bool = false) async {
        await base.setPresentedDevices([panel]); self.selected = selected; self.relativeAllowed = relativeAllowed
    }
    func configureGate(_ variant: String) {
        if variant == "unavailable" { sceneGate = .object(["available": .bool(false)]) }
        if variant == "malformed_gate" { sceneGate = .object(["available": .string("true")]) }
        if variant == "unauthorized" { canControl = false }
        if variant == "missing_scene" { scenePresent = false }
    }
    func configure(_ mode: String) { self.mode = mode }
    func holdNext() { hold = true }
    func release() { hold = false; held?.resume(); held = nil }
    func writes() -> [URLRequest] { posts }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        precondition(request.url?.host == "home.example")
        if request.httpMethod == "POST" {
            posts.append(request)
            if hold { await withCheckedContinuation { held = $0 } }
            if mode == "timeout" { throw URLError(.timedOut) }
            if mode == "denied" { return .init(data: try HomeJSON.object(["error": .string("source_scope_changed")]).data(), status: 403) }
            let body = try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody))
            var response: HomeJSON = .object(["outcome": .string("accepted"), "request_id": body["request_id"]])
            if mode == "wrong_id" { response["request_id"] = .string(UUID().uuidString) }
            if mode == "missing_id" { response = response.removing(["request_id"]) }
            return .init(data: try response.data(), status: 202)
        }
        let response = try await base.send(request)
        guard request.url?.path == "/api/v1/scenes" else { return response }
        var value = try JSONDecoder().decode(HomeJSON.self, from: response.data)
        var scene = try #require(value["scenes"].values.first)
        scene["selected"] = selected; scene["relative_unknown_allowed"] = .bool(relativeAllowed)
        scene["can_control"] = .bool(canControl); scene["action_gate"] = sceneGate
        value["scenes"] = scenePresent ? .array([scene]) : .array([])
        return .init(data: try value.data(), status: response.status)
    }
}

@Suite @MainActor struct HomeKeypadSceneExecuteTests {
    private func panel(action: HomeJSON? = nil) -> HomeJSON {
        var raw = HomeKeypadTests.record().raw, keys = raw["keys"].values
        keys[0]["action"] = action ?? .object(["kind": .string("scene_execute"), "scene": .string("reading"), "path": .string("/api/v1/scene-actions")])
        keys[0]["card_action"] = .null
        raw["keys"] = .array(keys)
        return raw
    }
    private func connected(_ raw: HomeJSON? = nil, selected: HomeJSON = .null, relativeAllowed: Bool = false) async throws -> (HomeServerStore, SceneExecuteHTTP, HomeDiagnostics) {
        let http = SceneExecuteHTTP(); await http.prepare(raw ?? panel(), selected: selected, relativeAllowed: relativeAllowed)
        let log = HomeDiagnostics(fileURL: nil, emitSystemLog: false)
        let home = HomeServerStore(transport: http, defaults: nil, credentialStore: InMemoryCredentials(), diagnostics: log)
        home.pause(); home.endpointText = "https://home.example:8444"; await home.connect(token: "test-token")
        try #require(home.connected)
        return (home, http, log)
    }
    @Test func exactExecuteDescriptorKeepsLEDAndParentAuthoritySeparate() throws {
        let key = try HomeKeypad(record: HomeRecord(panel())).keys[0]
        #expect(key.canActivate && key.canActivateCard && key.executesScene)
        #expect(key.actionScene == "reading" && key.actionDevice == nil && key.motorAction == nil)
        #expect(key.cardAction == .absolute && key.led.actual == .unknown && key.led.lastWritten == .red)
    }
    @Test(arguments: ["extra", "review_evidence", "wrong_path", "external_path", "empty_scene", "whitespace_scene", "number_scene", "missing_kind", "missing_scene", "missing_path", "motor_kind", "motor_fields", "array", "string", "forged_card", "unconfigured", "no_endpoint"])
    func invalidExecuteDescriptorDisablesCardAndDetailsWithoutRequests(_ variant: String) async throws {
        var raw = panel(), keys = raw["keys"].values
        switch variant {
        case "extra": keys[0]["action"]["extra"] = .bool(true)
        case "review_evidence": keys[0]["action"]["review_evidence"] = .string("not part of DTO")
        case "wrong_path": keys[0]["action"]["path"] = .string("/api/v1/scene-relative-actions")
        case "external_path": keys[0]["action"]["path"] = .string("https://untrusted.example/api/v1/scene-actions")
        case "empty_scene": keys[0]["action"]["scene"] = .string("")
        case "whitespace_scene": keys[0]["action"]["scene"] = .string(" \n")
        case "number_scene": keys[0]["action"]["scene"] = .integer(1)
        case "missing_kind": keys[0]["action"] = keys[0]["action"].removing(["kind"])
        case "missing_scene": keys[0]["action"] = keys[0]["action"].removing(["scene"])
        case "missing_path": keys[0]["action"] = keys[0]["action"].removing(["path"])
        case "motor_kind": keys[0]["action"]["kind"] = .string("motor")
        case "motor_fields": keys[0]["action"]["device"] = .string("curtain"); keys[0]["action"]["command"] = .object(["action": .string("open")])
        case "array": keys[0]["action"] = .array([])
        case "string": keys[0]["action"] = .string("scene_execute")
        case "forged_card": keys[0]["card_action"] = .object(["kind": .string("motor_press")])
        case "unconfigured": keys[0]["configured"] = .bool(false)
        case "no_endpoint": keys[0]["endpoint_id"] = .null
        default: Issue.record("Unknown test variant")
        }
        raw["keys"] = .array(keys)
        let key = try HomeKeypad(record: HomeRecord(raw)).keys[0]
        #expect(!key.executesScene && !key.canActivate && !key.canActivateCard)
        let (home, http, _) = try await connected(raw); defer { home.disconnect() }
        #expect(!home.canActivateKeypad("panel", key: 1, card: true) && !home.canActivateKeypad("panel", key: 1))
        await home.activateKeypadCard("panel", key: 1); await home.activateKeypad("panel", key: 1)
        #expect(await http.writes().isEmpty)
    }
    @Test(arguments: [HomeJSON.null, .bool(true), .bool(false)])
    func everyExecuteClickUsesOneExplicitActivationRegardlessOfSelection(_ selected: HomeJSON) async throws {
        let (home, http, log) = try await connected(selected: selected); defer { home.disconnect() }
        let initial = try #require(home.scenes.first).raw
        var operations: [HomeControlOperation] = []
        for card in [true, false, true] {
            #expect(home.canActivateKeypad("panel", key: 1, card: card))
            let op = home.beginKeypadClick("panel", key: 1, card: card); operations.append(op)
            if card { await home.activateKeypadCard("panel", key: 1, operation: op) }
            else { await home.activateKeypad("panel", key: 1, operation: op) }
            #expect(home.scenes.first?.raw == initial)
        }
        let writes = await http.writes(); try #require(writes.count == 3)
        let trace = try log.exportText().split(separator: "\n").dropFirst().map { try JSONDecoder().decode(HomeJSON.self, from: Data($0.utf8)) }
        var ids: Set<String> = []
        for (i, request) in writes.enumerated() {
            let body = try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody))
            #expect(request.url?.path == "/api/v1/scene-actions" && request.httpMethod == "POST")
            #expect(body["scene"].text == "reading" && body["on"] == .bool(true))
            let id = try #require(body["request_id"].text); #expect(UUID(uuidString: id) != nil); ids.insert(id)
            let chain = trace.filter { $0["operation_id"].text == operations[i].id.uuidString }
            #expect(chain.first?["event"].text == "button.click" && chain.last?["event"].text == "operation.complete")
            #expect(chain.contains { $0["event"].text == "scene.decision" && $0["on"] == .bool(true) })
            #expect(chain.filter { $0["event"].text == "http.send" && $0["method"].text == "POST" && $0["request_id"].text == id }.count == 1)
            #expect(chain.contains { $0["event"].text == "http.response" && $0["http_status"] == .integer(202) && $0["request_id"].text == id })
        }
        #expect(ids.count == 3)
        #expect(try HomeKeypad(record: #require(home.devices.first)).keys[0].led.actual == .unknown)
    }
    @Test(arguments: ["unavailable", "malformed_gate", "unauthorized", "missing_scene"])
    func executeRequiresCurrentAuthorizedAvailableScene(_ variant: String) async throws {
        let (home, http, _) = try await connected(); defer { home.disconnect() }
        await http.configureGate(variant); await home.refresh()
        #expect(!home.canActivateKeypad("panel", key: 1, card: true) && !home.canActivateKeypad("panel", key: 1))
        await home.activateKeypadCard("panel", key: 1); await home.activateKeypad("panel", key: 1)
        #expect(await http.writes().isEmpty)
    }
    @Test func executeBusyStaleAndOfflineCallsDoNotQueueOrReplay() async throws {
        let (home, http, _) = try await connected(); defer { home.disconnect() }
        await http.holdNext()
        let first = Task { await home.activateKeypadCard("panel", key: 1) }
        for _ in 0..<1000 { if await http.writes().count == 1 { break }; await Task.yield() }
        try #require(home.busy)
        await home.activateKeypad("panel", key: 1); await home.activateKeypadCard("panel", key: 1)
        #expect(await http.writes().count == 1)
        await http.release(); await first.value
        #expect(!home.busy)
        #expect(await http.writes().count == 1)
        home.pause()
        #expect(!home.snapshotIsCurrent && !home.canActivateKeypad("panel", key: 1, card: true))
        await home.activateKeypadCard("panel", key: 1)
        await home.refresh()
        #expect(home.canActivateKeypad("panel", key: 1, card: true))
        #expect(await http.writes().count == 1, "Refresh never replays blocked requests")
        home.disconnect(); await home.activateKeypadCard("panel", key: 1)
        #expect(await http.writes().count == 1)
    }
    @Test(arguments: ["timeout", "denied", "wrong_id", "missing_id"])
    func failedOrUnmatchedReceiptNeverRetriesOrFabricatesSelection(_ mode: String) async throws {
        let (home, http, _) = try await connected(); defer { home.disconnect() }
        let initial = home.scenes.first?.raw
        await http.configure(mode)
        await home.activateKeypadCard("panel", key: 1)
        #expect(await http.writes().count == 1)
        #expect(home.scenes.first?.raw == initial)
        if ["timeout", "denied"].contains(mode) { #expect(home.error != nil) }
    }
    @Test func legacyToggleStillInvertsKnownSelectionAndBlocksUnknownWithoutPermission() async throws {
        let action: HomeJSON = .object(["kind": .string("scene_toggle"), "scene": .string("reading"), "path": .string("/api/v1/scene-relative-actions")])
        let (home, http, _) = try await connected(panel(action: action), selected: .bool(true)); defer { home.disconnect() }
        #expect(home.canActivateKeypad("panel", key: 1, card: true))
        await home.activateKeypadCard("panel", key: 1)
        let first = try #require(await http.writes().first)
        let body = try JSONDecoder().decode(HomeJSON.self, from: #require(first.httpBody))
        #expect(first.url?.path == "/api/v1/scene-actions" && body["on"] == .bool(false))
        let (unknown, unknownHTTP, _) = try await connected(panel(action: action)); defer { unknown.disconnect() }
        #expect(!unknown.canActivateKeypad("panel", key: 1, card: true))
        await unknown.activateKeypadCard("panel", key: 1); #expect(await unknownHTTP.writes().isEmpty)
        let (relative, relativeHTTP, _) = try await connected(panel(action: action), relativeAllowed: true); defer { relative.disconnect() }
        #expect(relative.canActivateKeypad("panel", key: 1, card: true))
        await relative.activateKeypadCard("panel", key: 1)
        let request = try #require(await relativeHTTP.writes().first)
        let relativeBody = try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody))
        #expect(request.url?.path == "/api/v1/scene-relative-actions" && relativeBody["on"] == .null)
    }
}
