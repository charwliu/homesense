#if os(macOS)
import Foundation
import Darwin
import Security
import Testing
@testable import HomeSenseCore

/// Trust is scoped to this synthetic server certificate, not installed globally.
struct FixtureHTTPS: HomeHTTPTransport {
    let certificate: String
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        try await Task.detached {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/curl")
            var arguments = ["--silent", "--show-error", "--max-time", "4", "--cacert", certificate,
                             "--request", request.httpMethod ?? "GET", "--write-out", "\n%{http_code}"]
            for (name, value) in request.allHTTPHeaderFields ?? [:] { arguments += ["--header", "\(name): \(value)"] }
            if request.httpBody != nil { arguments += ["--data-binary", "@-"] }
            arguments.append(request.url!.absoluteString)
            process.arguments = arguments
            let output = Pipe(), input = Pipe(), errors = Pipe()
            process.standardOutput = output; process.standardError = errors; process.standardInput = input
            try process.run()
            if let body = request.httpBody { input.fileHandleForWriting.write(body) }
            try input.fileHandleForWriting.close()
            let received = output.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            guard process.terminationStatus == 0, let text = String(data: received, encoding: .utf8),
                  let split = text.lastIndex(of: "\n"), let status = Int(text[text.index(after: split)...]) else {
                throw HomeServerError.server("Synthetic HTTPS request failed")
            }
            return HomeHTTPResponse(data: Data(text[..<split].utf8), status: status)
        }.value
    }
}

/// Only this generated loopback fixture CA is trusted; production keeps system trust.
final class FixtureWebSocketTrust: NSObject, URLSessionDelegate, @unchecked Sendable {
    let certificate: SecCertificate
    init(pem: String) throws {
        let body = pem.components(separatedBy: .newlines).filter { !$0.hasPrefix("---") }.joined()
        guard let bytes = Data(base64Encoded: body), let certificate = SecCertificateCreateWithData(nil, bytes as CFData) else {
            throw HomeServerError.invalidDocument
        }
        self.certificate = certificate
    }
    func urlSession(_ session: URLSession, didReceive challenge: URLAuthenticationChallenge,
                    completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              challenge.protectionSpace.host == "localhost", let trust = challenge.protectionSpace.serverTrust else {
            completionHandler(.performDefaultHandling, nil); return
        }
        SecTrustSetAnchorCertificates(trust, [certificate] as CFArray)
        SecTrustSetAnchorCertificatesOnly(trust, true)
        SecTrustSetPolicies(trust, SecPolicyCreateSSL(true, "localhost" as CFString))
        if SecTrustEvaluateWithError(trust, nil) { completionHandler(.useCredential, URLCredential(trust: trust)) }
        else { completionHandler(.cancelAuthenticationChallenge, nil) }
    }
}

