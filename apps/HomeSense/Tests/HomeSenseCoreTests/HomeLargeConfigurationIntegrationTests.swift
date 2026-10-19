#if os(macOS)
import Foundation
import Darwin
import Security
import Testing
@testable import HomeSenseCore

@MainActor
struct HomeLargeConfigurationIntegrationTests {
    @Test(.enabled(if: ProcessInfo.processInfo.environment["HOMESENSE_LARGE_CONFIG_BINARY"] != nil,
                   "Set HOMESENSE_LARGE_CONFIG_BINARY for the synthetic loopback HTTPS fixture"))
    func nativeLargeConfigurationDraftUsesRealURLSessionAndKeepsActiveRevision() async throws {
        let binary = try #require(ProcessInfo.processInfo.environment["HOMESENSE_LARGE_CONFIG_BINARY"])
        let package = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let repository = package.deletingLastPathComponent().deletingLastPathComponent()
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("homesense-large-native-\(UUID())")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let devices = try Data(contentsOf: repository.appendingPathComponent("config/simulation.json"))
        try devices.write(to: root.appendingPathComponent("devices.json"))
        let definitions = (0..<384).map { index in
            HomeJSON.object(["id": .string("synthetic-disabled-\(index)"), "enabled": .bool(false),
                "review_evidence": .string(String(repeating: "x", count: 8000)), "activate": .array([])])
        }
        let automation = HomeJSON.object(["schema_version": .integer(1), "configuration_revision": .integer(1),
            "variables": .array([]), "rules": .array([]), "immediate_scenes": .array(definitions)])
        #expect(try automation.data().count > 2 * 1024 * 1024)
        try automation.data().write(to: root.appendingPathComponent("automation.json"))
        let tls = """
        [req]
        distinguished_name=dn
        [dn]
        CN=localhost
        [v3]
        basicConstraints=critical,CA:TRUE
        keyUsage=critical,digitalSignature,keyEncipherment,keyCertSign
        extendedKeyUsage=serverAuth
        subjectAltName=DNS:localhost,IP:127.0.0.1
        """
        try Data(tls.utf8).write(to: root.appendingPathComponent("tls.cnf"))
        let openssl = Process()
        openssl.executableURL = URL(fileURLWithPath: "/usr/bin/openssl")
        openssl.arguments = ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=localhost",
            "-config", root.appendingPathComponent("tls.cnf").path, "-extensions", "v3",
            "-keyout", root.appendingPathComponent("key.pem").path, "-out", root.appendingPathComponent("cert.pem").path]
        openssl.standardOutput = FileHandle.nullDevice; openssl.standardError = FileHandle.nullDevice
        try openssl.run(); openssl.waitUntilExit(); #expect(openssl.terminationStatus == 0)
        let token = "synthetic-large-native-token-01234567890123456789"
        try Data(token.utf8).write(to: root.appendingPathComponent("token"))
        for name in ["token", "key.pem"] {
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: root.appendingPathComponent(name).path)
        }
        let socket = Darwin.socket(AF_INET, SOCK_STREAM, 0)
        guard socket >= 0 else { throw HomeServerError.server("Cannot reserve a test port") }
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size); address.sin_family = sa_family_t(AF_INET)
        address.sin_addr.s_addr = inet_addr("127.0.0.1")
        let bound = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(socket, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        guard bound == 0 else { Darwin.close(socket); throw HomeServerError.server("Cannot reserve a test port") }
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        _ = withUnsafeMutablePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(socket, $0, &length) }
        }
        let port = UInt16(bigEndian: address.sin_port); Darwin.close(socket)
        let origin = "https://localhost:\(port)"
        let logURL = root.appendingPathComponent("server.log")
        FileManager.default.createFile(atPath: logURL.path, contents: nil)
        let log = try FileHandle(forWritingTo: logURL); defer { try? log.close() }
        let server = Process()
        server.executableURL = URL(fileURLWithPath: binary)
        server.arguments = ["serve-web-tls-simulation", root.appendingPathComponent("devices.json").path,
            root.appendingPathComponent("automation.json").path, root.appendingPathComponent("state").path,
            "120", "127.0.0.1:\(port)", root.appendingPathComponent("token").path, origin,
            root.appendingPathComponent("cert.pem").path, root.appendingPathComponent("key.pem").path, "--administrator"]
        server.standardOutput = log; server.standardError = log
        try server.run(); defer { if server.isRunning { server.terminate(); server.waitUntilExit() } }
        let trust = try FixtureWebSocketTrust(pem: String(contentsOf: root.appendingPathComponent("cert.pem"), encoding: .utf8))
        let transport = HomeURLSessionTransport(delegate: trust)
        let home = HomeServerStore(transport: transport, eventTransport: HomeWebSocketTransport(delegate: trust), defaults: nil, credentialStore: InMemoryCredentials())
        home.pause(); home.endpointText = origin
        for _ in 0..<30 {
            await home.connect(token: token)
            if home.connected { break }
            guard server.isRunning else { throw HomeServerError.server((try? String(contentsOf: logURL, encoding: .utf8)) ?? "Synthetic server exited") }
            try await Task.sleep(for: .milliseconds(100))
        }
        defer { home.disconnect() }
        #expect(home.connected && home.canManage)
        await home.loadActiveConfiguration()
        #expect(home.error == nil, "\(home.error ?? "")")
        #expect(home.automationDocument?["immediate_scenes"] == .array(definitions))
        await home.saveDraft()
        #expect(home.error == nil, "\(home.error ?? "")")
        let id = try #require(home.draftID)
        await home.loadActiveConfiguration()
        await home.loadDraft(id)
        #expect(home.error == nil, "\(home.error ?? "")")
        #expect(home.draftID == id && home.baseRevision == "1" && home.candidateRevision == "2")
        #expect(home.automationDocument?["immediate_scenes"] == .array(definitions))
        await home.previewDraft()
        #expect(home.error == nil && home.preview != nil, "\(home.error ?? "")")
        let endpoint = try HomeServerEndpoint(origin)
        let response = try await transport.send(endpoint.request(path: "/api/v1/config/versions?limit=1", method: "GET", token: token, body: nil))
        let catalog = try JSONDecoder().decode(HomeJSON.self, from: response.data)
        #expect(response.status == 200 && catalog["active_revision"].text == "1")
        #expect(home.revision == "1", "Saving, reloading and previewing must not publish")
    }
}
#endif
