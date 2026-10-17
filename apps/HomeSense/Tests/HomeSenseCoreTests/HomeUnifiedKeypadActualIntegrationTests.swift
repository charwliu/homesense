#if os(macOS)
import Foundation
import Testing
@testable import HomeSenseCore

private actor UnifiedActualHTTP: HomeHTTPTransport {
    let base: HomeURLSessionTransport
    var posts: [HomeJSON] = []
    var moduleSnapshots: [HomeJSON] = []
    var holdFirst: Bool
    var held: CheckedContinuation<Void, Never>?
    init(trust: FixtureWebSocketTrust, holdFirst: Bool) { base = HomeURLSessionTransport(delegate: trust); self.holdFirst = holdFirst }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        precondition(request.url?.host == "localhost")
        let response = try await base.send(request)
        if request.url!.path == "/api/v1/modules" {
            moduleSnapshots.append(try JSONDecoder().decode(HomeJSON.self, from: response.data))
        }
        if request.httpMethod == "POST" {
            posts.append(.object(["path": .string(request.url!.path), "body": try JSONDecoder().decode(HomeJSON.self, from: request.httpBody!),
                "status": .integer(Int64(response.status)), "response": try JSONDecoder().decode(HomeJSON.self, from: response.data)]))
            if request.url!.path == "/api/v1/keypad-actions", holdFirst {
                holdFirst = false
                await withCheckedContinuation { held = $0 }
            }
        }
        return response
    }
    func isHeld() -> Bool { held != nil }
    func release() { held?.resume(); held = nil }
    func captured() -> [HomeJSON] { posts }
    func modules() -> [HomeJSON] { moduleSnapshots }
}

private final class UnifiedActualEvents: HomeEventTransport, @unchecked Sendable {
    let base: HomeWebSocketTransport
    let lock = NSLock()
    private var frames: [Data] = []
    init(_ trust: FixtureWebSocketTrust) { base = HomeWebSocketTransport(delegate: trust) }
    func save(_ path: URL) throws {
        let data = lock.withLock { frames }
        try HomeJSON.array(data.map { try JSONDecoder().decode(HomeJSON.self, from: $0) }).data(pretty: true).write(to: path)
    }
    func cursor() throws -> HomeJSON {
        let data = try #require(lock.withLock { frames.last })
        return try JSONDecoder().decode(HomeJSON.self, from: data)["cursor"]
    }
    func events(_ request: URLRequest) -> AsyncThrowingStream<HomeEventMessage, Error> {
        let source = base.events(request)
        return AsyncThrowingStream(bufferingPolicy: .bufferingOldest(32)) { continuation in
            let reader = Task {
                do {
                    for try await message in source {
                        if case .state(let data) = message { lock.withLock { frames.append(data) } }
                        if case .dropped = continuation.yield(message) { throw HomeServerError.invalidResponse }
                    }
                    continuation.finish()
                } catch { continuation.finish(throwing: error) }
            }
            continuation.onTermination = { _ in reader.cancel() }
        }
    }
}

@MainActor private final class UnifiedActualFixture {
    let root: URL
    let home: HomeServerStore
    let http: UnifiedActualHTTP
    let events: UnifiedActualEvents
    let origin: String
    let token: String
    let panel: String
    let expectedSlots: [String: String]
    var activation: Task<Void, Never>?

