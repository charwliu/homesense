import Foundation
import Testing
@testable import HomeSenseCore

private actor SceneRoomHTTP: HomeHTTPTransport {
    let base = HomeTestServer()
    var scenes: [HomeJSON] = [
        .object(["id": .string("reading"), "name": .string("阅读"), "room": .object(["id": .string("study"), "name": .string("书房"), "floor": .string("一层")]), "can_control": .bool(true), "selected": .null]),
        .object(["id": .string("all-off"), "name": .string("全关"), "room": .null]),
        .object(["id": .string("legacy"), "name": .string("书房旧场景")]),
        .object(["id": .string("unknown-room"), "name": .string("未知归属"), "room": .object(["id": .string("missing"), "name": .string("不存在的房间")])])
    ]
    var metadata: [HomeJSON] = [.object(["id": .string("reading"), "name": .string("阅读"), "room": .string("study"), "future": .object(["keep": .bool(true)])])]
    var presentations: [HomeJSON] = []
    func setPresentations(_ rows: [HomeJSON]) { presentations = rows }
    func setScenes(_ rows: [HomeJSON]) { scenes = rows }
    func setMetadata(_ rows: [HomeJSON]) { metadata = rows }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        let response = try await base.send(request)
        guard response.status == 200 else { return response }
        var body = try JSONDecoder().decode(HomeJSON.self, from: response.data)
        if request.url?.path == "/api/v1/scenes" { body["scenes"] = .array(scenes) }
        if request.url?.path.hasSuffix("/export") == true {
            body["automation"]["scene_metadata"] = .array(metadata)
            body["automation"]["scene_presentations"] = .array(presentations)
            body["automation"]["timed_scenes"] = .array([.object(["id": .string("later"), "enabled": .bool(false), "steps": .array([])])])
        }
        return HomeHTTPResponse(data: try body.data(), status: response.status)
    }
}

private final class SceneRoomEvents: HomeEventTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var subscriber: AsyncThrowingStream<HomeEventMessage, Error>.Continuation?
    var ready: Bool { lock.withLock { subscriber != nil } }
    func events(_ request: URLRequest) -> AsyncThrowingStream<HomeEventMessage, Error> {
        AsyncThrowingStream { continuation in lock.withLock { subscriber = continuation } }
    }
    func push(_ frame: HomeJSON) throws { lock.withLock { subscriber }?.yield(.state(try frame.data())) }
}

