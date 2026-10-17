#if os(macOS)
import Foundation
import Testing
@testable import HomeSenseCore

/// Generic private-catalog reader. Real IDs and names enter only through the
/// environment-provided private expectation file, never this source or fixtures.
private actor FullCatalogReadOnlyHTTP: HomeHTTPTransport {
    let base: HomeURLSessionTransport
    let output: URL
    var requests: [HomeJSON] = []
    var rejectedWrites = 0
    var sceneResponses: [HomeJSON] = []
    init(_ base: HomeURLSessionTransport, output: URL) { self.base = base; self.output = output }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        precondition(request.url?.host == "localhost")
        guard request.httpMethod == "GET" else {
            rejectedWrites += 1
            try? save(output)
            throw HomeServerError.invalidResponse // Fail closed before a write reaches the server.
        }
        let response = try await base.send(request)
        requests.append(.object(["method": .string("GET"), "path": .string(request.url!.path),
            "status": .integer(Int64(response.status)), "bytes": .integer(Int64(response.data.count))]))
        if request.url?.path == "/api/v1/scenes" {
            sceneResponses.append(try JSONDecoder().decode(HomeJSON.self, from: response.data))
        }
        try save(output)
        return response
    }
    func save(_ root: URL) throws {
        try HomeJSON.object(["requests": .array(requests), "rejected_non_GET": .integer(Int64(rejectedWrites)),
            "scene_responses": .array(sceneResponses)]).data(pretty: true).write(to: root.appendingPathComponent("native-http-readonly.json"))
    }
    func verifyReadOnly() -> Bool { rejectedWrites == 0 && !requests.isEmpty }
    func firstSceneIDs() -> Set<String> { Set(sceneResponses.first?["scenes"].values.compactMap { $0["id"].text } ?? []) }
}

private final class FullCatalogEvents: HomeEventTransport, @unchecked Sendable {
    let base: HomeWebSocketTransport
    let lock = NSLock()
    private var frames: [Data] = []
    init(_ base: HomeWebSocketTransport) { self.base = base }
    private func record(_ data: Data) { lock.withLock { if frames.count < 8 { frames.append(data) } } }
    func snapshot() throws -> [HomeJSON] { try lock.withLock { frames }.map { try JSONDecoder().decode(HomeJSON.self, from: $0) } }
    func save(_ root: URL) throws { try HomeJSON.array(snapshot()).data(pretty: true).write(to: root.appendingPathComponent("native-initial-wss.json")) }
    func events(_ request: URLRequest) -> AsyncThrowingStream<HomeEventMessage, Error> {
        precondition(request.url?.host == "localhost")
        let source = base.events(request)
        return AsyncThrowingStream(bufferingPolicy: .bufferingOldest(16)) { continuation in
            let receiver = Task {
                do {
                    for try await message in source {
                        if case .state(let data) = message { record(data) }
                        if case .dropped = continuation.yield(message) { throw HomeServerError.invalidResponse }
                    }
                    continuation.finish()
                } catch { continuation.finish(throwing: error) }
            }
            continuation.onTermination = { _ in receiver.cancel() }
        }
    }
}

