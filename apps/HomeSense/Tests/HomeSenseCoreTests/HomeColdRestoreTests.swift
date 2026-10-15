import Foundation
import Testing
@testable import HomeSenseCore

private let coldOrigin = "https://127.0.0.1:39443"
private let coldToken = "cold-restore-fixture-only-never-a-real-token"
enum ColdFailure: String, CaseIterable, Sendable { case unavailable, unreachable, unauthorized, untrustedTLS, malformed }
private actor ColdGate {
    private(set) var entered = false
    private var waiter: CheckedContinuation<Void, Never>?
    func wait() async {
        entered = true
        await withCheckedContinuation { waiter = $0 }
    }
    func release() { waiter?.resume(); waiter = nil }
}
private actor ColdCredentials: HomeCredentialStore {
    private var saved: String? = coldToken
    private(set) var removes = 0
    private let gate: ColdGate?
    private var loads = 0
    init(gate: ColdGate? = nil) { self.gate = gate }
    func load(for origin: String) async -> String? {
        loads += 1
        let captured = origin == coldOrigin ? saved : nil
        if loads == 1, let gate { await gate.wait() }
        return captured
    }
    func save(_ token: String, for origin: String) { precondition(origin == coldOrigin); saved = token }
    func remove(for origin: String) { precondition(origin == coldOrigin); saved = nil; removes += 1 }
}
private actor ColdServer: HomeHTTPTransport {
    private var failure: ColdFailure
    private var remaining: Int
    private var requests: [URLRequest] = []
    private var sessionTimes: [Date] = []
    private let gate: ColdGate?
    private let gatedSession: Int
    init(_ failure: ColdFailure, count: Int = 1, gate: ColdGate? = nil, gatedSession: Int = 0) {
        self.failure = failure; remaining = count; self.gate = gate; self.gatedSession = gatedSession
    }
    func recover() { remaining = 0 }
    func rejectNext(_ failure: ColdFailure) { self.failure = failure; remaining = 100 }
    func captured() -> [URLRequest] { requests }
    func times() -> [Date] { sessionTimes }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        precondition(request.url?.host == "127.0.0.1")
        requests.append(request)
        precondition(request.value(forHTTPHeaderField:"Authorization") == "Bearer \(coldToken)")
        precondition(request.httpMethod == "GET", "The recovery path must never submit or replay an action")
        func response(_ json: HomeJSON, _ status: Int = 200) throws -> HomeHTTPResponse { .init(data:try json.data(),status:status) }
        if request.url!.path == "/api/v1/session" {
            sessionTimes.append(Date())
            if sessionTimes.count == gatedSession, let gate { await gate.wait() }
            if remaining > 0 {
                remaining -= 1
                switch failure {
                case .unavailable: return try response(.object(["error":.string("runtime_reconfiguring")]),503)
                case .unreachable: throw URLError(.cannotConnectToHost)
                case .unauthorized: return try response(.object(["error":.string("authentication_required")]),401)
                case .untrustedTLS: throw URLError(.serverCertificateUntrusted)
                case .malformed: return try response(.object(["can_manage_configuration":.null]))
                }
            }
            return try response(.object(["can_manage_configuration":.bool(false)]))
        }
        let common: [String:HomeJSON] = ["revision":.integer(10),"boot":.string("cold-boot")]
        switch request.url!.path {
        case "/api/v1/devices": return try response(.object(common.merging(["devices":.array([.object(["id":.string("light"),"name":.string("Synthetic light"),"kind":.string("Dimmer"),"can_control":.bool(true)])])]) { _,new in new }))
        case "/api/v1/scenes": return try response(.object(["revision":.integer(10),"scenes":.array([])]))
        case "/api/v1/rooms": return try response(.object(common.merging(["rooms":.array([]),"floors":.array([])]) { _,new in new }))
        default: return try response(.object(["error":.string("unexpected_fixture_path")]),404)
        }
    }
}
private final class ColdEvents: HomeEventTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var streams: [AsyncThrowingStream<HomeEventMessage,Error>.Continuation] = []
    func events(_ request: URLRequest) -> AsyncThrowingStream<HomeEventMessage,Error> {
        precondition(request.url?.host == "127.0.0.1")
        precondition(request.value(forHTTPHeaderField:"Authorization") == "Bearer \(coldToken)")
        return AsyncThrowingStream { continuation in lock.withLock { streams.append(continuation) } }
    }
    var count: Int { lock.withLock { streams.count } }
    func state() throws {
        let data=try HomeJSON.object(["type":.string("state"),"reset":.bool(true),"revision":.integer(10),
            "cursor":.object(["boot":.string("cold-boot"),"revision":.integer(10),"transport_epoch":.integer(1),"scope":.string("same-synthetic-scope"),"sequence":.integer(1)]),
            "devices":.array([.object(["id":.string("light"),"name":.string("Synthetic light"),"kind":.string("Dimmer"),"can_control":.bool(true)])]),"scenes":.array([])]).data()
        lock.withLock { streams.last }?.yield(.state(data))
    }
}
private enum ColdTestError: Error { case timeout }
@Suite @MainActor
struct HomeColdRestoreTests {
    private func until(_ predicate: () -> Bool, seconds: Double = 4) async throws {
        let end=Date().addingTimeInterval(seconds)
        while Date() < end {
            if predicate() { return }
            try await Task.sleep(for:.milliseconds(20))
        }
        throw ColdTestError.timeout
    }
    private func make(_ server: ColdServer, _ credentials: ColdCredentials, _ events: ColdEvents) -> HomeServerStore {
        let home=HomeServerStore(transport:server,eventTransport:events,defaults:nil,credentialStore:credentials)
        home.endpointText=coldOrigin
        return home
    }
    private func requireFreshState(_ home: HomeServerStore, _ events: ColdEvents) async throws {
        try await until { home.connected && events.count == 1 }
        #expect(!home.controlsAvailable, "Catalog authentication alone must not enable controls")
        try events.state()
        try await until { home.controlsAvailable }
    }

