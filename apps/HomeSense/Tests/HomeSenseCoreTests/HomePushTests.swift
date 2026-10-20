import Foundation
import Combine
import Testing
@testable import HomeSenseCore

private final class EventFixture: HomeEventTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var subscribers: [AsyncThrowingStream<HomeEventMessage, Error>.Continuation] = []
    private var requests: [URLRequest] = []
    private var terminations = 0
    func events(_ request: URLRequest) -> AsyncThrowingStream<HomeEventMessage, Error> {
        AsyncThrowingStream { continuation in
            lock.withLock { requests.append(request); subscribers.append(continuation) }
            continuation.onTermination = { [weak self] _ in
                guard let self else { return }
                self.lock.withLock { self.terminations += 1 }
            }
        }
    }
    var count: Int { lock.withLock { subscribers.count } }
    var activeCount: Int { lock.withLock { subscribers.count - terminations } }
    var lastRequest: URLRequest? { lock.withLock { requests.last } }
    func push(_ frame: HomeJSON, index: Int? = nil) throws {
        let continuation = lock.withLock { subscribers[index ?? (subscribers.count - 1)] }
        continuation.yield(.state(try frame.data()))
    }
    func loseConnection() { lock.withLock { subscribers.last }?.finish(throwing: URLError(.networkConnectionLost)) }
    func heartbeat() { lock.withLock { subscribers.last }?.yield(.heartbeat) }
}

