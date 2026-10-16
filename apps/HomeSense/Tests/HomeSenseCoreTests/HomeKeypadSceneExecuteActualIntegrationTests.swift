#if os(macOS)
import Foundation
import Testing
@testable import HomeSenseCore

/// Only synthetic loopback traffic is retained; production logging remains the
/// separate allowlisted HomeDiagnostics encoder and never records whole bodies.
private actor SceneExecuteActualHTTP: HomeHTTPTransport {
    let base: HomeURLSessionTransport
    var posts: [(URLRequest, HomeHTTPResponse)] = []
    init(_ base: HomeURLSessionTransport) { self.base = base }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        precondition(request.url?.host == "localhost")
        let response = try await base.send(request)
        if request.httpMethod == "POST" { posts.append((request, response)) }
        return response
    }
    func captured() -> [(URLRequest, HomeHTTPResponse)] { posts }
    // Explicit duplicate at the test transport boundary, never an App retry.
    func repeatExactly(_ request: URLRequest) async throws -> HomeHTTPResponse { try await base.send(request) }
}

private final class SceneExecuteActualEvents: HomeEventTransport, @unchecked Sendable {
    let base: HomeWebSocketTransport
    let lock = NSLock()
    private var frames: [Data] = []
    init(_ base: HomeWebSocketTransport) { self.base = base }
    private func record(_ data: Data) { lock.withLock { if frames.count < 1024 { frames.append(data) } } }
    func save(_ url: URL) throws {
        let snapshot = lock.withLock { frames }
        let values = try snapshot.map { try JSONDecoder().decode(HomeJSON.self, from: $0) }
        try HomeJSON.array(values).data(pretty: true).write(to: url)
    }
    func events(_ request: URLRequest) -> AsyncThrowingStream<HomeEventMessage, Error> {
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

@Suite @MainActor struct HomeKeypadSceneExecuteActualIntegrationTests {
    @Test(.enabled(if: ProcessInfo.processInfo.environment["HOMESENSE_SCENE_EXECUTE_ACTUAL_FIXTURE"] != nil,
                   "Requires the final scene_execute backend and private loopback conditional AST fixture"))
    func nativeSceneExecuteUsesFreshFeedbackBranchesAndOneExplicitActivationPerClick() async throws {
        let root = URL(fileURLWithPath: try #require(ProcessInfo.processInfo.environment["HOMESENSE_SCENE_EXECUTE_ACTUAL_FIXTURE"]))
        let service = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("service.json")))
        try #require(service["fixture_kind"].text == "synthetic-loopback-keypad-scene-execute")
        let origin = try #require(service["origin"].text)
        try #require(URL(string: origin)?.host == "localhost")
        let config = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("devices.json")))
        try #require(config["gateways"].values.allSatisfy { $0["endpoint"].text?.hasPrefix("127.0.0.1:") == true })
        let automation = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("automation.json")))
        try #require(automation["rules"] == .array([]) && automation["indicator_links"] == .null)
        let token = try String(contentsOf: root.appendingPathComponent("token"), encoding: .utf8)
        let trust = try FixtureWebSocketTrust(pem: String(contentsOf: root.appendingPathComponent("cert.pem"), encoding: .utf8))
        let http = SceneExecuteActualHTTP(HomeURLSessionTransport(delegate: trust))
        let events = SceneExecuteActualEvents(HomeWebSocketTransport(delegate: trust))
        let diagnostics = HomeDiagnostics(fileURL: nil, emitSystemLog: false)
        let home = HomeServerStore(transport: http, eventTransport: events, defaults: nil,
                                   credentialStore: InMemoryCredentials(), diagnostics: diagnostics)
        home.endpointText = origin
        await home.connect(token: token)
        defer {
            home.disconnect()
            try? Data(diagnostics.exportText().utf8).write(to: root.appendingPathComponent("home-diagnostics.jsonl"))
            try? events.save(root.appendingPathComponent("actual-client-wss.json"))
        }
        try #require(home.connected, "\(home.error ?? "")")
        func light() -> HomeJSON { home.devices.first { $0.id == "light" }?.raw ?? .null }
        func scene() -> HomeJSON { home.scenes.first { $0.id == "conditional" }?.raw ?? .null }
        func key() throws -> HomeKeypadKey { try HomeKeypad(record: #require(home.devices.first { $0.id == "panel" })).keys[0] }
        func rawLED() -> HomeJSON { home.devices.first { $0.id == "panel" }?.raw["keys"].values.first?["led"] ?? .null }
        // Missing or unknown physical feedback never qualifies the first action.
        try await until("fresh initial feedback and WSS", home: home) {
            home.receivingPush && home.canActivateKeypad("panel", key: 1, card: true) &&
            light()["quality"].text == "Confirmed" && light()["observed"]["on"] == .bool(true) &&
            light()["state_authority"].text == "feedback"
        }
        let initialKey = try key()
        #expect(initialKey.executesScene && initialKey.actionScene == "conditional" && initialKey.cardAction == .absolute)
        #expect(scene()["execution_kind"].text == "immediate" && scene()["selected"] == .null)
        #expect(scene()["relative_unknown_allowed"] == .bool(false))
        #expect(initialKey.led.actual == .unknown)
        try HomeJSON.array(home.devices.map(\.raw)).data(pretty: true).write(to: root.appendingPathComponent("actual-client-initial-dto.json"))
        try scene().data(pretty: true).write(to: root.appendingPathComponent("actual-client-initial-scene.json"))
        var phases: [HomeJSON] = []
        for (stage, on, color) in [("then-from-physical-on", false, "green"), ("else-from-physical-off", true, "red")] {
            try #require(home.canActivateKeypad("panel", key: 1, card: true))
            let prior = await http.captured().count
            let operation = home.beginKeypadClick("panel", key: 1, card: true)
            await home.activateKeypadCard("panel", key: 1, operation: operation)
            try #require(home.error == nil, "\(stage): \(home.error ?? "")")
            let posts = await http.captured(); try #require(posts.count == prior + 1, "Exactly one explicit request per click, no retry")
            let (request, response) = try #require(posts.last)
            let body = try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody))
            let receipt = try JSONDecoder().decode(HomeJSON.self, from: response.data)
            #expect(request.url?.path == "/api/v1/scene-actions" && request.httpMethod == "POST")
            #expect(body["scene"].text == "conditional" && body["on"] == .bool(true))
            #expect(response.status == 202 && receipt["outcome"].text == "accepted" && receipt["execution_kind"].text == "immediate")
            #expect(body["request_id"] == receipt["request_id"] && body["request_id"].text.flatMap(UUID.init(uuidString:)) != nil)
            #expect(!phases.contains { $0["request"]["request_id"] == body["request_id"] })
            let commands = receipt["receipt"]["commands"].values.compactMap(\.integerText)
            try #require(commands.count == 2 && Set(commands).count == 2)
            #expect(receipt["receipt"]["unknown_branches"] == .array([]))
            try await until(stage + " actual relay feedback and written LED", home: home) {
                home.receivingPush && light()["quality"].text == "Confirmed" &&
                light()["state_authority"].text == "feedback" && light()["observed"]["on"] == .bool(on) &&
                rawLED()["last_written"]["color"].text == color && rawLED()["last_written"]["status"].text == "written" &&
                commands.contains(rawLED()["last_written"]["command"].integerText ?? "")
            }
            #expect(scene()["selected"] == .null, "No client or backend toggle selection is needed to execute the script")
            #expect(try key().led.actual == .unknown, "Written LED target is not measured LED feedback")
            phases.append(.object(["stage": .string(stage), "operation_id": .string(operation.id.uuidString),
                "request": body, "receipt": receipt, "http_status": .integer(Int64(response.status)),
                "light": light(), "LED": rawLED(), "scene": scene()]))
            try HomeJSON.array(phases).data(pretty: true).write(to: root.appendingPathComponent("actual-client-phases.json"))
        }
        try #require(phases.count == 2)
        #expect(await http.captured().count == 2)
        let trace = try diagnostics.exportText().split(separator: "\n").dropFirst().map {
            try JSONDecoder().decode(HomeJSON.self, from: Data($0.utf8))
        }
        for phase in phases {
            let operation = phase["operation_id"], requestID = phase["request"]["request_id"]
            let chain = trace.filter { $0["operation_id"] == operation }
            #expect(chain.first?["event"].text == "button.click" && chain.last?["event"].text == "operation.complete")
            #expect(chain.contains { $0["event"].text == "scene.decision" && $0["on"] == .bool(true) })
            #expect(chain.filter { $0["event"].text == "http.send" && $0["request_id"] == requestID && $0["method"].text == "POST" }.count == 1)
            #expect(chain.contains { $0["event"].text == "http.response" && $0["request_id"] == requestID && $0["http_status"] == .integer(202) })
        }
        try HomeJSON.array(home.devices.map(\.raw)).data(pretty: true).write(to: root.appendingPathComponent("actual-client-final-dto.json"))
    }
    private func until(_ stage: String, home: HomeServerStore, seconds: Double = 5, _ predicate: () -> Bool) async throws {
        let limit = ContinuousClock.now.advanced(by: .milliseconds(Int64(seconds * 1000)))
        while ContinuousClock.now < limit { if predicate() { return }; try await Task.sleep(for: .milliseconds(20)) }
        throw HomeServerError.server("Timeout \(stage); \(home.message); \(home.error ?? "")")
    }
}
#endif
