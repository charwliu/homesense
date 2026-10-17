import Foundation
import Testing
@testable import HomeSenseCore

private enum UnifiedInputs {
    static let boot = "00000000-0000-4000-8000-000000000001"
    static let revision = "9007199254740993"
    static var capability: HomeJSON { .object(["version": .integer(1), "path": .string("/api/v1/keypad-actions"), "gestures": .array([.string("activate")]), "indication_schema": .integer(1)]) }
    static func panel() -> HomeJSON {
        var panel = HomeKeypadTests.record().raw, keys = panel["keys"].values
        for index in keys.indices {
            let number = index + 1
            keys[index]["configured"] = .bool(true); keys[index]["endpoint_id"] = .string("key:\(number)")
            keys[index]["activation"] = .object(["version": .integer(1), "path": .string("/api/v1/keypad-actions"), "gestures": .array([.string("activate")]),
                "revision": .string(revision), "keypad": .string("panel"), "key": .integer(Int64(number)), "available": .bool(true)])
            keys[index]["key_event"] = .object(["source": .string("button_feedback"), "quality": .string("Confirmed"),
                "pressed": .bool(false), "version": .string("1"), "observed_at_ms": .string("1")])
            keys[index]["indication"] = .null
            switch number {
            case 3, 6: keys[index]["action"] = .object(["kind": .string("motor"), "device": .string("curtain"),
                "path": .string("/api/v1/device-commands"), "command": .object(["action": .string(number == 3 ? "open" : "stop")])])
            case 4:
                keys[index]["action"] = .object(["kind": .string("away_off"), "path": .string("/api/v1/away-off")])
                keys[index]["card_action"] = .object(["kind": .string("away_off"), "path": .string("/api/v1/away-off"),
                    "command": .object(["action": .string("keypad_press"), "keypad": .string("panel"), "key": .integer(4)])])
            default: keys[index]["action"] = .object(["kind": .string(number == 1 ? "scene_toggle" : "scene_execute"),
                "scene": .string(number == 5 ? "fan" : "reading"), "path": .string(number == 1 ? "/api/v1/scene-relative-actions" : "/api/v1/scene-actions")])
            }
        }
        panel["keys"] = .array(keys); return panel
    }
    static func cue(_ body: HomeJSON, sequence: UInt64 = 1, status: String = "pending", color: HomeJSON = .string("red"), source: String = "authenticated_click", eventSequence: HomeJSON = .null) -> HomeJSON {
        .object(["request_id": body["request_id"], "operation_sequence": .string(String(sequence)),
            "endpoint_id": .string("key:\(body["key"].integerText!)"), "source": .string(source), "event_sequence": eventSequence,
            "status": .string(status), "color": color, "observed": .bool(false)])
    }
    static func frame(_ panel: HomeJSON, reset: Bool = false, wire: Int64 = 1) -> HomeJSON {
        .object(["type": .string("state"), "reset": .bool(reset), "revision": .string(revision),
            "cursor": .object(["boot": .string(UnifiedInputs.boot), "revision": .string(revision), "transport_epoch": .integer(wire),
                "sequence": .integer(1), "scope": .string("synthetic-scope")]),
            "devices": .array([panel]), "scenes": .array([.object(["id": .string("reading"), "can_control": .bool(true)])])])
    }
    static func setting(_ panel: HomeJSON, key: Int = 1, field: String, value: HomeJSON) -> HomeJSON {
        var panel = panel, keys = panel["keys"].values; keys[key - 1][field] = value; panel["keys"] = .array(keys); return panel
    }
}