@Suite @MainActor
struct HomeSceneRoomTests {
    private func connect(_ server: SceneRoomHTTP, events: (any HomeEventTransport)? = nil) async -> HomeServerStore {
        let home = HomeServerStore(transport: server, eventTransport: events ?? HomeWebSocketTransport(), defaults: nil, credentialStore: InMemoryCredentials())
        if events == nil { home.pause() }
        home.endpointText = "https://home.example:8444"; await home.connect(token: "test-token")
        return home
    }
    private func waitFor(_ predicate: () -> Bool) async throws {
        for _ in 0..<200 {
            if predicate() { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        Issue.record("Scene room state did not arrive")
    }
    @Test func explicitRoomsAndLegacyHouseholdFallbackNeverInferFromNamesOrDrafts() async throws {
        let server = SceneRoomHTTP(), home = await connect(server)
        defer { home.disconnect() }
        let count = await server.base.captured().count
        #expect(home.scenes(inRoom: "study").map(\.id) == ["reading"])
        #expect(home.householdScenes.map(\.id) == ["all-off", "legacy", "unknown-room"])
        #expect(home.scenes(inRoom: "missing").isEmpty)
        try home.upsertRoom(id: "missing", name: "尚未发布", floor: "二层")
        #expect(home.scenes(inRoom: "missing").isEmpty)
        #expect(home.householdScenes.contains { $0.id == "unknown-room" })
        home.selectInterfaceMode(.management); home.selectInterfaceMode(.daily)
        #expect(home.scenes.count == 4 && home.editableScenes.count == 2)
        #expect(await server.base.captured().count == count)
        #expect(await server.base.captured().allSatisfy { $0.httpMethod == "GET" })
    }
    @Test func metadataRoomEditsPreserveProgramsUnknownFieldsAndActiveViewsUntilPublication() async throws {
        let server = SceneRoomHTTP(), home = await connect(server)
        defer { home.disconnect() }
        let automation = try #require(home.automationDocument)
        let scene = try #require(home.editableScenes.first { $0.id == "reading" })
        #expect(scene.roomID == "study")
        try home.upsertRoom(id: "new-room", name: "新房间", floor: "二层")
        try home.upsertScene(scene.raw, collection: "immediate_scenes", name: "新阅读名", originalID: scene.id, room: "new-room")
        let updated = try #require(home.automationDocument)
        #expect(updated["immediate_scenes"] == automation["immediate_scenes"])
        #expect(updated["timed_scenes"] == automation["timed_scenes"])
        #expect(updated["rules"] == automation["rules"] && updated["variables"] == automation["variables"])
        let metadata = try #require(updated["scene_metadata"].values.first)
        #expect(metadata["future"] == .object(["keep": .bool(true)]))
        #expect(metadata["room"].text == "new-room")
        #expect(home.scenes(inRoom: "study").map(\.id) == ["reading"])
        #expect(home.scenes(inRoom: "new-room").isEmpty)
        #expect(await server.base.captured().allSatisfy { $0.httpMethod == "GET" })
        #expect(throws: HomeServerError.self) { try home.removeRoom("new-room") }
        await home.saveDraft()
        #expect(home.error == nil)
        let post = try #require(await server.base.captured().last { $0.httpMethod == "POST" })
        #expect(post.url?.path.contains("/api/v1/config/drafts/") == true)
        let body = try JSONDecoder().decode(HomeJSON.self, from: #require(post.httpBody))
        #expect(body["automation"]["scene_metadata"] == updated["scene_metadata"])
        #expect(body["automation"]["immediate_scenes"] == automation["immediate_scenes"])
        #expect(await server.base.captured().filter { $0.httpMethod == "POST" }.count == 1)
    }
    @Test func clearRenameTimedScenesAndInvalidRoomEditsKeepDraftValidationAtomic() async throws {
        let server = SceneRoomHTTP(), home = await connect(server)
        defer { home.disconnect() }
        let reading = try #require(home.editableScenes.first { $0.id == "reading" })
        try home.upsertScene(reading.raw, collection: "immediate_scenes", name: "仅改名", originalID: reading.id)
        #expect(home.editableScenes.first { $0.id == reading.id }?.roomID == "study")
        try home.upsertScene(reading.raw, collection: "immediate_scenes", name: "全屋阅读", originalID: reading.id, room: "")
        #expect(home.editableScenes.first { $0.id == reading.id }?.roomID == nil)
        let later = try #require(home.editableScenes.first { $0.id == "later" })
        try home.upsertScene(later.raw, collection: "timed_scenes", name: "定时", originalID: later.id, room: "study")
        #expect(home.editableScenes.first { $0.id == later.id }?.roomID == "study")
        await home.saveDraft(); await home.previewDraft()
        let document = home.automationDocument, preview = home.preview, draftID = home.draftID
        for room in ["missing", " study ", "\n"] {
            #expect(throws: HomeServerError.self) { try home.upsertScene(reading.raw, collection: "immediate_scenes", name: "非法", originalID: reading.id, room: room) }
            #expect(home.automationDocument == document && home.preview == preview && home.draftID == draftID)
        }
        await server.base.setAdministrator(false)
        await home.refresh()
        #expect(!home.canManage && home.editableScenes.isEmpty)
        #expect(throws: HomeServerError.self) { try home.upsertScene(reading.raw, collection: "immediate_scenes", name: "无权限", originalID: reading.id, room: "study") }
    }
    @Test func dedupUsesOnlyDirectBindingsOnVisibleValidCardsAndKeepsStateStable() async throws {
        let server = SceneRoomHTTP()
        var panel = HomeKeypadTests.record().raw
        var keys = panel["keys"].values
        keys[0]["can_activate"] = .bool(false)
        keys[1]["action"] = .object(["kind": .string("scene_execute"), "scene": .string("all-off"), "path": .string("/api/v1/scene-actions")])
        panel["keys"] = .array(keys)
        await server.base.setPresentedDevices([panel])
        let home = await connect(server)
        defer { home.disconnect() }
        let requests = await server.base.captured().count, full = home.scenes, keysBefore = home.devices[0].raw["keys"]
        #expect(home.independentScenes(home.scenes, alongside: home.dailyDevices).map(\.id) == ["legacy", "unknown-room"])
        #expect(home.independentScenes(home.scenes, alongside: []).map(\.id) == full.map(\.id))
        #expect(home.independentScenes(home.scenes(inRoom: "study"), alongside: home.dailyDevices).isEmpty)
        home.pause()
        #expect(!home.snapshotIsCurrent)
        #expect(home.independentScenes(home.scenes, alongside: home.dailyDevices).map(\.id) == ["legacy", "unknown-room"])
        #expect(home.scenes == full && home.devices[0].raw["keys"] == keysBefore)
        #expect(home.editableScenes.map(\.id) == ["reading", "later"])
        #expect(await server.base.captured().count == requests)
    }
    @Test(arguments: ["hidden", "wrong-room", "legacy-panel", "invalid-card-action", "nested-only", "missing-action"])
    func unavailableCardLocationsAndIndirectProgramsKeepIndependentEntry(_ variant: String) async throws {
        let server = SceneRoomHTTP()
        var panel = HomeKeypadTests.record().raw
        switch variant {
        case "hidden": panel["catalog_visible"] = .bool(false)
        case "wrong-room": panel["room"] = .object(["id": .string("other"), "name": .string("其他房间")])
        case "legacy-panel": panel = panel.removing(["module_id"])
        default:
            var keys = panel["keys"].values
            if variant == "invalid-card-action" { keys[0]["card_action"] = .object(["kind": .string("unsupported")]) }
            if variant == "nested-only" { keys[0]["action"]["scene"] = .string("wrapper-calls-reading") }
            if variant == "missing-action" { keys[0]["action"] = .null }
            panel["keys"] = .array(keys)
        }
        await server.base.setPresentedDevices([panel])
        let home = await connect(server)
        defer { home.disconnect() }
        let roomDevices = home.dailyDevices.filter { $0.roomID == "study" }
        #expect(home.independentScenes(home.scenes(inRoom: "study"), alongside: roomDevices).map(\.id) == ["reading"])
        #expect(home.scenes.count == 4)
        #expect(await server.base.captured().allSatisfy { $0.httpMethod == "GET" })
    }
    @Test func malformedSixKeyCatalogKeepsExistingFailClosedBehavior() async throws {
        let server = SceneRoomHTTP()
        var panel = HomeKeypadTests.record().raw
        panel["keys"] = .array(Array(panel["keys"].values.dropLast()))
        await server.base.setPresentedDevices([panel])
        let home = await connect(server)
        defer { home.disconnect() }
        #expect(!home.connected && !home.controlsAvailable && home.error != nil)
        #expect(home.devices.isEmpty && home.scenes.isEmpty)
        #expect(await server.base.captured().allSatisfy { $0.httpMethod == "GET" })
    }
    @Test func roomShowsOnlyVisibleLightsCurtainsAndEveryAssignedSceneWhileCatalogKeepsPanel() async throws {
        let server = SceneRoomHTTP(), panel = HomeKeypadTests.record().raw
        func device(_ id: String, _ kind: String, room: String = "study", visible: Bool = true) -> HomeJSON {
            .object(["id": .string(id), "name": .string("相同名称"), "kind": .string(kind), "room": .object(["id": .string(room)]), "catalog_visible": .bool(visible)])
        }
        let rows = [panel, device("lamp", "Switch"), device("dimmer", "Dimmer"), device("curtain", "Motor"),
                    device("legacy-led", "Keypad"), device("sensor", "Sensor"),
                    device("hidden-alias", "Switch", visible: false), device("other-room-lamp", "Switch", room: "other")]
        await server.base.setPresentedDevices(rows)
        var scenes = await server.scenes
        scenes[1]["room"] = scenes[0]["room"]
        scenes[1]["name"] = scenes[0]["name"]
        await server.setScenes(scenes)
        let home = await connect(server)
        defer { home.disconnect() }
        let count = await server.base.captured().count, catalog = home.devices
        #expect(home.roomDevices(inRoom: "study").map(\.id) == ["lamp", "dimmer", "curtain"])
        #expect(home.dailyActuators.map(\.id) == ["lamp", "dimmer", "curtain", "other-room-lamp"])
        #expect(home.roomDevices(inRoom: "other").isEmpty)
        let assigned = home.scenes(inRoom: "study")
        #expect(assigned.map(\.id) == ["reading", "all-off"])
        #expect(Set(assigned.map(\.id)).count == assigned.count)
        #expect(home.independentScenes(assigned, alongside: home.roomDevices(inRoom: "study")) == assigned)
        #expect(home.dailyDevices.contains { $0.id == "panel" })
        let keypad = try HomeKeypad(record: #require(home.dailyDevices.first { $0.id == "panel" }))
        #expect(keypad.keys[0].actionScene == "reading" && keypad.keys[0].led.lastWritten == .red)
        home.selectInterfaceMode(.management); home.selectInterfaceMode(.daily); home.pause()
        #expect(home.roomDevices(inRoom: "study").map(\.id) == ["lamp", "dimmer", "curtain"])
        #expect(home.scenes(inRoom: "study") == assigned && home.devices == catalog)
        #expect(await server.base.captured().count == count)
    }
    @Test func virtualScenePanelsGroupByRoomPaginateSixAndDeduplicateIDsWithoutMergingNames() async throws {
        let server = SceneRoomHTTP(), home = await connect(server)
        defer { home.disconnect() }
        let count = await server.base.captured().count
        let room: HomeJSON = .object(["id": .string("study")])
        let rows = (0..<7).map { index in
            HomeRecord(.object(["id": .string("scene-\(index)"), "name": .string("相同名称"), "room": room]))
        }
        let global = HomeRecord(.object(["id": .string("global"), "room": .null]))
        let unknown = HomeRecord(.object(["id": .string("unresolved"), "room": .object(["id": .string("not-visible")])]))
        let input = rows + [rows[0], global, unknown]
        let panels = home.scenePanels(for: input)
        #expect(panels.map { $0.scenes.count } == [6, 1, 2])
        #expect(panels[0].title == "书房场景" && panels[0].page == 1 && panels[0].pageCount == 2)
        #expect(panels[1].page == 2 && panels[1].pageCount == 2)
        #expect(panels[2].title == "全屋与未分配场景")
        #expect(panels.flatMap(\.scenes).map(\.id) == rows.map(\.id) + ["global", "unresolved"])
        #expect(Set(panels.map(\.id)).count == panels.count)
        #expect(home.scenePanels(for: []).isEmpty)
        home.pause()
        #expect(home.scenePanels(for: input) == panels)
        #expect(await server.base.captured().count == count)
    }
    @Test(arguments: ["momentary", "selection-on", "selection-off", "selection-unknown"])
    func virtualSceneKeyUsesOneExistingExplicitRequestAndNeverInventsSelectedState(_ mode: String) async throws {
        let server = SceneRoomHTTP()
        var rows = await server.scenes
        rows[0]["switch_kind"] = .string(mode == "momentary" ? "momentary" : "selection")
        rows[0]["selected"] = mode == "selection-on" ? .bool(true) : mode == "selection-off" ? .bool(false) : .null
        await server.setScenes(rows)
        let home = await connect(server)
        defer { home.disconnect() }
        let scene = try #require(home.scenes.first { $0.id == "reading" }), action = scene.sceneCardAction
        let expected: HomeSceneCardAction = mode == "momentary" ? .execute : .selection(mode != "selection-on")
        #expect(action == expected)
        #expect(home.canControlScene(scene.id))
        await home.controlScene(scene.id, on: action.on)
        let posts = await server.base.captured().filter { $0.httpMethod == "POST" }
        #expect(posts.count == 1)
        let post = try #require(posts.first)
        #expect(post.url?.path == "/api/v1/scene-actions")
        let body = try JSONDecoder().decode(HomeJSON.self, from: #require(post.httpBody))
        #expect(body["scene"].text == "reading" && body["request_id"].text.flatMap(UUID.init(uuidString:)) != nil)
        if let on = expected.on { #expect(body["on"] == .bool(on)) }
        else if case .object(let fields) = body { #expect(fields["on"] == nil) }
        #expect(home.scenes.first { $0.id == scene.id }?.raw["selected"] == scene.raw["selected"])
        home.pause()
        let count = await server.base.captured().count
        await home.controlScene(scene.id, on: action.on)
        #expect(await server.base.captured().count == count)
    }
    @Test func websocketPreservesRoomPlacementAndFullCatalogAcrossOfflineState() async throws {
        let server = SceneRoomHTTP(), events = SceneRoomEvents(), home = await connect(server, events: events)
        defer { home.disconnect() }
        try await waitFor { events.ready }
        let rows: [HomeJSON] = [.object(["id": .string("reading"), "name": .string("阅读"), "room": .null]), .object(["id": .string("all-off"), "room": .object(["id": .string("study"), "name": .string("书房")])])]
        try events.push(.object(["type": .string("state"), "reset": .bool(true), "revision": .string("9007199254740993"), "cursor": .object(["boot": .string("synthetic-boot"), "revision": .integer(9_007_199_254_740_993), "transport_epoch": .integer(1), "sequence": .integer(1), "scope": .string("synthetic-scene-room-scope")]), "devices": .array([]), "scenes": .array(rows)]))
        try await waitFor { home.receivingPush }
        #expect(home.scenes(inRoom: "study").map(\.id) == ["all-off"])
        #expect(home.householdScenes.map(\.id) == ["reading"])
        home.pause()
        #expect(!home.controlsAvailable && home.scenes == rows.map(HomeRecord.init))
        #expect(home.scenes(inRoom: "study").map(\.id) == ["all-off"])
        #expect(await server.base.captured().allSatisfy { $0.httpMethod == "GET" })
    }
    @Test func movingPublicSceneUpdatesOnlyItsExplicitSelectionPeerAndPreservesDefinitions() async throws {
        let server = SceneRoomHTTP()
        await server.setMetadata([
            .object(["id": .string("reading"), "name": .string("阅读"), "room": .string("study")]),
            .object(["id": .string("later"), "name": .string("内部配方"), "room": .string("study"), "future": .bool(true)]),
            .object(["id": .string("legacy"), "name": .string("阅读"), "room": .string("study")])])
        await server.setPresentations([.object(["scene": .string("reading"), "selection_scene": .string("later"), "review_evidence": .string("synthetic explicit pair")])])
        let home = await connect(server)
        defer { home.disconnect() }
        await home.loadActiveConfiguration()
        let scene = try #require(home.editableScenes.first { $0.id == "reading" })
        let definitions = home.automationDocument?["timed_scenes"], pairs = home.automationDocument?["scene_presentations"]
        try home.upsertScene(scene.raw, collection: "immediate_scenes", name: "新名称", originalID: scene.id, room: "")
        var metadata = try #require(home.automationDocument?["scene_metadata"].values)
        #expect(metadata.first { $0["id"].text == "reading" }?["room"] == .null)
        #expect(metadata.first { $0["id"].text == "later" }?["room"] == .null)
        #expect(metadata.first { $0["id"].text == "later" }?["name"] == .string("内部配方"))
        #expect(metadata.first { $0["id"].text == "later" }?["future"] == .bool(true))
        #expect(metadata.first { $0["id"].text == "legacy" }?["room"] == .string("study"))
        try home.upsertScene(scene.raw, collection: "immediate_scenes", name: "新名称", originalID: scene.id, room: "study")
        metadata = try #require(home.automationDocument?["scene_metadata"].values)
        #expect(metadata.first { $0["id"].text == "later" }?["room"] == .string("study"))
        #expect(home.automationDocument?["timed_scenes"] == definitions)
        #expect(home.automationDocument?["scene_presentations"] == pairs)
        #expect(await server.base.captured().allSatisfy { $0.httpMethod == "GET" })
    }

}