    @Test(arguments:[ColdFailure.unavailable, .unreachable])
    func savedTokenRecoversAfterInitialFailureWithoutManualConnectOrReopening(_ failure: ColdFailure) async throws {
        let server=ColdServer(failure),credentials=ColdCredentials(),events=ColdEvents()
        let home=make(server,credentials,events);defer { home.disconnect() }
        await home.restoreSavedConnection()
        #expect(!home.connected && !home.controlsAvailable && home.hasSavedCredential)
        #expect(await credentials.load(for:coldOrigin) == coldToken)
        try await requireFreshState(home,events)
        #expect(await credentials.removes == 0)
        let times=await server.times()
        #expect(times.count >= 2)
        if times.count >= 2 { #expect(times[1].timeIntervalSince(times[0]) >= 1.8) }
        #expect(await server.captured().allSatisfy { $0.httpMethod == "GET" })
    }

    @Test func repeatedTransientFailureUsesBackoffAndStillOnlyReads() async throws {
        let server=ColdServer(.unavailable,count:2),credentials=ColdCredentials(),events=ColdEvents()
        let home=make(server,credentials,events);defer { home.disconnect() }
        await home.restoreSavedConnection()
        try await until({ home.connected && events.count == 1 },seconds:9)
        #expect(!home.controlsAvailable)
        try events.state();try await until { home.controlsAvailable }
        let times=await server.times()
        #expect(times.count >= 3)
        if times.count >= 3 {
            #expect(times[1].timeIntervalSince(times[0]) >= 1.8)
            #expect(times[2].timeIntervalSince(times[1]) >= 3.8)
        }
        #expect(await credentials.removes == 0)
        #expect(await server.captured().allSatisfy { $0.httpMethod == "GET" })
    }

    @Test(arguments:["disconnect","remove","origin","pause"])
    func explicitActionsCancelPendingRecovery(_ action: String) async throws {
        let server=ColdServer(.unavailable,count:100),credentials=ColdCredentials(),events=ColdEvents()
        let home=make(server,credentials,events);defer { home.disconnect() }
        await home.restoreSavedConnection()
        let before=await server.captured().count
        switch action {
        case "disconnect": home.disconnect();home.resume()
        case "remove": await home.removeSavedCredential();home.resume()
        case "origin": home.endpointText="https://127.0.0.1:39444";home.resume()
        case "pause": home.pause()
        default: Issue.record("unexpected test input")
        }
        await server.recover()
        try await Task.sleep(for:.milliseconds(2300))
        #expect(await server.captured().count == before)
        #expect(!home.connected && !home.controlsAvailable && events.count == 0)
        if action == "remove" { #expect(await credentials.load(for:coldOrigin) == nil) }
        else { #expect(await credentials.load(for:coldOrigin) == coldToken) }
        if action == "pause" {
            home.resume()
            try await requireFreshState(home,events)
        }
    }

    @Test(arguments:[ColdFailure.unauthorized,.untrustedTLS,.malformed])
    func permanentRejectionOrInvalidTrustNeverLoops(_ failure: ColdFailure) async throws {
        let server=ColdServer(failure,count:100),credentials=ColdCredentials(),events=ColdEvents()
        let home=make(server,credentials,events);defer { home.disconnect() }
        await home.restoreSavedConnection()
        let before=await server.captured().count
        try await Task.sleep(for:.milliseconds(2300))
        #expect(await server.captured().count == before)
        #expect(!home.connected && !home.controlsAvailable && events.count == 0)
        if failure == .unauthorized {
            #expect(await credentials.load(for:coldOrigin) == nil)
            #expect(await credentials.removes == 1)
        } else {
            #expect(await credentials.load(for:coldOrigin) == coldToken)
            #expect(await credentials.removes == 0)
        }
    }

    @Test(arguments: ["lookup", "initialHTTP", "retryHTTP"], ["disconnect", "remove", "origin", "pause"])
    func cancellationRejectsLateLookupAndHTTP(_ phase: String, _ action: String) async throws {
        let gate = ColdGate()
        let credentials = ColdCredentials(gate: phase == "lookup" ? gate : nil)
        let server = ColdServer(.unavailable, count: phase == "retryHTTP" ? 1 : 0,
                                gate: phase == "lookup" ? nil : gate,
                                gatedSession: phase == "retryHTTP" ? 2 : 1)
        let events = ColdEvents(), home = make(server, credentials, events)
        defer { home.disconnect() }
        let restoration = Task { await home.restoreSavedConnection() }
        let deadline = Date().addingTimeInterval(4)
        while !(await gate.entered), Date() < deadline { try await Task.sleep(for: .milliseconds(20)) }
        #expect(await gate.entered)
        let before = await server.captured().count
        switch action {
        case "disconnect": home.disconnect()
        case "remove": await home.removeSavedCredential()
        case "origin": home.endpointText = "https://127.0.0.1:39444"
        case "pause": home.pause()
        default: Issue.record("unexpected test input")
        }
        await gate.release()
        await restoration.value
        try await Task.sleep(for: .milliseconds(2300))
        #expect(await server.captured().count == before, "Late completion must not start more requests")
        #expect(!home.connected && !home.controlsAvailable && events.count == 0)
        #expect(!home.busy && !home.restoringCredential)
        #expect(home.devices.isEmpty && home.revision.isEmpty)
        #expect(await credentials.load(for: coldOrigin) == (action == "remove" ? nil : coldToken))
        if action == "pause" {
            home.resume()
            try await requireFreshState(home, events)
        } else {
            home.resume()
            try await Task.sleep(for: .milliseconds(100))
            #expect(await server.captured().count == before)
        }
    }


    @Test(arguments: [ColdFailure.unauthorized, .untrustedTLS, .malformed])
    func permanentRejectionAfterTransientRetryStops(_ rejection: ColdFailure) async throws {
        let server = ColdServer(.unavailable), credentials = ColdCredentials(), events = ColdEvents()
        let home = make(server, credentials, events)
        defer { home.disconnect() }
        await home.restoreSavedConnection()
        await server.rejectNext(rejection)
        try await until { !home.restoringCredential }
        let before = await server.captured().count
        #expect(before == 2)
        try await Task.sleep(for: .milliseconds(2300))
        #expect(await server.captured().count == before)
        #expect(!home.connected && !home.controlsAvailable && events.count == 0)
        #expect(await credentials.load(for: coldOrigin) == (rejection == .unauthorized ? nil : coldToken))
        #expect(await credentials.removes == (rejection == .unauthorized ? 1 : 0))
    }

}
