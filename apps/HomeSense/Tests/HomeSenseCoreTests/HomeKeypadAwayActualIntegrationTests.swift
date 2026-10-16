#if os(macOS)
import Foundation
import Testing
@testable import HomeSenseCore

/// Only synthetic loopback traffic is retained; production logging remains the
/// separate allowlisted HomeDiagnostics encoder and never records whole bodies.
private actor AwayActualHTTP: HomeHTTPTransport {
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

private final class AwayActualEvents: HomeEventTransport, @unchecked Sendable {
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

@Suite @MainActor struct HomeKeypadAwayActualIntegrationTests {
    @Test(.enabled(if: ProcessInfo.processInfo.environment["HOMESENSE_KEYPAD_AWAY_ACTUAL_FIXTURE"] != nil,
                   "Requires the pinned Away backend, synthetic loopback and temporary TLS origin"))
    func nativeAwayCardUsesOnePOSTAndRetainsHiddenWSSFactsUntilActualFeedback() async throws {
        let root = URL(fileURLWithPath: try #require(ProcessInfo.processInfo.environment["HOMESENSE_KEYPAD_AWAY_ACTUAL_FIXTURE"]))
        let service = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("service.json")))
        try #require(service["fixture_kind"].text == "synthetic-loopback-keypad-away")
        let origin = try #require(service["origin"].text); try #require(URL(string: origin)?.host == "localhost")
        let config = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("devices.json")))
        try #require(config["gateways"].values.allSatisfy { $0["endpoint"].text?.hasPrefix("127.0.0.1:") == true })
        let token = try String(contentsOf: root.appendingPathComponent("token"), encoding: .utf8)
        let trust = try FixtureWebSocketTrust(pem: String(contentsOf: root.appendingPathComponent("cert.pem"), encoding: .utf8))
        let http = AwayActualHTTP(HomeURLSessionTransport(delegate: trust))
        let diagnostics = HomeDiagnostics(fileURL: nil, emitSystemLog: false)
        let events = AwayActualEvents(HomeWebSocketTransport(delegate: trust))
        let home = HomeServerStore(transport: http, eventTransport: events, defaults: nil,
                                  credentialStore: InMemoryCredentials(), diagnostics: diagnostics)
        home.endpointText = origin; await home.connect(token: token)
        defer {
            home.disconnect()
            try? Data(diagnostics.exportText().utf8).write(to: root.appendingPathComponent("home-diagnostics.jsonl"))
            try? events.save(root.appendingPathComponent("actual-client-wss.json"))
        }
        try #require(home.connected, "\(home.error ?? "")")
        try await until { home.receivingPush && home.canActivateKeypad("entry-panel", key: 6, card: true) }
        let initial = try HomeKeypad(record: #require(home.devices.first { $0.id == "entry-panel" }))
        #expect(initial.keys[5].cardAction == .awayOff && !home.canActivateKeypad("entry-panel", key: 6))
        #expect((1...5).allSatisfy { home.canActivateKeypad("entry-panel", key: $0, card: true) })
        #expect(home.devices.filter { $0.raw["catalog_visible"] == .bool(false) }.count == 16)
        #expect(home.dailyDevices.map(\.id) == ["entry-panel"])
        #expect(home.canControlDevice("relay-a:1") && home.canControlScene("lights-on"))
        try HomeJSON.array(home.devices.map(\.raw)).data(pretty: true).write(to: root.appendingPathComponent("actual-client-initial-dto.json"))
        let operation = home.beginKeypadClick("entry-panel", key: 6, card: true)
        await home.activateKeypadCard("entry-panel", key: 6, operation: operation)
        let posts = await http.captured(); try #require(posts.count == 1)
        let (request,response) = posts[0]
        let body = try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody))
        let receipt = try JSONDecoder().decode(HomeJSON.self, from: response.data)
        #expect(request.url?.path == "/api/v1/away-off" && response.status == 202 && receipt["outcome"].text == "accepted")
        #expect(body == .object(["revision": .string(home.revision), "request_id": body["request_id"],
            "command": .object(["action": .string("keypad_press"), "keypad": .string("entry-panel"), "key": .integer(6)])]))
        #expect(body["request_id"].text.flatMap(UUID.init(uuidString:)) != nil)
        try await until {
            let lamps = home.devices.filter { $0.id.hasPrefix("relay-") }
            let parent = home.devices.first { $0.id == "entry-panel" }.flatMap { try? HomeKeypad(record: $0) }
            return lamps.count == 16 && lamps.allSatisfy { $0.raw["observed"]["on"] == .bool(false) && $0.raw["quality"].text == "Confirmed" }
                && parent?.keys[5].led.lastWritten == .green
        }
        #expect(await http.captured().count == 1, "Actual feedback/LED green never causes a follow-up write")
        #expect(home.dailyDevices.map(\.id) == ["entry-panel"] && home.devices.count == 17)
        try HomeJSON.array(home.devices.map(\.raw)).data(pretty: true).write(to: root.appendingPathComponent("actual-client-final-dto.json"))
        try HomeJSON.object(["operation_id": .string(operation.id.uuidString), "request": body, "http_status": .integer(Int64(response.status)), "receipt": receipt]).data(pretty: true)
            .write(to: root.appendingPathComponent("actual-client-away-operation.json"))
        let chain = try diagnostics.exportText().split(separator: "\n").dropFirst().map { try JSONDecoder().decode(HomeJSON.self, from: Data($0.utf8)) }.filter { $0["operation_id"].text == operation.id.uuidString }
        #expect(chain.compactMap { $0["event"].text } == ["button.click","operation.begin","gate.check","http.send","http.response","operation.complete"])
        for secret in [token,origin,"Authorization","entry-panel"] { #expect(!diagnostics.exportText().contains(secret)) }
    }
    private func until(_ condition: () -> Bool) async throws {
        for _ in 0..<500 { if condition() { return }; try await Task.sleep(for: .milliseconds(20)) }
        throw URLError(.timedOut)
    }
}
#endif
