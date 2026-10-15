import Foundation
import Testing
@testable import HomeSenseCore

@Suite @MainActor struct HomeAvailabilityTests {
    @Test func actualDTOHealthIsLocalAndRetainedOfflineValuesAreHistorical() throws {
        let input = try GatewayInputs()
        let initial = input.devices["initial"]["devices"].values.map(HomeRecord.init)
        let partial = input.devices["partial"]["devices"].values.map(HomeRecord.init)
        let recovered = input.devices["recovered"]["devices"].values.map(HomeRecord.init)
        #expect(initial.allSatisfy { $0.targetAvailable && $0.feedbackIsConfirmed(current: true) })
        #expect(!partial[0].targetAvailable && !partial[0].feedbackIsConfirmed(current: true))
        #expect(partial[0].raw["observed"]["level"] == .integer(25))
        #expect(partial[0].targetAvailability.description == "网关离线")
        #expect(partial[1].targetAvailable && partial[1].feedbackIsConfirmed(current: true))
        #expect(recovered.allSatisfy { $0.targetAvailable && $0.feedbackIsConfirmed(current: true) })
        #expect(!recovered[0].feedbackIsConfirmed(current: false))
        var stale = partial[0].raw; stale["quality"] = .string("Confirmed")
        #expect(!HomeRecord(stale).feedbackIsConfirmed(current: true), "Availability must not be overridden by a retained quality field")
    }
    @Test(arguments: ["legacy", "null", "largeEpoch", "numericEpoch", "missingGateway", "synchronizing", "futureState", "gateInvalid", "requiredOffline", "requiredInvalid"])
    func optionalContractIsCompatibleButExplicitInvalidOrNotReadyCannotEnableCommands(_ variant: String) throws {
        let input = try GatewayInputs(); var raw = input.devices["initial"]["devices"].values[0]
        let allowed = ["legacy","null","largeEpoch"].contains(variant)
        switch variant {
        case "legacy": raw["availability"] = .null; raw["action_gate"] = .null
        case "null": raw = .object(["availability": .null, "action_gate": .null, "quality": .string("Confirmed")])
        case "largeEpoch": raw["availability"]["connection_epoch"] = .string("18446744073709551615")
        case "numericEpoch": raw["availability"]["connection_epoch"] = .integer(2)
        case "missingGateway": raw["availability"]["gateway"] = .null
        case "synchronizing": raw["availability"]["state"] = .string("synchronizing")
        case "futureState": raw["availability"]["state"] = .string("unrecognized")
        case "gateInvalid": raw["action_gate"]["available"] = .string("true")
        case "requiredOffline": raw["action_gate"]["required_gateways"] = .array([input.devices["partial"]["devices"].values[0]["availability"]])
        case "requiredInvalid": raw["action_gate"]["required_gateways"] = .object([:])
        default: Issue.record("invalid fixture variant")
        }
        #expect(HomeRecord(raw).targetAvailable == allowed)
        #expect(HomeRecord(raw).feedbackIsConfirmed(current: true) == allowed)
    }
    @Test func globalUnavailableAndDisconnectStillGateHealthyTargetsAndAllKeypadEntrances() async throws {
        let input=try GatewayInputs(); var frame=input.frame("initial")
        frame["devices"] = .array(frame["devices"].values + [GatewayInputs.panel()])
        let (home,http,events)=try await gatewayConnect(frame); defer { home.disconnect() }
        #expect(home.canControlDevice("lamp-b") && home.canControlScene("later-b"))
        #expect(home.canWriteKeypadLED("panel", key: 1) && home.canWriteKeypadSetting("panel", field: "sound"))
        try events.push(.object(["type": .string("unavailable")]))
        try await gatewayUntil { !home.receivingPush }
        #expect(!home.controlsAvailable && !home.canControlDevice("lamp-b") && !home.canControlScene("later-b"))
        #expect(!home.canWriteKeypadLED("panel", key: 1) && !home.canWriteKeypadSetting("panel", field: "sound"))
        await home.controlDevice("lamp-b", command: .object(["action": .string("power"), "on": .bool(true)]))
        await home.controlScene("later-b", on: nil)
        await home.writeKeypadLED("panel", key: 1, color: .red)
        await home.writeKeypadSetting("panel", command: .object(["action": .string("read_sound")]))
        #expect(await http.writes().isEmpty)
        frame["cursor"]["sequence"] = .integer(30); try events.push(frame)
        try await gatewayUntil { home.receivingPush }
        #expect(home.canControlDevice("lamp-b") && home.canControlScene("later-b"))
        #expect(await http.writes().isEmpty)
        home.disconnect()
        #expect(!home.canControlDevice("lamp-b") && !home.canControlScene("later-b"))
    }
    @Test func legacyBackendAndKeypadWithoutMetadataRetainExistingAuthorizedBehavior() async throws {
        let input=try GatewayInputs(); var frame=input.frame("initial"), rows=frame["devices"].values
        rows += [GatewayInputs.motor("motor-a", view: rows[0]["availability"]), GatewayInputs.panel()]
        rows = rows.map { row in var row=row; row["availability"] = .null; row["action_gate"] = .null; return row }
        frame["devices"] = .array(rows)
        frame["scenes"] = .array(frame["scenes"].values.map { scene in var scene=scene; scene["action_gate"] = .null; scene["selected"] = .bool(false); return scene })
        let (home,http,_) = try await gatewayConnect(frame); defer { home.disconnect() }
        #expect(home.canControlDevice("lamp-a") && home.canControlDevice("motor-a") && home.canControlScene("later-a"))
        #expect(home.canActivateKeypad("panel", key: 1) && home.canActivateKeypad("panel", key: 3))
        #expect(home.canWriteKeypadLED("panel", key: 1) && home.canWriteKeypadSetting("panel", field: "sound"))
        await home.controlDevice("lamp-a", command: .object(["action": .string("power"), "on": .bool(true)]))
        await home.controlDevice("motor-a", command: .object(["action": .string("stop")]))
        await home.controlScene("later-a", on: true)
        await home.activateKeypad("panel", key: 1); await home.activateKeypad("panel", key: 3)
        await home.writeKeypadLED("panel", key: 1, color: .red)
        await home.writeKeypadSetting("panel", command: .object(["action": .string("read_sound")]))
        #expect(await http.writes().count == 7)
    }
    @Test func synchronizingGatewayAndLostWriteResponseNeverReplayOnLocalRecovery() async throws {
        let input=try GatewayInputs(), (home,http,events)=try await gatewayConnect(input.frame("initial"))
        defer { home.disconnect() }
        await http.loseWriteResponse()
        await home.controlDevice("lamp-a", command: .object(["action": .string("brightness"), "level": .integer(90)]))
        #expect(await http.writes().count == 1 && home.error != nil)
        var frame=input.frame("partial"), rows=frame["devices"].values
        rows[0]["availability"]["state"] = .string("synchronizing"); rows[0]["quality"] = .string("Confirmed")
        frame["devices"] = .array(rows); try events.push(frame)
        try await gatewayUntil { home.devices[0].raw["availability"]["state"].text == "synchronizing" }
        #expect(!home.canControlDevice("lamp-a") && home.canControlDevice("lamp-b"))
        #expect(!home.devices[0].feedbackIsConfirmed(current: home.snapshotIsCurrent))
        await home.controlDevice("lamp-a", command: .object(["action": .string("brightness"), "level": .integer(20)]))
        #expect(await http.writes().count == 1)
        try events.push(input.frame("recovered")); try await gatewayUntil { home.canControlDevice("lamp-a") }
        try await Task.sleep(for: .milliseconds(150))
        #expect(await http.writes().count == 1)
        #expect(home.devices[0].raw["observed"]["level"] == .integer(25), "No optimistic 90/20 target promoted to actual")
        await home.controlDevice("lamp-a", command: .object(["action": .string("brightness"), "level": .integer(30)]))
        #expect(await http.writes().count == 2)
    }
    @Test func explicitOfflineParentCannotConfirmLEDFeedback() throws {
        let input=try GatewayInputs(); var panel=GatewayInputs.panel(), keys=panel["keys"].values
        panel["capabilities"]["led_readback"] = .bool(true)
        keys[0]["led"]["quality"] = .string("Confirmed"); keys[0]["led"]["observed"] = .string("green")
        panel["keys"] = .array(keys); panel["availability"] = input.devices["partial"]["devices"].values[0]["availability"]
        #expect(try HomeKeypad(record: HomeRecord(panel)).keys[0].led.actual == .unknown)
        panel["availability"] = input.devices["recovered"]["devices"].values[0]["availability"]
        #expect(try HomeKeypad(record: HomeRecord(panel)).keys[0].led.actual == .green)
    }
}
