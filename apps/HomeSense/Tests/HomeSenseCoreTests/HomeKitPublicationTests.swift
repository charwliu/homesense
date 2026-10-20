import Foundation
import Testing
@testable import HomeSenseCore

private actor PlacementServer: HomeHTTPTransport {
    let base = HomeTestServer()
    var reads = 0
    var boot = "synthetic-boot"
    var status = 200
    func setBoot(_ value: String) { boot = value }
    func setStatus(_ value: Int) { status = value }
    func count() -> Int { reads }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        if request.url?.path != "/api/v1/homekit/placement" { return try await base.send(request) }
        reads += 1
        #expect(request.httpMethod == "GET" && request.httpBody == nil)
        #expect(request.value(forHTTPHeaderField: "Authorization") == "Bearer test-token")
        let publication = HomeKitRoomPlacement.Publication(boot: boot, revision: "9007199254740993", generation: "graph", bridges: [
            .init(id: "bridge", name: "HomeSense", available: true, accessories: [
                .init(kind: .light, id: "light", aid: 2, serialNumber: "HS-2", roomID: "study")])])
        return HomeHTTPResponse(data: try JSONEncoder().encode(publication), status: status)
    }
}

@Suite @MainActor struct HomeKitPublicationTests {
    private func store(_ server: PlacementServer) -> HomeServerStore {
        let home = HomeServerStore(transport: server, defaults: nil, credentialStore: InMemoryCredentials())
        home.pause(); home.endpointText = "https://home.example:8444"
        return home
    }

    @Test func explicitAuthenticatedReadPreservesExactRevisionAndNeverPollsOrSendsControl() async throws {
        let server = PlacementServer(), home = store(server)
        await home.connect(token: "test-token")
        #expect(await server.count() == 0)
        let publication = try await home.homeKitPlacementPublication()
        #expect(publication.revision == "9007199254740993" && publication.bridges[0].accessories[0].roomID == "study")
        #expect(await server.count() == 1)
        #expect(await server.base.captured().allSatisfy { $0.httpMethod == "GET" })
        home.disconnect()
    }

    @Test func staleBootAndUnavailableMetadataNeverReturnAnOldFallbackOrRetry() async throws {
        let server = PlacementServer(), home = store(server)
        await home.connect(token: "test-token")
        await server.setBoot("previous-boot")
        do { _ = try await home.homeKitPlacementPublication(); Issue.record("stale boot accepted") }
        catch { if case HomeServerError.invalidResponse = error {} else { Issue.record("wrong failure: \(error)") } }
        await server.setStatus(503)
        do { _ = try await home.homeKitPlacementPublication(); Issue.record("503 accepted") }
        catch { #expect(error.localizedDescription.contains("HTTP 503")) }
        #expect(await server.count() == 2)
        home.disconnect()
    }

    @Test func membersAndDisconnectedStoresCannotRequestTheAdministratorCatalog() async {
        let server = PlacementServer(), home = store(server)
        do { _ = try await home.homeKitPlacementPublication(); Issue.record("disconnected read accepted") }
        catch { if case HomeServerError.denied = error {} else { Issue.record("wrong failure: \(error)") } }
        await server.base.setAdministrator(false)
        await home.connect(token: "test-token")
        #expect(home.connected && !home.canManage)
        do { _ = try await home.homeKitPlacementPublication(); Issue.record("member read accepted") }
        catch { if case HomeServerError.denied = error {} else { Issue.record("wrong failure: \(error)") } }
        #expect(await server.count() == 0)
        home.disconnect()
    }
}