@Suite @MainActor
struct HomePushTests {
    private func waitFor(_ predicate: () -> Bool) async throws {
        for _ in 0..<200 {
            if predicate() { return }
            try await Task.sleep(for: .milliseconds(20))
        }
        Issue.record("Timed out waiting for state synchronization")
    }
    private func frame(_ level: Int64, sequence: Int64 = 1, reset: Bool = true) -> HomeJSON {
        .object(["type": .string("state"), "reset": .bool(reset), "revision": .string("9007199254740993"),
            "cursor": .object(["boot": .string("synthetic-boot"), "revision": .integer(9_007_199_254_740_993),
                "transport_epoch": .integer(1), "sequence": .integer(sequence), "scope": .string("synthetic-member-scope")]),
            "devices": .array([.object(["id": .string("light"), "name": .string("书房灯"), "kind": .string("Dimmer"),
                "can_control": .bool(true), "quality": .string("Confirmed"),
                "observed": .object(["level": .integer(level), "on": .bool(level > 0)])])]),
            "scenes": .array([.object(["id": .string("reading"), "name": .string("阅读"), "can_control": .bool(true)])])])
    }
    private func connect(_ server: HomeTestServer, _ events: EventFixture) async -> HomeServerStore {
        let home = HomeServerStore(transport: server, eventTransport: events, defaults: nil, credentialStore: InMemoryCredentials())
        home.endpointText = "https://home.example:8444"
        await home.connect(token: "test-token")
        return home
    }
    @Test func pushesUpdateWithoutPollingOrDisablingControls() async throws {
        let server = HomeTestServer(), events = EventFixture(), home = await connect(server, events)
        defer { home.disconnect() }
        try await waitFor { events.count == 1 }
        let request = try #require(events.lastRequest)
        #expect(request.url?.absoluteString == "wss://home.example:8444/api/v1/events")
        #expect(request.value(forHTTPHeaderField: "Origin") == "https://home.example:8444")
        #expect(request.value(forHTTPHeaderField: "Authorization") == "Bearer test-token")
        #expect(request.value(forHTTPHeaderField: "Cookie") == nil)
        try events.push(frame(30))
        try await waitFor { home.receivingPush }
        let count = await server.captured().count
        try home.upsertRoom(id: "draft-only", name: "未发布房间", floor: "未发布楼层")
        let draft = home.deviceDocument
        home.selectInterfaceMode(.management); home.selectInterfaceMode(.daily)
        #expect(home.deviceDocument == draft && home.draftChanged)
        #expect(home.rooms.allSatisfy { $0.id != "draft-only" })
        #expect(home.receivingPush && home.controlsAvailable && events.count == 1)
        try await Task.sleep(for: .milliseconds(3300))
        #expect(await server.captured().count == count, "No polling or HTTP requests from mode changes")
        try events.push(frame(17, sequence: 2, reset: false))
        try await waitFor { home.devices.first?.raw["observed"]["level"] == .integer(17) }
        #expect(!home.busy && home.controlsAvailable)
        await home.controlDevice("light", command: .object(["action": .string("brightness"), "level": .integer(40)]))
        #expect(await server.captured().count == count + 1, "Only the explicit command, no snapshot polling")
        #expect(home.devices.first?.raw["observed"]["level"] == .integer(17), "Accepted is not actual feedback")
    }
    @Test func roomSceneKeyStaysStableUntilRelevantPushAndNeverPollsWhileIdle() async throws {
        let server = HomeTestServer(), events = EventFixture(), home = await connect(server, events)
        defer { home.disconnect() }
        try await waitFor { events.count == 1 }
        var state = frame(30)
        var scene: HomeJSON = .object(["id": .string("reading"), "name": .string("阅读"),
            "room": .object(["id": .string("study")]), "can_control": .bool(true),
            "switch_kind": .string("selection"), "selected": .bool(false), "activity": .string("Inactive")])
        state["scenes"] = .array([scene])
        try events.push(state); try await waitFor { home.receivingPush }
        func card() throws -> HomeSceneCardState {
            let row = try #require(home.scenes(inRoom: "study").first)
            return HomeSceneCardState(scene: row, current: home.snapshotIsCurrent && row.targetAvailable,
                available: home.canControlScene(row.id), pending: home.sceneActionPresentation(row.id))
        }
        let initial = try card(), requests = await server.captured().count
        var publications = 0
        let subscription = home.$scenes.dropFirst().sink { _ in publications += 1 }
        defer { subscription.cancel() }
        // Healthy silent streams remain current beyond the old snapshot age.
        events.heartbeat()
        try await Task.sleep(for: .seconds(11))
        #expect(try card() == initial)
        #expect(publications == 0 && home.controlsAvailable)
        #expect(await server.captured().count == requests)

        let before = home.capturedAt
        state = frame(17, sequence: 2, reset: false); state["scenes"] = .array([scene])
        try events.push(state); try await waitFor { home.capturedAt != before }
        #expect(publications == 0, "An unrelated lamp update must not republish the scene catalog")
        #expect(try card() == initial)

        // Timestamp-only changes remain available to details, without changing
        // any input at the equatable SwiftUI scene-key boundary.
        scene["observed_ms"] = .integer(12345); state["scenes"] = .array([scene])
        try events.push(state); try await waitFor { publications == 1 }
        #expect(try card() == initial)
        #expect(home.scenes.first?.raw["observed_ms"] == .integer(12345))

        // A scene change may legally arrive on the very same device cursor.
        scene["selected"] = .bool(true); scene["activity"] = .string("Active")
        state["scenes"] = .array([scene]); try events.push(state)
        try await waitFor { publications == 2 }
        let active = try card()
        #expect(active != initial && active.selected && active.action == .selection(false))
        #expect(active.activityDescription == "负载：匹配")
        try events.push(.object(["type": .string("unavailable")]))
        try await waitFor { !home.controlsAvailable }
        let offline = try card()
        #expect(!offline.available && !offline.current && offline.selected)
        #expect(publications == 2, "Going offline retains historical selection and load feedback")
        #expect(await server.captured().count == requests, "No background polling or control requests")
    }
    @Test func backgroundGateStopsStreamAndCannotBeReopenedByViewCallbacksOrRetry() async throws {
        let server = HomeTestServer(), events = EventFixture(), home = await connect(server, events)
        defer { home.disconnect() }
        try await waitFor { events.count == 1 }; try events.push(frame(30))
        try await waitFor { home.receivingPush }
        let devices = home.devices, requests = await server.captured().count
        home.setApplicationActive(false)
        try await waitFor { events.activeCount == 0 }
        #expect(!home.receivingPush && !home.controlsAvailable)
        // A sheet dismissal or tab callback can request resume after background.
        home.resume(); await home.restoreSavedConnection()
        try events.push(frame(99, sequence: 2), index: 0)
        try await Task.sleep(for: .milliseconds(100))
        #expect(events.count == 1 && events.activeCount == 0)
        #expect(home.devices == devices && !home.snapshotIsCurrent)
        #expect(await server.captured().count == requests)

        home.setApplicationActive(true); home.resume()
        try await waitFor { events.count == 2 }
        try events.push(frame(17, sequence: 3))
        try await waitFor { home.receivingPush }
        #expect(home.devices.first?.raw["observed"]["level"] == .integer(17))
        events.loseConnection(); try await waitFor { !home.receivingPush }
        home.setApplicationActive(false); home.resume()
        let afterPause = await server.captured().count
        try await Task.sleep(for: .milliseconds(2200))
        #expect(events.count == 2 && events.activeCount == 0)
        #expect(await server.captured().count == afterPause, "No reconnect traffic while backgrounded")
        #expect(await server.captured().allSatisfy { $0.httpMethod == "GET" })
    }
    @Test func backgroundCredentialRestorationDoesNotStartNetworking() async throws {
        let server = HomeTestServer(), events = EventFixture(), credentials = InMemoryCredentials()
        await credentials.save("test-token", for: "https://home.example:8444")
        let home = HomeServerStore(transport: server, eventTransport: events, defaults: nil, credentialStore: credentials)
        defer { home.disconnect() }
        home.endpointText = "https://home.example:8444"
        home.setApplicationActive(false); home.resume(); await home.restoreSavedConnection()
        #expect(!home.connected && !home.restoringCredential && events.count == 0)
        #expect(await server.captured().isEmpty)
        #expect(await credentials.load(for: "https://home.example:8444") == "test-token")
    }
    @Test func disconnectReconnectResyncAndRevocationNeverReplayWrites() async throws {
        let server = HomeTestServer(), events = EventFixture(), home = await connect(server, events)
        defer { home.disconnect() }
        try await waitFor { events.count == 1 }; try events.push(frame(30))
        try await waitFor { home.receivingPush }
        events.loseConnection()
        try await waitFor { !home.controlsAvailable }
        #expect(!home.snapshotIsCurrent)
        try await waitFor { events.count == 2 }
        try events.push(frame(0, sequence: 7))
        try await waitFor { home.receivingPush && home.devices.first?.raw["observed"]["on"] == .bool(false) }
        #expect(home.snapshotIsCurrent)
        #expect(await server.captured().allSatisfy { $0.httpMethod == "GET" })
        await server.setUnauthorized(); events.loseConnection()
        try await waitFor { !home.connected }
        #expect(home.devices.isEmpty && home.rooms.isEmpty && home.serverFloors.isEmpty)
    }
    @Test func unavailableAndPausedConnectionsCannotRestoreOldState() async throws {
        let server = HomeTestServer(), events = EventFixture(), home = await connect(server, events)
        defer { home.disconnect() }
        try await waitFor { events.count == 1 }; try events.push(frame(30))
        try await waitFor { home.receivingPush }
        try events.push(.object(["type": .string("unavailable")]))
        try await waitFor { !home.controlsAvailable }
        try events.push(frame(17, sequence: 2))
        try await waitFor { home.receivingPush }
        home.pause()
        try events.push(frame(100, sequence: 3), index: 0)
        try await Task.sleep(for: .milliseconds(50))
        #expect(!home.receivingPush && !home.controlsAvailable)
        #expect(home.devices.first?.raw["observed"]["level"] == .integer(17))
        home.resume(); try await waitFor { events.count == 2 }
        try events.push(frame(40, sequence: 4)); try await waitFor { home.receivingPush }
        #expect(home.devices.first?.raw["observed"]["level"] == .integer(40))
    }
    @Test func independentEmptyFloorsAndRoomMaintenanceSurvivePublication() async throws {
        let server = HomeTestServer(), home = HomeServerStore(transport: server, defaults: nil, credentialStore: InMemoryCredentials())
        home.pause(); home.endpointText = "https://home.example:8444"; await home.connect(token: "test-token")
        defer { home.disconnect() }
        try home.renameFloor("一层", to: "2层")
        try home.addFloor("1层"); try home.addFloor("3层")
        try home.upsertRoom(id: "artstudio", name: "画室", floor: "1层")
        #expect(home.floors == ["1层", "2层", "3层"])
        #expect(home.editableRooms.count == 2)
        #expect(home.editableRooms.first { $0.id == "study" }?.raw["floor"].text == "2层")
        #expect(home.editableRooms.allSatisfy { $0.raw["floor"].text != "3层" })
        #expect(throws: HomeServerError.self) { try home.addFloor("3层") }
        await home.saveDraft(); await home.previewDraft(); await home.publishDraft()
        #expect(home.error == nil)
        #expect(home.serverFloors == ["1层", "2层", "3层"])
        #expect(home.floors == ["1层", "2层", "3层"])
        try home.renameFloor("3层", to: nil)
        #expect(home.floors == ["1层", "2层"] && home.editableRooms.count == 2)
    }
    @Test func keypadPushKeepsOneParentAndSeparatesPressFromWrittenColor() async throws {
        let server = HomeTestServer(), events = EventFixture(), home = await connect(server, events)
        defer { home.disconnect() }
        try await waitFor { events.count == 1 }
        var state = frame(0), panel = HomeKeypadTests.record().raw
        state["devices"] = .array([panel]); try events.push(state)
        try await waitFor { home.receivingPush && home.devices.first?.id == "panel" }
        #expect(home.devices.count == 1)
        home.selectInterfaceMode(.management); home.selectInterfaceMode(.daily)
        #expect(events.count == 1 && home.receivingPush)
        var keys = panel["keys"].values
        keys[0]["key_event"]["pressed"] = .bool(false)
        keys[0]["led"]["last_written"]["color"] = .string("green")
        panel["keys"] = .array(keys)
        state = frame(0, sequence: 2, reset: false); state["devices"] = .array([panel])
        try events.push(state)
        try await waitFor { home.devices.first?.raw["keys"].values.first?["led"]["last_written"]["color"].text == "green" }
        let projected = try HomeKeypad(record: #require(home.devices.first))
        #expect(projected.keys[0].led.actual == .unknown && projected.keys[0].led.lastWritten == .green)
        // A malformed partial parent must not reset the omitted key to a made-up state.
        panel["keys"] = .array(Array(keys.dropLast()))
        state = frame(0, sequence: 3, reset: false); state["devices"] = .array([panel])
        try events.push(state)
        try await waitFor { !home.controlsAvailable }
        #expect(home.devices.first?.raw["keys"].values.count == 6)
        #expect(await server.captured().allSatisfy { $0.httpMethod == "GET" })
    }
    @Test func changedPushScopeRechecksRoleAndClosesManagementWithoutReconnect() async throws {
        let server = HomeTestServer(), events = EventFixture(), home = await connect(server, events)
        defer { home.disconnect() }
        try await waitFor { events.count == 1 }; try events.push(frame(0))
        try await waitFor { home.receivingPush }
        home.selectInterfaceMode(.management)
        await server.setAdministrator(false)
        var next = frame(0, sequence: 2)
        next["cursor"]["scope"] = .string("new-member-scope")
        try events.push(next)
        try await waitFor { !home.canManage }
        #expect(home.interfaceMode == .daily && home.deviceDocument == nil)
        #expect(events.count == 1)
        #expect(await server.captured().allSatisfy { $0.httpMethod == "GET" })
    }
    @Test func awayRetryProgressUpdatesOnSameCursorWithoutClientReplayAndClearsAfterDisconnect() async throws {
        let server = HomeTestServer(), events = EventFixture(), home = await connect(server, events)
        defer { home.disconnect() }
        try await waitFor { events.count == 1 }
        var state = frame(0)
        func operation(_ status: String, _ round: Int64, _ confirmed: Int64) -> HomeJSON {
            .object(["operation": .string("synthetic-boot-1"), "boot": .string("synthetic-boot"),
                "revision": .string("9007199254740993"), "state": .string(status), "rounds": .integer(round), "max_rounds": .integer(3),
                "total_outputs": .integer(8), "confirmed_outputs": .integer(confirmed),
                "missing": confirmed == 8 ? .array([]) : .array([.object(["module_id": .string("relay"), "channels": .array((1...8).map { .integer(Int64($0)) })])]),
                "reason": status == "failed" ? .string("attempts_exhausted") : .null])
        }
        state["away_operations"] = .array([operation("confirming", 1, 0)])
        try events.push(state); try await waitFor { home.awayStatus?.rounds == 1 }
        let count = await server.captured().count
        state["reset"] = .bool(false); state["away_operations"] = .array([operation("retrying", 2, 0)])
        try events.push(state); try await waitFor { home.awayStatus?.state == "retrying" }
        #expect(home.awayStatus?.failed == false)
        state["away_operations"] = .array([operation("failed", 3, 0)])
        try events.push(state); try await waitFor { home.awayStatus?.failed == true }
        #expect(home.awayStatus?.message.contains("relay [1,2,3,4,5,6,7,8]") == true)
        #expect(await server.captured().count == count, "Server updates never cause a client retry or polling request")
        home.disconnect(); #expect(home.awayStatus == nil && home.awayOperations.isEmpty)
    }

}
