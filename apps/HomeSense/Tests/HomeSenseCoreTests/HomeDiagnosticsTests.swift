import Foundation
import Testing
@testable import HomeSenseCore

private actor DiagnosticTestHTTP: HomeHTTPTransport {
    let base = HomeTestServer()
    var mode = "success", held = false, arrived = false
    var continuation: CheckedContinuation<Void, Never>?
    func configure(_ mode: String) { self.mode = mode }
    func hold() { held = true }
    func waiting() -> Bool { arrived }
    func release() { held = false; continuation?.resume(); continuation = nil }
    func captured() async -> [URLRequest] { await base.captured() }
    func prepare() async {
        await base.setPresentedDevices([HomeKeypadTests.record().raw])
        await base.setSceneSelected(.bool(true), relativeAllowed: false)
    }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        if request.httpMethod == "POST", request.url?.path == "/api/v1/scene-actions" {
            if held { arrived = true; await withCheckedContinuation { continuation = $0 } }
            switch mode {
            case "denied": return .init(data: Data("{\"error\":\"source_scope_changed\",\"detail\":\"secret-response\"}".utf8), status: 403)
            case "timeout": throw URLError(.timedOut, userInfo: [NSLocalizedDescriptionKey: "secret-description", NSURLErrorFailingURLStringErrorKey: "https://secret-origin/?token=secret-token"])
            case "cancel": throw URLError(.cancelled, userInfo: [NSLocalizedDescriptionKey: "secret-description"])
            case "malformed": return .init(data: Data("secret-response".utf8), status: 200)
            case "unsafeCode": return .init(data: Data("{\"error\":\"secret-response\"}".utf8), status: 409)
            default: break
            }
        }
        return try await base.send(request)
    }
}

