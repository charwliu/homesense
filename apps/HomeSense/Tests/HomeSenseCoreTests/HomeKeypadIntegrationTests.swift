#if os(macOS)
import Foundation
import Darwin
import Testing
@testable import HomeSenseCore

/// Opt-in actual Rust HTTPS/WSS test. Inputs must be a frozen candidate and
/// dedicated simulator fixtures, never the deployment service or admin token.
@Suite @MainActor
struct HomeKeypadIntegrationTests {
    @Test(.enabled(if: ProcessInfo.processInfo.environment["HOMESENSE_KEYPAD_TEST_BINARY"] != nil
                   && ProcessInfo.processInfo.environment["HOMESENSE_KEYPAD_TEST_FIXTURES"] != nil,
                   "Requires a frozen Rust binary and isolated Keypad fixtures"))
    func realKeypadHTTPSWSSKeepsOneParentAndNeverPromotesWritesToActual() async throws {
        let environment = ProcessInfo.processInfo.environment
        let binary = try #require(environment["HOMESENSE_KEYPAD_TEST_BINARY"])
        let fixtures = URL(fileURLWithPath: try #require(environment["HOMESENSE_KEYPAD_TEST_FIXTURES"]))
        let document = try HomeJSON.parse(String(contentsOf: fixtures.appendingPathComponent("devices.json"), encoding: .utf8))
        try #require(document["gateways"].values.allSatisfy { $0["endpoint"].text?.hasPrefix("127.0.0.1:") == true })
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("native-keypad-\(UUID())")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let config = """
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
        try Data(config.utf8).write(to: root.appendingPathComponent("tls.cnf"))
        let cert = root.appendingPathComponent("cert.pem"), key = root.appendingPathComponent("key.pem")
        let openssl = Process(); openssl.executableURL = URL(fileURLWithPath: "/usr/bin/openssl")
        openssl.arguments = ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=localhost",
            "-config", root.appendingPathComponent("tls.cnf").path, "-extensions", "v3", "-keyout", key.path, "-out", cert.path]
        openssl.standardOutput = FileHandle.nullDevice; openssl.standardError = FileHandle.nullDevice
        try openssl.run(); openssl.waitUntilExit(); try #require(openssl.terminationStatus == 0)
        let token = "isolated-keypad-fixture-token-only"
        let tokenURL = root.appendingPathComponent("token")
        try Data(token.utf8).write(to: tokenURL)
        for path in [tokenURL, key] { try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: path.path) }
        let socket = Darwin.socket(AF_INET, SOCK_STREAM, 0)
        try #require(socket >= 0)
        var address = sockaddr_in(); address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET); address.sin_addr.s_addr = inet_addr("127.0.0.1")
        let bound = withUnsafePointer(to: &address) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(socket, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) } }
        guard bound == 0 else { Darwin.close(socket); throw HomeServerError.server("Cannot reserve loopback fixture port") }
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        _ = withUnsafeMutablePointer(to: &address) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(socket, $0, &length) } }
        let port = UInt16(bigEndian: address.sin_port); Darwin.close(socket)
        let origin = "https://localhost:\(port)"
        let server = Process(); server.executableURL = URL(fileURLWithPath: binary)
        server.arguments = ["serve-web-tls-simulation", fixtures.appendingPathComponent("devices.json").path,
            fixtures.appendingPathComponent("automation.json").path, root.appendingPathComponent("state").path,
            "60", "127.0.0.1:\(port)", tokenURL.path, origin, cert.path, key.path, "--administrator"]
        let logURL = root.appendingPathComponent("server.log")
        FileManager.default.createFile(atPath: logURL.path, contents: nil)
        let log = try FileHandle(forWritingTo: logURL)
        server.standardOutput = log; server.standardError = log
        try server.run()
        defer { if server.isRunning { server.terminate(); server.waitUntilExit() }; try? log.close() }
        let https = FixtureHTTPS(certificate: cert.path)
        let trust = try FixtureWebSocketTrust(pem: String(contentsOf: cert, encoding: .utf8))
        let home = HomeServerStore(transport: https, eventTransport: HomeWebSocketTransport(delegate: trust), defaults: nil, credentialStore: InMemoryCredentials())
        home.pause(); home.endpointText = origin
        for _ in 0..<50 {
            await home.connect(token: token)
            if home.connected { break }
            if !server.isRunning { throw HomeServerError.server(try String(contentsOf: logURL, encoding: .utf8)) }
            try await Task.sleep(for: .milliseconds(100))
        }
        defer { home.disconnect() }
        try #require(home.connected && home.canManage, "\(home.error ?? "")")
        #expect(home.devices.filter { $0.raw["kind"].text == "Keypad" && $0.raw["module_id"].text != nil }.map(\.id) == ["panel"])
        #expect(!home.devices.contains { $0.id.hasPrefix("legacy-key-") })
        #expect(home.editableDevices.count == 2 && home.editableKeypads.count == 1)
        func panelState() -> HomeJSON { home.devices.first { $0.id == "panel" }?.raw ?? .null }
        let parent = try HomeKeypad(record: #require(home.devices.first { $0.id == "panel" }))
        #expect(parent.keys.count == 6 && parent.keys.allSatisfy { $0.led.actual == .unknown })
        home.resume()
        try await waitFor("initial WSS", home: home) { home.receivingPush }
        try #require(home.controlsAvailable)
        try #require(try HomeKeypad(record: #require(home.devices.first { $0.id == "panel" })).keys[0].canWriteLED)
        await home.writeKeypadLED("panel", key: 1, color: .green)
        try #require(home.error == nil, "\(home.error ?? "")")
        try await waitFor("green write", home: home) { panelState()["keys"].values.first?["led"]["last_written"]["color"].text == "green" }
        try await waitFor("scene selection initialization", home: home) { home.scenes.first { $0.id == "reading" }?.raw["selected"] == .bool(false) }
        await home.activateKeypad("panel", key: 1)
        try #require(home.error == nil, "\(home.error ?? "")")
        try await waitFor("mapped scene light feedback", home: home) { home.devices.first { $0.id == "fixture-light" }?.raw["observed"]["level"] == .integer(100) }
        #expect(panelState()["keys"].values.first?["led"]["last_written"]["color"].text == "green", "Mapped scene action is not an LED toggle")
        try await waitFor("scene selected after mapped first click", home: home) {
            home.scenes.first { $0.id == "reading" }?.raw["selected"] == .bool(true)
        }
        try #require(home.canActivateKeypad("panel", key: 1), "An already selected scene must remain actionable for deactivation")
        await home.activateKeypad("panel", key: 1)
        try #require(home.error == nil, "Second mapped click: \(home.error ?? "")")
        try await waitFor("mapped scene off after second click", home: home) {
            home.devices.first { $0.id == "fixture-light" }?.raw["observed"]["level"] == .integer(0)
                && home.scenes.first { $0.id == "reading" }?.raw["selected"] == .bool(false)
        }
        #expect(!home.busy && home.canActivateKeypad("panel", key: 1))
        let trace = try home.diagnostics.exportText().split(separator: "\n").dropFirst().map {
            try JSONDecoder().decode(HomeJSON.self, from: Data($0.utf8))
        }
        let sends = trace.filter { $0["event"].text == "http.send" && $0["path"].text == "/api/v1/scene-actions" }
        try #require(sends.count == 2)
        #expect(sends[0]["operation_id"] != sends[1]["operation_id"] && sends[0]["request_id"] != sends[1]["request_id"])
        let offOperation = sends[1]["operation_id"]
        #expect(trace.contains { $0["event"].text == "scene.decision" && $0["operation_id"] == offOperation && $0["on"] == .bool(false) })
        #expect(trace.contains { $0["event"].text == "http.response" && $0["operation_id"] == offOperation && $0["request_id"] == sends[1]["request_id"] && $0["http_status"] == .integer(202) && $0["outcome"].text == "accepted" })
        #expect(trace.contains { $0["event"].text == "operation.complete" && $0["operation_id"] == offOperation })


        await home.writeKeypadLED("panel", key: 1, color: .off)
        try await waitFor("LED off", home: home) { panelState()["keys"].values.first?["led"]["last_written"]["color"].text == "off" }
        await home.writeKeypadSetting("panel", command: .object(["action": .string("backlight"), "mode": .string("bright")]))
        try await waitFor("backlight written record", home: home) { panelState()["settings"]["backlight"]["last_written"]["value"].text == "bright" }
        #expect(panelState()["settings"]["backlight"]["observed"] == .null)
        #expect(try HomeKeypad(record: #require(home.devices.first { $0.id == "panel" })).keys.allSatisfy { $0.led.actual == .unknown })
        var renamed = try #require(home.editableKeypads.first).raw
        renamed["name"] = .string("改名后的面板")
        try home.upsertKeypad(renamed, originalID: "panel")
        let draft = home.deviceDocument
        home.selectInterfaceMode(.management); home.selectInterfaceMode(.daily)
        #expect(home.deviceDocument == draft && home.draftChanged && home.receivingPush)
        await home.saveDraft(); try #require(home.error == nil, "\(home.error ?? "")")
        await home.previewDraft(); try #require(home.error == nil, "\(home.error ?? "")")
        #expect(home.preview != nil && panelState()["name"].text != "改名后的面板")
        #expect(home.editableKeypads.first?.raw["keys"].values.first?["led_endpoint"]["id"].text == "legacy-key-1")
    }
    private func waitFor(_ stage: String, home: HomeServerStore, _ predicate: () -> Bool) async throws {
        for _ in 0..<100 {
            if predicate() { return }
            try await Task.sleep(for: .milliseconds(100))
        }
        throw HomeServerError.server("Timed out: \(stage); \(home.message); \(home.error ?? ""); devices=\(HomeJSON.array(home.devices.map(\.raw)).formatted.prefix(8000)); scenes=\(HomeJSON.array(home.scenes.map(\.raw)).formatted.prefix(3000))")
    }
}
#endif
