#if os(macOS)
import Foundation
import Testing
@testable import HomeSenseCore

private actor PresentationActualHTTP: HomeHTTPTransport {
    let base: HomeURLSessionTransport
    var posts: [HomeJSON] = []
    var gets = 0
    init(_ trust: FixtureWebSocketTrust) { base = HomeURLSessionTransport(delegate: trust) }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        precondition(request.url?.host == "localhost")
        let response = try await base.send(request)
        if request.httpMethod == "GET" { gets += 1 }
        if request.httpMethod == "POST" {
            posts.append(.object(["path": .string(request.url!.path),
                "body": try JSONDecoder().decode(HomeJSON.self, from: request.httpBody!),
                "status": .integer(Int64(response.status)),
                "response": try JSONDecoder().decode(HomeJSON.self, from: response.data)]))
        }
        return response
    }
    func captured() -> [HomeJSON] { posts }
    func reads() -> Int { gets }
}

@Suite @MainActor struct HomeScenePresentationActualIntegrationTests {
    @Test(.enabled(if: ProcessInfo.processInfo.environment["HOMESENSE_SCENE_PRESENTATION_ACTUAL_FIXTURE"] != nil,
                   "Requires synthetic scene-presentation loopback HTTPS/WSS fixture"))
    func roomAbsoluteControlsAndKeyTogglesShareOnePublicConfirmedScene() async throws {
        let root = URL(fileURLWithPath: try #require(ProcessInfo.processInfo.environment["HOMESENSE_SCENE_PRESENTATION_ACTUAL_FIXTURE"]))
        let service = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("service.json")))
        try #require(service["fixture_kind"] == .string("synthetic-loopback-scene-presentation"))
        let origin = try #require(service["origin"].text)
        try #require(URL(string: origin)?.host == "localhost")
        let config = try JSONDecoder().decode(HomeJSON.self, from: Data(contentsOf: root.appendingPathComponent("devices.json")))
        try #require(config["gateways"].values.allSatisfy { $0["endpoint"].text?.hasPrefix("127.0.0.1:") == true })
        let trust = try FixtureWebSocketTrust(pem: String(contentsOf: root.appendingPathComponent("cert.pem"), encoding: .utf8))
        let http = PresentationActualHTTP(trust)
        let home = HomeServerStore(transport: http, eventTransport: HomeWebSocketTransport(delegate: trust), defaults: nil,
                                   credentialStore: InMemoryCredentials(), diagnostics: HomeDiagnostics(fileURL: nil, emitSystemLog: false))
        home.endpointText = origin
        let token = try String(contentsOf: root.appendingPathComponent("token"), encoding: .utf8)
        await home.connect(token: token)
        defer { home.disconnect() }
        func device(_ id: String) -> HomeJSON { home.devices.first { $0.id == id }?.raw ?? .null }
        func scene(_ id: String) -> HomeJSON { home.scenes.first { $0.id == id }?.raw ?? .null }
        func selected(_ n: Int) -> Bool {
            scene("action-1")["selected"] == .bool(n == 1) && scene("action-2")["selected"] == .bool(n == 2)
        }
        func colors(_ n: Int) -> Bool {
            let keys = device("panel")["keys"].values
            return (1...2).allSatisfy { k in keys.first { $0["number"].integerText == String(k) }?["led"]["last_written"]["color"].text == (k == n ? "red" : "green") }
        }
        func until(_ title: String, _ predicate: () -> Bool) async throws {
            let deadline = ContinuousClock.now.advanced(by: .seconds(12))
            while ContinuousClock.now < deadline {
                if predicate() { return }
                try await Task.sleep(for: .milliseconds(20))
            }
            throw HomeServerError.server("Timeout \(title): \(home.error ?? home.message)")
        }
        try await until("current ambiguous initial feedback") {
            home.receivingPush && home.unifiedKeypadActionsSupported && home.canActivateUnifiedKeypad("panel", key: 2) &&
            device("fixture-light")["quality"] == .string("Confirmed") && device("fixture-light")["observed"]["level"] == .integer(100)
        }
        #expect(Set(home.scenes.map(\.id)) == Set(["action-1", "action-2", "off", "light-activity"]))
        #expect(Set(home.scenes(inRoom: "study").map(\.id)) == Set(["action-1", "action-2", "off", "light-activity"]))
        #expect(Set(home.independentScenes(home.scenes, alongside: home.dailyDevices).map(\.id)) == Set(["off", "light-activity"]))
        #expect(scene("action-1")["selected"] == .null && scene("action-2")["selected"] == .null)
        #expect(scene("action-1")["relative_unknown_allowed"] == .bool(false))
        try await until("read-only actual activity") { HomeRecord(scene("light-activity")).sceneCardConfirmedActive == true }
        #expect(HomeRecord(scene("light-activity")).sceneCardAction == .execute)
        let initialMembers = HomeRecord(scene("action-1")).sceneLightStates(devices: home.devices, current: home.snapshotIsCurrent)
        #expect(scene("action-1")["lights_complete"] == .bool(true))
        #expect(initialMembers.map(\.id) == ["fixture-light"], "LEDs and read-only prerequisites are not light members")
        #expect(initialMembers.first?.status == "实测 · 100%")
        let reads = await http.reads()
        var phases: [HomeJSON] = []
        for (label, action, on, active) in [
            ("Off-while-ambiguous", 1, false, 0), ("absolute-On", 1, true, 1),
            ("repeated-absolute-On-stays-On", 1, true, 1), ("key-switch-same-profile", 2, true, 2),
            ("key-repeat-Off", 2, false, 0), ("room-second-On", 2, true, 2), ("room-second-Off", 2, false, 0)
        ] {
            let before = device("fixture-light")["observed_at_ms"]
            let prior = await http.captured().count
            let key = label.hasPrefix("key-")
            if key { await home.activateUnifiedKeypad("panel", key: action) }
            else { await home.controlScene("action-\(action)", on: on) }
            try #require(home.error == nil, "\(label): \(home.error ?? "")")
            let posts = await http.captured()
            try #require(posts.count == prior + 1, "one POST per operation")
            let post = try #require(posts.last)
            #expect(post["path"] == .string(key ? "/api/v1/keypad-actions" : "/api/v1/scene-actions"))
            #expect(post["status"] == .integer(202) && post["response"]["outcome"] == .string("accepted"))
            if !key { #expect(post["body"]["scene"] == .string("action-\(action)") && post["body"]["on"] == .bool(on)) }
            let requestID = try #require(post["body"]["request_id"].text)
            #expect(UUID(uuidString: requestID) != nil && !phases.contains { $0["post"]["body"]["request_id"] == .string(requestID) })
            try await until(label + " new actual feedback and selection") {
                home.receivingPush && selected(active) && colors(active) &&
                device("fixture-light")["observed"]["level"] == .integer(active == 0 ? 0 : 100) &&
                device("fixture-light")["observed_at_ms"] != before &&
                HomeRecord(scene("light-activity")).sceneCardConfirmedActive == (active != 0)
            }
            #expect(device("fixture-downlight")["observed"]["level"] == .integer(0))
            let members = HomeRecord(scene("action-\(action)")).sceneLightStates(devices: home.devices, current: home.snapshotIsCurrent)
            #expect(members.map(\.id) == ["fixture-light"])
            #expect(members.first?.isConfirmed == true && members.first?.isOn == (active != 0))
            #expect(members.first?.status == (active == 0 ? "实测 · 关闭 · 0%" : "实测 · 100%"), "scene details follow actual pushed readback")
            #expect(HomeRecord(scene("light-activity")).sceneCardAction == .execute)
            #expect(scene("light-activity")["selected"] == .null)
            #expect(await http.reads() == reads, "Control updates must arrive by WSS, without App polling")
            phases.append(.object(["label": .string(label), "post": post, "light": device("fixture-light"),
                                   "scene": scene("action-\(action)"), "keys": device("panel")["keys"]]))
        }
        try HomeJSON.array(phases).data(pretty: true).write(to: root.appendingPathComponent("actual-client-phases.json"))
        let posts = await http.captured()
        #expect(phases.count == 7 && posts.count == 7)
    }
}
#endif
