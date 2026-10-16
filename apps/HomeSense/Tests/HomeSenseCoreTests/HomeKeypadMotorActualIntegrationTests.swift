#if os(macOS)
import Foundation
import Testing
@testable import HomeSenseCore

/// Only synthetic loopback traffic is retained; production logging remains the
/// separate allowlisted HomeDiagnostics encoder and never records whole bodies.
private actor MotorActualHTTP: HomeHTTPTransport {
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

private final class MotorActualEvents: HomeEventTransport, @unchecked Sendable {
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

@Suite @MainActor struct HomeKeypadMotorActualIntegrationTests {
    @Test(.enabled(if: ProcessInfo.processInfo.environment["HOMESENSE_KEYPAD_MOTOR_ACTUAL_FIXTURE"] != nil,
                   "Requires the fixed new backend, synthetic TCP fixture and temporary TLS origin"))
    func nativeURLSessionCardPressUsesAtomicServerDecisionAndDurableReceipt() async throws {
        let root = URL(fileURLWithPath: try #require(ProcessInfo.processInfo.environment["HOMESENSE_KEYPAD_MOTOR_ACTUAL_FIXTURE"]))
        let service = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("service.json")))
        try #require(service["fixture_kind"].text == "synthetic-loopback-keypad-motor")
        let origin = try #require(service["origin"].text)
        try #require(URL(string: origin)?.host == "localhost")
        let config = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("devices.json")))
        try #require(config["gateways"].values.allSatisfy { $0["endpoint"].text?.hasPrefix("127.0.0.1:") == true })
        let token = try String(contentsOf: root.appendingPathComponent("token"), encoding: .utf8)
        let trust = try FixtureWebSocketTrust(pem: String(contentsOf: root.appendingPathComponent("cert.pem"), encoding: .utf8))
        let http = MotorActualHTTP(HomeURLSessionTransport(delegate: trust))
        let diagnostics = HomeDiagnostics(fileURL: nil, emitSystemLog: false)
        let events = MotorActualEvents(HomeWebSocketTransport(delegate: trust))
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
        try await until("initial WSS", home: home) { home.receivingPush && home.canActivateKeypad("panel", key: 1, card: true) }
        let parent = try HomeKeypad(record: #require(home.devices.first { $0.id == "panel" }))
        #expect(parent.keys[0].cardAction == .motorPress && parent.keys[1].cardAction == .motorPress)
        #expect(parent.keys[2].cardAction == .absolute && parent.keys[3].cardAction == .absolute)
        #expect(parent.keys[0].motorAction == "open" && parent.keys[1].motorAction == "close")
        #expect(parent.keys[2].motorAction == "stop" && parent.keys[3].motorAction == "open")
        try HomeJSON.array(home.devices.map(\.raw)).data(pretty: true).write(to: root.appendingPathComponent("actual-client-initial-dto.json"))
        var phases: [HomeJSON] = []
        func motor() -> HomeJSON { home.devices.first { $0.id == "curtain" }?.raw["motor_estimate"] ?? .null }
        func perform(_ stage: String, key: Int, card: Bool = true, written: String, movement: String? = nil) async throws -> (URLRequest, HomeHTTPResponse) {
            let count = await http.captured().count
            let operation = home.beginKeypadClick("panel", key: key, card: card)
            if card { await home.activateKeypadCard("panel", key: key, operation: operation) }
            else { await home.activateKeypad("panel", key: key, operation: operation) }
            try #require(home.error == nil, "\(stage): \(home.error ?? "")")
            let posts = await http.captured(); try #require(posts.count == count + 1)
            let (request,response) = try #require(posts.last)
            let body = try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody))
            let receipt = try JSONDecoder().decode(HomeJSON.self, from: response.data)
            #expect(response.status == 202 && receipt["outcome"].text == "accepted")
            #expect(body["request_id"] == receipt["request_id"])
            #expect(body["request_id"].text != operation.id.uuidString)
            if card && [1,2].contains(key) {
                #expect(body["command"] == .object(["action": .string("keypad_press"), "keypad": .string("panel"), "key": .integer(Int64(key))]))
                #expect(body["revision"] == .string("1"))
            } else {
                #expect(body["command"]["action"].text == (key == 3 ? "stop" : "open"))
                #expect(body["revision"] == .null)
            }
            let id = try #require(receipt["commands"].values.first?.integerText)
            try await until(stage + " Written", home: home) {
                motor()["last_written_id"].integerText == id && motor()["last_written_command"].text == written && motor()["pending"] == .null
            }
            if let movement { try await until(stage + " movement", home: home) { motor()["movement"].text == movement } }
            phases.append(.object(["stage": .string(stage), "operation_id": .string(operation.id.uuidString), "request": body,
                "receipt": receipt, "http_status": .integer(Int64(response.status)), "motor_estimate": motor()]))
            try HomeJSON.array(phases).data(pretty: true).write(to: root.appendingPathComponent("actual-client-phases.json"))
            return (request,response)
        }
        let (firstRequest, firstResponse) = try await perform("unknown-to-open", key: 1, written: "Open", movement: "Opening")
        let firstEstimate = motor()
        let repeated = try await http.repeatExactly(firstRequest)
        let sameReceipt = try JSONDecoder().decode(HomeJSON.self, from: repeated.data) == JSONDecoder().decode(HomeJSON.self, from: firstResponse.data)
        #expect(repeated.status == 200 && sameReceipt)
        try await Task.sleep(for: .milliseconds(120))
        #expect(motor()["last_written_id"] == firstEstimate["last_written_id"])
        try HomeJSON.object(["request": try JSONDecoder().decode(HomeJSON.self, from: #require(firstRequest.httpBody)), "status": .integer(Int64(repeated.status)),
                             "same_recorded_receipt": .bool(sameReceipt)]).data(pretty: true).write(to: root.appendingPathComponent("explicit-test-duplicate.json"))
        _ = try await perform("same-open-to-stop", key: 1, written: "Stop", movement: "Stopped")
        _ = try await perform("stopped-to-close", key: 2, written: "Close", movement: "Closing")
        _ = try await perform("reverse-to-open", key: 1, written: "Open", movement: "Opening")
        _ = try await perform("details-open-stays-absolute", key: 1, card: false, written: "Open", movement: "Opening")
        _ = try await perform("standalone-card-stop", key: 3, written: "Stop", movement: "Stopped")
        _ = try await perform("legacy-absolute-card-open", key: 4, written: "Open", movement: "Opening")
        _ = try await perform("same-open-stops-again", key: 1, written: "Stop", movement: "Stopped")
        _ = try await perform("close-to-endpoint", key: 2, written: "Close", movement: "Closing")
        try await until("closed endpoint", home: home, seconds: 8) { motor()["movement"].text == "Stopped" && motor()["position"].text == "Closed" }
        _ = try await perform("closed-endpoint-press-is-close", key: 2, written: "Close")
        _ = try await perform("final-explicit-stop", key: 3, card: false, written: "Stop", movement: "Stopped")
        try #require(phases.count == 11)
        let trace = try diagnostics.exportText().split(separator: "\n").dropFirst().map { try JSONDecoder().decode(HomeJSON.self, from: Data($0.utf8)) }
        for phase in phases {
            let operation = phase["operation_id"], requestID = phase["request"]["request_id"]
            #expect(trace.contains { $0["operation_id"] == operation && $0["event"].text == "button.click" })
            #expect(trace.contains { $0["operation_id"] == operation && $0["event"].text == "http.send" && $0["request_id"] == requestID })
            #expect(trace.contains { $0["operation_id"] == operation && $0["event"].text == "http.response" && $0["request_id"] == requestID && $0["http_status"] == .integer(202) })
            #expect(trace.contains { $0["operation_id"] == operation && $0["event"].text == "operation.complete" })
        }
        let final = try HomeKeypad(record: #require(home.devices.first { $0.id == "panel" }))
        #expect(final.keys.allSatisfy { $0.led.actual == .unknown })
        try HomeJSON.array(home.devices.map(\.raw)).data(pretty: true).write(to: root.appendingPathComponent("actual-client-final-dto.json"))
    }
    private func until(_ stage: String, home: HomeServerStore, seconds: Double = 5, _ predicate: () -> Bool) async throws {
        let limit = ContinuousClock.now.advanced(by: .milliseconds(Int64(seconds * 1000)))
        while ContinuousClock.now < limit { if predicate() { return }; try await Task.sleep(for: .milliseconds(20)) }
        throw HomeServerError.server("Timeout \(stage); \(home.message); \(home.error ?? ""); devices=\(HomeJSON.array(home.devices.map(\.raw)).formatted)")
    }
}
#endif
