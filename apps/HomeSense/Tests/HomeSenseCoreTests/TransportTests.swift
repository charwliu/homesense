import Foundation
import Network
import Testing
@testable import HomeSenseCore

@MainActor
private final class TestGateway {
    let listener: NWListener
    var connections: [NWConnection] = []
    var frames: [Frame] = []
    var decoder = FrameDecoder()
    var address: UInt8 = 0x10
    var ready = false
    var respond = true
    var error: NWError?
    init() throws {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        listener = try NWListener(using: parameters)
        listener.stateUpdateHandler = { [weak self] state in
            Task { @MainActor [weak self] in
                if case .ready = state { self?.ready = true }
                if case .failed(let error) = state { self?.error = error }
            }
        }
        listener.newConnectionHandler = { [weak self] connection in
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.connections.append(connection)
                connection.start(queue: .main)
                self.read(connection)
            }
        }
        listener.start(queue: .main)
    }
    func stop() { connections.forEach { $0.cancel() }; listener.cancel() }
    func emit(_ frames: [Frame]) throws {
        let connection = try #require(connections.last)
        let data = frames.reduce(into: Data()) { $0.append($1.data) }
        connection.send(content: data, completion: .contentProcessed { _ in })
    }
    func emitSplit(_ frame: Frame) async throws {
        let connection = try #require(connections.last)
        connection.send(content: frame.data.prefix(2), completion: .contentProcessed { _ in })
        try await Task.sleep(for: .milliseconds(30))
        connection.send(content: frame.data.dropFirst(2), completion: .contentProcessed { _ in })
    }
    func read(_ connection: NWConnection) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 4096) { [weak self] data, _, complete, error in
            Task { @MainActor [weak self] in
                guard let self else { return }
                if let data {
                    for frame in self.decoder.feed(data) {
                        self.frames.append(frame)
                        guard self.respond, frame.address == 0 || frame.address == self.address else { continue }
                        if frame.command == 0xC3 {
                            connection.send(content: Data([self.address]), completion: .contentProcessed { _ in })
                        } else if frame.command == 0xCC {
                            connection.send(content: Data("TS6-86/B\r\n".utf8), completion: .contentProcessed { _ in })
                            try? await Task.sleep(for: .milliseconds(40))
                            connection.send(content: Data("V4.3\r\n".utf8), completion: .contentProcessed { _ in })
                        } else if frame.command == 0xC4 {
                            self.address = frame.value
                        }
                    }
                }
                if !complete && error == nil { self.read(connection) }
            }
        }
    }
}

private enum TestFailure: Error { case timeout }
@MainActor
private func until(_ predicate: () -> Bool) async throws {
    for _ in 0..<200 {
        if predicate() { return }
        try await Task.sleep(for: .milliseconds(25))
    }
    throw TestFailure.timeout
}

@Suite(.serialized)
@MainActor
struct TransportTests {
    @Test func endpointValidationAndOfflineAddressGuard() throws {
        let client = TCPClient()
        for (host, port) in [("", "12"), ("localhost", "0"), ("localhost", "65536"), ("bad host", "80")] {
            #expect(throws: TransportFailure.self) { try client.connect(host: host, port: port) }
        }
        let session = CommissioningSession(defaults: nil)
        session.changeAddress()
        #expect(session.error?.contains("按键①") == true)
        session.queryUnknownAddress()
        #expect(session.error?.contains("一块") == true)
        #expect(session.logs.allSatisfy { $0.kind != .tx })
    }

    @Test func realTCPReadWriteAndReadbackWorkflow() async throws {
        let gateway = try TestGateway()
        defer { gateway.stop() }
        try await until { gateway.ready || gateway.error != nil }
        #expect(gateway.error == nil)
        let port = try #require(gateway.listener.port)
        let session = CommissioningSession(defaults: nil)
        session.host = "127.0.0.1"; session.port = "\(port.rawValue)"
        session.connect()
        defer { session.disconnect() }
        try await until { session.status == .connected }
        #expect(gateway.frames.isEmpty) // Connecting never sends commands.
        session.isolatedPanel = true
        session.queryUnknownAddress()
        // Queue remains occupied throughout the reply window.
        #expect(session.busy)
        session.send(.firmware, label: "must be rejected")
        try await until { !session.busy }
        #expect(session.queriedAddress?.description == "10")
        #expect(gateway.frames.count == 1)
        session.useAddress(0x10)
        session.send(.firmware, label: "firmware")
        try await until { !session.busy }
        #expect(session.lastReply.contains("TS6-86/B"))
        #expect(session.lastReply.contains("V4.3"))

        session.newAddress = "41"; session.holdingKeyOne = true
        session.changeAddress()
        try await until { !session.busy }
        #expect(session.writtenAddress?.description == "41")
        #expect(session.address == "10") // Hardware write does not silently change local target.
        #expect(!session.holdingKeyOne)
        #expect(session.notice.contains("尚未确认"))
        session.queryWrittenAddress()
        try await until { !session.busy }
        #expect(session.queriedAddress?.description == "41")
        #expect(gateway.frames.map(\.hex) == ["AE 00 C3 00 EE", "AE 10 CC 00 EE", "AE 10 C4 41 EE", "AE 41 C3 00 EE"])
        session.disconnect()
        #expect(session.queriedAddress == nil)
        #expect(session.observedAddresses.isEmpty)
    }

