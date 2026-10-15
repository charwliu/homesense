import Foundation
import Network
import Testing
@testable import HomeSenseCore

struct LHIO1600ProtocolTests {
    @Test func manualRegisterFramesAndCRC() throws {
        #expect(try LHIO1600Request.read(target: "1", isolated: false).hex == "01 03 03 EA 00 01 A5 BA")
        #expect(try LHIO1600Request.write(target: "1", newAddress: "10", isolated: true).hex == "01 06 03 EA 00 0A 28 7D")
        #expect(try LHIO1600Request.read(target: "10", isolated: false).hex == "0A 03 03 EA 00 01 A4 C1")
        // Independent manufacturer example confirms CRC byte order and polynomial.
        #expect(LHIO1600Request.withCRC([0xFE, 5, 0, 0, 0xFF, 0]).hex == "FE 05 00 00 FF 00 98 35")
    }

    @Test func decimalInputsIsolationAndReservedTargets() throws {
        for input in ["", "0x10", "FF", "256", "-1", "1.0", "１"] {
            #expect(throws: LHIO1600Failure.invalidTarget) { try LHIO1600Request.read(target: input, isolated: true) }
        }
        for input in ["0", "254"] {
            #expect(throws: LHIO1600Failure.isolationRequired) { try LHIO1600Request.read(target: input, isolated: false) }
            #expect(try LHIO1600Request.read(target: input, isolated: true).target == UInt8(input))
        }
        #expect(throws: LHIO1600Failure.isolationRequired) {
            try LHIO1600Request.write(target: "1", newAddress: "10", isolated: false)
        }
        for next in ["0", "248", "254", "255", "0A", "-1"] {
            #expect(throws: LHIO1600Failure.invalidNewAddress) {
                try LHIO1600Request.write(target: "1", newAddress: next, isolated: true)
            }
        }
        #expect(throws: LHIO1600Failure.sameAddress) {
            try LHIO1600Request.write(target: "1", newAddress: "1", isolated: true)
        }
    }

    @Test func readFiltersCRCUnitCountAndRequestEcho() throws {
        let request = try LHIO1600Request.read(target: "1", isolated: false)
        let valid = LHIO1600Request.withCRC([1, 3, 2, 0, 10])
        #expect(try request.reply(in: Data([0x99]) + request.data + valid) == .address(10))
        for bytes in [request.data, Data(valid.dropLast()), LHIO1600Request.withCRC([2, 3, 2, 0, 10]),
                      LHIO1600Request.withCRC([1, 3, 2, 1, 10]), LHIO1600Request.withCRC([1, 3, 1, 0, 10]),
                      Data([1, 3, 2, 0, 10, 0, 0])] {
            #expect(throws: LHIO1600Failure.invalidReply) { try request.reply(in: bytes) }
        }
        #expect(throws: LHIO1600Failure.noReply) { try request.reply(in: Data()) }
        #expect(throws: LHIO1600Failure.ambiguousReply) { try request.reply(in: valid + valid) }
        #expect(throws: LHIO1600Failure.exception(2)) {
            try request.reply(in: LHIO1600Request.withCRC([1, 0x83, 2]))
        }
    }

    @Test func writeEchoIsDistinctFromVerificationAndChecksRegisterValue() throws {
        let request = try LHIO1600Request.write(target: "1", newAddress: "10", isolated: true)
        #expect(try request.reply(in: request.data) == .writeEcho)
        #expect(try request.reply(in: LHIO1600Request.withCRC([10, 6, 3, 0xEA, 0, 10])) == .writeEcho)
        for payload: [UInt8] in [[1, 6, 3, 0xEB, 0, 10], [1, 6, 3, 0xEA, 0, 11], [2, 6, 3, 0xEA, 0, 10]] {
            #expect(throws: LHIO1600Failure.invalidReply) { try request.reply(in: LHIO1600Request.withCRC(payload)) }
        }
        #expect(throws: LHIO1600Failure.exception(3)) {
            try request.reply(in: LHIO1600Request.withCRC([1, 0x86, 3]))
        }
        let unknown = try LHIO1600Request.read(target: "254", isolated: true)
        #expect(try unknown.reply(in: LHIO1600Request.withCRC([254, 3, 2, 0, 0])) == .address(0))
        #expect(try unknown.reply(in: LHIO1600Request.withCRC([0, 3, 2, 0, 0])) == .address(0))
    }
}

@MainActor
private final class RelayGateway {
    let listener: NWListener
    var connections: [NWConnection] = []
    var buffer = Data()
    var requests: [Data] = []
    var address: UInt8 = 1
    var reply = true
    var writeEchoOnly = false
    var corruptCRC = false
    var ready = false
    var failure: NWError?
    init() throws {
        let params = NWParameters.tcp
        params.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        listener = try NWListener(using: params)
        listener.stateUpdateHandler = { [weak self] state in
            Task { @MainActor [weak self] in
                if case .ready = state { self?.ready = true }
                if case .failed(let error) = state { self?.failure = error }
            }
        }
        listener.newConnectionHandler = { [weak self] connection in
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.connections.append(connection)
                connection.start(queue: .main)
                self.receive(connection)
            }
        }
        listener.start(queue: .main)
    }
    func stop() { connections.forEach { $0.cancel() }; listener.cancel() }
    func receive(_ connection: NWConnection) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 1024) { [weak self] data, _, done, error in
            Task { @MainActor [weak self] in
                guard let self else { return }
                if let data { self.buffer.append(data) }
                while self.buffer.count >= 8 {
                    let request = Data(self.buffer.prefix(8)); self.buffer.removeFirst(8)
                    self.requests.append(request)
                    let b = Array(request)
                    guard self.reply, b[0] == self.address || b[0] == 254,
                          LHIO1600Request.withCRC(Array(b.prefix(6))) == request else { continue }
                    var response: Data
                    if b[1] == 6 {
                        if !self.writeEchoOnly { self.address = b[5] }
                        response = request
                    } else {
                        response = LHIO1600Request.withCRC([b[0], 3, 2, 0, self.address])
                    }
                    if self.corruptCRC { response[response.count - 1] ^= 1 }
                    // Deliberately fragment replies across TCP receive calls.
                    connection.send(content: response.prefix(2), completion: .contentProcessed { _ in })
                    try? await Task.sleep(for: .milliseconds(25))
                    connection.send(content: response.dropFirst(2), completion: .contentProcessed { _ in })
                }
                if !done && error == nil { self.receive(connection) }
            }
        }
    }
}