    init(scenario: String) async throws {
        root = URL(fileURLWithPath: try #require(ProcessInfo.processInfo.environment["HOMESENSE_UNIFIED_KEYPAD_ACTUAL_FIXTURE"]))
        let service = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("service.json")))
        try #require(service["fixture_kind"] == .string("synthetic-unified-keypad") && service["scenario"] == .string(scenario))
        origin = try #require(service["origin"].text)
        try #require(URL(string: origin)?.host == "localhost")
        let devices = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("devices.json")))
        try #require(devices["gateways"].values.allSatisfy { $0["endpoint"].text?.hasPrefix("127.0.0.1:") == true })
        var moduleLocations: [String: (String, String)] = [:], expected: [String: String] = [:]
        for gateway in devices["gateways"].values {
            for module in gateway["modules"].values where module["profile"] == .string("r8i8_din_pro_v2_7_7") {
                moduleLocations[module["id"].text!] = (gateway["id"].text!, module["address"].text!.uppercased())
            }
        }
        for device in devices["devices"].values {
            if let id = device["id"].text, let module = device["module_id"].text, let location = moduleLocations[module], let channel = device["channel"].integerText {
                expected[id] = Self.locator(gateway: location.0, address: location.1, module: module, channel: channel)
            }
        }
        expectedSlots = expected
        panel = scenario == "away" ? "entry-panel" : "panel"
        token = try String(contentsOf: root.appendingPathComponent("token"), encoding: .utf8)
        let trust = try FixtureWebSocketTrust(pem: String(contentsOf: root.appendingPathComponent("cert.pem"), encoding: .utf8))
        http = UnifiedActualHTTP(trust: trust, holdFirst: scenario == "scene" || scenario == "motor"); events = UnifiedActualEvents(trust)
        home = HomeServerStore(transport: http, eventTransport: events, defaults: nil, credentialStore: InMemoryCredentials(), diagnostics: HomeDiagnostics(fileURL: nil, emitSystemLog: false))
        home.endpointText = origin; await home.connect(token: token)
        try await until("initial authenticated current WSS") { self.home.receivingPush && self.home.unifiedKeypadActionsSupported && self.home.canActivateUnifiedKeypad(self.panel, key: 1) }
    }
    func until(_ title: String, _ predicate: () throws -> Bool) async throws {
        try await until(title, deadline: ContinuousClock.now.advanced(by: .seconds(5)), predicate)
    }
    func until(_ title: String, deadline: ContinuousClock.Instant, _ predicate: () throws -> Bool) async throws {
        while ContinuousClock.now < deadline {
            if try predicate() { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        Issue.record("Timed out: \(title); \(home.error ?? home.message)")
        throw URLError(.timedOut)
    }
    func held() async throws {
        for _ in 0..<500 { if await http.isHeld() { return }; try await Task.sleep(for: .milliseconds(10)) }
        throw URLError(.timedOut)
    }
    func start(_ key: Int) {
        let operation = home.beginUnifiedKeypadClick(panel, key: key)
        #expect(home.keypadActionPresentation(panel, key: key)?.phase == .submitting)
        activation = Task { await self.home.activateUnifiedKeypad(self.panel, key: key, operation: operation) }
    }
    func release() async { await http.release(); await activation?.value; activation = nil }
    func send(_ key: Int) async throws {
        try #require(home.canActivateUnifiedKeypad(panel, key: key))
        await home.activateUnifiedKeypad(panel, key: key)
        try #require(home.error == nil, "\(home.error ?? "")")
    }
    func frames(after offset: Int = 0) throws -> [HomeJSON] {
        let url = root.appendingPathComponent("gateway-frames.json")
        guard FileManager.default.fileExists(atPath: url.path) else { return [] }
        return Array(try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: url)).values.dropFirst(offset))
    }
    func has(_ raw: String, after offset: Int = 0) throws -> Bool { try frames(after: offset).contains { $0["raw"] == .string(raw) } }
    func marker(_ name: String, text: String = "release") throws { try Data(text.utf8).write(to: root.appendingPathComponent(name), options: .atomic) }
    func device(_ id: String) -> HomeJSON { home.devices.first { $0.id == id }?.raw ?? .null }
    func key(_ number: Int) throws -> HomeKeypadKey { try HomeKeypad(record: #require(home.devices.first { $0.id == panel })).keys[number-1] }
    func postCount() async -> Int { await http.captured().count }
    private static func locator(gateway: String, address: String, module: String, channel: String) -> String {
        "\(gateway.utf8.count):\(gateway)/\(address)/\(module.utf8.count):\(module)/\(channel)"
    }
    func moduleSamples() -> [String: UInt64]? {
        let modules = home.physicalHardwareModules.filter { $0.profile == "r8i8_din_pro_v2_7_7" }
        guard modules.count == 12 else { return nil }
        var samples: [String: UInt64] = [:]
        for module in modules {
            for slot in module.slots {
                guard let id = slot["binding"]["id"].text, let channel = slot["channel"].integerText, let moduleID = module.moduleID,
                      let time = slot["observed_ms"].text.flatMap(UInt64.init), samples[id] == nil,
                      expectedSlots[id] == Self.locator(gateway: module.gateway, address: module.address, module: moduleID, channel: channel) else { return nil }
                samples[id] = time
            }
        }
        return samples.count == 96 ? samples : nil
    }
    func assertModuleContext(_ header: HomeJSON, matches previous: HomeJSON? = nil) throws {
        let cursor = try events.cursor()
        try #require(header["boot"] == cursor["boot"] && header["revision"].integerText == cursor["revision"].integerText)
        try #require(header["transport_epoch"].integerText == cursor["transport_epoch"].integerText
            && header["observed_transport_epoch"] == header["transport_epoch"] && header["truth_source"] == .string("canonical_module_channels"))
        if let previous {
            for field in ["boot", "revision", "transport_epoch", "observed_transport_epoch"] { try #require(header[field] == previous[field]) }
        }
    }
    func freshModuleFeedback(after baseline: [String: UInt64], context: HomeJSON, deadline: ContinuousClock.Instant) async throws {
        let releaseBoundary = try #require(context["captured_ms"].text.flatMap(UInt64.init))
        while ContinuousClock.now < deadline {
            await home.loadPhysicalModules() // Metadata GET, never a physical query or control retry.
            guard ContinuousClock.now < deadline else { break }
            let header = try #require(await http.modules().last)
            try assertModuleContext(header, matches: context)
            if header["ready"] == .bool(true), let samples = moduleSamples(), samples.allSatisfy({ $0.value > max(baseline[$0.key] ?? UInt64.max, releaseBoundary) }),
               home.physicalHardwareModules.filter({ $0.profile == "r8i8_din_pro_v2_7_7" }).allSatisfy({ module in
                   module.slots.allSatisfy { $0["quality"] == .string("confirmed") && $0["actual"]["on"] == .bool(false) }
               }) {
                let devices = home.devices.filter { $0.raw["kind"] == .string("Switch") }
                if devices.count == 96 && devices.allSatisfy({ $0.raw["quality"] == .string("Confirmed") && $0.raw["observed"]["on"] == .bool(false) && $0.raw["state_authority"] == .string("feedback") }) {
                    try HomeJSON.array(home.moduleInventory.map(\.raw)).data(pretty: true).write(to: root.appendingPathComponent("native-away-fresh-modules.json"))
                    return
                }
            }
            try await Task.sleep(for: .milliseconds(10))
        }
        Issue.record("Timed out: all 96 genuine module receive timestamps must advance within the unchanged five-second window")
        throw URLError(.timedOut)
    }
    func finish() async throws {
        let snapshot: HomeJSON = .object(["connected": .bool(home.connected), "current": .bool(home.snapshotIsCurrent),
            "busy": .bool(home.busy), "revision": .string(home.revision), "devices": .array(home.devices.map(\.raw)), "scenes": .array(home.scenes.map(\.raw))])
        try snapshot.data(pretty: true).write(to: root.appendingPathComponent("native-final-or-failure-snapshot.json"))
        await release()
        let posts = await http.captured()
        try HomeJSON.array(posts).data(pretty: true).write(to: root.appendingPathComponent("native-posts.json"))
        let modules = await http.modules()
        try HomeJSON.array(modules).data(pretty: true).write(to: root.appendingPathComponent("native-module-gets.json"))
        try events.save(root.appendingPathComponent("native-wss.json"))
        try Data(home.diagnostics.exportText().utf8).write(to: root.appendingPathComponent("native-diagnostics.jsonl"))
        home.disconnect()
    }
    func assertBodies(unified expected: Int, total: Int) async throws {
        let posts = await http.captured(), unified = posts.filter { $0["path"] == .string("/api/v1/keypad-actions") }
        #expect(posts.count == total && unified.count == expected)
        #expect(Set(unified.compactMap { $0["body"]["request_id"].text }).count == expected)
        for request in unified {
            let body = request["body"]
            #expect(body == .object(["revision": .string("1"), "keypad": .string(panel), "key": body["key"], "request_id": body["request_id"], "gesture": .string("activate")]))
            #expect(request["status"] == .integer(202) && request["response"]["outcome"] == .string("accepted"))
            #expect(request["response"]["request_id"] == body["request_id"] && request["response"]["retry"] == .bool(false))
        }
    }
    func assertLinkedAfterFeedback(_ raw: String, write: Int) throws {
        let received = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("gateway-received.json")))
        let proof = try #require(received.values.first { $0["write"] == .integer(Int64(write)) })
        let led = try #require(try frames().first { $0["raw"] == .string(raw) })
        #expect(proof["journal_status"] == .string("written"))
        #expect(try #require(Double(led["at_ms"].formatted)) > #require(Double(proof["at_ms"].formatted)))
    }
}