    @Test func disconnectCancelsReadAndNeverReplays() async throws {
        let gateway = try TestGateway()
        gateway.respond = false
        defer { gateway.stop() }
        try await until { gateway.ready }
        let session = CommissioningSession(defaults: nil)
        session.host = "127.0.0.1"
        session.port = "\(try #require(gateway.listener.port).rawValue)"
        session.connect()
        try await until { session.status == .connected }
        session.send(.readAddress, label: "read", expectsReply: true)
        try await until { gateway.frames.count == 1 }
        session.disconnect()
        try await until { !session.busy }
        #expect(session.queriedAddress == nil)
        session.connect()
        try await until { session.status == .connected }
        try await Task.sleep(for: .milliseconds(200))
        #expect(gateway.frames.count == 1)
        session.disconnect()
    }

    @Test func noResponseDoesNotFabricateAddress() async throws {
        let gateway = try TestGateway()
        gateway.respond = false
        defer { gateway.stop() }
        try await until { gateway.ready }
        let session = CommissioningSession(defaults: nil)
        session.host = "127.0.0.1"; session.port = "\(try #require(gateway.listener.port).rawValue)"
        session.connect()
        defer { session.disconnect() }
        try await until { session.status == .connected }
        session.send(.readAddress, label: "read", expectsReply: true)
        try await until { !session.busy }
        #expect(session.queriedAddress == nil)
        #expect(session.notice.contains("未收到回复"))
        #expect(gateway.frames.count == 1)
    }

    @Test func zeroReadbackAndIsolatedAddressChange() async throws {
        let gateway = try TestGateway()
        gateway.address = 0
        defer { gateway.stop() }
        try await until { gateway.ready }
        let session = CommissioningSession(defaults: nil)
        session.host = "127.0.0.1"; session.port = "\(try #require(gateway.listener.port).rawValue)"
        session.connect()
        defer { session.disconnect() }
        try await until { session.status == .connected }
        session.isolatedPanel = true
        session.queryUnknownAddress()
        try await until { !session.busy }
        #expect(session.queriedAddress?.description == "00")
        session.useAddress(0)
        #expect(session.address == "00")
        session.newAddress = "43"; session.holdingKeyOne = true
        session.isolatedPanel = false
        session.changeAddress()
        #expect(session.error?.contains("一块") == true)
        #expect(gateway.frames.count == 1)
        session.isolatedPanel = true
        session.changeAddress()
        try await until { !session.busy }
        #expect(session.writtenAddress?.description == "43")
        #expect(session.notice.contains("尚未确认"))
        session.queryWrittenAddress()
        try await until { !session.busy }
        #expect(session.queriedAddress?.description == "43")
        #expect(gateway.frames.map(\.hex) == ["AE 00 C3 00 EE", "AE 00 C4 43 EE", "AE 43 C3 00 EE"])
    }

    @Test func physicalPressTogglesOnlyItsOwnLEDAndReleaseDoesNotToggle() async throws {
        let gateway = try TestGateway()
        defer { gateway.stop() }
        try await until { gateway.ready }
        let session = CommissioningSession(defaults: nil)
        session.host = "127.0.0.1"; session.port = "\(try #require(gateway.listener.port).rawValue)"
        session.connect()
        defer { session.disconnect() }
        try await until { session.status == .connected }
        session.setKeyLEDLinkEnabled(true)
        try await until { !session.busy }
        #expect(session.keyLEDLinkEnabled)
        #expect(gateway.frames.map(\.hex) == ["AE 10 C8 02 EE"] + (1...6).map { "AE 10 0\($0) F2 EE" })
        #expect(session.lastWrittenLEDColors.count == 6)

        try await gateway.emitSplit(Frame(address: 0x10, command: 0xD1, value: 0xF1))
        try await until { gateway.frames.count == 8 && !session.busy }
        #expect(gateway.frames.last?.hex == "AE 10 01 F1 EE")
        #expect(session.lastWrittenLEDColors[1] == .red)
        try gateway.emit([
            Frame(address: 0x10, command: 0xD1, value: 0xF1), // duplicate press
            Frame(address: 0x11, command: 0xD1, value: 0xF1), // different module
            Frame(address: 0x10, command: 0xD7, value: 0xF1), // invalid key
            Frame(address: 0x10, command: 0xD2, value: 0xFF), // invalid value
            Frame(address: 0x10, command: 0x01, value: 0xF1), // command echo
            Frame(address: 0x10, command: 0xD1, value: 0xF2) // release
        ])
        try await until { session.keyEvents[1]?.pressed == false }
        try await Task.sleep(for: .milliseconds(200))
        #expect(gateway.frames.count == 8)
        #expect(session.lastWrittenLEDColors[1] == .red)

        try gateway.emit([Frame(address: 0x10, command: 0xD1, value: 0xF1)])
        try await until { gateway.frames.count == 9 && !session.busy }
        #expect(gateway.frames.last?.hex == "AE 10 01 F2 EE")
        try gateway.emit([Frame(address: 0x10, command: 0xD2, value: 0xF1)])
        try await until { gateway.frames.count == 10 && !session.busy }
        #expect(gateway.frames.last?.hex == "AE 10 02 F1 EE")
        #expect(session.lastWrittenLEDColors[1] == .green)
        #expect(session.lastWrittenLEDColors[2] == .red)

        // Explicit manual control stops automatic linking.
        session.send(.led(key: 2, color: .green), label: "manual")
        try await until { gateway.frames.count == 11 && !session.busy }
        #expect(!session.keyLEDLinkEnabled)
        #expect(gateway.frames.last?.hex == "AE 10 02 F2 EE")
    }