private enum RelayTestFailure: Error { case timedOut }
@MainActor
private func relayUntil(_ predicate: () -> Bool) async throws {
    for _ in 0..<220 {
        if predicate() { return }
        try await Task.sleep(for: .milliseconds(25))
    }
    throw RelayTestFailure.timedOut
}

@Suite(.serialized)
@MainActor
struct LHIO1600TransportTests {
    private func connect(_ gateway: RelayGateway) async throws -> CommissioningSession {
        try await relayUntil { gateway.ready || gateway.failure != nil }
        #expect(gateway.failure == nil)
        let session = CommissioningSession(defaults: nil)
        session.host = "127.0.0.1"; session.port = "\(try #require(gateway.listener.port).rawValue)"
        session.connect()
        try await relayUntil { session.status == .connected }
        #expect(gateway.requests.isEmpty)
        return session
    }

    @Test func readChangeAndExplicitNewAddressVerification() async throws {
        let gateway = try RelayGateway(); defer { gateway.stop() }
        let session = try await connect(gateway); defer { session.disconnect() }
        session.readLHAddress()
        session.send(.firmware, label: "busy must reject TS6")
        try await relayUntil { !session.busy }
        #expect(session.lhQueriedAddress == 1)
        #expect(session.queriedAddress == nil)
        #expect(gateway.requests.count == 1)
        session.changeLHAddress() // Requires isolation, no write.
        #expect(gateway.requests.count == 1)
        session.isolatedRelay = true
        session.changeLHAddress()
        try await relayUntil { !session.busy }
        #expect(gateway.address == 10)
        #expect(session.lhWrittenAddress == 10)
        #expect(session.lhVerifiedAddress == nil) // Echo is not a confirmation.
        #expect(session.lhAddress == "1")
        #expect(!session.isolatedRelay)
        session.lhNewAddress = "11" // Verification uses the actually written target.
        session.verifyLHAddress()
        try await relayUntil { !session.busy }
        #expect(session.lhQueriedAddress == 10)
        #expect(session.lhVerifiedAddress == 10)
        #expect(gateway.requests.map(\.hex) == ["01 03 03 EA 00 01 A5 BA", "01 06 03 EA 00 0A 28 7D", "0A 03 03 EA 00 01 A4 C1"])
        session.useLHAddress(10)
        #expect(session.lhAddress == "10")
        #expect(session.address == "10") // Original TS6 HEX target unchanged.
        session.disconnect()
        #expect(session.lhVerifiedAddress == nil && session.lhWrittenAddress == nil && session.lhQueriedAddress == nil)
    }

    @Test func zeroAddressCanBeReadAndChangedWhenIsolated() async throws {
        let gateway = try RelayGateway(); gateway.address = 0; defer { gateway.stop() }
        let session = try await connect(gateway); defer { session.disconnect() }
        session.readLHAddress(unknown: true)
        #expect(session.error?.contains("隔离") == true)
        #expect(gateway.requests.isEmpty)
        session.isolatedRelay = true
        session.readLHAddress(unknown: true)
        try await relayUntil { !session.busy }
        #expect(session.lhQueriedAddress == 0)
        session.useLHAddress(0)
        session.changeLHAddress()
        try await relayUntil { !session.busy }
        #expect(gateway.address == 10)
        session.verifyLHAddress()
        try await relayUntil { !session.busy }
        #expect(session.lhVerifiedAddress == 10)
    }

    @Test func badCRCAndWriteEchoNeverConfirmAnAddress() async throws {
        let gateway = try RelayGateway(); gateway.corruptCRC = true; defer { gateway.stop() }
        let session = try await connect(gateway); defer { session.disconnect() }
        session.readLHAddress()
        try await relayUntil { !session.busy }
        #expect(session.lhQueriedAddress == nil)
        #expect(session.error?.contains("CRC") == true)
        gateway.corruptCRC = false; gateway.writeEchoOnly = true
        session.isolatedRelay = true; session.changeLHAddress()
        try await relayUntil { !session.busy }
        #expect(session.lhVerifiedAddress == nil)
        session.verifyLHAddress()
        try await relayUntil { !session.busy }
        #expect(session.lhVerifiedAddress == nil)
        #expect(session.error?.contains("未收到回复") == true)
        #expect(gateway.requests.count == 3)
    }

    @Test func disconnectCancelsAndReconnectNeverReplays() async throws {
        let gateway = try RelayGateway(); gateway.reply = false; defer { gateway.stop() }
        let session = try await connect(gateway)
        session.readLHAddress()
        try await relayUntil { gateway.requests.count == 1 }
        session.disconnect()
        try await relayUntil { !session.busy }
        #expect(session.lhQueriedAddress == nil)
        session.connect(); defer { session.disconnect() }
        try await relayUntil { session.status == .connected }
        try await Task.sleep(for: .milliseconds(200))
        #expect(gateway.requests.count == 1)
        #expect(session.lhVerifiedAddress == nil)
    }
}
