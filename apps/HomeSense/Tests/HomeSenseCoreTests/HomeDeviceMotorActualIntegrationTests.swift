#if os(macOS)
import Foundation
import Testing
@testable import HomeSenseCore

/// Only synthetic loopback traffic is retained; production logging remains the
/// separate allowlisted HomeDiagnostics encoder and never records whole bodies.
private actor DeviceMotorActualHTTP: HomeHTTPTransport {
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

private final class DeviceMotorActualEvents: HomeEventTransport, @unchecked Sendable {
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

@Suite @MainActor struct HomeDeviceMotorActualIntegrationTests {
    @Test(.enabled(if: ProcessInfo.processInfo.environment["HOMESENSE_DEVICE_MOTOR_ACTUAL_FIXTURE"] != nil,
                   "Requires the pinned final backend, isolated loopback bus and temporary TLS origin"))
    func nativeMotorCardPressUsesRevisionAndAtomicServerDecisionOverTLSWSS() async throws {
        let root = URL(fileURLWithPath: try #require(ProcessInfo.processInfo.environment["HOMESENSE_DEVICE_MOTOR_ACTUAL_FIXTURE"]))
        let service = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("service.json")))
        try #require(service["fixture_kind"].text == "synthetic-loopback-device-motor")
        let origin = try #require(service["origin"].text)
        try #require(URL(string: origin)?.host == "localhost")
        let config = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("devices.json")))
        try #require(config["gateways"].values.allSatisfy { $0["endpoint"].text?.hasPrefix("127.0.0.1:") == true })
        let token = try String(contentsOf: root.appendingPathComponent("token"), encoding: .utf8)
        let trust = try FixtureWebSocketTrust(pem: String(contentsOf: root.appendingPathComponent("cert.pem"), encoding: .utf8))
        let http = DeviceMotorActualHTTP(HomeURLSessionTransport(delegate: trust))
        let diagnostics = HomeDiagnostics(fileURL: nil, emitSystemLog: false)
        let events = DeviceMotorActualEvents(HomeWebSocketTransport(delegate: trust))
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
        try await until("initial WSS", home: home) { home.receivingPush && home.canControlDevice("curtain") }
        try #require(home.revision == "1")
        try HomeJSON.array(home.devices.map(\.raw)).data(pretty: true).write(to: root.appendingPathComponent("actual-client-initial-dto.json"))
        var phases: [HomeJSON] = []
        func motor() -> HomeJSON { home.devices.first { $0.id == "curtain" }?.raw["motor_estimate"] ?? .null }
        func perform(_ stage: String, action: HomeMotorControlAction, legacy: Bool = false,
                     written: String, movement: String) async throws -> (URLRequest, HomeHTTPResponse) {
            let count = await http.captured().count
            let operation = home.beginDeviceControl("curtain")
            let command: HomeJSON = legacy ? .object(["action": .string(action.rawValue)]) : action.command
            let result = await home.controlDevice("curtain", command: command, operation: operation)
            try #require(result.accepted && home.error == nil, "\(stage): \(home.error ?? "")")
            let posts = await http.captured(); try #require(posts.count == count + 1, "Exactly one POST per App intent; no retry")
            let (request, response) = try #require(posts.last)
            let body = try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody))
            let receipt = try JSONDecoder().decode(HomeJSON.self, from: response.data)
            #expect(request.url?.path == "/api/v1/device-commands" && request.httpMethod == "POST")
            #expect(body["device"].text == "curtain")
            if !legacy && action != .stop {
                #expect(body["command"] == .object(["action": .string("motor_press"), "direction": .string(action.rawValue)]))
                #expect(body["revision"] == .string("1"))
            } else {
                #expect(body["command"] == .object(["action": .string(action.rawValue)]))
                #expect(body["revision"] == .null)
            }
            #expect(response.status == 202 && receipt["outcome"].text == "accepted")
            #expect(body["request_id"] == receipt["request_id"])
            let requestID = try #require(body["request_id"].text)
            #expect(result == .accepted(requestID: requestID))
            #expect(!phases.contains { $0["request"]["request_id"].text == requestID })
            let id = try #require(receipt["commands"].values.first?.integerText)
            try await until(stage + " Written and WSS movement", home: home) {
                home.receivingPush && motor()["last_written_id"].integerText == id &&
                motor()["last_written_command"].text == written && motor()["pending"] == .null &&
                motor()["movement"].text == movement
            }
            phases.append(.object(["stage": .string(stage), "operation_id": .string(operation.id.uuidString),
                "request": body, "receipt": receipt, "http_status": .integer(Int64(response.status)), "motor_estimate": motor()]))
            try HomeJSON.array(phases).data(pretty: true).write(to: root.appendingPathComponent("actual-client-phases.json"))
            return (request, response)
        }
        let (firstRequest, firstResponse) = try await perform("typed-open", action: .open, written: "Open", movement: "Opening")
        let firstEstimate = motor()
        // A deliberate test-boundary duplicate, never an automatic App retry.
        let repeated = try await http.repeatExactly(firstRequest)
        let sameReceipt = try JSONDecoder().decode(HomeJSON.self, from: repeated.data) == JSONDecoder().decode(HomeJSON.self, from: firstResponse.data)
        #expect(repeated.status == 200 && sameReceipt)
        try await Task.sleep(for: .milliseconds(120))
        #expect(motor()["last_written_id"] == firstEstimate["last_written_id"])
        try HomeJSON.object(["request": try JSONDecoder().decode(HomeJSON.self, from: #require(firstRequest.httpBody)),
            "status": .integer(Int64(repeated.status)), "same_recorded_receipt": .bool(sameReceipt)])
            .data(pretty: true).write(to: root.appendingPathComponent("explicit-test-duplicate.json"))
        _ = try await perform("same-open-stops", action: .open, written: "Stop", movement: "Stopped")
        _ = try await perform("typed-close", action: .close, written: "Close", movement: "Closing")
        _ = try await perform("same-close-stops", action: .close, written: "Stop", movement: "Stopped")
        _ = try await perform("open-before-independent-stop", action: .open, written: "Open", movement: "Opening")
        _ = try await perform("independent-stop", action: .stop, written: "Stop", movement: "Stopped")
        _ = try await perform("legacy-absolute-open-first", action: .open, legacy: true, written: "Open", movement: "Opening")
        _ = try await perform("legacy-absolute-open-again-stays-open", action: .open, legacy: true, written: "Open", movement: "Opening")
        _ = try await perform("final-independent-stop", action: .stop, written: "Stop", movement: "Stopped")
        try #require(phases.count == 9)
        let finalWrittenID = motor()["last_written_id"]
        var rejected: [HomeJSON] = []
        for (label, revision, status) in [("missing", HomeJSON.null, 400), ("stale", .string("999999"), 409)] {
            var request = firstRequest
            var body = try JSONDecoder().decode(HomeJSON.self, from: #require(firstRequest.httpBody))
            body["request_id"] = .string(UUID().uuidString)
            body = revision == .null ? body.removing(["revision"]) : body
            if revision != .null { body["revision"] = revision }
            request.httpBody = try body.data()
            let before = await http.captured().count
            let response = try await http.send(request) // Explicit negative protocol test; not a UI operation.
            #expect(response.status == status)
            let responseBody = try JSONDecoder().decode(HomeJSON.self, from: response.data)
            #expect(responseBody["outcome"].text != "accepted")
            #expect(await http.captured().count == before + 1)
            rejected.append(.object(["case": .string(label), "request": body,
                "status": .integer(Int64(response.status)), "response": responseBody]))
        }
        try await Task.sleep(for: .milliseconds(150))
        #expect(motor()["last_written_id"] == finalWrittenID && motor()["movement"].text == "Stopped")
        try HomeJSON.array(rejected).data(pretty: true).write(to: root.appendingPathComponent("revision-rejections.json"))
        let trace = try diagnostics.exportText().split(separator: "\n").dropFirst().map {
            try JSONDecoder().decode(HomeJSON.self, from: Data($0.utf8))
        }
        for phase in phases {
            let operation = phase["operation_id"], requestID = phase["request"]["request_id"]
            let chain = trace.filter { $0["operation_id"] == operation }
            #expect(chain.first?["event"].text == "button.click" && chain.last?["event"].text == "operation.complete")
            #expect(chain.filter { $0["event"].text == "http.send" && $0["method"].text == "POST" && $0["request_id"] == requestID }.count == 1)
            #expect(chain.contains { $0["event"].text == "http.response" && $0["request_id"] == requestID && $0["http_status"] == .integer(202) })
        }
        #expect(await http.captured().count == 11, "Nine App intents plus two explicit negative protocol probes")
        try HomeJSON.array(home.devices.map(\.raw)).data(pretty: true).write(to: root.appendingPathComponent("actual-client-final-dto.json"))
    }
    private func until(_ stage: String, home: HomeServerStore, seconds: Double = 5, _ predicate: () -> Bool) async throws {
        let limit = ContinuousClock.now.advanced(by: .milliseconds(Int64(seconds * 1000)))
        while ContinuousClock.now < limit { if predicate() { return }; try await Task.sleep(for: .milliseconds(20)) }
        throw HomeServerError.server("Timeout \(stage); \(home.message); \(home.error ?? "")")
    }
}
#endif
