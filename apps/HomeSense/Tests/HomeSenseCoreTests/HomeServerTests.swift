import Foundation
import Testing
@testable import HomeSenseCore

actor HomeTestServer: HomeHTTPTransport {
    var requests: [URLRequest] = []
    var administrator = true
    var moduleEvents = false
    var failAuthentication = false
    var mismatchScenes = false
    var dropWrites = false
    var networkFailure: URLError.Code?
    var malformedSession = false
    var sceneSelected: HomeJSON = .null
    var relativeUnknownAllowed = true
    var active: Int64 = 9_007_199_254_740_993
    var saved: [String: HomeJSON] = [:]
    var presentedDevices: [HomeJSON]?
    var devicesDocument: HomeJSON = .object([
        "schema_version": .integer(2), "revision": .integer(9_007_199_254_740_993), "manual_degraded_enabled": .bool(false),
        "rooms": .array([.object(["id": .string("study"), "name": .string("书房"), "floor": .string("一层")])]),
        "gateways": .array([.object(["id": .string("bus"), "endpoint": .string("127.0.0.1:50007"), "modules": .array([
            .object(["id": .string("module"), "address": .string("15"), "profile": .string("gt_d8_rl_v2_2_6_2"), "verification": .object(["status": .string("simulated")])])
        ])])]),
        "devices": .array([.object(["id": .string("light"), "name": .string("书房灯"), "room": .string("study"),
                                   "module_id": .string("module"), "channel": .integer(1), "enabled": .bool(true), "default_on_level": .integer(35)])])
    ])
    var automation: HomeJSON = .object([
        "schema_version": .integer(1), "configuration_revision": .integer(9_007_199_254_740_993),
        "variables": .array([]), "rules": .array([]),
        "immediate_scenes": .array([.object(["id": .string("reading"), "enabled": .bool(false), "review_evidence": .string("Synthetic test only"), "activate": .array([])])])
    ])
    func setModuleEvents() { moduleEvents = true }
    func setAdministrator(_ value: Bool) { administrator = value }
    func setUnauthorized() { failAuthentication = true }
    func setMismatch() { mismatchScenes = true }
    func setDropWrites() { dropWrites = true }
    func setNetworkFailure(_ code: URLError.Code?) { networkFailure = code }
    func setMalformedSession() { malformedSession = true }
    func setSceneSelected(_ value: HomeJSON, relativeAllowed: Bool) { sceneSelected = value; relativeUnknownAllowed = relativeAllowed }
    func setPresentedDevices(_ rows: [HomeJSON]) { presentedDevices = rows }
    func advanceRevision() { active += 1 }
    func captured() -> [URLRequest] { requests }
    func currentDevices() -> HomeJSON { devicesDocument }
    func setLargeAutomation() {
        let extra = (0..<144).map { index in
            HomeJSON.object(["id": .string("synthetic-disabled-\(index)"), "enabled": .bool(false),
                "review_evidence": .string(String(repeating: "x", count: 6000)), "activate": .array([])])
        }
        automation["immediate_scenes"] = .array(automation["immediate_scenes"].values + extra)
    }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        requests.append(request)
        if let networkFailure { throw URLError(networkFailure) }
        func response(_ value: HomeJSON, _ status: Int = 200) throws -> HomeHTTPResponse {
            HomeHTTPResponse(data: try value.data(), status: status)
        }
        guard request.value(forHTTPHeaderField: "Origin") == "https://home.example:8444",
              request.value(forHTTPHeaderField: "Authorization") == "Bearer test-token", !failAuthentication else {
            return try response(.object(["error": .string("authentication_required")]), 401)
        }
        let path = request.url!.path, method = request.httpMethod ?? "GET"
        if method == "POST", dropWrites { throw URLError(.networkConnectionLost) }
        let body = try request.httpBody.map { try JSONDecoder().decode(HomeJSON.self, from: $0) } ?? .null
        if path == "/api/v1/session" { return try response(.object(["can_manage_configuration": malformedSession ? .null : .bool(administrator), "capabilities": .object(["module_events": moduleEvents ? .object(["version": .integer(1)]) : .null])])) }
        if path == "/api/v1/devices" {
            return try response(.object(["boot": .string("synthetic-boot"), "revision": .integer(active), "revision_text": .string(String(active)),
                "devices": .array(presentedDevices ?? devicesDocument["devices"].values.map {
                    .object(["id": $0["id"], "name": $0["name"], "kind": .string("Dimmer"), "can_control": .bool(true),
                             "quality": .string("Confirmed"), "observed": .object(["on": .bool(false)])])
                })]))
        }
        if path == "/api/v1/rooms" { return try response(.object(["boot": .string("synthetic-boot"), "revision": .string(String(active)), "rooms": devicesDocument["rooms"], "floors": devicesDocument["floors"]])) }
        if path == "/api/v1/scenes" {
            return try response(.object(["revision": .string(String(active + (mismatchScenes ? 1 : 0))), "scenes": .array([
                .object(["id": .string("reading"), "name": .string("阅读"), "can_control": .bool(true), "switch_kind": .string("selection"), "selected": sceneSelected, "relative_unknown_allowed": .bool(relativeUnknownAllowed)])
            ])]))
        }
        if ["/api/v1/device-commands", "/api/v1/scene-actions", "/api/v1/scene-relative-actions", "/api/v1/keypad-settings"].contains(path) {
            guard method == "POST", body["request_id"].text.flatMap(UUID.init(uuidString:)) != nil else {
                return try response(.object(["error": .string("invalid_request")]), 400)
            }
            return try response(.object(["outcome": .string("accepted"), "status": .string("pending")]))
        }
        if path == "/api/v1/config/versions" { return try response(.object(["active_revision": .string(String(active)), "versions": .array([])])) }
        if path.hasSuffix("/export") {
            return try response(.object(["active_revision": .string(String(active)), "revision": .string(String(active)),
                                         "devices": devicesDocument, "automation": automation]))
        }
        if path == "/api/v1/config/drafts" { return try response(.object(["drafts": .array(saved.keys.sorted().map { .object(["id": .string($0)]) })])) }
        if path.hasPrefix("/api/v1/config/drafts/") {
            let parts = path.split(separator: "/")
            let id = String(parts[4])
            if method == "POST" {
                guard request.value(forHTTPHeaderField: "Idempotency-Key").flatMap(UUID.init(uuidString:)) != nil,
                      body["expected_revision"].integerText == String(active) else {
                    return try response(.object(["error": .string("configuration_changed")]), 409)
                }
                saved[id] = body
                return try response(.object(["draft": .string(id), "active": .bool(false)]))
            }
            guard let draft = saved[id] else { return try response(.object(["error": .string("draft_not_found")]), 404) }
            return try response(.object(["id": .string(id), "expected_revision": .string(draft["expected_revision"].integerText!),
                "revision": .string(draft["revision"].integerText!), "devices": .string(draft["devices"].formatted), "automation": .string(draft["automation"].formatted)]))
        }
        if path.hasPrefix("/api/v1/config/previews/") {
            let id = String(path.split(separator: "/").last!)
            guard let draft = saved[id] else { return try response(.object(["error": .string("draft_not_found")]), 404) }
            return try response(.object(["draft": .string(id), "impact": .object(["boot": .string("synthetic-boot"),
                "active_revision": .integer(active), "candidate_revision": draft["revision"], "device_changes": .array([]), "interrupted_runs": .array([])])]))
        }
        if path.hasPrefix("/api/v1/config/activations/") {
            let id = String(path.split(separator: "/").last!)
            guard method == "POST", let draft = saved[id], body["boot"].text == "synthetic-boot" else {
                return try response(.object(["error": .string("configuration_changed")]), 409)
            }
            active = Int64(draft["revision"].integerText!)!
            devicesDocument = draft["devices"]; automation = draft["automation"]
            return try response(.object(["outcome": .string("activated"), "revision": .string(String(active))]))
        }
        return try response(.object(["error": .string("not_found")]), 404)
    }
}

