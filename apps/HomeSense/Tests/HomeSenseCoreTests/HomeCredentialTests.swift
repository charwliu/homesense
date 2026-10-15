import Foundation
import Testing
@testable import HomeSenseCore

actor InMemoryCredentials: HomeCredentialStore {
    private var values: [String: String] = [:]
    func load(for origin: String) -> String? { values[origin] }
    func save(_ token: String, for origin: String) { values[origin] = token }
    func remove(for origin: String) { values.removeValue(forKey: origin) }
}

private actor DelayedCredentialLookup: HomeCredentialStore {
    private var continuation: CheckedContinuation<String?, Never>?
    private(set) var started = false
    func load(for origin: String) async -> String? {
        started = true
        return await withCheckedContinuation { continuation = $0 }
    }
    func release() { continuation?.resume(returning: "test-token"); continuation = nil }
    func save(_ token: String, for origin: String) {}
    func remove(for origin: String) {}
}

@Suite @MainActor
struct HomeCredentialTests {
    private let origin = "https://home.example:8444"
    private func make(_ server: HomeTestServer, _ credentials: any HomeCredentialStore) -> HomeServerStore {
        let home = HomeServerStore(transport: server, defaults: nil, credentialStore: credentials)
        home.pause(); home.endpointText = origin
        return home
    }
    @Test func successfulAuthenticationPersistsAndReopenRestoresWithoutWrites() async throws {
        let server = HomeTestServer(), credentials = InMemoryCredentials()
        let home = make(server, credentials)
        await home.connect(token: "test-token")
        #expect(home.connected && home.hasSavedCredential)
        #expect(await credentials.load(for: origin) == "test-token")
        home.disconnect()
        #expect(await credentials.load(for: origin) == "test-token")
        let reopened = make(server, credentials)
        await reopened.restoreSavedConnection()
        #expect(reopened.connected && reopened.hasSavedCredential && !reopened.restoringCredential)
        #expect(await server.captured().allSatisfy { $0.httpMethod == "GET" })
        reopened.disconnect()
    }
    @Test func differentOriginsNeverReceiveAnotherServersToken() async throws {
        let server = HomeTestServer(), credentials = InMemoryCredentials()
        await credentials.save("test-token", for: origin)
        let home = make(server, credentials)
        home.endpointText = "https://other.example:8444"
        await home.restoreSavedConnection()
        #expect(!home.connected && !home.hasSavedCredential)
        #expect(await server.captured().isEmpty)
        #expect(await credentials.load(for: origin) == "test-token")
        home.endpointText = "https://home.example:8445"
        await home.restoreSavedConnection()
        #expect(await server.captured().isEmpty)
    }
    @Test func canonicalOriginsPreservePortsAndNormalizeHostAndDefaultPort() throws {
        #expect(try HomeServerEndpoint(" HTTPS://HOME.EXAMPLE:8444/ ").origin == origin)
        #expect(try HomeServerEndpoint("https://HOME.EXAMPLE:443/").origin == "https://home.example")
        #expect(try HomeServerEndpoint("https://home.example:8445").origin != origin)
    }
    @Test func networkFailureAndUntrustedTLSKeepStoredCredential() async throws {
        for failure in [URLError.cannotConnectToHost, .networkConnectionLost, .serverCertificateUntrusted] {
            let server = HomeTestServer(), credentials = InMemoryCredentials()
            await credentials.save("test-token", for: origin)
            await server.setNetworkFailure(failure)
            let home = make(server, credentials)
            await home.restoreSavedConnection()
            #expect(!home.connected && home.error != nil && home.hasSavedCredential)
            #expect(await credentials.load(for: origin) == "test-token")
            #expect(await server.captured().count == 1, "No automatic retry or replay after restoration fails")
        }
    }
    @Test func unauthorizedResponseRemovesOnlyMatchingOrigin() async throws {
        let server = HomeTestServer(), credentials = InMemoryCredentials()
        await credentials.save("test-token", for: origin)
        await credentials.save("other-test-token", for: "https://other.example")
        await server.setUnauthorized()
        let home = make(server, credentials)
        await home.restoreSavedConnection()
        #expect(!home.connected && !home.hasSavedCredential)
        #expect(await credentials.load(for: origin) == nil)
        #expect(await credentials.load(for: "https://other.example") == "other-test-token")
    }
    @Test func connectionRefusedCanRecoverUsingTheSameSavedCredential() async throws {
        let server = HomeTestServer(), credentials = InMemoryCredentials(), home = make(server, credentials)
        await credentials.save("test-token", for: origin)
        await server.setNetworkFailure(.cannotConnectToHost)
        await home.restoreSavedConnection()
        #expect(!home.connected && home.hasSavedCredential)
        await server.setNetworkFailure(nil)
        await home.restoreSavedConnection()
        #expect(home.connected && home.hasSavedCredential)
        #expect(await server.captured().allSatisfy { $0.httpMethod == "GET" })
        home.disconnect()
    }
    @Test func malformedSessionCannotOverwritePreviouslySavedCredential() async throws {
        let server = HomeTestServer(), credentials = InMemoryCredentials(), home = make(server, credentials)
        await credentials.save("previous-synthetic-token", for: origin)
        await server.setMalformedSession()
        await home.connect(token: "test-token")
        #expect(!home.connected)
        #expect(await credentials.load(for: origin) == "previous-synthetic-token")
    }
    @Test func rejectedManualInputDoesNotDeleteADifferentPreviouslySavedToken() async throws {
        let server = HomeTestServer(), credentials = InMemoryCredentials(), home = make(server, credentials)
        await credentials.save("test-token", for: origin)
        await home.connect(token: "wrong-synthetic-token")
        #expect(!home.connected && home.hasSavedCredential)
        #expect(await credentials.load(for: origin) == "test-token")
        await home.restoreSavedConnection()
        #expect(home.connected)
        home.disconnect()
    }
    @Test func explicitForgetClearsMemoryAndSavedOriginWithoutServerRevocation() async throws {
        let server = HomeTestServer(), credentials = InMemoryCredentials(), home = make(server, credentials)
        await home.connect(token: "test-token")
        let requests = await server.captured().count
        await home.removeSavedCredential()
        #expect(!home.connected && !home.hasSavedCredential && !home.busy)
        #expect(await credentials.load(for: origin) == nil)
        #expect(await server.captured().count == requests)
    }
    @Test func endpointChangedDuringLookupCannotLeakLoadedToken() async throws {
        let server = HomeTestServer(), credentials = DelayedCredentialLookup(), home = make(server, credentials)
        let restore = Task { await home.restoreSavedConnection() }
        for _ in 0..<100 {
            if await credentials.started { break }
            try await Task.sleep(for: .milliseconds(5))
        }
        try #require(await credentials.started)
        home.endpointText = "https://other.example"
        await credentials.release(); await restore.value
        #expect(await server.captured().isEmpty)
        #expect(!home.connected && !home.restoringCredential)
    }
}