private actor UnifiedHTTP: HomeHTTPTransport {
    let base = HomeTestServer()
    var panel = UnifiedInputs.panel()
    var motor: HomeJSON = .object(["id": .string("curtain"), "kind": .string("Motor"), "can_control": .bool(true), "motor_calibrated": .bool(true)])
    var capability = UnifiedInputs.capability
    var requests: [URLRequest] = []
    var mode = "normal"
    var color: HomeJSON = .string("red")
    var sequence: UInt64 = 0
    var held: CheckedContinuation<Void, Never>?
    var heldStop: CheckedContinuation<Void, Never>?
    var holdFirstStop = false
    var stopCount = 0
    func setMode(_ value: String) { mode = value }
    func setColor(_ value: HomeJSON) { color = value }
    func setHoldFirstStop() { holdFirstStop = true }
    func setCapability(_ value: HomeJSON) { capability = value }
    func setPanel(_ value: HomeJSON) { panel = value }
    func currentPanel() -> HomeJSON { panel }
    func setMotor(_ value: HomeJSON) { motor = value }
    func release() { held?.resume(); held = nil }
    func isHeld() -> Bool { held != nil }
    func isStopHeld() -> Bool { heldStop != nil }
    func releaseStop() { heldStop?.resume(); heldStop = nil }
    func posts() -> [URLRequest] { requests.filter { $0.httpMethod == "POST" } }
    func lastBody() throws -> HomeJSON { try JSONDecoder().decode(HomeJSON.self, from: requests.last(where: { $0.httpMethod == "POST" })!.httpBody!) }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        requests.append(request)
        let path = request.url!.path
        if path == "/api/v1/keypad-actions" {
            let body = try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody))
            sequence += 1; let current = sequence, selectedColor = color
            if mode == "hold", current == 1 { await withCheckedContinuation { held = $0 } }
            if mode == "drop" { throw URLError(.networkConnectionLost) }
            if mode == "reject" || mode == "404" {
                return .init(data: try HomeJSON.object(["request_id": body["request_id"], "outcome": .string("rejected"),
                    "retry": .bool(false), "error": .string(mode == "404" ? "not_found" : "keypad_scope_denied")]).data(), status: mode == "404" ? 404 : 403)
            }
            if mode == "unknown" {
                return .init(data: try HomeJSON.object(["request_id": body["request_id"], "outcome": .string("unknown"),
                    "retry": .bool(false), "error": .string("execution_outcome_unknown"), "indication": .null]).data(), status: 202)
            }
            let number = Int(body["key"].integerText!)!
            let cue = mode == "wall-response"
                ? UnifiedInputs.cue(body, sequence: current, color: selectedColor, source: "button_event", eventSequence: .string("20"))
                : UnifiedInputs.cue(body, sequence: current, color: selectedColor)
            panel = UnifiedInputs.setting(panel, key: number, field: "indication", value: cue)
            return .init(data: try HomeJSON.object(["request_id": body["request_id"], "outcome": .string("accepted"),
                "boot": .string(UnifiedInputs.boot), "revision": body["revision"], "keypad": body["keypad"], "key": body["key"],
                "gesture": .string("activate"), "retry": .bool(false), "execution": .object(["kind": .string([3, 6].contains(number) ? "motor" : number == 4 ? "away" : "scene")]), "indication": cue]).data(), status: 202)
        }
        if path == "/api/v1/device-commands", request.httpMethod == "POST" {
            let body = try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody))
            if body["command"] == .object(["action": .string("stop")]) {
                stopCount += 1
                if holdFirstStop, stopCount == 1 { await withCheckedContinuation { heldStop = $0 } }
                return .init(data: try HomeJSON.object(["outcome": .string("accepted"), "request_id": body["request_id"]]).data(), status: 202)
            }
        }
        if path == "/api/v1/scene-actions", request.httpMethod == "POST", mode == "hold-scene" {
            await withCheckedContinuation { held = $0 }
        }
        if path == "/api/v1/devices" { await base.setPresentedDevices([panel, motor]) }
        let response = try await base.send(request)
        if path == "/api/v1/session", response.status == 200 {
            var json = try JSONDecoder().decode(HomeJSON.self, from: response.data)
            json["capabilities"] = .object(["keypad_actions": capability]); json["boot"] = .string(UnifiedInputs.boot)
            return .init(data: try json.data(), status: 200)
        }
        if response.status == 200, var json = try? JSONDecoder().decode(HomeJSON.self, from: response.data), json["boot"].text != nil {
            json["boot"] = .string(UnifiedInputs.boot)
            return .init(data: try json.data(), status: response.status)
        }
        return response
    }
}

private final class UnifiedEvents: HomeEventTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var subscribers: [AsyncThrowingStream<HomeEventMessage, Error>.Continuation] = []
    func events(_ request: URLRequest) -> AsyncThrowingStream<HomeEventMessage, Error> {
        AsyncThrowingStream { c in lock.withLock { subscribers.append(c) } }
    }
    var count: Int { lock.withLock { subscribers.count } }
    func push(_ frame: HomeJSON) throws { lock.withLock { subscribers.last }?.yield(.state(try frame.data())) }
}