@Suite
@MainActor
struct HomeServerTests {
    private func connected(_ server: HomeTestServer) async -> HomeServerStore {
        let home = HomeServerStore(transport: server, defaults: nil, credentialStore: InMemoryCredentials())
        home.pause(); home.endpointText = "https://home.example:8444"
        await home.connect(token: "test-token")
        return home
    }
    @Test func presentationModesPreserveDraftsAndActiveStateWithoutRequests() async throws {
        let server = HomeTestServer(), home = await connected(server)
        #expect(home.interfaceMode == .daily)
        try home.upsertRoom(id: "unpublished", name: "未发布房间", floor: "未发布楼层")
        try home.renameFloor("一层", to: "编辑楼层")
        await home.saveDraft(); await home.previewDraft()
        let savedID = try #require(home.draftID), preview = try #require(home.preview)
        // Keep an unsaved edit as well as a saved draft and its preview.
        try home.upsertRoom(id: "second", name: "未保存房间", floor: "候选楼层")
        let document = home.deviceDocument, automation = home.automationDocument
        let requests = await server.captured().count
        let synchronization = home.synchronization
        for mode in [HomeInterfaceMode.management, .daily, .management, .daily] {
            home.selectInterfaceMode(mode)
            #expect(home.interfaceMode == mode)
            #expect(home.rooms.map(\.id) == ["study"])
            #expect(home.activeFloors == ["一层"])
            #expect(home.devices.first?.raw["observed"]["on"] == .bool(false))
            #expect(home.deviceDocument == document && home.automationDocument == automation)
            #expect(home.draftChanged && home.connected && home.canManage)
            #expect(home.synchronization == synchronization)
        }
        #expect(await server.captured().count == requests)
        #expect(savedID != "" && preview.isObject)
        // Explicit daily controls still use the existing authenticated endpoint.
        await home.controlDevice("light", command: .object(["action": .string("power"), "on": .bool(true)]))
        #expect(home.error == nil)
        #expect(await server.captured().dropFirst(requests).filter { $0.httpMethod == "POST" }.count == 1)
        #expect(await server.captured().dropFirst(requests).allSatisfy { $0.value(forHTTPHeaderField: "Authorization") == "Bearer test-token" })
        #expect(home.devices.first?.raw["observed"]["on"] == .bool(false))
        home.disconnect()
        #expect(home.interfaceMode == .daily && !home.controlsAvailable)
    }
    @Test func savedPreviewAndMemberModeAuthorizationArePreserved() async throws {
        let server = HomeTestServer(), home = await connected(server)
        try home.upsertRoom(id: "candidate", name: "候选房间", floor: "候选楼层")
        await home.saveDraft(); await home.previewDraft()
        let savedID = try #require(home.draftID), preview = try #require(home.preview)
        let requests = await server.captured().count
        home.selectInterfaceMode(.management); home.selectInterfaceMode(.daily)
        #expect(home.draftID == savedID && home.preview == preview && !home.draftChanged)
        #expect(await server.captured().count == requests)
        await server.setAdministrator(false)
        let member = await connected(server)
        let memberRequests = await server.captured().count
        member.selectInterfaceMode(.management)
        #expect(member.interfaceMode == .daily && member.availableInterfaceModes == [.daily])
        #expect(await server.captured().count == memberRequests)
        #expect(member.deviceDocument == nil)
    }
    @Test func administratorDowngradeReturnsToDailyWithoutWriting() async throws {
        let server = HomeTestServer(), home = await connected(server)
        defer { home.disconnect() }
        home.selectInterfaceMode(.management)
        try home.upsertRoom(id: "candidate", name: "候选房间", floor: "候选楼层")
        let count = await server.captured().count
        await server.setAdministrator(false)
        await home.refresh()
        #expect(home.interfaceMode == .daily && !home.canManage)
        #expect(home.deviceDocument == nil && home.automationDocument == nil)
        #expect(home.availableInterfaceModes == [.daily])
        #expect(await server.captured().dropFirst(count).allSatisfy { $0.httpMethod == "GET" })
    }
    @Test func endpointAndCredentialBoundaries() throws {
        for value in ["http://home.example", "https://user:password@home.example", "https://home.example/api", "https://home.example/?token=x", "https://home.example/#x", "ngr"] {
            #expect(throws: HomeServerError.self) { try HomeServerEndpoint(value) }
        }
        let endpoint = try HomeServerEndpoint("https://home.example:8444/")
        let request = try endpoint.request(path: "/api/v1/config/drafts/\(UUID())", method: "POST", token: "test-token", body: .object([:]))
        #expect(request.value(forHTTPHeaderField: "Origin") == "https://home.example:8444")
        #expect(request.value(forHTTPHeaderField: "Host") == "home.example:8444")
        #expect(request.value(forHTTPHeaderField: "Authorization") == "Bearer test-token")
        #expect(request.value(forHTTPHeaderField: "Idempotency-Key").flatMap(UUID.init(uuidString:)) != nil)
        #expect(request.value(forHTTPHeaderField: "Cookie") == nil)
        #expect(!request.url!.absoluteString.contains("test-token"))
    }
    @Test func numericConfigurationTokensRoundTripWithoutDouble() throws {
        let document = try HomeJSON.parse("{\"large\":9223372036854775807,\"other\":9007199254740993,\"unsigned\":18446744073709551615,\"decimal\":0.125}")
        let encoded = String(data: try document.data(), encoding: .utf8)!
        #expect(encoded.contains("9223372036854775807"))
        #expect(encoded.contains("9007199254740993"))
        #expect(encoded.contains("18446744073709551615"))
        #expect(try HomeJSON.parse(encoded) == document)
    }
    @Test func fullHomeDraftCanBeReloadedWithoutLosingSourceOrPublishing() async throws {
        let server = HomeTestServer()
        await server.setLargeAutomation()
        let home = await connected(server)
        await home.loadActiveConfiguration()
        let original = try #require(home.automationDocument)
        #expect(try original.data().count > 800_000)
        let active = home.revision
        await home.saveDraft()
        let id = try #require(home.draftID)
        await home.loadActiveConfiguration()
        #expect(home.draftID == nil)
        await home.loadDraft(id)
        #expect(home.error == nil)
        #expect(home.draftID == id)
        #expect(home.automationDocument?["immediate_scenes"] == original["immediate_scenes"])
        #expect(home.automationDocument?["configuration_revision"].integerText == home.candidateRevision)
        #expect(home.revision == active)
        #expect(await server.captured().allSatisfy { !($0.url?.path.contains("/config/activations/") ?? false) })
        home.pause()
    }
    @Test func completeConfigurationParsingKeepsInlineAndUTF8SizeBoundaries() throws {
        let document = HomeJSON.object(["review": .string(String(repeating: "x", count: 300_000)),
                                       "revision": .unsigned(UInt64.max)])
        let source = document.formatted
        #expect(throws: HomeServerError.self) { try HomeJSON.parse(source) }
        #expect(try HomeJSON.parseConfiguration(source) == document)
        let oversized = HomeJSON.object(["review": .string(String(repeating: "界", count: 1_400_000))]).formatted
        #expect(oversized.count < 4 * 1024 * 1024)
        #expect(oversized.utf8.count > 4 * 1024 * 1024)
        #expect(throws: HomeServerError.self) { try HomeJSON.parseConfiguration(oversized) }
    }
    @Test func managementEditsAreDraftOnlyUntilPreviewAndPublication() async throws {
        let server = HomeTestServer(), home = await connected(server)
        #expect(home.connected && home.canManage)
        #expect(home.revision == "9007199254740993")
        #expect(home.candidateRevision == "9007199254740994")
        #expect(await server.captured().allSatisfy { $0.httpMethod == "GET" })
        try home.upsertRoom(id: "bedroom", name: "卧室", floor: "二层")
        try home.renameFloor("一层", to: "首层")
        var device = try #require(home.editableDevices.first).raw
        device["name"] = .string("阅读灯"); device["room"] = .string("bedroom")
        try home.upsertDevice(device, originalID: "light")
        #expect(home.rooms.count == 1)
        #expect(home.editableRooms.count == 2)
        #expect(await server.currentDevices()["rooms"].values.count == 1)
        await home.saveDraft()
        #expect(home.error == nil)
        let id = try #require(home.draftID)
        #expect(!home.draftChanged)
        let savedRequest = try #require(await server.captured().last { $0.url!.path == "/api/v1/config/drafts/\(id)" })
        let text = String(data: savedRequest.httpBody!, encoding: .utf8)!
        #expect(text.contains("\"expected_revision\":9007199254740993"))
        #expect(home.deviceDocument?["devices"].values[0]["default_on_level"] == .integer(35))
        await home.previewDraft()
        #expect(home.preview != nil)
        await home.publishDraft()
        #expect(home.error == nil)
        #expect(home.revision == "9007199254740994")
        #expect(home.rooms.count == 2)
        #expect(home.devices.first?.name == "阅读灯")
        #expect(home.preview == nil)
        let posts = await server.captured().filter { $0.httpMethod == "POST" }
        #expect(posts.count == 2)
    }
    @Test func editInvalidatesSavedPreviewAndStaleBaseCannotPublish() async throws {
        let server = HomeTestServer(), home = await connected(server)
        await home.saveDraft(); await home.previewDraft()
        #expect(home.preview != nil)
        try home.upsertRoom(id: "study", name: "新书房", floor: "一层")
        #expect(home.preview == nil && home.draftID == nil && home.draftChanged)
        await home.publishDraft()
        #expect(await server.captured().filter { $0.url!.path.contains("activations") }.isEmpty)
        await server.advanceRevision(); await home.refresh()
        #expect(home.configurationStale)
        #expect(throws: HomeServerError.self) { try home.renameFloor("一层", to: "二层") }
    }
    @Test func physicalRebindingInvalidatesCalibrationButMetadataDoesNot() async throws {
        let server = HomeTestServer(), home = await connected(server)
        var row = try #require(home.editableDevices.first).raw
        row["motor_calibration"] = .object(["evidence": .string("old binding")])
        row["curtain_state"] = .object(["old": .bool(true)])
        try home.upsertDevice(row, originalID: "light")
        row["name"] = .string("重命名")
        try home.upsertDevice(row, originalID: "light")
        #expect(home.editableDevices[0].raw["motor_calibration"].isObject)
        row["channel"] = .integer(2)
        try home.upsertDevice(row, originalID: "light")
        #expect(home.editableDevices[0].raw["motor_calibration"] == .null)
        #expect(home.editableDevices[0].raw["curtain_state"] == .null)
        #expect(throws: HomeServerError.self) { try home.removeRoom("study") }
    }
    @Test func sceneEditingPreservesAutomationAndDeviceCommandsUseServerOnly() async throws {
        let server = HomeTestServer(), home = await connected(server)
        let scene = try #require(home.editableScenes.first)
        var row = scene.raw.removing(["name", "collection"])
        row["enabled"] = .bool(true)
        try home.upsertScene(row, collection: "immediate_scenes", name: "阅读模式", originalID: scene.id)
        #expect(home.automationDocument?["rules"] == .array([]))
        #expect(home.editableScenes.first?.name == "阅读模式")
        await home.controlDevice("light", command: .object(["action": .string("power"), "on": .bool(true)]))
        await home.controlScene("reading", on: true)
        let commands = await server.captured().filter { $0.httpMethod == "POST" }
        #expect(commands.count == 2)
        #expect(commands.allSatisfy { $0.url!.path == "/api/v1/device-commands" || $0.url!.path == "/api/v1/scene-actions" })
    }
    @Test func revokedOrInconsistentReadsDisableControl() async throws {
        let server = HomeTestServer(), home = await connected(server)
        #expect(home.controlsAvailable)
        await server.setMismatch(); await home.refresh()
        #expect(home.error != nil)
        #expect(!home.controlsAvailable)
        await server.setUnauthorized(); await home.refresh()
        #expect(!home.connected)
        #expect(home.devices.isEmpty && home.deviceDocument == nil)
    }
    @Test func nonAdministratorCannotReadOrEditConfiguration() async throws {
        let server = HomeTestServer()
        await server.setAdministrator(false)
        let home = await connected(server)
        #expect(home.connected && !home.canManage)
        #expect(home.deviceDocument == nil)
        #expect(await server.captured().allSatisfy { !$0.url!.path.contains("config/") })
        #expect(throws: HomeServerError.self) { try home.upsertRoom(id: "x", name: "房间", floor: nil) }
    }
    @Test func lostControlResponseIsUncertainAndNeverAutomaticallyRetried() async throws {
        let server = HomeTestServer(), home = await connected(server)
        await server.setDropWrites()
        await home.controlDevice("light", command: .object(["action": .string("power"), "on": .bool(true)]))
        #expect(home.error != nil)
        #expect(home.message.contains("结果未知"))
        #expect(!home.controlsAvailable)
        #expect(await server.captured().filter { $0.httpMethod == "POST" }.count == 1)
    }
}
