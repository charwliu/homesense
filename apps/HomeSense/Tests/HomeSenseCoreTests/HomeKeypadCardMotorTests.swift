import Foundation
import Testing
@testable import HomeSenseCore

private actor HeldCardHTTP: HomeHTTPTransport {
    let base: GatewayHTTP
    var arrived = false
    var continuation: CheckedContinuation<Void, Never>?
    init(_ frame: HomeJSON) { base = GatewayHTTP(frame) }
    func waiting() -> Bool { arrived }
    func release() { continuation?.resume(); continuation = nil }
    func writes() async -> [URLRequest] { await base.writes() }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        if request.httpMethod == "POST" {
            arrived = true
            await withCheckedContinuation { continuation = $0 }
        }
        return try await base.send(request)
    }
}

@Suite @MainActor struct HomeKeypadCardMotorTests {
    private func samples() throws -> HomeJSON {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        return try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("test-inputs/keypad-motor-press-v2-samples.json")))
    }
    private func panel(id: String = "future-panel", target: String = "future-curtain", key: Int = 1, direction: String = "open", conditional: Bool = true) throws -> HomeJSON {
        var raw = try samples()["valid_parent"], keys = raw["keys"].values
        raw["id"] = .string(id)
        for index in keys.indices { keys[index]["action"] = .null; keys[index]["card_action"] = .null }
        keys[key - 1]["action"] = .object(["kind": .string("motor"), "device": .string(target), "path": .string("/api/v1/device-commands"), "command": .object(["action": .string(direction)])])
        keys[key - 1]["card_action"] = conditional ? .object([
            "kind": .string("motor_press"), "device": .string(target), "behavior": .string("repeat_to_stop"), "path": .string("/api/v1/device-commands"),
            "command": .object(["action": .string("keypad_press"), "keypad": .string(id), "key": .integer(Int64(key))])
        ]) : .null
        raw["keys"] = .array(keys)
        return raw
    }
    private func frame(_ raw: HomeJSON, movement: String = "Opening") throws -> HomeJSON {
        var frame = try GatewayInputs().frame("initial")
        var motor = GatewayInputs.motor("future-curtain", view: frame["devices"].values[0]["availability"])
        motor["motor_estimate"] = .object(["movement": .string(movement), "position": .integer(30)])
        frame["devices"] = .array(frame["devices"].values + [motor, raw])
        return frame
    }
    private func body(_ request: URLRequest) throws -> HomeJSON {
        try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody))
    }
    @Test func stableContractPreservesAbsoluteDescriptorAndAcceptsOnlyOptedInDirection() throws {
        let raw = try samples()["valid_parent"], key = try HomeKeypad(record: HomeRecord(raw)).keys[0]
        #expect(key.cardAction == .motorPress && key.canActivateCard && key.canActivate)
        #expect(key.actionDevice == "curtain" && key.motorAction == "open")
        for number in [1,2,6] {
            let alternate = try panel(key: number, direction: number == 2 ? "close" : "open")
            let parsed = try HomeKeypad(record: HomeRecord(alternate)).keys[number - 1]
            #expect(parsed.cardAction == .motorPress && parsed.canActivateCard)
            #expect(parsed.actionDevice == "future-curtain")
        }
        let stop = try HomeKeypad(record: HomeRecord(panel(key: 4, direction: "stop", conditional: false))).keys[3]
        #expect(stop.cardAction == .absolute && stop.canActivateCard && stop.motorAction == "stop")
    }
    @Test(arguments: ["wrong_parent", "wrong_key", "wrong_action", "target_mismatch", "unknown_behavior", "external_path", "wrong_kind", "empty_object", "string", "boolean", "array", "string_key", "extra_command", "extra_metadata", "stop_optin"])
    func invalidNonNullMetadataDisablesOnlyCardAndNeverFallsBack(_ variant: String) async throws {
        var raw = try samples()["valid_parent"], keys = raw["keys"].values
        let fixture = try samples()["invalid_card_samples"][variant]
        if fixture != .null { keys[0]["card_action"] = fixture }
        else {
            switch variant {
            case "wrong_kind": keys[0]["card_action"]["kind"] = .string("motor")
            case "empty_object": keys[0]["card_action"] = .object([:])
            case "string": keys[0]["card_action"] = .string("repeat_to_stop")
            case "boolean": keys[0]["card_action"] = .bool(false)
            case "array": keys[0]["card_action"] = .array([])
            case "string_key": keys[0]["card_action"]["command"]["key"] = .string("1")
            case "extra_command": keys[0]["card_action"]["command"]["direction"] = .string("stop")
            case "extra_metadata": keys[0]["card_action"]["future_behavior"] = .bool(true)
            case "stop_optin": keys[0]["action"]["command"]["action"] = .string("stop")
            default: Issue.record("Unknown test variant")
            }
        }
        raw["keys"] = .array(keys)
        let key = try HomeKeypad(record: HomeRecord(raw)).keys[0]
        #expect(key.cardAction == .invalid && !key.canActivateCard && key.canActivate)
        // Keep actual grammar parent/target IDs; no product ID special case.
        var state = try GatewayInputs().frame("initial")
        state["devices"] = .array([raw, GatewayInputs.motor("curtain", view: state["devices"].values[0]["availability"])])
        let (home,http,_) = try await gatewayConnect(state); defer { home.disconnect() }
        #expect(!home.canActivateKeypad("panel", key: 1, card: true))
        #expect(home.canActivateKeypad("panel", key: 1))
        await home.activateKeypadCard("panel", key: 1)
        #expect(await http.writes().isEmpty)
        await home.activateKeypad("panel", key: 1)
        let explicit = try body(#require(await http.writes().last))
        #expect(explicit["command"]["action"].text == (variant == "stop_optin" ? "stop" : "open"))
        #expect(explicit["revision"] == .null)
    }
    @Test(arguments: ["Opening", "Closing", "Stopped", "Unknown"])
    func cardClickSendsOneAtomicIntentRegardlessOfCachedMovementAndLED(_ movement: String) async throws {
        var raw = try panel(key: 6, direction: "close"), keys = raw["keys"].values
        keys[5]["led"]["observed"] = .string("red")
        keys[5]["led"]["last_written"] = .object(["status": .string("written"), "color": .string(movement == "Closing" ? "green" : "red")])
        raw["keys"] = .array(keys)
        let (home,http,_) = try await gatewayConnect(frame(raw, movement: movement)); defer { home.disconnect() }
        let before = home.devices.first { $0.id == "future-panel" }?.raw
        #expect(home.canActivateKeypad("future-panel", key: 6, card: true))
        let operation = home.beginKeypadClick("future-panel", key: 6, card: true)
        await home.activateKeypadCard("future-panel", key: 6, operation: operation)
        let writes = await http.writes(); try #require(writes.count == 1)
        let request = try body(writes[0])
        #expect(writes[0].url?.path == "/api/v1/device-commands")
        #expect(request["device"].text == "future-curtain" && request["revision"] == .string("1"))
        #expect(request["command"] == .object(["action": .string("keypad_press"), "keypad": .string("future-panel"), "key": .integer(6)]))
        #expect(request["request_id"].text.flatMap(UUID.init(uuidString:)) != nil)
        #expect(request["request_id"].text != operation.id.uuidString)
        #expect(home.devices.first { $0.id == "future-panel" }?.raw == before, "No optimistic color or local movement mutation")
        let events = try home.diagnostics.exportText().split(separator: "\n").dropFirst().map { try JSONDecoder().decode(HomeJSON.self, from: Data($0.utf8)) }
            .filter { $0["operation_id"].text == operation.id.uuidString }
        #expect(events.first?["event"].text == "button.click" && events.last?["event"].text == "operation.complete")
        #expect(events.contains { $0["event"].text == "http.send" && $0["request_id"] == request["request_id"] })
        #expect(events.contains { $0["event"].text == "http.response" && $0["request_id"] == request["request_id"] && $0["http_status"] == .integer(200) })
        #expect(!home.diagnostics.exportText().contains("future-curtain"))
    }
    @Test func detailsAndLegacyCardsKeepAbsoluteCommandsAndStandaloneStop() async throws {
        for (direction,number) in [("open",2),("close",6),("stop",4)] {
            let raw = try panel(key: number, direction: direction, conditional: direction != "stop")
            let (home,http,_) = try await gatewayConnect(frame(raw)); defer { home.disconnect() }
            await home.activateKeypad("future-panel", key: number)
            let detail = try body(#require(await http.writes().last))
            #expect(detail["command"] == .object(["action": .string(direction)]) && detail["revision"] == .null)
            if direction == "stop" {
                await home.activateKeypadCard("future-panel", key: number)
                #expect(try body(#require(await http.writes().last))["command"] == .object(["action": .string("stop")]))
            }
        }
        for missing in [true,false] {
            var raw = try panel(conditional: false), keys = raw["keys"].values
            if missing { keys[0] = keys[0].removing(["card_action"]) }
            raw["keys"] = .array(keys)
            let (home,http,_) = try await gatewayConnect(frame(raw)); defer { home.disconnect() }
            await home.activateKeypadCard("future-panel", key: 1)
            #expect(try body(#require(await http.writes().last))["command"] == .object(["action": .string("open")]))
        }
    }
    @Test func targetReadinessAuthorizationCalibrationAndGlobalBusyKeepExistingGates() async throws {
        try await conditionalPressRequiresHealthyParentButAbsoluteActionsDoNot()
        let raw = try panel(), initial = try frame(raw)
        let (home,http,events) = try await gatewayConnect(initial); defer { home.disconnect() }
        var next = initial; let rows = next["devices"].values
        let motorIndex = try #require(rows.firstIndex { $0["id"].text == "future-curtain" })
        for (index,field) in ["offline","synchronizing","permission","calibration"].enumerated() {
            var motor = rows[motorIndex]
            switch field {
            case "offline", "synchronizing": motor["availability"]["state"] = .string(field)
            case "permission": motor["can_control"] = .bool(false)
            default: motor["motor_calibrated"] = .bool(false)
            }
            var changed = rows; changed[motorIndex] = motor
            next["devices"] = .array(changed); next["cursor"]["sequence"] = .integer(Int64(index + 20)); try events.push(next)
            try await gatewayUntil { home.devices.first { $0.id == "future-curtain" }?.raw == motor }
            #expect(!home.canActivateKeypad("future-panel", key: 1, card: true))
            await home.activateKeypadCard("future-panel", key: 1)
            #expect(await http.writes().isEmpty)
        }
        next = initial; next["cursor"]["sequence"] = .integer(30); try events.push(next)
        try await gatewayUntil { home.canActivateKeypad("future-panel", key: 1, card: true) }
        #expect(await http.writes().isEmpty, "Recovery never replays blocked press")
        await home.activateKeypadCard("future-panel", key: 1)
        #expect(await http.writes().count == 1)
    }
    private func conditionalPressRequiresHealthyParentButAbsoluteActionsDoNot() async throws {
        var raw = try panel(), keys = raw["keys"].values
        for (number, direction) in [(2,"open"),(4,"stop")] {
            keys[number - 1]["action"] = .object(["kind": .string("motor"), "device": .string("future-curtain"), "path": .string("/api/v1/device-commands"), "command": .object(["action": .string(direction)])])
            keys[number - 1]["card_action"] = .null
        }
        raw["keys"] = .array(keys)
        var initial = try frame(raw), rows = initial["devices"].values
        let input = try GatewayInputs()
        let motorIndex = try #require(rows.firstIndex { $0["id"].text == "future-curtain" })
        let panelIndex = try #require(rows.firstIndex { $0["id"].text == "future-panel" })
        // Parent is on A, healthy mapped motor is on B. Parent can_control is
        // deliberately false: child/target grants, not parent control, apply.
        rows[motorIndex]["availability"] = input.frame("initial")["devices"].values[1]["availability"]
        initial["devices"] = .array(rows)
        let (home,http,events) = try await gatewayConnect(initial); defer { home.disconnect() }
        #expect(home.canActivateKeypad("future-panel", key: 1, card: true))
        for (index, variant) in ["gate_only","availability_only","both"].enumerated() {
            var parent = raw
            parent["availability"] = variant == "gate_only" ? .null : input.frame("partial")["devices"].values[0]["availability"]
            parent["action_gate"]["available"] = .bool(variant == "availability_only")
            var changed = rows; changed[panelIndex] = parent
            var update = initial; update["devices"] = .array(changed); update["cursor"]["sequence"] = .integer(Int64(index + 40))
            try events.push(update)
            try await gatewayUntil { home.devices.first { $0.id == "future-panel" }?.raw == parent }
            #expect(home.receivingPush && home.snapshotIsCurrent && home.canControlDevice("future-curtain"))
            #expect(!home.canActivateKeypad("future-panel", key: 1, card: true))
            #expect(!home.keypadGate("future-panel", key: 1, card: true).targetAvailable)
            #expect(home.canActivateKeypad("future-panel", key: 1), "Details absolute direction must stay available")
            #expect(home.canActivateKeypad("future-panel", key: 2, card: true), "Legacy absolute card must not inherit parent requirement")
            #expect(home.canActivateKeypad("future-panel", key: 4, card: true), "Absolute STOP must stay available")
            let before = await http.writes().count
            await home.activateKeypadCard("future-panel", key: 1)
            #expect(await http.writes().count == before, "Offline parent conditional intent must send zero requests")
            if index == 0 {
                await home.activateKeypad("future-panel", key: 1)
                await home.activateKeypadCard("future-panel", key: 2)
                await home.activateKeypadCard("future-panel", key: 4)
                let explicit = await http.writes().suffix(3)
                #expect(try explicit.map { try body($0)["command"]["action"].text } == ["open","open","stop"])
            }
        }
        var recovered = initial; recovered["cursor"]["sequence"] = .integer(50)
        let before = await http.writes().count
        try events.push(recovered)
        try await gatewayUntil { home.devices.first { $0.id == "future-panel" }?.raw == raw }
        #expect(home.canActivateKeypad("future-panel", key: 1, card: true), "Parent can_control=false cannot block child-granted press")
        #expect(await http.writes().count == before, "Recovery must not replay blocked requests")
        await home.activateKeypadCard("future-panel", key: 1)
        #expect(await http.writes().count == before + 1)
        #expect(try body(#require(await http.writes().last))["command"]["action"].text == "keypad_press")
    }
    @Test func busyCardIgnoresAnotherQueuedPressWithoutChangingTheFirstIntent() async throws {
        let state = try frame(panel()), http = HeldCardHTTP(state), events = GatewayEvents()
        let home = HomeServerStore(transport: http, eventTransport: events, defaults: nil, credentialStore: InMemoryCredentials())
        home.endpointText = "https://127.0.0.1:39443"
        await home.connect(token: "synthetic-gateway-client-only")
        defer { home.disconnect() }
        try await gatewayUntil { events.count == 1 }
        try events.push(state); try await gatewayUntil { home.receivingPush }
        let first = Task { await home.activateKeypadCard("future-panel", key: 1) }
        for _ in 0..<100 { if await http.waiting() { break }; try await Task.sleep(for: .milliseconds(5)) }
        try #require(await http.waiting() && home.busy)
        #expect(!home.canActivateKeypad("future-panel", key: 1, card: true))
        await home.activateKeypadCard("future-panel", key: 1)
        await http.release(); await first.value
        let writes = await http.writes(); try #require(writes.count == 1)
        #expect(try body(writes[0])["command"]["action"].text == "keypad_press")
        #expect(!home.busy && home.error == nil)
    }
    @Test func lostResponseAndRecoveryNeverReuseOrReplayConditionalRequest() async throws {
        let (home,http,events) = try await gatewayConnect(frame(panel())); defer { home.disconnect() }
        await http.loseWriteResponse()
        await home.activateKeypadCard("future-panel", key: 1)
        try #require(home.error != nil && !home.busy)
        let first = try body(#require(await http.writes().first))
        var next = try frame(panel()); next["cursor"]["sequence"] = .integer(30); try events.push(next)
        try await Task.sleep(for: .milliseconds(150))
        #expect(await http.writes().count == 1)
        await home.activateKeypadCard("future-panel", key: 1)
        let writes = await http.writes(); try #require(writes.count == 2)
        let second = try body(writes[1])
        #expect(first["request_id"] != second["request_id"])
        #expect(first["command"] == second["command"] && second["revision"] == .string("1"))
        let trace = home.diagnostics.exportText()
        #expect(trace.contains("http.failed") && trace.contains("operation.complete"))
    }
}