@Suite @MainActor struct HomeFullCatalogActualIntegrationTests {
    @Test(.enabled(if: ProcessInfo.processInfo.environment["HOMESENSE_FULL_CATALOG_ACTUAL_FIXTURE"] != nil,
                   "Requires private full-catalog expectations and a pinned loopback TLS CLI"))
    func nativeFullCatalogReadsAllScenesTypedPanelsRoomsAndInitialWSSWithoutControls() async throws {
        let root = URL(fileURLWithPath: try #require(ProcessInfo.processInfo.environment["HOMESENSE_FULL_CATALOG_ACTUAL_FIXTURE"]))
        func read(_ name: String) throws -> HomeJSON { try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent(name))) }
        let service = try read("service.json"), expected = try read("expected.json"), config = try read("devices.json")
        try #require(service["fixture_kind"].text == "private-full-catalog-readonly-loopback")
        let origin = try #require(service["origin"].text)
        try #require(URL(string: origin)?.host == "localhost")
        try #require(config["gateways"].values.allSatisfy { $0["endpoint"].text?.hasPrefix("127.0.0.1:") == true })
        let expectedSceneIDs = Set(expected["scene_ids"].values.compactMap(\.text))
        try #require(expectedSceneIDs.count == 130 && expected["keypads"].values.count == 28 && expected["floors"].values.count == 3)
        let trust = try FixtureWebSocketTrust(pem: String(contentsOf: root.appendingPathComponent("cert.pem"), encoding: .utf8))
        let http = FullCatalogReadOnlyHTTP(HomeURLSessionTransport(delegate: trust), output: root)
        let events = FullCatalogEvents(HomeWebSocketTransport(delegate: trust))
        let diagnostics = HomeDiagnostics(fileURL: nil, emitSystemLog: false)
        let home = HomeServerStore(transport: http, eventTransport: events, defaults: nil,
                                   credentialStore: InMemoryCredentials(), diagnostics: diagnostics)
        home.pause(); home.endpointText = origin
        await home.connect(token: try String(contentsOf: root.appendingPathComponent("token"), encoding: .utf8))
        defer {
            home.disconnect()
            try? events.save(root)
            try? Data(diagnostics.exportText().utf8).write(to: root.appendingPathComponent("native-diagnostics.jsonl"))
        }
        try #require(home.connected && home.error == nil, "\(home.error ?? "")")
        await home.loadPhysicalModules()
        let configuredModules = config["gateways"].values.flatMap { $0["modules"].values }
        let profiles: Set<String> = ["r8i8_din_pro_v2_7_7", "ad8_din_rs485_v2_2_1", "c4_din_pro_v2_7_7"]
        #expect(home.moduleInventory.count == configuredModules.count)
        #expect(home.physicalHardwareModules.count == configuredModules.filter { profiles.contains($0["profile"].text ?? "") }.count)
        #expect(home.dailyPanels.allSatisfy { home.physicalModule(for: $0) != nil })
        var phases: [HomeJSON] = []
        func verify(_ stage: String) throws {
            #expect(Set(home.scenes.map(\.id)) == expectedSceneIDs && home.scenes.count == 130)
            let actualPanels = home.devices.filter { $0.raw["kind"].text == "Keypad" }
            let expectedPanels = expected["keypads"].values
            #expect(Set(actualPanels.map(\.id)) == Set(expectedPanels.compactMap { $0["id"].text }))
            #expect(actualPanels.count == 28)
            #expect(Set(home.activeFloors) == Set(expected["floors"].values.compactMap(\.text)))
            #expect(Set(home.rooms.map(\.id)) == Set(expected["rooms"].values.compactMap { $0["id"].text }))
            for room in expected["rooms"].values {
                let actual = try #require(home.rooms.first { $0.id == room["id"].text })
                #expect(actual.name == room["name"].text && actual.raw["floor"] == room["floor"])
            }
            var keys = 0, executeKeys = 0, unknownExecutableKeys = 0
            for row in expectedPanels {
                let record = try #require(actualPanels.first { $0.id == row["id"].text })
                let panel = try HomeKeypad(record: record, current: home.snapshotIsCurrent)
                #expect(panel.name == row["name"].text && panel.moduleID == row["module_id"].text)
                #expect(record.raw["room"]["id"] == row["room"])
                #expect(panel.keys.count == 6 && panel.keys.map(\.number) == [1,2,3,4,5,6])
                for wanted in row["keys"].values {
                    let number = try #require(wanted["number"].integerText.flatMap(Int.init))
                    let key = try #require(panel.keys.first { $0.number == number })
                    #expect(key.endpointID == wanted["endpoint_id"].text && key.canActivate && key.canActivateCard)
                    keys += 1
                    switch wanted["action_kind"].text {
                    case "scene_execute", "scene_toggle":
                        let executes = wanted["action_kind"].text == "scene_execute"
                        #expect(key.actionScene == wanted["scene"].text && key.executesScene == executes)
                        if executes {
                            executeKeys += 1
                            let scene = try #require(home.scenes.first { $0.id == key.actionScene })
                            #expect(scene.raw["selected"] == .null)
                            #expect(home.canActivateKeypad(panel.id, key: number, card: true))
                            if scene.raw["selected"] == .null && home.canActivateKeypad(panel.id, key: number, card: true) { unknownExecutableKeys += 1 }
                        }
                    case "motor":
                        #expect(key.actionDevice == wanted["device"].text && key.motorAction == wanted["command"].text)
                    case "away_off": #expect(key.cardAction == .awayOff)
                    default: Issue.record("Unexpected private-catalog action kind")
                    }
                }
            }
            #expect(keys == 168 && executeKeys == 46 && unknownExecutableKeys == 46)
            phases.append(.object(["stage": .string(stage), "scenes": .integer(Int64(home.scenes.count)),
                "panels": .integer(Int64(actualPanels.count)), "keys": .integer(Int64(keys)), "floors": .integer(Int64(home.activeFloors.count)),
                "rooms": .integer(Int64(home.rooms.count)), "unknown_scene_execute_keys_available": .integer(Int64(unknownExecutableKeys))]))
            try HomeJSON.object(["devices": .array(home.devices.map(\.raw)), "scenes": .array(home.scenes.map(\.raw)),
                "rooms": .array(home.rooms.map(\.raw)), "floors": .array(home.activeFloors.map(HomeJSON.string))])
                .data(pretty: true).write(to: root.appendingPathComponent("native-" + stage + "-snapshot.json"))
        }
        try verify("HTTP")
        let httpIDs = await http.firstSceneIDs(); #expect(httpIDs == expectedSceneIDs)
        home.resume()
        let limit = ContinuousClock.now.advanced(by: .seconds(25))
        while !home.receivingPush && ContinuousClock.now < limit { try await Task.sleep(for: .milliseconds(20)) }
        try #require(home.receivingPush && home.snapshotIsCurrent, "Initial full-catalog WSS did not arrive")
        try verify("WSS")
        let frames = try events.snapshot(); let first = try #require(frames.first)
        #expect(Set(first["scenes"].values.compactMap { $0["id"].text }) == expectedSceneIDs && first["scenes"].values.count == 130)
        #expect(await http.verifyReadOnly())
        try await http.save(root)
        let trace = try diagnostics.exportText().split(separator: "\n").dropFirst().map { try JSONDecoder().decode(HomeJSON.self, from: Data($0.utf8)) }
        #expect(!trace.contains { $0["event"].text == "button.click" })
        #expect(!trace.contains { $0["event"].text == "http.send" && $0["method"].text != "GET" })
        try HomeJSON.array(phases).data(pretty: true).write(to: root.appendingPathComponent("native-readonly-phases.json"))
    }
}
#endif
