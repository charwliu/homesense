import Foundation
import Testing
@testable import HomeSenseCore

private actor RoomAllHTTP: HomeHTTPTransport {
    let base = HomeTestServer()
    let active: Bool
    init(active: Bool) { self.active = active }
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        let reply = try await base.send(request)
        guard reply.status == 200, request.url?.path == "/api/v1/scenes" else { return reply }
        var body = try JSONDecoder().decode(HomeJSON.self, from: reply.data)
        body["scenes"] = .array([.object([
            "id": .string("reading"), "name": .string("全部"),
            "can_control": .bool(true), "switch_kind": .string("momentary"),
            "activity_kind": .string("read_only"), "activity": .string(active ? "Active" : "Inactive"),
            "selected": .null
        ])])
        return HomeHTTPResponse(data: try body.data(), status: reply.status)
    }
}

@Suite @MainActor
struct HomeRoomAllSceneTests {
    private func scene(name: String = "全部", activity: String, kind: String = "read_only") -> HomeRecord {
        HomeRecord(.object([
            "id": .string("all"), "name": .string(name), "switch_kind": .string("momentary"),
            "activity_kind": .string(kind), "activity": .string(activity), "selected": .null
        ]))
    }

    @Test(arguments: [true, false])
    func anyLitClosesAndAllDarkOpensWithoutSendingAnExplicitSelection(active: Bool) async throws {
        let row = scene(activity: active ? "Active" : "Inactive")
        #expect(row.sceneCardAction == .serverToggle(active: active))
        #expect(row.sceneCardAction.title == (active ? "关闭" : "开启"))
        #expect(row.sceneCardAction.on == nil)
        let server = RoomAllHTTP(active: active)
        let home = HomeServerStore(transport: server, defaults: nil, credentialStore: InMemoryCredentials())
        home.pause()
        home.endpointText = "https://home.example:8444"
        await home.connect(token: "test-token")
        defer { home.disconnect() }
        let record = try #require(home.scenes.first)
        #expect(record.sceneCardAction == row.sceneCardAction)
        await home.controlScene(record.id, on: record.sceneCardAction.on)
        let requests = await server.base.captured().filter { $0.httpMethod == "POST" }
        #expect(requests.count == 1)
        let request = try #require(requests.first)
        #expect(request.url?.path == "/api/v1/scene-actions")
        let body = try JSONDecoder().decode(HomeJSON.self, from: #require(request.httpBody))
        guard case .object(let fields) = body else { Issue.record("expected request object"); return }
        #expect(fields["scene"] == .string("reading"))
        #expect(fields["on"] == nil, "the server must decide from its current feedback, even if the displayed state has changed")
    }

    @Test(arguments: ["Unknown", "Pending", "Partial", "unrecognized"])
    func unknownActivityDoesNotGuessTheNextDirection(activity: String) {
        #expect(scene(activity: activity).sceneCardAction == .execute)
        #expect(scene(activity: activity).sceneCardAction.title == "执行")
    }

    @Test(arguments: ["阅读", "离家", "全关"])
    func otherMomentaryActionsKeepTheirOriginalExecuteLabel(name: String) {
        #expect(scene(name: name, activity: "Active").sceneCardAction == .execute)
    }

    @Test func missingReadOnlyActivityCannotTurnAnArbitraryAllNamedActionIntoAToggle() {
        #expect(scene(activity: "Active", kind: "unconfigured").sceneCardAction == .execute)
    }
}
