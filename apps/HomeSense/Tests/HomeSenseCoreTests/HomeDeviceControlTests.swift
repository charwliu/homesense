import Foundation
import Testing
@testable import HomeSenseCore

private actor DeviceCommandHTTP: HomeHTTPTransport {
    let base = HomeTestServer()
    var mode = "accepted"
    var posts: [URLRequest] = []
    var held: CheckedContinuation<Void, Never>?
    var hold = false
    func configure(_ mode: String) { self.mode = mode }
    func holdNext() { hold = true }
    func release() { hold = false; held?.resume(); held = nil }
    func writes() -> [URLRequest] { posts }
    func prepare() async {
        await base.setPresentedDevices([
            .object(["id": .string("light"), "name": .string("Synthetic light"), "kind": .string("Dimmer"), "can_control": .bool(true), "quality": .string("Confirmed"), "action_gate": .object(["available": .bool(true)]), "observed": .object(["level": .integer(20), "on": .bool(true)]), "observed_at_ms": .integer(1)]),
            .object(["id": .string("curtain"), "name": .string("Synthetic curtain"), "kind": .string("Motor"), "motor_calibrated": .bool(true), "can_control": .bool(true), "action_gate": .object(["available": .bool(true)]), "motor_estimate": .object(["movement": .string("Stopped")])])
        ])
    }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        if request.url?.path == "/api/v1/device-commands", request.httpMethod == "POST" {
            posts.append(request)
            if hold { await withCheckedContinuation { held = $0 } }
            if mode == "timeout" { throw URLError(.timedOut) }
            if mode == "denied" { return HomeHTTPResponse(data: try HomeJSON.object(["error": .string("source_scope_changed")]).data(), status: 403) }
            let body = try JSONDecoder().decode(HomeJSON.self, from: request.httpBody!)
            var result: HomeJSON = .object(["outcome": .string("accepted"), "request_id": body["request_id"]])
            if mode == "wrong_id" { result["request_id"] = .string(UUID().uuidString) }
            if mode == "missing_id" { result["request_id"] = .null }
            return HomeHTTPResponse(data: try result.data(), status: 202)
        }
        return try await base.send(request)
    }
}

@Suite @MainActor struct HomeDeviceControlTests {
    private func connected() async throws -> (HomeServerStore, DeviceCommandHTTP, HomeDiagnostics) {
        let http = DeviceCommandHTTP(); await http.prepare()
        let log = HomeDiagnostics(fileURL: nil, emitSystemLog: false)
        let home = HomeServerStore(transport: http, defaults: nil, credentialStore: InMemoryCredentials(), diagnostics: log)
        home.pause(); home.endpointText = "https://home.example:8444"; await home.connect(token: "test-token")
        try #require(home.canControlDevice("light") && home.canControlDevice("curtain"))
        return (home, http, log)
    }
    @Test func explicitMotorDirectionsAndLightPowerRemainIndependentAndNeverFabricateFeedback() async throws {
        let (home, http, _) = try await connected(); defer { home.disconnect() }
        let commands: [(String, HomeJSON)] = [
            ("curtain", .object(["action": .string("open")])),
            ("curtain", .object(["action": .string("open")])),
            ("curtain", .object(["action": .string("stop")])),
            ("curtain", .object(["action": .string("close")])),
            ("light", .object(["action": .string("power"), "on": .bool(false)])),
            ("light", .object(["action": .string("power"), "on": .bool(true)])),
            ("light", .object(["action": .string("brightness"), "level": .integer(0)])),
            ("light", .object(["action": .string("brightness"), "level": .integer(65)]))
        ]
        for (id, command) in commands {
            let result = await home.controlDevice(id, command: command)
            #expect(result.accepted)
        }
        let requests = await http.writes()
        #expect(requests.count == commands.count)
        var ids: Set<String> = []
        for (request, pair) in zip(requests, commands) {
            let body = try JSONDecoder().decode(HomeJSON.self, from: request.httpBody!)
            #expect(body["device"].text == pair.0 && body["command"] == pair.1)
            ids.insert(try #require(body["request_id"].text))
        }
        #expect(ids.count == commands.count)
        #expect(home.devices.first { $0.id == "light" }?.raw["observed"]["level"] == .integer(20))
    }
    @Test func aDirectControlKeepsOneCorrelatedClickRequestAndTerminalChain() async throws {
        let (home, http, log) = try await connected(); defer { home.disconnect() }
        let operation = home.beginDeviceControl("light")
        let result = await home.controlDevice("light", command: .object(["action": .string("brightness"), "level": .integer(70)]), operation: operation)
        let requests = await http.writes(); let body = try JSONDecoder().decode(HomeJSON.self, from: requests[0].httpBody!)
        #expect(result == .accepted(requestID: try #require(body["request_id"].text)))
        let rows = try log.exportText().split(separator: "\n").dropFirst().map { try JSONDecoder().decode(HomeJSON.self, from: Data($0.utf8)) }
        let chain = rows.filter { $0["operation_id"].text == operation.id.uuidString }
        #expect(chain.first?["event"].text == "button.click" && chain.last?["event"].text == "operation.complete")
        for stage in ["operation.begin", "http.send", "http.response"] { #expect(chain.contains { $0["event"].text == stage }) }
        #expect(chain.filter { $0["event"].text == "http.send" && $0["method"].text == "POST" && $0["path"].text == "/api/v1/device-commands" }.count == 1)
        #expect(chain.first { $0["event"].text == "http.response" }?["request_id"] == body["request_id"])
    }
    @Test(arguments: ["wrong_id", "missing_id", "timeout", "denied"])
    func missingOrFailedReceiptIsUnconfirmedWithoutRetry(_ mode: String) async throws {
        let (home, http, _) = try await connected(); defer { home.disconnect() }
        await http.configure(mode)
        let result = await home.controlDevice("light", command: .object(["action": .string("brightness"), "level": .integer(90)]))
        #expect(!result.accepted && result != .notSent)
        #expect(await http.writes().count == 1)
        #expect(home.devices.first { $0.id == "light" }?.raw["observed"]["level"] == .integer(20))
    }
    @Test func disconnectedAndBusyCallsReturnNotSentWithoutASecondCommand() async throws {
        let (home, http, _) = try await connected(); defer { home.disconnect() }
        await http.holdNext()
        let first = Task { await home.controlDevice("light", command: .object(["action": .string("power"), "on": .bool(false)])) }
        for _ in 0..<1000 { if await http.writes().count == 1 { break }; await Task.yield() }
        try #require(home.busy)
        let blocked = await home.controlDevice("curtain", command: .object(["action": .string("stop")]))
        let busyWrites = await http.writes()
        #expect(blocked == .notSent && busyWrites.count == 1)
        await http.release(); let accepted = await first.value
        #expect(accepted.accepted)
        home.disconnect()
        let offline = await home.controlDevice("light", command: .object(["action": .string("power"), "on": .bool(true)]))
        let offlineWrites = await http.writes()
        #expect(offline == .notSent && offlineWrites.count == 1)
    }
}