@MainActor
struct HomeServerIntegrationTests {
    @Test(.enabled(if: ProcessInfo.processInfo.environment["HOMESENSE_TEST_BINARY"] != nil,
                   "Set HOMESENSE_TEST_BINARY to run the isolated Rust HTTPS server"))
    func nativeManagementAgainstActualRustHTTPSAPI() async throws {
        let binary = try #require(ProcessInfo.processInfo.environment["HOMESENSE_TEST_BINARY"])
        let project = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let repository = project.deletingLastPathComponent().deletingLastPathComponent()
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("homesense-native-\(UUID())")
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
        let openssl = Process()
        openssl.executableURL = URL(fileURLWithPath: "/usr/bin/openssl")
        openssl.arguments = ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=localhost",
                             "-config", root.appendingPathComponent("tls.cnf").path, "-extensions", "v3",
                             "-keyout", root.appendingPathComponent("key.pem").path,
                             "-out", root.appendingPathComponent("cert.pem").path]
        openssl.standardOutput = FileHandle.nullDevice; openssl.standardError = FileHandle.nullDevice
        try openssl.run(); openssl.waitUntilExit()
        #expect(openssl.terminationStatus == 0)
        let token = "synthetic-native-test-token-only"
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
        let port = UInt16(bigEndian: address.sin_port)
        Darwin.close(socket)
        let origin = "https://localhost:\(port)"
        let logURL = root.appendingPathComponent("server.log")
        FileManager.default.createFile(atPath: logURL.path, contents: nil)
        let log = try FileHandle(forWritingTo: logURL)
        defer { try? log.close() }
        let server = Process()
        server.executableURL = URL(fileURLWithPath: binary)
        server.arguments = ["serve-web-tls-simulation", repository.appendingPathComponent("config/simulation.json").path,
            repository.appendingPathComponent("config/automation.simulation.json").path, root.appendingPathComponent("state").path,
            "60", "127.0.0.1:\(port)", root.appendingPathComponent("token").path, origin,
            root.appendingPathComponent("cert.pem").path, root.appendingPathComponent("key.pem").path, "--administrator"]
        server.standardOutput = log; server.standardError = log
        try server.run()
        defer { if server.isRunning { server.terminate(); server.waitUntilExit() } }
        let https = FixtureHTTPS(certificate: root.appendingPathComponent("cert.pem").path)
        let trust = try FixtureWebSocketTrust(pem: String(contentsOf: root.appendingPathComponent("cert.pem"), encoding: .utf8))
        let home = HomeServerStore(transport: https, eventTransport: HomeWebSocketTransport(delegate: trust), defaults: nil, credentialStore: InMemoryCredentials())
        home.pause(); home.endpointText = origin
        for _ in 0..<30 {
            await home.connect(token: token)
            if home.connected { break }
            guard server.isRunning else {
                throw HomeServerError.server((try? String(contentsOf: logURL, encoding: .utf8)) ?? "Synthetic server exited")
            }
            try await Task.sleep(for: .milliseconds(100))
        }
        defer { home.disconnect() }
        #expect(home.connected && home.canManage)
        #expect(home.error == nil, "\(home.error ?? "")")
        #expect(home.devices.first?.id == "simulation:light")
        try home.addFloor("空楼层")
        try home.upsertRoom(id: "native-study", name: "原生客户端测试房间", floor: "测试楼层")
        let existingScene = try #require(home.editableScenes.first { $0.id == "simulation-evening" })
        let originalProgram = existingScene.raw.removing(["name", "collection", "room", "scope"])
        #expect(home.unassignedScenes.contains { $0.id == existingScene.id })
        try home.upsertScene(existingScene.raw, collection: "immediate_scenes", name: "房间内原有场景", originalID: existingScene.id, room: "native-study")
        var device = try #require(home.editableDevices.first).raw
        device["room"] = .string("native-study")
        try home.upsertDevice(device, originalID: "simulation:light")
        let scene: HomeJSON = .object([
            "id": .string("native-scene"), "enabled": .bool(false), "review_evidence": .string("Synthetic native API fixture only"),
            "activate": .array([.object(["source": .string("native:test"), "action": .object([
                "kind": .string("command"), "device": .string("simulation:light"),
                "command": .object(["kind": .string("level"), "value": .integer(25), "ramp": .bool(true)])
            ])])])
        ])
        try home.upsertScene(scene, collection: "immediate_scenes", name: "原生测试场景", room: "native-study")
        await home.saveDraft()
        #expect(home.error == nil, "\(home.error ?? "")")
        #expect(home.draftID != nil)
        await home.previewDraft()
        #expect(home.error == nil, "\(home.error ?? "")")
        #expect(home.preview != nil)
        await home.publishDraft()
        #expect(home.error == nil, "\(home.error ?? "")")
        #expect(home.revision == "2")
        #expect(home.editableRooms.first?.name == "原生客户端测试房间")
        #expect(home.editableDevices.first?.raw["room"].text == "native-study")
        #expect(home.editableScenes.contains { $0.id == "native-scene" && $0.name == "原生测试场景" })
        #expect(home.editableScenes.first { $0.id == existingScene.id }?.raw.removing(["name", "collection", "room", "scope"]) == originalProgram)
        #expect(home.editableScenes.first { $0.id == "native-scene" }?.roomID == "native-study")
        #expect(home.scenes(inRoom: "native-study").contains { $0.id == existingScene.id })
        #expect(!home.unassignedScenes.contains { $0.id == existingScene.id })
        let roomDTO = try #require(home.scenes.first { $0.id == existingScene.id }).raw["room"]
        if home.scenePlacementSupported { #expect(home.scenes.first { $0.id == existingScene.id }?.raw["scope"].text == "room") }
        #expect(roomDTO["id"].text == "native-study" && roomDTO["name"].text == "原生客户端测试房间" && roomDTO["floor"].text == "测试楼层")
        #expect(home.serverFloors.contains("空楼层"))
        home.resume()
        for _ in 0..<60 {
            if home.receivingPush { break }
            try await Task.sleep(for: .milliseconds(100))
        }
        #expect(home.receivingPush, "Real URLSession WebSocket handshake and full snapshot")
        #expect(home.scenes.first { $0.id == existingScene.id }?.raw["room"] == roomDTO)
        let endpoint = try HomeServerEndpoint(origin)
        for level: Int64 in [40, 17] {
            // A second client writes; the App must receive feedback without HTTP refresh.
            let request = try endpoint.request(path: "/api/v1/device-commands", method: "POST", token: token, body: .object([
                "device": .string("simulation:light"), "request_id": .string(UUID().uuidString),
                "command": .object(["action": .string("brightness"), "level": .integer(level)])
            ]))
            let accepted = try await https.send(request)
            #expect(accepted.status == 202)
            for _ in 0..<60 {
                if home.devices.first?.raw["observed"]["level"] == .integer(level) { break }
                try await Task.sleep(for: .milliseconds(100))
            }
            #expect(home.devices.first?.raw["observed"]["level"] == .integer(level))
            #expect(home.receivingPush && home.controlsAvailable && !home.busy)
            #expect(home.scenes(inRoom: "native-study").contains { $0.id == existingScene.id })
        }
        try await Task.sleep(for: .seconds(11))
        #expect(home.receivingPush && home.controlsAvailable, "Pong liveness does not age out an idle healthy stream")
    }
}
#endif