@Suite @MainActor struct HomeUnifiedKeypadActualIntegrationTests {
    private func run(_ scenario: String, work: (UnifiedActualFixture) async throws -> Void) async throws {
        let fixture = try await UnifiedActualFixture(scenario: scenario)
        do { try await work(fixture); try await fixture.finish() }
        catch { try? await fixture.finish(); throw error }
    }
    @Test(.enabled(if: ProcessInfo.processInfo.environment["HOMESENSE_UNIFIED_KEYPAD_ACTUAL_FIXTURE"] != nil))
    func nativeUnifiedSceneFanSourceOrderRetirementAndPassiveWSS() async throws {
        try await run("scene") { f in
            try await f.until("initial genuine switch feedback") { f.device("light")["quality"] == .string("Confirmed") && f.device("light")["observed"]["on"] == .bool(true) }
            let baseline = try f.frames().count
            f.start(1); try await f.held()
            try await f.until("source LED and lamp write before held feedback") {
                try f.has("ae4101f2ee", after: baseline) && f.has("ae0301f2ee", after: baseline) && f.home.keypadActionPresentation("panel", key: 1)?.requestedColor == .green
            }
            let initial = try f.frames(after: baseline)
            #expect(try #require(initial.firstIndex { $0["raw"] == .string("ae4101f2ee") }) < #require(initial.firstIndex { $0["raw"] == .string("ae0301f2ee") }))
            #expect(try !f.has("ae4201f2ee", after: baseline))
            #expect(try f.key(1).led.actual == .unknown)
            try f.marker("release-feedback-1")
            try await f.until("confirmed lamp and gated linked LED") { try f.device("light")["state_authority"] == .string("feedback") && f.device("light")["observed"]["on"] == .bool(false) && f.has("ae4201f2ee", after: baseline) }
            try f.assertLinkedAfterFeedback("ae4201f2ee", write: 1)
            // Another authenticated client executes the ordinary room scene
            // while this App's first HTTP receipt is deliberately held.
            let beforeRoom = try f.frames().count
            let endpoint = try HomeServerEndpoint(f.origin)
            let request = try endpoint.request(path: "/api/v1/scene-actions", method: "POST", token: f.token,
                body: .object(["scene": .string("conditional"), "on": .bool(true), "request_id": .string(UUID().uuidString)]))
            let response = try await f.http.send(request); #expect(response.status == 202)
            try await f.until("ordinary room lighting write") { try f.has("ae0301f1ee", after: beforeRoom) }
            #expect(try !f.has("ae4101f1ee", after: beforeRoom) && !f.has("ae4201f1ee", after: beforeRoom))
            try f.marker("release-feedback-2")
            try await f.until("retired source cue and new LED provenance") {
                try f.has("ae4101f1ee", after: beforeRoom) && f.has("ae4201f1ee", after: beforeRoom)
                    && f.home.keypadActionPresentation("panel", key: 1) == nil && f.key(1).led.lastWritten == .red
            }
            try f.assertLinkedAfterFeedback("ae4101f1ee", write: 2)
            try f.assertLinkedAfterFeedback("ae4201f1ee", write: 2)
            await f.release()
            #expect(f.home.keypadActionPresentation("panel", key: 1) == nil)
            let beforeFan = try f.frames().count
            try await f.send(2)
            try await f.until("fan source cue before fan feedback") { try f.has("ae4102f1ee", after: beforeFan) && f.has("ae0302f1ee", after: beforeFan) }
            #expect(try !f.has("ae4202f1ee", after: beforeFan))
            try f.marker("release-feedback-3")
            try await f.until("fan confirmed and linked LED released") { try f.device("fan")["state_authority"] == .string("feedback") && f.device("fan")["observed"]["on"] == .bool(true) && f.has("ae4202f1ee", after: beforeFan) }
            try f.assertLinkedAfterFeedback("ae4202f1ee", write: 3)
            let posts = await f.postCount(), actuatorCount = try f.frames().filter { $0["kind"] == .string("switch") }.count
            try f.marker("inject-1", text: "ae41d1f1ee")
            try await f.until("first physical down frame updates key") { f.home.keypadEventIsPressed("panel", key: 1) }
            try f.marker("inject-2", text: "ae41d1f1ee")
            try f.marker("inject-3", text: "ae41d1f2ee")
            try await f.until("release frame updates key without replay") { !f.home.keypadEventIsPressed("panel", key: 1) }
            #expect(await f.postCount() == posts)
            #expect(try f.frames().filter { $0["kind"] == .string("switch") }.count == actuatorCount)
            #expect(try f.key(1).led.actual == .unknown)
            try await f.assertBodies(unified: 2, total: 3)
        }
    }

