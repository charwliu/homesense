import Foundation
import Testing
@testable import HomeSenseCore

struct ProtocolTests {
    @Test(arguments: ["00", "0", "1", "100", "GG", "-1", "١٠", "0x00", "01 02"])
    func rejectsInvalidAddresses(_ text: String) {
        #expect(throws: ProtocolFailure.self) { try BusAddress(hex: text) }
    }

    @Test func hexadecimalAddressAndBoundaryValues() throws {
        #expect(try BusAddress(hex: "10").value == 16)
        #expect(try BusAddress(hex: " 0xae ").description == "AE")
        #expect(try BusAddress(hex: "FF").value == 255)
        #expect(try BusAddress(hex: "01").value == 1)
    }

    @Test func addressAndFirmwareCommands() throws {
        let target = try BusAddress(hex: "10")
        #expect(try TS6Command.readAddress.frame(to: target).hex == "AE 10 C3 00 EE")
        #expect(try TS6Command.setAddress(BusAddress(hex: "41")).frame(to: target).hex == "AE 10 C4 41 EE")
        #expect(try TS6Command.firmware.frame(to: target).hex == "AE 10 CC 00 EE")
        #expect(throws: ProtocolFailure.sameAddress) { try TS6Command.setAddress(target).frame(to: target) }
        #expect(throws: ProtocolFailure.broadcastRequiresIsolation) { try TS6Command.unknownAddressQuery(isolated: false) }
        #expect(try TS6Command.unknownAddressQuery(isolated: true).hex == "AE 00 C3 00 EE")
    }

    @Test func panelCommandsMatchBothTS6Manuals() throws {
        let target = try BusAddress(hex: "55")
        let cases: [(TS6Command, String)] = [
            (.led(key: 3, color: .red), "AE 55 03 F1 EE"),
            (.led(key: 6, color: .green), "AE 55 06 F2 EE"),
            (.led(key: 1, color: .off), "AE 55 01 F0 EE"),
            (.readSound, "AE 55 C1 00 EE"), (.setSound(true), "AE 55 C2 01 EE"),
            (.setSound(false), "AE 55 C2 02 EE"), (.readBacklight, "AE 55 C5 00 EE"),
            (.setBacklight(3), "AE 55 C6 03 EE"), (.readFeedbackMode, "AE 55 C7 00 EE"),
            (.setManualFeedback(true), "AE 55 C8 02 EE"), (.setManualFeedback(false), "AE 55 C8 01 EE")
        ]
        for (command, hex) in cases { #expect(try command.frame(to: target).hex == hex) }
        for key in [0, 7, -1, 256] {
            #expect(throws: ProtocolFailure.invalidKey) { try TS6Command.led(key: key, color: .red).frame(to: target) }
        }
        for mode in [0, 4, 255] {
            #expect(throws: ProtocolFailure.invalidValue) { try TS6Command.setBacklight(mode).frame(to: target) }
        }
    }

    @Test func everyTCPChunkSplitPreservesPayloadMarkers() {
        let frames = [Frame(address: 0xAE, command: 0xC3, value: 0xEE), Frame(address: 0x10, command: 0xD1, value: 0xF1)]
        let data = frames.reduce(into: Data()) { $0.append($1.data) }
        for split in 0...data.count {
            var decoder = FrameDecoder()
            let first = decoder.feed(Data(data.prefix(split)))
            let second = decoder.feed(Data(data.dropFirst(split)))
            #expect(first + second == frames)
        }
    }

    @Test func noiseResynchronizationAndReset() {
        var decoder = FrameDecoder()
        let frame = Frame(address: 0x10, command: 0xD6, value: 0xF2)
        #expect(decoder.feed(Data([0, 0xEE, 0xAE, 1, 2, 3, 4]) + frame.data) == [frame])
        #expect(decoder.feed(Data([0xAE, 0x10])) == [])
        decoder.reset()
        #expect(decoder.feed(Data([0xD1, 0xF1, 0xEE])) == [])
        #expect(decoder.feed(frame.data) == [frame])
    }

    @Test func fieldSingleByteReplyIsWholeWindowOnly() {
        #expect(AddressReply.candidate(in: Data([0x10]), queriedAddress: 0)?.description == "10")
        #expect(AddressReply.candidate(in: Data([0x10]), queriedAddress: 0x10)?.description == "10")
        #expect(AddressReply.candidate(in: Data([0x10]), queriedAddress: 0x11) == nil)
        #expect(AddressReply.candidate(in: Data([0]), queriedAddress: 0)?.description == "00")
        #expect(AddressReply.candidate(in: Data([0]), queriedAddress: 0x10) == nil)
        for data in [Data(), Data([0x10, 0x10]), Data("10".utf8), Frame(address: 0x10, command: 0xC3, value: 0x10).data] {
            #expect(AddressReply.candidate(in: data, queriedAddress: 0) == nil)
        }
    }

    @Test func zeroAddressCanBeCommissionedOnIsolatedBus() throws {
        let next = try BusAddress(hex: "43")
        #expect(try TS6Command.addressQuery(targetHex: "00", isolated: true).hex == "AE 00 C3 00 EE")
        #expect(try TS6Command.addressChange(fromHex: "00", to: next, isolated: true).hex == "AE 00 C4 43 EE")
        #expect(throws: ProtocolFailure.broadcastRequiresIsolation) {
            try TS6Command.addressChange(fromHex: "00", to: next, isolated: false)
        }
        #expect(throws: ProtocolFailure.broadcastRequiresIsolation) {
            try TS6Command.addressQuery(targetHex: "00", isolated: false)
        }
        #expect(throws: ProtocolFailure.invalidAddress) { try BusAddress(hex: "00") }
    }

    @Test func unknownFeedbackAndASCIIStayDiagnostic() {
        #expect(Frame(address: 0x10, command: 0xD1, value: 0xF1).summary.contains("按下"))
        #expect(Frame(address: 0x10, command: 0xD1, value: 0xFF).summary.contains("未解释"))
        #expect(Frame(address: 0x10, command: 0xC3, value: 0x99).summary.contains("原始帧"))
        #expect(Data("TS6-86/B\r\nV4.3\r\n".utf8).printableASCII == "TS6-86/B··V4.3··")
    }
}
