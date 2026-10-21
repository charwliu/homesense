import Foundation
import Testing
@testable import HomeSenseCore

private func moduleFixture(_ id: String, address: String, profile: String, channels: Int = 8) -> HomeJSON {
    let slots: [HomeJSON] = (1...channels).map { n in
        let binding: HomeJSON = n == 1 && id == "relay" ? .object([
            "id": .string("light"), "name": .string("绑定负载"), "enabled": .bool(true),
            "supply_for": .array([]), "motor_calibrated": .bool(false), "actions": .array([.string("power")])]) : .null
        return .object(["channel": .integer(Int64(n)), "actual": .null, "quality": .string("unknown"), "binding": binding])
    }
    return .object(["module_id": .string(id), "gateway": .string("bus"), "address": .string(address), "profile": .string(profile),
                    "enabled": .bool(true), "channels": .integer(Int64(channels)),
                    "query_mode": .string(profile == "c4_din_pro_v2_7_7" ? "unsupported" : "individual_channels"), "slots": .array(slots)])
}

private actor ModuleHTTP: HomeHTTPTransport {
    let base = HomeTestServer()
    var mode = "normal"
    var held: CheckedContinuation<Void, Never>?
    func setMode(_ value: String) { mode = value }
    func release() { held?.resume(); held = nil }
    func isHeld() -> Bool { held != nil }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        let response = try await base.send(request)
        if response.status == 401 { return response }
        let path = request.url!.path
        func reply(_ json: HomeJSON, status: Int = 200) throws -> HomeHTTPResponse { HomeHTTPResponse(data: try json.data(), status: status) }
        if path == "/api/v1/modules" {
            if mode == "hold" { await withCheckedContinuation { held = $0 } }
            if mode == "missing" { return try reply(.object(["error": .string("not_found")]), status: 404) }
            if mode == "forbidden" { return try reply(.object(["error": .string("administrator_required")]), status: 403) }
            let offset = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == "offset" }?.value ?? "0"
            let first = [moduleFixture("relay", address: "04", profile: "r8i8_din_pro_v2_7_7"), moduleFixture("panel-module", address: "41", profile: "ts6_86_rs485_v2_6_5_6key", channels: 6)]
            let second = [moduleFixture("dimmer", address: "15", profile: "ad8_din_rs485_v2_2_1"), moduleFixture("motor", address: "22", profile: "c4_din_pro_v2_7_7", channels: 4)]
            var body: HomeJSON = .object(["boot": .string("synthetic-boot"), "revision": .string("9007199254740993"), "ready": .bool(true), "modules": .array(offset == "0" ? first : second), "next_offset": offset == "0" ? .integer(2) : .null])
            if offset != "0" {
                if mode == "bad-boot" { body["boot"] = .string("different-boot") }
                if mode == "bad-revision" { body["revision"] = .string("9007199254740994") }
                if mode == "loop" { body["next_offset"] = .integer(2) }
                if mode == "duplicate" { body["modules"] = .array(first) }
                if mode == "bad-slot" { var rows = second; rows[0]["slots"] = .array([]); body["modules"] = .array(rows) }
            }
            return try reply(body)
        }
        if path == "/api/v1/module-commands" || path == "/api/v1/module-queries" {
            if mode == "lost-ack" { throw URLError(.networkConnectionLost) }
            let body = try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody))
            return try reply(.object(["outcome": .string("accepted"), "request_id": body["request_id"]]))
        }
        return response
    }
}

