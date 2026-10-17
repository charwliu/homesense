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
            if hold { hold = false; await withCheckedContinuation { held = $0 } }
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
        let blocked = await home.controlDevice("curtain", command: .object(["action": .string("open")]))
        let busyWrites = await http.writes()
        #expect(blocked == .notSent && busyWrites.count == 1)
        await http.release(); let accepted = await first.value
        #expect(accepted.accepted)
        home.disconnect()
        let offline = await home.controlDevice("light", command: .object(["action": .string("power"), "on": .bool(true)]))
        let offlineWrites = await http.writes()
        #expect(offline == .notSent && offlineWrites.count == 1)
    }
    @Test func motorCardDirectionsSendAtomicPressAndKeepCorrelatedReceipts() async throws {
        let (home, http, log) = try await connected(); defer { home.disconnect() }
        let initial = home.devices.first { $0.id == "curtain" }?.raw
        let actions: [HomeMotorControlAction] = [.open, .open, .close, .close, .stop]
        var operations: [HomeControlOperation] = []
        for action in actions {
            let operation = home.beginDeviceControl("curtain"); operations.append(operation)
            let result = await home.controlDevice("curtain", command: action.command, operation: operation)
            #expect(result.accepted)
            #expect(home.devices.first { $0.id == "curtain" }?.raw == initial)
        }
        let writes = await http.writes()
        #expect(writes.count == 5)
        let rows = try log.exportText().split(separator: "\n").dropFirst().map {
            try JSONDecoder().decode(HomeJSON.self, from: Data($0.utf8))
        }
        var ids: Set<String> = []
        for (index, request) in writes.enumerated() {
            let body = try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody))
            let expected: HomeJSON = index == 4 ? .object(["action": .string("stop")]) :
                .object(["action": .string("motor_press"), "direction": .string(index < 2 ? "open" : "close")])
            #expect(request.url?.path == "/api/v1/device-commands" && request.httpMethod == "POST")
            #expect(body["device"].text == "curtain" && body["command"] == expected)
            #expect(index == 4 ? body["revision"] == .null : body["revision"].text == home.revision)
            let id = try #require(body["request_id"].text); ids.insert(id)
            let chain = rows.filter { $0["operation_id"].text == operations[index].id.uuidString }
            #expect(chain.first?["event"].text == "button.click" && chain.last?["event"].text == "operation.complete")
            #expect(chain.contains { $0["event"].text == "operation.begin" })
            #expect(chain.filter { $0["event"].text == "http.send" && $0["method"].text == "POST" }.count == 1)
            #expect(chain.contains { $0["event"].text == "http.response" && $0["http_status"].integerText == "202" && $0["request_id"].text == id })
        }
        #expect(ids.count == 5)
    }
    @Test(arguments: ["wrong_id", "missing_id", "timeout", "denied"])
    func motorPressDoesNotRetryOrFabricateMovementAfterUnconfirmedReceipt(_ mode: String) async throws {
        let (home, http, _) = try await connected(); defer { home.disconnect() }
        let initial = home.devices.first { $0.id == "curtain" }?.raw
        await http.configure(mode)
        let result = await home.controlDevice("curtain", command: HomeMotorControlAction.open.command)
        #expect(!result.accepted && result != .notSent)
        #expect(await http.writes().count == 1)
        #expect(home.devices.first { $0.id == "curtain" }?.raw == initial)
    }
    @Test func motorPressKeepsOrdinaryBusyGateButExplicitStopBypassesWithoutReplay() async throws {
        let (home, http, _) = try await connected(); defer { home.disconnect() }
        await http.holdNext()
        let first = Task { await home.controlDevice("curtain", command: HomeMotorControlAction.open.command) }
        for _ in 0..<1000 { if await http.writes().count == 1 { break }; await Task.yield() }
        try #require(home.busy)
        let repeated = await home.controlDevice("curtain", command: HomeMotorControlAction.open.command)
        let stop = await home.controlDevice("curtain", command: HomeMotorControlAction.stop.command)
        #expect(repeated == .notSent && stop.accepted)
        #expect(await http.writes().count == 2)
        await http.release(); let accepted = await first.value
        #expect(!accepted.accepted && accepted != .notSent && !home.busy, "The older movement receipt cannot replace the newer STOP")
        #expect(await http.writes().count == 2)
        let closing = await home.controlDevice("curtain", command: HomeMotorControlAction.close.command)
        let afterClosing = await http.writes()
        #expect(closing.accepted && afterClosing.count == 3)
        home.disconnect()
        let offline = await home.controlDevice("curtain", command: HomeMotorControlAction.close.command)
        let afterOffline = await http.writes()
        #expect(offline == .notSent && afterOffline.count == 3)
    }

    @Test func legacyAbsoluteMotorCommandsKeepRevisionFreeEnvelope() async throws {
        let (home, http, _) = try await connected(); defer { home.disconnect() }
        for action in ["open", "close", "stop"] {
            let result = await home.controlDevice("curtain", command: .object(["action": .string(action)]))
            #expect(result.accepted)
        }
        let writes = await http.writes()
        #expect(writes.count == 3)
        for (request, action) in zip(writes, ["open", "close", "stop"]) {
            let body = try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody))
            #expect(body["revision"] == .null)
            #expect(body["command"] == .object(["action": .string(action)]))
        }
    }

}