    @Test(.enabled(if: ProcessInfo.processInfo.environment["HOMESENSE_UNIFIED_KEYPAD_ACTUAL_FIXTURE"] != nil))
    func nativeOwnedWallGestureUpdatesCueWithoutClientPOST() async throws {
        try await run("wall") { f in
            try await f.until("initial genuine wall target feedback") {
                f.device("light")["quality"] == .string("Confirmed") && f.device("light")["observed"]["on"] == .bool(true)
            }
            // Establish an actual released state before a fresh short gesture.
            try f.marker("inject-1", text: "ae41d1f2ee")
            try await f.until("initial raw released proof") {
                let raw = f.device("panel")["keys"].values[0]["key_event"]
                return raw["quality"] == .string("Confirmed") && raw["pressed"] == .bool(false)
            }
            try await Task.sleep(for: .milliseconds(40))
            let baseline = try f.frames().count
            try f.marker("inject-2", text: "ae41d1f1ee")
            try await f.until("physical press outline") { f.home.keypadEventIsPressed("panel", key: 1) }
            #expect(try !f.has("ae0301f2ee", after: baseline))
            try f.marker("inject-3", text: "ae41d1f1ee")
            try await Task.sleep(for: .milliseconds(80))
            try f.marker("inject-4", text: "ae41d1f2ee")
            try await f.until("server-owned physical gesture and source LED without feedback") {
                try f.has("ae0301f2ee", after: baseline) && f.has("ae4101f2ee", after: baseline)
                    && f.home.keypadActionPresentation("panel", key: 1)?.requestedColor == .green
                    && f.home.keypadActionPresentation("panel", key: 1)?.phase == .written
            }
            let raw = try #require(f.home.devices.first { $0.id == "panel" }).raw["keys"].values[0]["indication"]
            let cursor = try f.events.cursor()
            let cue = try HomeKeypadActionIndication(raw, endpoint: "key:1", boot: cursor["boot"].text)
            #expect(cue.source == .buttonEvent && cue.eventSequence != nil)
            #expect(!f.home.keypadEventIsPressed("panel", key: 1))
            #expect(f.device("light")["observed"]["on"] == .bool(true))
            #expect(try f.key(1).led.actual == .unknown)
            #expect(try !f.has("ae4201f2ee", after: baseline))
            #expect(await f.postCount() == 0)
            try f.marker("release-feedback-1")
            try await f.until("wall target confirmed then linked LED") {
                try f.device("light")["state_authority"] == .string("feedback")
                    && f.device("light")["observed"]["on"] == .bool(false) && f.has("ae4201f2ee", after: baseline)
            }
            try f.assertLinkedAfterFeedback("ae4201f2ee", write: 1)
            try f.marker("inject-5", text: "ae41d1f2ee")
            try await Task.sleep(for: .milliseconds(150))
            #expect(try f.frames(after: baseline).filter { $0["kind"] == .string("switch") }.count == 1)
            #expect(try f.key(1).led.actual == .unknown)
            try await f.assertBodies(unified: 0, total: 0)
        }
    }