@Suite @MainActor struct HomePhysicalModuleTests {
    private func connect(_ server: ModuleHTTP) async -> HomeServerStore {
        await server.base.setPresentedDevices([HomeKeypadTests.record().raw, .object(["id": .string("light"), "kind": .string("Dimmer"), "can_control": .bool(true)])])
        let home = HomeServerStore(transport: server, defaults: nil, credentialStore: InMemoryCredentials())
        home.pause(); home.endpointText = "https://home.example:8444"; await home.connect(token: "test-token")
        return home
    }
    @Test func inventoryPaginatesOneCoherentVersionWithoutCommandsAndKeepsPanelAddress() async throws {
        let server = ModuleHTTP(), home = await connect(server)
        defer { home.disconnect() }
        let devices = home.devices
        await home.loadPhysicalModules()
        #expect(home.moduleInventory.count == 4)
        #expect(home.physicalHardwareModules.compactMap(\.modelName) == ["R818-DIN", "AD8-DIN", "C4-DIN"])
        #expect(home.physicalHardwareModules.map(\.address) == ["04", "15", "22"])
        #expect(home.dailyPanels.map(\.id) == ["panel"])
        let panel = try #require(home.dailyPanels.first)
        #expect(home.physicalModule(for: panel)?.address == "41")
        #expect(home.devices == devices && home.controlsAvailable)
        let requests = await server.base.captured()
        #expect(requests.allSatisfy { $0.httpMethod == "GET" })
        let pages = requests.filter { $0.url?.path == "/api/v1/modules" }
        #expect(pages.count == 2)
        #expect(pages[0].url?.query == "offset=0&limit=64&revision=9007199254740993")
        #expect(pages[1].url?.query == "offset=2&limit=64&revision=9007199254740993")
    }
    @Test(arguments: ["missing", "forbidden", "bad-boot", "bad-revision", "loop", "duplicate", "bad-slot"])
    func incompleteInventoryNeverPublishesPartialPagesOrDisablesExistingControls(_ mode: String) async throws {
        let server = ModuleHTTP(), home = await connect(server)
        defer { home.disconnect() }
        await server.setMode(mode); await home.loadPhysicalModules()
        #expect(home.moduleInventory.isEmpty && !home.loadingModules && !home.moduleInventoryMessage.isEmpty)
        #expect(home.connected && home.controlsAvailable && home.devices.count == 2 && home.error == nil)
        #expect(await server.base.captured().allSatisfy { $0.httpMethod == "GET" })
    }
    @Test func administratorDowngradeAndDisconnectClearInventoryWhileMembersNeverRequestIt() async throws {
        let server = ModuleHTTP(), home = await connect(server)
        await home.loadPhysicalModules(); #expect(home.moduleInventory.count == 4)
        await server.base.setAdministrator(false); await home.refresh()
        #expect(home.moduleInventory.isEmpty && !home.canManage)
        let count = await server.base.captured().count
        await home.loadPhysicalModules()
        #expect(await server.base.captured().count == count)
        home.disconnect()
        #expect(home.moduleInventory.isEmpty && !home.loadingModules)
    }
    @Test func explicitQueryAndTestUseOneVersionedRequestEachWithoutOptimisticFeedback() async throws {
        let server = ModuleHTTP(), home = await connect(server)
        defer { home.disconnect() }
        await home.loadPhysicalModules()
        let module = try #require(home.physicalHardwareModules.first), before = home.moduleInventory
        let power: HomeJSON = .object(["action": .string("power"), "on": .bool(true)])
        #expect(home.canQueryPhysicalModule(module.id) && home.canControlPhysicalModule(module.id, channel: 1, command: power))
        await home.queryPhysicalModule(module.id)
        await home.controlPhysicalModule(module.id, channel: 1, command: power)
        await home.controlPhysicalModule(module.id, channel: 2, command: power)
        await home.controlPhysicalModule(module.id, channel: 1, command: .object(["action": .string("motor_press"), "direction": .string("open")]))
        let posts = await server.base.captured().filter { $0.httpMethod == "POST" }
        #expect(posts.count == 2)
        var ids = Set<String>()
        for post in posts {
            let body = try JSONDecoder().decode(HomeJSON.self, from: #require(post.httpBody))
            #expect(body["module_id"].text == "relay" && body["revision"].text == "9007199254740993")
            let id = try #require(body["request_id"].text); #expect(UUID(uuidString: id) != nil); ids.insert(id)
            if post.url?.path == "/api/v1/module-queries" { #expect(body["scope"] == .object(["kind": .string("whole_module")])) }
            else { #expect(post.url?.path == "/api/v1/module-commands" && body["channel"] == .integer(1) && body["command"] == power) }
        }
        #expect(ids.count == 2 && home.moduleInventory == before)
        #expect(!home.canQueryPhysicalModule(try #require(home.physicalHardwareModules.last).id))
        home.pause(); let count = await server.base.captured().count
        await home.queryPhysicalModule(module.id); await home.controlPhysicalModule(module.id, channel: 1, command: power)
        #expect(await server.base.captured().count == count)
    }
    @Test func lostTestAcknowledgementIsNeverRetriedAndDisconnectDropsCache() async throws {
        let server = ModuleHTTP(), home = await connect(server)
        await home.loadPhysicalModules()
        let id = try #require(home.physicalHardwareModules.first).id
        await server.setMode("lost-ack")
        await home.controlPhysicalModule(id, channel: 1, command: .object(["action": .string("power"), "on": .bool(true)]))
        #expect(home.error != nil && home.message.contains("结果未知"))
        #expect(await server.base.captured().filter { $0.httpMethod == "POST" }.count == 1)
        home.disconnect()
        #expect(home.moduleInventory.isEmpty && !home.canQueryPhysicalModule(id))
    }
    @Test func lateInventoryResponseCannotRestoreDisconnectedSession() async throws {
        let server = ModuleHTTP(), home = await connect(server)
        await server.setMode("hold")
        let loading = Task { await home.loadPhysicalModules() }
        for _ in 0..<200 { if await server.isHeld() { break }; try await Task.sleep(for: .milliseconds(5)) }
        try #require(await server.isHeld())
        home.disconnect(); await server.release(); await loading.value
        #expect(!home.connected && !home.loadingModules && home.moduleInventory.isEmpty)
        #expect(await server.base.captured().filter { $0.url?.path == "/api/v1/modules" }.count == 1)
    }
    @Test func channelSettingsRespectProfileSupplyOwnershipRangeAndMotorCalibration() throws {
        var dimmer = moduleFixture("dimmer", address: "15", profile: "ad8_din_rs485_v2_2_1")
        var slots = dimmer["slots"].values
        let binding: HomeJSON = .object(["id": .string("light"), "enabled": .bool(true), "supply_for": .array([]),
            "motor_calibrated": .bool(false), "actions": .array([.string("power"), .string("brightness")])])
        slots[0]["binding"] = binding; dimmer["slots"] = .array(slots)
        let module = try HomePhysicalModule(dimmer)
        for level: Int64 in [0, 50, 100] {
            #expect(module.permits(channel: 1, command: .object(["action": .string("brightness"), "level": .integer(level)])))
        }
        for level: HomeJSON in [.integer(-1), .integer(101), .string("50")] {
            #expect(!module.permits(channel: 1, command: .object(["action": .string("brightness"), "level": level])))
        }
        slots[0]["binding"]["supply_for"] = .array([.string("compound-light")]); dimmer["slots"] = .array(slots)
        #expect(!(try HomePhysicalModule(dimmer)).permits(channel: 1, command: .object(["action": .string("power"), "on": .bool(true)])))
        var motor = moduleFixture("motor", address: "22", profile: "c4_din_pro_v2_7_7", channels: 4)
        slots = motor["slots"].values; slots[0]["binding"] = binding
        slots[0]["binding"]["actions"] = .array([.string("open"), .string("close"), .string("stop")]); motor["slots"] = .array(slots)
        #expect(!(try HomePhysicalModule(motor)).permits(channel: 1, command: .object(["action": .string("open")])))
        slots[0]["binding"]["motor_calibrated"] = .bool(true); motor["slots"] = .array(slots)
        #expect((try HomePhysicalModule(motor)).permits(channel: 1, command: .object(["action": .string("stop")])))
        #expect(!(try HomePhysicalModule(motor)).permits(channel: 1, command: .object(["action": .string("motor_press"), "direction": .string("open")])))
    }
    @Test func dinIndicatorsUseConfirmedFeedbackForPowerAndEightChannels() throws {
        var relay = moduleFixture("relay", address: "04", profile: "r8i8_din_pro_v2_7_7")
        #expect(!(try HomePhysicalModule(relay)).dinHasFeedback)
        var slots = relay["slots"].values
        slots[0]["quality"] = .string("confirmed")
        slots[0]["actual"] = .object(["on": .bool(false)])
        slots[1]["quality"] = .string("confirmed")
        slots[1]["actual"] = .object(["on": .bool(true)])
        slots[2]["quality"] = .string("stale")
        slots[2]["actual"] = .object(["on": .bool(true)])
        relay["slots"] = .array(slots)
        let relayModule = try HomePhysicalModule(relay)
        #expect(relayModule.dinHasFeedback)
        #expect(relayModule.dinChannelIndicators.count == 8)
        #expect(relayModule.dinChannelIndicators.map(\.number) == Array(1...8))
        #expect(relayModule.dinChannelIndicators[0].isOn == false)
        #expect(relayModule.dinChannelIndicators[1].isOn == true)
        #expect(relayModule.dinChannelIndicators[2].isOn == nil)
        #expect(relayModule.dinChannelIndicators.allSatisfy { $0.brightness == nil })

        var dimmer = moduleFixture("dimmer", address: "15", profile: "ad8_din_rs485_v2_2_1")
        slots = dimmer["slots"].values
        slots[0]["quality"] = .string("confirmed")
        slots[0]["actual"] = .object(["on": .bool(true), "level": .integer(37)])
        slots[1]["quality"] = .string("confirmed")
        slots[1]["actual"] = .object(["on": .bool(false), "level": .integer(0)])
        slots[2]["quality"] = .string("stale")
        slots[2]["actual"] = .object(["on": .bool(true), "level": .integer(88)])
        dimmer["slots"] = .array(slots)
        let dimmerModule = try HomePhysicalModule(dimmer)
        #expect(dimmerModule.dinHasFeedback)
        #expect(dimmerModule.dinChannelIndicators[0].isOn == true && dimmerModule.dinChannelIndicators[0].brightness == 37)
        #expect(dimmerModule.dinChannelIndicators[1].isOn == false && dimmerModule.dinChannelIndicators[1].brightness == 0)
        #expect(dimmerModule.dinChannelIndicators[2].isOn == nil && dimmerModule.dinChannelIndicators[2].brightness == nil)
        #expect((try HomePhysicalModule(moduleFixture("motor", address: "22", profile: "c4_din_pro_v2_7_7", channels: 4))).dinChannelIndicators.isEmpty)
    }
}
