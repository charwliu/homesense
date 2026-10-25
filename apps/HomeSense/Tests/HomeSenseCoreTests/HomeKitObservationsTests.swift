import Foundation
import Testing
@testable import HomeSenseCore

@Suite struct HomeKitObservationsTests {
    @Test func ignoresPreviousHomeAndUnknownCharacteristics() {
        let id = UUID(), owner = UUID()
        var state = HomeKitObservations()
        state.reset(members: [id: owner]); let old = state.generation
        state.received(.boolean(true), for: id, generation: old)
        #expect(state.values[id] == .boolean(true))
        state.reset(members: [id: owner])
        state.received(.boolean(true), for: id, generation: old)
        state.received(.number(1), for: UUID(), generation: state.generation)
        #expect(state.values.isEmpty)
    }
    @Test func failureInvalidatesOnlyAffectedCharacteristic() {
        let a = UUID(), b = UUID(), owner = UUID()
        var state = HomeKitObservations(); state.reset(members: [a: owner, b: owner])
        state.received(.boolean(true), for: a, generation: state.generation)
        state.received(.number(42), for: b, generation: state.generation)
        state.received(nil, for: a, generation: state.generation, error: "read failed")
        #expect(state.values[a] == nil)
        #expect(state.failures[a] == "read failed")
        #expect(state.values[b] == .number(42))
        state.received(.boolean(false), for: a, generation: state.generation)
        #expect(state.values[a] == .boolean(false))
        #expect(state.failures[a] == nil)
    }
    @Test func disconnectionAndRevocationDiscardConfirmation() {
        let a = UUID(), b = UUID(), owner = UUID(), other = UUID()
        var state = HomeKitObservations(); state.reset(members: [a: owner, b: other])
        state.received(.boolean(true), for: a, generation: state.generation)
        state.received(.boolean(false), for: b, generation: state.generation)
        state.unreachable(owner)
        #expect(state.values[a] == nil)
        #expect(state.values[b] == .boolean(false))
        let old = state.generation; state.reset()
        state.received(.boolean(true), for: a, generation: old)
        #expect(state.values.isEmpty)
        #expect(state.failures.isEmpty)
    }
    @Test func notificationWinsOverOlderReadAndItsLateError() throws {
        let id = UUID(), owner = UUID()
        var state = HomeKitObservations(); state.reset(members: [id: owner])
        let issued = state.beginRead(for: id)
        let read = try #require(issued)
        state.received(.boolean(true), for: id, generation: state.generation)
        let revision = state.revisions[id]
        state.receivedRead(.boolean(false), ticket: read)
        state.receivedRead(nil, ticket: read, error: "late timeout")
        #expect(state.values[id] == .boolean(true))
        #expect(state.failures[id] == nil && state.revisions[id] == revision)
    }
    @Test func disconnectFencesPendingReadsWithoutInvalidatingOtherDevices() throws {
        let id = UUID(), other = UUID(), owner = UUID()
        var state = HomeKitObservations(); state.reset(members: [id: owner, other: UUID()])
        let issued = state.beginRead(for: id)
        let old = try #require(issued)
        state.received(.number(25), for: other, generation: state.generation)
        state.unreachable(owner)
        state.receivedRead(.boolean(true), ticket: old)
        #expect(state.values[id] == nil && state.failures[id] == "设备未响应")
        #expect(state.values[other] == .number(25))
        let freshIssued = state.beginRead(for: id)
        let fresh = try #require(freshIssued)
        state.receivedRead(.boolean(false), ticket: fresh)
        state.receivedRead(.boolean(true), ticket: old)
        #expect(state.values[id] == .boolean(false) && state.failures[id] == nil)
    }
    @Test func newestReadAndHomeGenerationOwnTheReply() throws {
        let id = UUID(), owner = UUID()
        var state = HomeKitObservations(); state.reset(members: [id: owner])
        let one = state.beginRead(for: id), two = state.beginRead(for: id)
        let old = try #require(one), latest = try #require(two)
        state.receivedRead(.number(75), ticket: latest)
        state.receivedRead(.number(10), ticket: old)
        #expect(state.values[id] == .number(75))
        state.reset(members: [id: owner])
        state.receivedRead(.number(75), ticket: latest)
        #expect(state.values[id] == nil && state.revisions[id] == nil)
    }
}