    @Test func querySerializesPendingPressesAndPreservesFinalToggleParity() async throws {
        let gateway = try TestGateway()
        defer { gateway.stop() }
        try await until { gateway.ready }
        let session = CommissioningSession(defaults: nil)
        session.host = "127.0.0.1"; session.port = "\(try #require(gateway.listener.port).rawValue)"
        session.connect()
        defer { session.disconnect() }
        try await until { session.status == .connected }
        session.setKeyLEDLinkEnabled(true)
        try await until { !session.busy }
        session.send(.readAddress, label: "read")
        try await until { gateway.frames.count == 8 }
        try gateway.emit([
            Frame(address: 0x10, command: 0xD1, value: 0xF1),
            Frame(address: 0x10, command: 0xD1, value: 0xF2),
            Frame(address: 0x10, command: 0xD1, value: 0xF1),
            Frame(address: 0x10, command: 0xD2, value: 0xF1)
        ])
        try await until { session.keyEvents[2]?.pressed == true }
        #expect(session.busy)
        #expect(gateway.frames.count == 8) // no LED write inside the read window
        try await until { gateway.frames.count == 10 && !session.busy }
        #expect(gateway.frames.suffix(2).map(\.hex) == ["AE 10 01 F2 EE", "AE 10 02 F1 EE"])
        #expect(session.queriedAddress == nil) // mixed key frames cannot become an address reply
        #expect(session.lastWrittenLEDColors[1] == .green)
        #expect(session.lastWrittenLEDColors[2] == .red)
    }

    @Test func disablingOrChangingTargetDropsQueuedLEDWrites() async throws {
        let gateway = try TestGateway()
        defer { gateway.stop() }
        try await until { gateway.ready }
        let session = CommissioningSession(defaults: nil)
        session.host = "127.0.0.1"; session.port = "\(try #require(gateway.listener.port).rawValue)"
        session.connect()
        defer { session.disconnect() }
        try await until { session.status == .connected }
        session.setKeyLEDLinkEnabled(true)
        try await until { !session.busy }
        session.send(.readAddress, label: "read")
        try await until { gateway.frames.count == 8 }
        try gateway.emit([Frame(address: 0x10, command: 0xD1, value: 0xF1)])
        try await until { session.keyEvents[1]?.pressed == true }
        session.setKeyLEDLinkEnabled(false)
        try await until { !session.busy }
        #expect(gateway.frames.count == 8)
        #expect(!session.keyLEDLinkEnabled)

        session.setKeyLEDLinkEnabled(true)
        try await until { !session.busy }
        #expect(gateway.frames.count == 15)
        session.address = "11"
        #expect(!session.keyLEDLinkEnabled)
        #expect(session.lastWrittenLEDColors.isEmpty)
        try gateway.emit([Frame(address: 0x10, command: 0xD1, value: 0xF1)])
        try await Task.sleep(for: .milliseconds(200))
        #expect(gateway.frames.count == 15)
    }

    @Test func disconnectDuringLinkInitializationNeverReplaysOnReconnect() async throws {
        let gateway = try TestGateway()
        defer { gateway.stop() }
        try await until { gateway.ready }
        let session = CommissioningSession(defaults: nil)
        session.host = "127.0.0.1"; session.port = "\(try #require(gateway.listener.port).rawValue)"
        session.connect()
        try await until { session.status == .connected }
        session.setKeyLEDLinkEnabled(true)
        try await until { gateway.frames.count >= 1 }
        session.disconnect()
        try await until { !session.busy }
        let count = gateway.frames.count
        #expect(count < 7)
        #expect(!session.keyLEDLinkEnabled)
        #expect(session.lastWrittenLEDColors.isEmpty)
        session.connect()
        defer { session.disconnect() }
        try await until { session.status == .connected }
        try gateway.emit([Frame(address: 0x10, command: 0xD1, value: 0xF1)])
        try await until { session.keyEvents[1]?.pressed == true }
        try await Task.sleep(for: .milliseconds(250))
        #expect(gateway.frames.count == count)
    }
}
