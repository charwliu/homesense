import Foundation
import Testing
@testable import HomeSenseCore

// All transports and credentials below are injected in-memory fixtures. Neither
// URLSession nor the system Keychain is used; every URL must target loopback.
private let grantOrigin = "https://127.0.0.1:38443"
private let grantSyntheticToken = "grant-refresh-synthetic-token-never-a-user-credential"
private let grantRevision: Int64 = 10
private let grantSceneIDs = ["study-reading", "study-daily", "study-energy", "study-off", "studio-reading", "studio-daily", "studio-off"]

private actor GrantCredentials: HomeCredentialStore {
    var values: [String: String] = [:]
    private(set) var saves = 0
    private(set) var removals = 0
    func load(for origin: String) -> String? { values[origin] }
    func save(_ token: String, for origin: String) { values[origin] = token; saves += 1 }
    func remove(for origin: String) { values.removeValue(forKey: origin); removals += 1 }
}

private func grantPanel(_ study: Bool, expanded: Bool) -> HomeJSON {
    let id = study ? "study-panel" : "studio-panel"
    let visible = study ? (expanded ? Set(1...6) : Set([1,3,5])) : Set(1...4)
    let keys: [HomeJSON] = (1...6).map { number in
        let configured = visible.contains(number)
        let motor = study ? number.isMultiple(of: 2) : number <= 2
        let scene = study ? [1:"study-reading",3:"study-daily",5:"study-energy"][number] : [3:"studio-reading",4:"studio-daily"][number]
        let action: HomeJSON
        if !configured { action = .null }
        else if motor {
            action = .object(["kind":.string("motor"),"device":.string(study ? "study-motor" : "studio-motor"),
                "command":.object(["action":.string(study ? [2:"open",4:"stop",6:"close"][number]! : (number == 1 ? "open" : "close"))]),
                "path":.string("/api/v1/device-commands")])
        } else {
            action = .object(["kind":.string("scene_toggle"),"scene":.string(scene!),"path":.string("/api/v1/scene-relative-actions")])
        }
        return .object(["number":.integer(Int64(number)),"label":.string("Key \(number)"),
            "configured":.bool(configured),"endpoint_id":configured ? .string("\(id):key:\(number)") : .null,
            "can_write_led":.bool(configured),"action":action,
            "led":.object(["observed":.null,"quality":.string("Unknown"),"last_written":.null])])
    }
    let whole = study && expanded
    return .object(["id":.string(id),"name":.string(study ? "Study" : "Studio"),"kind":.string("Keypad"),
        "module_id":.string(id+"-module"),"can_control":.bool(false),"room":.object(["id":.string(study ? "study" : "studio"),"name":.string(study ? "Study" : "Studio")]),
        "keys":.array(keys),"capabilities":.object(["led_readback":.bool(false),"led_colors":.array([.string("red"),.string("green"),.string("off")]),"backlight_write":.bool(whole),"sound_write":.bool(whole),"work_mode_write":.bool(whole)]),
        "settings":.object(["work_mode":.object(["configured":.string("manual"),"observed":.null,"can_write":.bool(whole)]),"backlight":.object(["can_write":.bool(whole)]),"sound":.object(["can_write":.bool(whole)])])])
}