@Suite @MainActor struct HomeDiagnosticsTests {
    private func rows(_ log: HomeDiagnostics) throws -> [HomeJSON] {
        try log.exportText().split(separator: "\n").dropFirst().map { try JSONDecoder().decode(HomeJSON.self, from: Data($0.utf8)) }
    }
    private func connected(_ http: DiagnosticTestHTTP, log: HomeDiagnostics) async throws -> HomeServerStore {
        await http.prepare()
        let home = HomeServerStore(transport: http, defaults: nil, credentialStore: InMemoryCredentials(), diagnostics: log)
        home.pause(); home.endpointText = "https://home.example:8444"
        await home.connect(token: "test-token")
        try #require(home.connected)
        return home
    }
    @Test func actualButtonEntryCorrelatesExplicitOffRequestAndResponseWithoutUserContent() async throws {
        let http = DiagnosticTestHTTP(), log = HomeDiagnostics(fileURL: nil, emitSystemLog: false)
        let home = try await connected(http, log: log); defer { home.disconnect() }
        let operation = home.beginKeypadClick("panel", key: 1)
        await home.activateKeypad("panel", key: 1, operation: operation)
        let events = try rows(log).filter { $0["operation_id"].text == operation.id.uuidString }
        let controlEvents = events.filter { !$0["event"].text!.hasPrefix("http.") || $0["path"].text == "/api/v1/scene-actions" }
        #expect(controlEvents.compactMap { $0["event"].text } == ["button.click","operation.begin","gate.check","scene.decision","http.send","http.response","operation.complete"])
        #expect(events.first?["selected"] == .bool(true))
        #expect(events.first { $0["event"].text == "scene.decision" }?["on"] == .bool(false))
        let send = try #require(events.first { $0["event"].text == "http.send" })
        let response = try #require(events.first { $0["event"].text == "http.response" })
        let writes = await http.captured().filter { $0.httpMethod == "POST" }
        try #require(writes.count == 1)
        let body = try JSONDecoder().decode(HomeJSON.self, from: #require(writes[0].httpBody))
        #expect(body["on"] == .bool(false))
        #expect(send["request_id"] == body["request_id"] && response["request_id"] == body["request_id"])
        #expect(send["request_id"].text != operation.id.uuidString)
        #expect(response["http_status"] == .integer(200) && response["outcome"] == .string("accepted"))
        #expect(events.last?["elapsed_ms"].integerText.flatMap(Int.init) != nil)
        let exported = log.exportText()
        for forbidden in ["test-token","Authorization","home.example","书房","legacy-key","reading","\"panel\""] { #expect(!exported.contains(forbidden)) }
        #expect(!home.busy && home.error == nil)
    }
    @Test func busyAndDisabledStateAreRecordedWithoutEnablingOrReplayingActions() async throws {
        let http = DiagnosticTestHTTP(), log = HomeDiagnostics(fileURL: nil, emitSystemLog: false)
        let home = try await connected(http, log: log); defer { home.disconnect() }
        await http.hold()
        let first = Task { await home.activateKeypad("panel", key: 1) }
        for _ in 0..<100 { if await http.waiting() { break }; try await Task.sleep(for: .milliseconds(5)) }
        try #require(await http.waiting())
        #expect(!home.canActivateKeypad("panel", key: 1))
        home.recordKeypadGateStates("panel")
        let gateRows = try rows(log).filter { $0["event"].text == "gate.state" && $0["key"] == .integer(1) }
        #expect(gateRows.last?["gate"].text == "busy" && gateRows.last?["selected"] == .bool(true))
        #expect(gateRows.last?["operation_id"] == .null)
        #expect(try !rows(log).contains { $0["event"].text == "button.click" }, "Disabled UI is state only, never a fabricated click")
        // A previously queued/programmatic activation is still rejected by the guard.
        await home.activateKeypad("panel", key: 1)
        #expect(try rows(log).contains { $0["event"].text == "operation.ignored" && $0["gate"].text == "busy" })
        await http.release(); await first.value
        #expect(await http.captured().filter { $0.httpMethod == "POST" }.count == 1)
        #expect(!home.busy)
        home.disconnect(); home.recordKeypadGateStates("panel")
        #expect(try rows(log).contains { $0["event"].text == "gate.state" && $0["gate"].text == "disconnected" })
    }
    @Test(arguments: ["denied", "timeout", "cancel", "malformed", "unsafeCode"])
    func failuresAlwaysHaveTerminalCorrelatedEventsAndNeverLogRawErrors(_ mode: String) async throws {
        let http = DiagnosticTestHTTP(), log = HomeDiagnostics(fileURL: nil, emitSystemLog: false)
        let home = try await connected(http, log: log); defer { home.disconnect() }
        await http.configure(mode)
        let operation = home.beginKeypadClick("panel", key: 1)
        await home.activateKeypad("panel", key: 1, operation: operation)
        let events = try rows(log).filter { $0["operation_id"].text == operation.id.uuidString }
        #expect(events.last?["event"].text == "operation.complete")
        #expect(events.contains { $0["event"].text == "operation.failed" })
        #expect(home.error != nil && !home.busy)
        let send = try #require(events.first { $0["event"].text == "http.send" })
        let terminal = try #require(events.first { ["http.response","http.failed"].contains($0["event"].text ?? "") })
        #expect(send["request_id"] == terminal["request_id"])
        if mode == "denied" { #expect(terminal["http_status"] == .integer(403) && terminal["error_code"].text == "source_scope_changed") }
        if mode == "timeout" { #expect(terminal["error_domain"].text == NSURLErrorDomain && terminal["error_number"] == .integer(-1001)) }
        if mode == "cancel" { #expect(terminal["cancelled"] == .bool(true)) }
        if mode == "unsafeCode" { #expect(terminal["error_code"].text == "other") }
        for secret in ["secret-response","secret-description","secret-origin","secret-token","test-token","Authorization"] { #expect(!log.exportText().contains(secret)) }
    }
    @Test func boundedFilesRotatePreviousSessionAndExportOnlyValidatedOwnSnapshots() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("homesense-diagnostic-test-\(UUID())")
        defer { try? FileManager.default.removeItem(at: root) }
        let file = root.appendingPathComponent("control-trace.jsonl"), previous = root.appendingPathComponent("control-trace.previous.jsonl")
        let burst = HomeDiagnostics(fileURL: nil, capacity: 1024, emitSystemLog: false)
        var gate = HomeKeypadGateState(); gate.connected = true; gate.selected = true
        for _ in 0..<1200 { burst.record(.gateState, operation: burst.operation(target: "secret-target", key: 1), gate: gate) }
        #expect(burst.exportText().utf8.count <= HomeDiagnostics.maximumFileBytes && burst.entryCount < 1024)
        let first = HomeDiagnostics(fileURL: file, capacity: 3, emitSystemLog: false)
        for _ in 0..<8 { first.record(.operationComplete, operation: first.operation(target: "secret-target")) }
        for _ in 0..<100 {
            if let text = try? String(contentsOf: file, encoding: .utf8), text == first.exportText() { break }
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(try String(contentsOf: file, encoding: .utf8) == first.exportText())
        #expect(first.entryCount == 3)
        #expect((try FileManager.default.attributesOfItem(atPath: file.path)[.posixPermissions] as? NSNumber)?.intValue == 0o600)
        let second = HomeDiagnostics(fileURL: file, emitSystemLog: false)
        second.record(.operationBegin, operation: second.operation(target: "another-secret-target"))
        for _ in 0..<100 {
            if let text = try? String(contentsOf: file, encoding: .utf8), text == second.exportText() { break }
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(try String(contentsOf: previous, encoding: .utf8) == first.exportText())
        let exported = await second.exportIncludingPrevious()
        #expect(exported == first.exportText() + second.exportText())
        #expect(!exported.contains("secret-target"))
        #expect(exported.utf8.count <= HomeDiagnostics.maximumFileBytes * 2)
        // Invalid previous content is not included in an App export.
        try Data((first.exportText() + "secret-unparseable-line\n").utf8).write(to: previous)
        #expect(await second.exportIncludingPrevious() == second.exportText())
    }
    @Test func loggingDisabledOrUnwritableNeverChangesControlResult() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("homesense-diagnostic-failure-\(UUID())")
        try Data().write(to: root); defer { try? FileManager.default.removeItem(at: root) }
        let http = DiagnosticTestHTTP(), log = HomeDiagnostics(fileURL: root.appendingPathComponent("control-trace.jsonl"), emitSystemLog: false)
        let home = try await connected(http, log: log); defer { home.disconnect() }
        await home.activateKeypad("panel", key: 1)
        #expect(home.error == nil && !home.busy)
        let count = log.entryCount
        log.isEnabled = false
        await home.activateKeypad("panel", key: 1)
        #expect(home.error == nil && !home.busy && log.entryCount == count)
        #expect(await http.captured().filter { $0.httpMethod == "POST" }.count == 2)
    }
}