    @Test(.enabled(if: ProcessInfo.processInfo.environment["HOMESENSE_UNIFIED_KEYPAD_ACTUAL_FIXTURE"] != nil))
    func nativeUnifiedMotorRepeatStopAndExplicitStopSurviveHeldHTTP() async throws {
        try await run("motor") { f in
            f.start(1); try await f.held()
            try await f.until("open was written and remains estimated") { f.device("curtain")["motor_estimate"]["movement"] == .string("Opening") }
            #expect(f.home.busy && f.home.canActivateUnifiedKeypad("panel", key: 1) && f.home.canActivateUnifiedKeypad("panel", key: 3))
            let beforeStop = try f.frames().count
            try await f.send(1)
            let repeatReceipt = try #require(await f.http.captured().last)["response"]
            #expect(repeatReceipt["indication"]["color"] == .string("green"))
            try await f.until("repeat resolved STOP and source green") { try f.has("ae2201f0ee", after: beforeStop) && f.has("ae4101f2ee", after: beforeStop) && f.device("curtain")["motor_estimate"]["movement"] == .string("Stopped") }
            let frames = try f.frames(after: beforeStop)
            #expect(try #require(frames.firstIndex { $0["raw"] == .string("ae2201f0ee") }) < #require(frames.firstIndex { $0["raw"] == .string("ae4101f2ee") }))
            try await f.until("canonical intermediate idle colors retire the immediate direction cue") {
                try f.key(1).led.lastWritten == .red && f.key(2).led.lastWritten == .red && f.key(3).led.lastWritten == .green
                    && f.home.keypadActionPresentation("panel", key: 1) == nil
            }
            try await f.send(3)
            for _ in 0..<2 {
                #expect(f.home.canStopMotorDevice("curtain"))
                let result = await f.home.controlDevice("curtain", command: HomeMotorControlAction.stop.command)
                #expect(result.accepted)
            }
            let message = f.home.message
            await f.release()
            #expect(f.home.message == message && !f.home.busy)
            #expect(try f.key(1).led.actual == .unknown)
            try await f.until("all five accepted motor requests actually reach the wire") {
                try f.frames().filter { $0["kind"] == .string("motor") }.count == 5
            }
            let motor = try f.frames().filter { $0["kind"] == .string("motor") }.compactMap { $0["raw"].text }
            #expect(motor == ["ae2201f1ee", "ae2201f0ee", "ae2201f0ee", "ae2201f0ee", "ae2201f0ee"])
            try await f.assertBodies(unified: 3, total: 5)
        }
    }