private actor GrantServer: HomeHTTPTransport {
    private var generation: Int64 = 1
    private var unauthorized = false
    private var sessionFailures = 0
    private var requests: [URLRequest] = []
    func expandGrant(transientFailures: Int = 0) { generation = 2; sessionFailures = transientFailures }
    func rejectBearer() { unauthorized = true }
    func captured() -> [URLRequest] { requests }
    private func deviceRows() -> [HomeJSON] {
        let lights = (1...8).map { n in HomeJSON.object(["id":.string("light-\(n)"),"name":.string("Light \(n)"),"kind":.string("Dimmer"),"can_control":.bool(true)]) }
        let motors = ["study-motor","studio-motor"].map { id in HomeJSON.object(["id":.string(id),"name":.string(id),"kind":.string("Motor"),"can_control":.bool(true),"motor_calibrated":.bool(true)]) }
        return lights + motors + [grantPanel(true, expanded:generation == 2),grantPanel(false, expanded:false)]
    }
    private func sceneRows() -> [HomeJSON] {
        grantSceneIDs.map { .object(["id":.string($0),"name":.string($0),"can_control":.bool(true),"switch_kind":.string("selection"),"selected":.bool(false),"relative_unknown_allowed":.bool(false)]) }
    }
    func state() -> HomeJSON {
        .object(["type":.string("state"),"reset":.bool(true),"revision":.integer(grantRevision),
            "cursor":.object(["boot":.string("grant-fixture-boot"),"revision":.integer(grantRevision),"transport_epoch":.integer(1),"sequence":.integer(1),"scope":.string("grant-generation-\(generation)-scope-\(generation == 1 ? 17 : 20)")]),
            "devices":.array(deviceRows()),"scenes":.array(sceneRows())])
    }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        requests.append(request)
        precondition(request.url?.host == "127.0.0.1")
        func response(_ json: HomeJSON, status: Int = 200) throws -> HomeHTTPResponse { HomeHTTPResponse(data:try json.data(),status:status) }
        guard request.value(forHTTPHeaderField:"Authorization") == "Bearer \(grantSyntheticToken)", !unauthorized else {
            return try response(.object(["error":.string("authentication_required")]),status:401)
        }
        guard request.httpMethod == "GET" else { return try response(.object(["error":.string("writes_forbidden_in_fixture")]),status:405) }
        switch request.url!.path {
        case "/api/v1/session":
            if sessionFailures > 0 { sessionFailures -= 1; return try response(.object(["error":.string("runtime_reconfiguring")]),status:503) }
            return try response(.object(["can_manage_configuration":.bool(true),"role":.string("administrator"),"controller":.string("unchanged-synthetic-controller"),"credential_generation":.integer(generation)]))
        case "/api/v1/devices":
            return try response(.object(["boot":.string("grant-fixture-boot"),"revision":.integer(grantRevision),"revision_text":.string(String(grantRevision)),"devices":.array(deviceRows())]))
        case "/api/v1/scenes": return try response(.object(["revision":.integer(grantRevision),"scenes":.array(sceneRows())]))
        case "/api/v1/rooms": return try response(.object(["boot":.string("grant-fixture-boot"),"revision":.integer(grantRevision),"rooms":.array([]),"floors":.array([])]))
        case "/api/v1/config/versions": return try response(.object(["active_revision":.integer(grantRevision),"versions":.array([])]))
        case "/api/v1/config/versions/10/export":
            return try response(.object(["revision":.integer(grantRevision),"devices":.object(["revision":.integer(grantRevision),"rooms":.array([]),"devices":.array([]),"keypads":.array([])]),"automation":.object(["configuration_revision":.integer(grantRevision),"rules":.array([])])]))
        default: return try response(.object(["error":.string("unexpected_fixture_path")]),status:404)
        }
    }
}

private final class GrantEvents: HomeEventTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var continuations: [AsyncThrowingStream<HomeEventMessage, Error>.Continuation] = []
    private var requests: [URLRequest] = []
    func events(_ request: URLRequest) -> AsyncThrowingStream<HomeEventMessage, Error> {
        precondition(request.url?.host == "127.0.0.1")
        return AsyncThrowingStream { continuation in
            lock.withLock { requests.append(request); continuations.append(continuation) }
        }
    }
    var count: Int { lock.withLock { requests.count } }
    var captured: [URLRequest] { lock.withLock { requests } }
    func push(_ state: HomeJSON) throws {
        let c = lock.withLock { continuations.last! }
        c.yield(.state(try state.data()))
    }
    func invalidateGeneration(authenticationError: Bool = false) {
        let c = lock.withLock { continuations.last! }
        if authenticationError { c.finish(throwing:URLError(.userAuthenticationRequired)) }
        else { c.finish() }
    }
}

