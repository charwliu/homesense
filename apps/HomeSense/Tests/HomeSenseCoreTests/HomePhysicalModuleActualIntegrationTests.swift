#if os(macOS)
import Foundation
import Testing
@testable import HomeSenseCore

private actor PhysicalModuleActualHTTP: HomeHTTPTransport {
    let base: HomeURLSessionTransport
    let output: URL
    var writes: [HomeJSON] = []
    init(_ base: HomeURLSessionTransport, output: URL) { self.base = base; self.output = output }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        precondition(request.url?.host == "localhost")
        let response = try await base.send(request)
        if request.httpMethod == "POST" {
            writes.append(.object(["path": .string(request.url!.path), "body": try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody)),
                "status": .integer(Int64(response.status)), "reply": try JSONDecoder().decode(HomeJSON.self, from: response.data)]))
            try HomeJSON.array(writes).data(pretty: true).write(to: output.appendingPathComponent("native-module-posts.json"))
        }
        return response
    }
    func captured() -> [HomeJSON] { writes }
}

@Suite @MainActor struct HomePhysicalModuleActualIntegrationTests {
    @Test(.enabled(if: ProcessInfo.processInfo.environment["HOMESENSE_PHYSICAL_MODULE_ACTUAL_FIXTURE"] != nil,
                   "Requires a pinned TLS server and synthetic physical-module gateway"))
    func nativeModuleInventoryQueryRelayAndDimmerUseActualHTTPSFramesAndWSSFeedback() async throws {
        let root = URL(fileURLWithPath: try #require(ProcessInfo.processInfo.environment["HOMESENSE_PHYSICAL_MODULE_ACTUAL_FIXTURE"]))
        let service = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("service.json")))
        try #require(service["fixture_kind"].text == "synthetic-physical-modules-loopback")
        let origin = try #require(service["origin"].text); try #require(URL(string: origin)?.host == "localhost")
        let trust = try FixtureWebSocketTrust(pem: String(contentsOf: root.appendingPathComponent("cert.pem"), encoding: .utf8))
        let http = PhysicalModuleActualHTTP(HomeURLSessionTransport(delegate: trust), output: root)
        let home = HomeServerStore(transport: http, eventTransport: HomeWebSocketTransport(delegate: trust), defaults: nil,
            credentialStore: InMemoryCredentials(), diagnostics: HomeDiagnostics(fileURL: nil, emitSystemLog: false))
        home.endpointText = origin; await home.connect(token: try String(contentsOf: root.appendingPathComponent("token"), encoding: .utf8))
        defer {
            try? HomeJSON.object(["connected": .bool(home.connected), "receiving_push": .bool(home.receivingPush),
                "snapshot_current": .bool(home.snapshotIsCurrent), "busy": .bool(home.busy), "revision": .string(home.revision),
                "devices": .array(home.devices.map(\.raw)), "modules": .array(home.moduleInventory.map(\.raw))])
                .data(pretty: true).write(to: root.appendingPathComponent("native-final-or-failure-snapshot.json"))
            home.disconnect()
        }
        try #require(home.connected && home.error == nil, "\(home.error ?? "")")
        try await wait("initial WSS") { home.receivingPush && home.snapshotIsCurrent }
        await home.loadPhysicalModules()
        #expect(home.moduleInventory.count == 3)
        #expect(Set(home.physicalHardwareModules.compactMap(\.modelName)) == ["R818-DIN", "AD8-DIN", "C4-DIN"])
        let relay = try #require(home.physicalHardwareModules.first { $0.moduleID == "relay" })
        let dimmer = try #require(home.physicalHardwareModules.first { $0.moduleID == "dimmer" })
        let motor = try #require(home.physicalHardwareModules.first { $0.moduleID == "curtain" })
        #expect(relay.address == "03" && dimmer.address == "15" && motor.address == "22")
        try HomeJSON.array(home.moduleInventory.map(\.raw)).data(pretty: true).write(to: root.appendingPathComponent("native-modules-before.json"))
        #expect(home.canQueryPhysicalModule(relay.id) && !home.canQueryPhysicalModule(motor.id))
        await home.queryPhysicalModule(relay.id)
        try #require(home.error == nil, "\(home.error ?? "")")
        let off: HomeJSON = .object(["action": .string("power"), "on": .bool(false)])
        try #require(home.canControlPhysicalModule(relay.id, channel: 1, command: off))
        await home.controlPhysicalModule(relay.id, channel: 1, command: off)
        try #require(home.error == nil, "\(home.error ?? "")")
        try await wait("relay off") {
            home.devices.contains { $0.id == "relay-light" && $0.raw["quality"].text == "Confirmed" && $0.raw["state_authority"].text == "feedback" && $0.raw["observed"]["on"] == .bool(false) }
        }
        let level: HomeJSON = .object(["action": .string("brightness"), "level": .integer(40)])
        try #require(home.canControlPhysicalModule(dimmer.id, channel: 1, command: level))
        await home.controlPhysicalModule(dimmer.id, channel: 1, command: level)
        try #require(home.error == nil, "\(home.error ?? "")")
        // Six seconds for the hardware ramp, then two seconds for the genuine
        // confirmation query and WSS delivery. Other steps keep five seconds.
        try await wait("AD8 level40", budget: .seconds(8)) {
            home.devices.contains { $0.id == "dimmer-light" && $0.raw["quality"].text == "Confirmed" && $0.raw["state_authority"].text == "feedback" && $0.raw["observed"]["level"] == .integer(40) }
        }
        await home.controlPhysicalModule(relay.id, channel: 2, command: off)
        await home.queryPhysicalModule(motor.id)
        let posts = await http.captured(); #expect(posts.count == 3)
        #expect(posts.map { $0["path"].text } == ["/api/v1/module-queries", "/api/v1/module-commands", "/api/v1/module-commands"])
        var ids = Set<String>()
        for post in posts {
            #expect(post["status"] == .integer(202) && post["reply"]["outcome"].text == "accepted")
            #expect(post["body"]["revision"].text == home.revision && post["body"]["request_id"] == post["reply"]["request_id"])
            let id = try #require(post["body"]["request_id"].text); #expect(UUID(uuidString: id) != nil); ids.insert(id)
        }
        #expect(ids.count == 3 && home.receivingPush)
        try HomeJSON.array(home.devices.map(\.raw)).data(pretty: true).write(to: root.appendingPathComponent("native-module-feedback.json"))
    }
    private func wait(_ stage: String, budget: Duration = .seconds(5), _ predicate: () -> Bool) async throws {
        let limit = ContinuousClock.now.advanced(by: budget)
        while ContinuousClock.now < limit { if predicate() { return }; try await Task.sleep(for: .milliseconds(20)) }
        Issue.record("Physical-module \(stage) feedback did not arrive within \(budget) budget")
        throw HomeServerError.invalidResponse
    }
}
#endif