    @Test(.enabled(if: ProcessInfo.processInfo.environment["HOMESENSE_UNIFIED_KEYPAD_ACTUAL_FIXTURE"] != nil))
    func nativeUnifiedSelectionAndAwayKeepCompleteScope() async throws {
        try await run("away") { f in
            try await f.until("fresh selection baseline") { f.home.scenes.first { $0.id == "lights-on" }?.raw["selected"] == .bool(false) }
            let before = try f.frames().count
            try await f.send(1)
            try await f.until("selection on from actual relay feedback") { f.device("relay-a:1")["quality"] == .string("Confirmed") && f.device("relay-a:1")["observed"]["on"] == .bool(true) }
            #expect(try f.has("ae4101f1ee", after: before))
            let awayStart = try f.frames().count
            await f.home.loadPhysicalModules()
            let baselineSamples = try #require(f.moduleSamples())
            let baselineContext = try #require(await f.http.modules().last)
            try f.assertModuleContext(baselineContext)
            #expect(f.expectedSlots.count == 96)
            try HomeJSON.object(f.expectedSlots.mapValues(HomeJSON.string)).data(pretty: true).write(to: f.root.appendingPathComponent("native-away-exact-coverage.json"))
            try HomeJSON.array(f.home.moduleInventory.map(\.raw)).data(pretty: true).write(to: f.root.appendingPathComponent("native-away-baseline-modules.json"))
            let sourceDeadline = ContinuousClock.now.advanced(by: .seconds(2))
            try await f.send(6)
            try await f.until("twelve modules and source green Written without feedback within the short source window", deadline: sourceDeadline) {
                let path = f.root.appendingPathComponent("away-written-proof.json")
                guard FileManager.default.fileExists(atPath: path.path) else { return false }
                let proof = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: path))
                return try proof["module_tickets"].values.count == 12 && proof["feedback_released"] == .bool(false)
                    && f.key(6).led.lastWritten == .green && f.frames(after: awayStart).filter { $0["kind"] == .string("module_off") }.count == 12
            }
            #expect(f.device("relay-a:1")["observed"]["on"] == .bool(true), "The last genuine physical feedback cannot become Off before the withheld reply")
            let beforeRelease = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: f.root.appendingPathComponent("gateway-received.json")))
            #expect(beforeRelease.values.allSatisfy { $0["kind"] != .string("released_away_feedback") })
            await f.home.loadPhysicalModules()
            let releaseContext = try #require(await f.http.modules().last)
            try f.assertModuleContext(releaseContext, matches: baselineContext)
            try releaseContext.data(pretty: true).write(to: f.root.appendingPathComponent("native-away-pre-release-module-context.json"))
            let feedbackDeadline = ContinuousClock.now.advanced(by: .seconds(5))
            try f.marker("release-away-feedback")
            try await f.freshModuleFeedback(after: baselineSamples, context: releaseContext, deadline: feedbackDeadline)
            #expect(try f.key(6).led.actual == .unknown)
            let received = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: f.root.appendingPathComponent("gateway-received.json")))
            #expect(received.values.filter { $0["kind"] == .string("released_away_feedback") }.count == 12)
            try await f.assertBodies(unified: 2, total: 2)
        }
    }
}
#endif