@Suite(.serialized) @MainActor
struct HomeGrantRefreshTests {
    private func until(_ condition: () -> Bool) async throws {
        for _ in 0..<600 {
            if condition() { return }
            try await Task.sleep(for:.milliseconds(20))
        }
        throw URLError(.timedOut)
    }
    private func make(_ server: GrantServer, _ events: GrantEvents, _ credentials: GrantCredentials) -> HomeServerStore {
        let home = HomeServerStore(transport:server,eventTransport:events,defaults:nil,credentialStore:credentials)
        home.endpointText=grantOrigin
        return home
    }
    private func connected(_ server: GrantServer, _ events: GrantEvents, _ credentials: GrantCredentials) async throws -> HomeServerStore {
        let home=make(server,events,credentials)
        await home.connect(token:grantSyntheticToken)
        try await until { events.count == 1 }
        try events.push(await server.state())
        try await until { home.controlsAvailable }
        return home
    }
    private func endpointSet(_ home: HomeServerStore) -> Set<String> {
        Set(home.devices.flatMap { row -> [String] in
            if row.raw["kind"].text == "Keypad" { return row.raw["keys"].values.compactMap { $0["endpoint_id"].text } }
            return [row.id]
        })
    }
    private func actionCount(_ home: HomeServerStore, _ panel: String) -> Int { (1...6).filter { home.canActivateKeypad(panel,key:$0) }.count }
    private func checkNoWritesAndSameToken(_ server: GrantServer, _ events: GrantEvents) async {
        let requests=await server.captured()
        #expect(requests.allSatisfy { $0.httpMethod == "GET" && $0.value(forHTTPHeaderField:"Authorization") == "Bearer \(grantSyntheticToken)" })
        #expect(events.captured.allSatisfy { $0.value(forHTTPHeaderField:"Authorization") == "Bearer \(grantSyntheticToken)" && $0.url?.scheme == "wss" })
    }
    private func checkExpanded(_ home: HomeServerStore) throws {
        #expect(home.connected && home.receivingPush && home.canManage && home.controlsAvailable)
        #expect(home.revision == "10" && home.scenes.count == 7)
        #expect(endpointSet(home).count == 20)
        #expect(Set(home.scenes.map(\.id)) == Set(grantSceneIDs))
        #expect(actionCount(home,"study-panel") == 6 && actionCount(home,"studio-panel") == 4)
        let panel=try HomeKeypad(record:#require(home.devices.first { $0.id == "study-panel" }))
        #expect(panel.keys.count == 6 && panel.keys.allSatisfy { $0.endpointID != nil && $0.canWriteLED })
        #expect(panel.keys.allSatisfy { $0.led.actual == .unknown && $0.led.lastWritten == .unknown })
        #expect(panel.settings["work_mode"]["can_write"].flag)
    }

    @Test(arguments:[false,true])
    func changedGrantClosesOldWSSAndAutomaticallyRestoresSameSavedBearer(authenticationError: Bool) async throws {
        let server=GrantServer(),events=GrantEvents(),credentials=GrantCredentials()
        let home=try await connected(server,events,credentials);defer { home.disconnect() }
        #expect(actionCount(home,"study-panel") == 3 && actionCount(home,"studio-panel") == 4)
        let oldEndpoints=endpointSet(home)
        #expect(oldEndpoints.count == 17)
        let oldRequests=await server.captured().count
        await server.expandGrant()
        events.invalidateGeneration(authenticationError:authenticationError)
        try await until { !home.controlsAvailable }
        #expect(home.connected && home.hasSavedCredential)
        #expect(await credentials.load(for:grantOrigin) == grantSyntheticToken)
        #expect(await credentials.removals == 0)
        try await until { events.count == 2 }
        #expect(!home.controlsAvailable, "HTTP catalog refresh does not substitute for the new WSS state")
        try events.push(await server.state())
        try await until { home.controlsAvailable && actionCount(home,"study-panel") == 6 }
        try checkExpanded(home)
        #expect(endpointSet(home).subtracting(oldEndpoints) == Set(["study-panel:key:2","study-panel:key:4","study-panel:key:6"]))
        #expect(oldEndpoints.isSubset(of:endpointSet(home)))
        let newReads=await server.captured().dropFirst(oldRequests)
        for path in ["/api/v1/session","/api/v1/devices","/api/v1/scenes","/api/v1/rooms"] { #expect(newReads.contains { $0.url?.path == path }) }
        #expect(await credentials.saves == 1)
        #expect(await credentials.removals == 0)
        await checkNoWritesAndSameToken(server,events)
    }

    @Test func changedBearerScopeResetAlsoRefreshesSixKeysOnTheSameWSS() async throws {
        let server=GrantServer(),events=GrantEvents(),credentials=GrantCredentials()
        let home=try await connected(server,events,credentials);defer { home.disconnect() }
        #expect(actionCount(home,"study-panel") == 3 && endpointSet(home).count == 17)
        let before=await server.captured().count
        await server.expandGrant()
        // Bearer event streams may resolve the updated grant on their existing
        // connection and emit a reset instead of closing like an old Cookie session.
        try events.push(await server.state())
        try await until { home.controlsAvailable && actionCount(home,"study-panel") == 6 }
        try checkExpanded(home)
        #expect(events.count == 1)
        let reads=await server.captured().dropFirst(before)
        for path in ["/api/v1/session","/api/v1/devices","/api/v1/scenes","/api/v1/rooms"] {
            #expect(reads.contains { $0.url?.path == path })
        }
        #expect(await credentials.load(for:grantOrigin) == grantSyntheticToken)
        #expect(await credentials.removals == 0)
        #expect(await credentials.saves == 1)
        await checkNoWritesAndSameToken(server,events)
    }

    @Test func transient503DuringGrantRefreshRetainsCredentialAndRecoversWithoutManualConnect() async throws {
        let server=GrantServer(),events=GrantEvents(),credentials=GrantCredentials()
        let home=try await connected(server,events,credentials);defer { home.disconnect() }
        await server.expandGrant(transientFailures:1);events.invalidateGeneration()
        try await until { !home.controlsAvailable }
        try await until { events.count == 2 }
        #expect(home.connected && home.hasSavedCredential)
        #expect(await credentials.load(for:grantOrigin) == grantSyntheticToken)
        #expect(await credentials.removals == 0)
        try events.push(await server.state())
        try await until { home.controlsAvailable }
        try checkExpanded(home)
        #expect(await credentials.saves == 1)
        await checkNoWritesAndSameToken(server,events)
    }

    @Test func pausedAppRefreshesScopeWhenForegroundResumesWithoutAConnectCall() async throws {
        let server=GrantServer(),events=GrantEvents(),credentials=GrantCredentials()
        let home=try await connected(server,events,credentials);defer { home.disconnect() }
        home.pause();await server.expandGrant();events.invalidateGeneration()
        #expect(!home.controlsAvailable && home.hasSavedCredential)
        home.resume()
        try await until { events.count == 2 }
        try events.push(await server.state())
        try await until { home.controlsAvailable }
        try checkExpanded(home)
        #expect(await credentials.removals == 0)
        #expect(await credentials.saves == 1)
        await checkNoWritesAndSameToken(server,events)
    }

    @Test func freshAppInstanceRestoresSameSavedBearerAfterGrantChange() async throws {
        let server=GrantServer(),events=GrantEvents(),credentials=GrantCredentials()
        let first=try await connected(server,events,credentials)
        first.disconnect();await server.expandGrant()
        let nextEvents=GrantEvents(),reopened=make(server,nextEvents,credentials)
        defer { reopened.disconnect() }
        await reopened.restoreSavedConnection()
        try await until { nextEvents.count == 1 }
        try nextEvents.push(await server.state())
        try await until { reopened.controlsAvailable }
        try checkExpanded(reopened)
        #expect(await credentials.load(for:grantOrigin) == grantSyntheticToken)
        #expect(await credentials.removals == 0)
        await checkNoWritesAndSameToken(server,nextEvents)
    }

    @Test func actual401AfterWSSClosureStillClearsTheRejectedCredential() async throws {
        let server=GrantServer(),events=GrantEvents(),credentials=GrantCredentials()
        let home=try await connected(server,events,credentials);defer { home.disconnect() }
        await server.rejectBearer();events.invalidateGeneration()
        try await until { !home.connected }
        #expect(!home.hasSavedCredential && !home.controlsAvailable)
        #expect(await credentials.load(for:grantOrigin) == nil)
        #expect(await credentials.removals == 1)
        #expect(events.count == 1)
        await checkNoWritesAndSameToken(server,events)
    }
}