@Suite @MainActor struct HomeUnifiedKeypadTests {
    private func connect(_ http: UnifiedHTTP, events: UnifiedEvents? = nil) async throws -> HomeServerStore {
        let home = HomeServerStore(transport: http, eventTransport: events ?? UnifiedEvents(), defaults: nil,
            credentialStore: InMemoryCredentials(), diagnostics: HomeDiagnostics(fileURL: nil, emitSystemLog: false))
        if events == nil { home.pause() }
        home.endpointText = "https://home.example:8444"; await home.connect(token: "test-token")
        if let events {
            try await gatewayUntil { events.count == 1 }
            try events.push(UnifiedInputs.frame(await http.currentPanel(), reset: true))
            try await gatewayUntil { home.receivingPush }
        }
        return home
    }

    @Test func allPhysicalKeyKindsUseOneServerResolvedRequestAndKeepGroundTruth() async throws {
        let http = UnifiedHTTP(), home = try await connect(http); defer { home.disconnect() }
        let original = home.devices
        var ids = Set<String>()
        for key in 1...6 {
            #expect(home.canActivateUnifiedKeypad("panel", key: key))
            let before = await http.posts().count, op = home.beginUnifiedKeypadClick("panel", key: key)
            #expect(home.busy && home.keypadActionPresentation("panel", key: key)?.phase == .submitting)
            #expect(home.canDisplayKeypadActionCue("panel", key: key), "Busy disables repeat submission while preserving the visible cue layer")
            #expect(home.keypadActionPresentation("panel", key: key)?.requestedColor == nil)
            #expect(await http.posts().count == before, "Synchronous UI indication does not send another request")
            await home.activateUnifiedKeypad("panel", key: key, operation: op)
            let body = try await http.lastBody()
            #expect(body == .object(["revision": .string(UnifiedInputs.revision), "keypad": .string("panel"), "key": .integer(Int64(key)),
                "gesture": .string("activate"), "request_id": body["request_id"]]))
            #expect(body["request_id"].text.flatMap(UUID.init(uuidString:)) != nil && body["request_id"] != .string(op.id.uuidString))
            ids.insert(body["request_id"].text!)
            #expect(home.keypadActionPresentation("panel", key: key)?.requestedColor == .red && !home.busy)
        }
        let posts = await http.posts()
        #expect(ids.count == 6 && posts.count == 6)
        #expect(await http.posts().allSatisfy { $0.url?.path == "/api/v1/keypad-actions" })
        let beforePanel = try HomeKeypad(record: #require(original.first)), after = try HomeKeypad(record: #require(home.devices.first))
        #expect(beforePanel.keys.map(\.led) == after.keys.map(\.led))
        #expect(after.keys.allSatisfy { $0.led.actual == .unknown })
    }

    @Test(arguments: ["missing", "version", "path", "gestures", "schema"])
    func unsupportedServiceNeverProbesOrFallsBack(_ variant: String) async throws {
        let http = UnifiedHTTP(); var cap = UnifiedInputs.capability
        switch variant {
        case "missing": cap = .null
        case "version": cap["version"] = .integer(2)
        case "path": cap["path"] = .string("/api/v1/scene-actions")
        case "gestures": cap["gestures"] = .array([.string("press")])
        default: cap["indication_schema"] = .integer(2)
        }
        await http.setCapability(cap)
        let home = try await connect(http); defer { home.disconnect() }
        #expect(!home.canActivateUnifiedKeypad("panel", key: 1) && home.unifiedKeypadSupportMessage != nil)
        let op = home.beginUnifiedKeypadClick("panel", key: 1)
        await home.activateUnifiedKeypad("panel", key: 1, operation: op)
        #expect(await http.posts().isEmpty && !home.busy)
        await home.controlScene("reading", on: true)
        #expect(await http.posts().map { $0.url!.path } == ["/api/v1/scene-actions"])
    }

    @Test(arguments: ["revision", "keypad", "key", "path", "available", "missing"])
    func perKeyCapabilityMismatchIsDisabledBeforeAnyOutput(_ field: String) async throws {
        let http = UnifiedHTTP(); var panel = UnifiedInputs.panel(), keys = panel["keys"].values
        if field == "missing" { keys[0]["activation"] = .null }
        else { keys[0]["activation"][field] = field == "available" ? .bool(false) : field == "key" ? .integer(2) : .string("foreign") }
        panel["keys"] = .array(keys); await http.setPanel(panel)
        let home = try await connect(http); defer { home.disconnect() }
        #expect(!home.canActivateUnifiedKeypad("panel", key: 1) && home.canActivateUnifiedKeypad("panel", key: 2))
        await home.activateUnifiedKeypad("panel", key: 1)
        #expect(await http.posts().isEmpty)
    }

    @Test(arguments: [true, false])
    func equalPhysicalCursorAcceptsCueInBothHTTPWSSOrdersWithoutExtraPOST(_ wssFirst: Bool) async throws {
        let http = UnifiedHTTP(), events = UnifiedEvents(), home = try await connect(http, events: events)
        defer { home.disconnect() }
        if wssFirst { await http.setMode("hold") }
        let op = home.beginUnifiedKeypadClick("panel", key: 1)
        let task = Task { await home.activateUnifiedKeypad("panel", key: 1, operation: op) }
        if wssFirst {
            for _ in 0..<200 { if await http.isHeld() { break }; try await Task.sleep(for: .milliseconds(5)) }
            try #require(await http.isHeld())
        }
        else { await task.value }
        let body = try await http.lastBody()
        let written = UnifiedInputs.cue(body, status: "written")
        let panel = UnifiedInputs.setting(UnifiedInputs.panel(), field: "indication", value: written)
        try events.push(UnifiedInputs.frame(panel))
        try await gatewayUntil { home.keypadActionPresentation("panel", key: 1)?.phase == .written }
        if wssFirst { await http.release(); await task.value }
        #expect(home.keypadActionPresentation("panel", key: 1)?.phase == .written)
        let old = UnifiedInputs.setting(panel, field: "indication", value: UnifiedInputs.cue(body))
        try events.push(UnifiedInputs.frame(old))
        try await Task.sleep(for: .milliseconds(40))
        #expect(home.keypadActionPresentation("panel", key: 1)?.phase == .written)
        #expect(await http.posts().count == 1)
        #expect(try HomeKeypad(record: #require(home.devices.first)).keys[0].led.actual == .unknown)
    }

    @Test(arguments: ["drop", "unknown", "reject", "404", "wall-response"])
    func uncertainOrRejectedActionNeverRetriesOrUsesLegacyEndpoint(_ mode: String) async throws {
        let http = UnifiedHTTP(), home = try await connect(http); defer { home.disconnect() }
        await http.setMode(mode); await home.activateUnifiedKeypad("panel", key: 1)
        #expect(await http.posts().count == 1 && !home.busy)
        #expect(await http.posts().first?.url?.path == "/api/v1/keypad-actions")
        #expect(home.keypadActionPresentation("panel", key: 1)?.phase == (["reject", "404"].contains(mode) ? .rejected : .uncertain))
        #expect(home.keypadActionPresentation("panel", key: 1)?.requestedColor == nil)
        home.disconnect()
        #expect(home.keypadActionPresentation("panel", key: 1) == nil)
    }

    @Test func newerRequestAndNullSnapshotRejectOldCueWithoutChangingLEDHistory() async throws {
        let http = UnifiedHTTP(), events = UnifiedEvents(), home = try await connect(http, events: events)
        defer { home.disconnect() }
        await home.activateUnifiedKeypad("panel", key: 1); let older = try await http.lastBody()
        await http.setColor(.string("green"))
        await home.activateUnifiedKeypad("panel", key: 1); let newer = try await http.lastBody()
        #expect(home.keypadActionPresentation("panel", key: 1)?.requestedColor == .green)
        var panel = UnifiedInputs.setting(UnifiedInputs.panel(), field: "indication", value: UnifiedInputs.cue(older, status: "written"))
        try events.push(UnifiedInputs.frame(panel)); try await Task.sleep(for: .milliseconds(30))
        #expect(home.keypadActionPresentation("panel", key: 1)?.phase == .pending)
        #expect(home.keypadActionPresentation("panel", key: 1)?.requestedColor == .green)
        panel = UnifiedInputs.setting(panel, field: "indication", value: UnifiedInputs.cue(newer, sequence: 2, status: "written", color: .string("green")))
        try events.push(UnifiedInputs.frame(panel))
        try await gatewayUntil { home.keypadActionPresentation("panel", key: 1)?.phase == .written }
        panel = UnifiedInputs.setting(panel, field: "indication", value: .null)
        try events.push(UnifiedInputs.frame(panel))
        try await gatewayUntil { home.keypadActionPresentation("panel", key: 1) == nil }
        panel = UnifiedInputs.setting(panel, field: "indication", value: UnifiedInputs.cue(newer, sequence: 2, color: .string("green")))
        try events.push(UnifiedInputs.frame(panel)); try await Task.sleep(for: .milliseconds(30))
        #expect(home.keypadActionPresentation("panel", key: 1) == nil)
        let led = try HomeKeypad(record: #require(home.devices.first)).keys[0].led
        #expect(led.actual == .unknown && led.lastWritten == .red)
        #expect(await http.posts().count == 2)
    }

    @Test(arguments: ["reset", "wire", "unavailable"])
    func lifecycleInvalidationDropsInflightCueAndLateReplyWithoutReplay(_ variant: String) async throws {
        let http = UnifiedHTTP(), events = UnifiedEvents(), home = try await connect(http, events: events)
        defer { home.disconnect() }
        await http.setMode("hold")
        let task = Task { await home.activateUnifiedKeypad("panel", key: 1) }
        for _ in 0..<200 { if await http.isHeld() { break }; try await Task.sleep(for: .milliseconds(5)) }
        try #require(await http.isHeld())
        if variant == "unavailable" { try events.push(.object(["type": .string("unavailable")])) }
        else { try events.push(UnifiedInputs.frame(UnifiedInputs.panel(), reset: true, wire: variant == "wire" ? 2 : 1)) }
        try await gatewayUntil { !home.busy }
        #expect(home.keypadActionPresentation("panel", key: 1) == nil)
        await http.release(); await task.value
        #expect(home.keypadActionPresentation("panel", key: 1) == nil)
        #expect(await http.posts().count == 1)
    }

    @Test func unrelatedWSSDoesNotReplacePendingClickAndOlderHTTPCannotReplaceNewerOperation() async throws {
        let http = UnifiedHTTP(), events = UnifiedEvents(), home = try await connect(http, events: events)
        defer { home.disconnect() }
        await http.setMode("hold")
        let task = Task { await home.activateUnifiedKeypad("panel", key: 1) }
        for _ in 0..<200 { if await http.isHeld() { break }; try await Task.sleep(for: .milliseconds(5)) }
        try #require(await http.isHeld())
        var foreign = try await http.lastBody(); foreign["request_id"] = .string(UUID().uuidString)
        let panel = UnifiedInputs.setting(UnifiedInputs.panel(), field: "indication", value: UnifiedInputs.cue(foreign, sequence: 50, status: "written", color: .string("green")))
        try events.push(UnifiedInputs.frame(panel)); try await Task.sleep(for: .milliseconds(30))
        #expect(home.keypadActionPresentation("panel", key: 1)?.phase == .submitting)
        await http.release(); await task.value
        #expect(home.keypadActionPresentation("panel", key: 1)?.phase == .written)
        #expect(home.keypadActionPresentation("panel", key: 1)?.requestedColor == .green)
        #expect(await http.posts().count == 1)
    }

    @Test func earlyUnknownWSSCanUpgradeFromMatchingHTTPWithoutInventingColor() async throws {
        let http = UnifiedHTTP(), events = UnifiedEvents(), home = try await connect(http, events: events)
        defer { home.disconnect() }
        await http.setMode("hold")
        let task = Task { await home.activateUnifiedKeypad("panel", key: 1) }
        for _ in 0..<200 { if await http.isHeld() { break }; try await Task.sleep(for: .milliseconds(5)) }
        try #require(await http.isHeld())
        let body = try await http.lastBody()
        let panel = UnifiedInputs.setting(UnifiedInputs.panel(), field: "indication", value: UnifiedInputs.cue(body, status: "unknown", color: .null))
        try events.push(UnifiedInputs.frame(panel))
        try await gatewayUntil { home.keypadActionPresentation("panel", key: 1)?.phase == .unresolved }
        #expect(home.keypadActionPresentation("panel", key: 1)?.requestedColor == nil)
        await http.release(); await task.value
        #expect(home.keypadActionPresentation("panel", key: 1)?.phase == .pending)
        #expect(home.keypadActionPresentation("panel", key: 1)?.requestedColor == .red)
        #expect(await http.posts().count == 1)
    }

    @Test(arguments: ["retired", "null"])
    func linkedWriteClearsOtherKeysCueAndLateReceiptCannotResurrectIt(_ status: String) async throws {
        let http = UnifiedHTTP(), events = UnifiedEvents(), home = try await connect(http, events: events)
        defer { home.disconnect() }
        await http.setMode("hold")
        let task = Task { await home.activateUnifiedKeypad("panel", key: 1) }
        for _ in 0..<200 { if await http.isHeld() { break }; try await Task.sleep(for: .milliseconds(5)) }
        try #require(await http.isHeld())
        let body = try await http.lastBody()
        var panel = UnifiedInputs.setting(UnifiedInputs.panel(), field: "indication", value: UnifiedInputs.cue(body, status: "written"))
        try events.push(UnifiedInputs.frame(panel))
        try await gatewayUntil { home.keypadActionPresentation("panel", key: 1)?.requestedColor == .red }
        // The server has classified a later cross-key/room/other-panel write:
        // it clears the source cue and independently publishes last_written.
        let retired: HomeJSON = status == "null" ? .null : UnifiedInputs.cue(body, status: "retired", color: .null)
        panel = UnifiedInputs.setting(panel, field: "indication", value: retired)
        var keys = panel["keys"].values
        keys[0]["led"]["last_written"] = .object(["status": .string("written"), "color": .string("green")])
        panel["keys"] = .array(keys)
        try events.push(UnifiedInputs.frame(panel))
        try await gatewayUntil { home.keypadActionPresentation("panel", key: 1) == nil }
        await http.release(); await task.value
        #expect(home.keypadActionPresentation("panel", key: 1) == nil)
        let led = try HomeKeypad(record: #require(home.devices.first)).keys[0].led
        #expect(led.lastWritten == .green && led.actual == .unknown)
        #expect(await http.posts().count == 1)
    }

    @Test func sameDirectionAndExplicitKeyStopCanPassHungHTTPWithoutOldReplyChangingCue() async throws {
        let http = UnifiedHTTP(), events = UnifiedEvents(), home = try await connect(http, events: events)
        defer { home.disconnect() }
        await http.setMode("hold")
        let first = Task { await home.activateUnifiedKeypad("panel", key: 3) }
        for _ in 0..<200 { if await http.isHeld() { break }; try await Task.sleep(for: .milliseconds(5)) }
        try #require(await http.isHeld())
        #expect(home.busy && home.canActivateUnifiedKeypad("panel", key: 3) && home.canActivateUnifiedKeypad("panel", key: 6))
        #expect(!home.canActivateUnifiedKeypad("panel", key: 1))
        await http.setColor(.string("green"))
        await home.activateUnifiedKeypad("panel", key: 3)
        #expect(home.keypadActionPresentation("panel", key: 3)?.requestedColor == .green)
        #expect(home.busy, "The older HTTP owner is still pending")
        await home.activateUnifiedKeypad("panel", key: 6)
        let message = home.message
        await http.release(); await first.value
        #expect(!home.busy && home.message == message)
        #expect(home.keypadActionPresentation("panel", key: 3)?.requestedColor == .green)
        let posts = await http.posts()
        #expect(posts.count == 3 && posts.allSatisfy { $0.url?.path == "/api/v1/keypad-actions" })
        let bodies = try posts.map { try JSONDecoder().decode(HomeJSON.self, from: $0.httpBody!) }
        #expect(bodies.map { $0["key"] } == [.integer(3), .integer(3), .integer(6)])
        #expect(bodies.allSatisfy { $0["gesture"] == .string("activate") && $0["command"] == .null && $0["color"] == .null })
    }

    @Test func repeatedIndependentSTOPSurvivesHungSceneAndFirstSTOPAndOldBusyOwners() async throws {
        let http = UnifiedHTTP(), home = try await connect(http); defer { home.disconnect() }
        await http.setMode("hold-scene"); await http.setHoldFirstStop()
        let scene = Task { await home.controlScene("reading", on: true) }
        for _ in 0..<200 { if await http.isHeld() { break }; try await Task.sleep(for: .milliseconds(5)) }
        try #require(await http.isHeld())
        #expect(home.busy && home.canStopMotorDevice("curtain"))
        let firstStop = Task { await home.controlDevice("curtain", command: HomeMotorControlAction.stop.command) }
        for _ in 0..<200 { if await http.isStopHeld() { break }; try await Task.sleep(for: .milliseconds(5)) }
        try #require(await http.isStopHeld())
        let second = await home.controlDevice("curtain", command: HomeMotorControlAction.stop.command)
        #expect(second.accepted && home.message.contains("停止请求已受理"))
        await http.release(); await scene.value
        #expect(home.busy, "A completed scene cannot clear the pending STOP owner")
        let message = home.message
        await http.releaseStop(); _ = await firstStop.value
        #expect(!home.busy && home.message == message)
        let posts = await http.posts()
        #expect(posts.map { $0.url!.path } == ["/api/v1/scene-actions", "/api/v1/device-commands", "/api/v1/device-commands"])
        let stops = try posts.dropFirst().map { try JSONDecoder().decode(HomeJSON.self, from: $0.httpBody!) }
        #expect(stops.allSatisfy { $0["command"] == .object(["action": .string("stop")]) })
        #expect(stops[0]["request_id"] != stops[1]["request_id"])
    }

    @Test(arguments: ["can_control", "motor_calibrated", "kind", "offline"])
    func prioritySTOPRetainsPermissionCalibrationAndReadinessGates(_ field: String) async throws {
        let http = UnifiedHTTP()
        var motor: HomeJSON = .object(["id": .string("curtain"), "kind": .string("Motor"), "can_control": .bool(true), "motor_calibrated": .bool(true)])
        switch field {
        case "kind": motor[field] = .string("Switch")
        case "offline": motor["action_gate"] = .object(["available": .bool(false)])
        default: motor[field] = .bool(false)
        }
        await http.setMotor(motor)
        let home = try await connect(http); defer { home.disconnect() }
        #expect(!home.canStopMotorDevice("curtain"))
        let result = await home.controlDevice("curtain", command: HomeMotorControlAction.stop.command)
        let posts = await http.posts()
        #expect(result == .notSent && posts.isEmpty)
    }

    @Test func physicalDownReleaseHeldDuplicateAndEchoOnlyUpdatePresentation() async throws {
        let http = UnifiedHTTP(), events = UnifiedEvents()
        let initial = UnifiedInputs.setting(UnifiedInputs.panel(), field: "key_event", value: .object(["source": .string("button_feedback"),
            "quality": .string("Unknown"), "pressed": .null, "version": .null, "observed_at_ms": .null]))
        await http.setPanel(initial)
        let home = try await connect(http, events: events)
        defer { home.disconnect() }
        var panel = UnifiedInputs.panel()
        var latestVersion = 0, expectedPressed = false
        for (version, pressed) in [(1, true), (1, true), (2, true), (3, false), (1, true)] {
            if version > latestVersion { latestVersion = version; expectedPressed = pressed }
            let event: HomeJSON = .object(["source": .string("button_feedback"), "quality": .string("Confirmed"),
                "pressed": .bool(pressed), "version": .string(String(version)), "observed_at_ms": .string(String(version))])
            panel = UnifiedInputs.setting(panel, field: "key_event", value: event)
            try events.push(UnifiedInputs.frame(panel))
            try await Task.sleep(for: .milliseconds(25))
            #expect(home.keypadEventIsPressed("panel", key: 1) == expectedPressed)
            if version == 1 && pressed && home.keypadActionPresentation("panel", key: 1)?.phase == .released {
                #expect(!home.keypadEventIsPressed("panel", key: 1), "An older semantic version cannot reapply the press")
            }
        }
        #expect(await http.posts().isEmpty)
        #expect(try HomeKeypad(record: #require(home.devices.first)).keys[0].led.actual == .unknown)
    }

    @Test func trustedButtonEventCuesAdvanceIndependentlyAndRetireWithoutPOST() async throws {
        let http = UnifiedHTTP(), events = UnifiedEvents(), home = try await connect(http, events: events)
        defer { home.disconnect() }
        let body: HomeJSON = .object(["request_id": .string("wall-00000000000040008000000000000001-20"), "key": .integer(1)])
        let written = UnifiedInputs.cue(body, sequence: 10, status: "written", color: .string("green"), source: "button_event", eventSequence: .string("20"))
        var panel = UnifiedInputs.setting(UnifiedInputs.panel(), field: "indication", value: written)
        try events.push(UnifiedInputs.frame(panel))
        try await gatewayUntil { home.keypadActionPresentation("panel", key: 1)?.requestedColor == .green }
        #expect(home.keypadActionPresentation("panel", key: 1)?.phase == .written)
        for (operation, event) in [(10, 20), (9, 21)] {
            var old = written; old["operation_sequence"] = .string(String(operation)); old["event_sequence"] = .string(String(event)); old["request_id"] = .string("wall-00000000000040008000000000000001-\(event)"); old["color"] = .string("red")
            panel = UnifiedInputs.setting(panel, field: "indication", value: old)
            try events.push(UnifiedInputs.frame(panel)); try await Task.sleep(for: .milliseconds(25))
            #expect(home.keypadActionPresentation("panel", key: 1)?.requestedColor == .green)
        }
        // Event cursor is diagnostic, not a second ordering lane or the current key_event.version.
        var newer = written
        newer["operation_sequence"] = .string("11"); newer["event_sequence"] = .string("19")
        newer["request_id"] = .string("wall-00000000000040008000000000000001-19"); newer["color"] = .string("red")
        panel = UnifiedInputs.setting(panel, field: "key_event", value: .object(["source": .string("button_feedback"),
            "quality": .string("Confirmed"), "pressed": .bool(false), "version": .string("99"), "observed_at_ms": .string("99")]))
        panel = UnifiedInputs.setting(panel, field: "indication", value: newer)
        try events.push(UnifiedInputs.frame(panel))
        try await gatewayUntil { home.keypadActionPresentation("panel", key: 1)?.requestedColor == .red }
        var retired = newer; retired["status"] = .string("retired"); retired["color"] = .null
        panel = UnifiedInputs.setting(panel, field: "indication", value: retired)
        try events.push(UnifiedInputs.frame(panel))
        try await gatewayUntil { home.keypadActionPresentation("panel", key: 1)?.phase == .released }
        panel = UnifiedInputs.setting(panel, field: "indication", value: written)
        try events.push(UnifiedInputs.frame(panel)); try await Task.sleep(for: .milliseconds(25))
        #expect(home.keypadActionPresentation("panel", key: 1)?.phase == .released)
        let led = try HomeKeypad(record: #require(home.devices.first)).keys[0].led
        #expect(led.actual == .unknown && led.lastWritten == .red)
        #expect(await http.posts().isEmpty)
    }

    @Test func buttonEventRetirementDominatesLateHTTPFromAnOlderClick() async throws {
        let http = UnifiedHTTP(), events = UnifiedEvents(), home = try await connect(http, events: events)
        defer { home.disconnect() }
        await http.setMode("hold")
        let task = Task { await home.activateUnifiedKeypad("panel", key: 1) }
        for _ in 0..<200 { if await http.isHeld() { break }; try await Task.sleep(for: .milliseconds(5)) }
        try #require(await http.isHeld())
        let body: HomeJSON = .object(["request_id": .string("wall-00000000000040008000000000000001-20"), "key": .integer(1)])
        let retired = UnifiedInputs.cue(body, sequence: 10, status: "retired", color: .null, source: "button_event", eventSequence: .string("20"))
        let panel = UnifiedInputs.setting(UnifiedInputs.panel(), field: "indication", value: retired)
        try events.push(UnifiedInputs.frame(panel)); try await Task.sleep(for: .milliseconds(25))
        #expect(home.keypadActionPresentation("panel", key: 1)?.phase == .submitting)
        await http.release(); await task.value
        #expect(home.keypadActionPresentation("panel", key: 1) == nil)
        #expect(await http.posts().count == 1)
    }

    @Test(arguments: ["unknown-source", "missing-event", "zero-event", "future-event-format", "foreign-boot", "wrong-request-sequence"])
    func malformedPhysicalCueCannotColorKeyOrSendControl(_ variant: String) async throws {
        let http = UnifiedHTTP(), events = UnifiedEvents(), home = try await connect(http, events: events)
        defer { home.disconnect() }
        let body: HomeJSON = .object(["request_id": .string("wall-00000000000040008000000000000001-20"), "key": .integer(1)])
        var cue = UnifiedInputs.cue(body, sequence: 10, status: "written", color: .string("green"), source: "button_event", eventSequence: .string("20"))
        switch variant {
        case "unknown-source": cue["source"] = .string("unknown")
        case "missing-event": cue = cue.removing(["event_sequence"])
        case "zero-event": cue["event_sequence"] = .string("0")
        case "foreign-boot": cue["request_id"] = .string("wall-00000000000040008000000000000002-20")
        case "wrong-request-sequence": cue["request_id"] = .string("wall-00000000000040008000000000000001-21")
        default: cue["event_sequence"] = .integer(20)
        }
        let panel = UnifiedInputs.setting(UnifiedInputs.panel(), field: "indication", value: cue)
        try events.push(UnifiedInputs.frame(panel)); try await Task.sleep(for: .milliseconds(25))
        #expect(home.keypadActionPresentation("panel", key: 1) == nil && home.receivingPush)
        #expect(await http.posts().isEmpty)
    }
}
