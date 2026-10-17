import Foundation
import Testing
@testable import HomeSenseCore

@Suite struct HomeKeypadActivationTests {
    private func request() throws -> HomeKeypadActivationRequest {
        try HomeKeypadActivationRequest(revision: "9007199254740993", keypad: "panel", key: 3)
    }
    private func receipt(_ request: HomeKeypadActivationRequest) -> HomeJSON {
        .object(["request_id": .string(request.requestID.uuidString), "outcome": .string("accepted"),
                 "boot": .string("boot"), "revision": .string(request.revision), "keypad": .string(request.keypad),
                 "key": .integer(Int64(request.key)), "gesture": .string("activate"), "retry": .bool(false),
                 "execution": .object(["kind": .string("scene"), "commands": .array([]), "runs": .array([]), "relative_status": .null]),
                 "indication": .object(["request_id": .string(request.requestID.uuidString), "operation_sequence": .string("1"), "endpoint_id": .string("panel-key-3"),
                     "source": .string("authenticated_click"), "status": .string("pending"), "color": .string("red"), "observed": .bool(false)])])
    }

    @Test func virtualPressContainsOnlyServerResolvedOriginAndExactRevision() throws {
        let request = try request()
        #expect(HomeKeypadActivationRequest.path == "/api/v1/keypad-actions")
        #expect(request.body == .object(["revision": .string("9007199254740993"), "keypad": .string("panel"),
            "key": .integer(3), "request_id": .string(request.requestID.uuidString), "gesture": .string("activate")]))
        #expect(try self.request().requestID != request.requestID)
        for revision in ["0", "01", "-1", "1.0", "18446744073709551616"] {
            #expect(throws: HomeServerError.self) { try HomeKeypadActivationRequest(revision: revision, keypad: "panel", key: 1) }
        }
        #expect(throws: HomeServerError.self) { try HomeKeypadActivationRequest(revision: "1", keypad: " ", key: 1) }
        #expect(throws: HomeServerError.self) { try HomeKeypadActivationRequest(revision: "1", keypad: String(repeating: "楼", count: 84), key: 1) }
        for key in [0, 7] { #expect(throws: HomeServerError.self) { try HomeKeypadActivationRequest(revision: "1", keypad: "panel", key: key) } }
    }

    @Test func acceptedCueNeverClaimsPhysicalLEDOrLoadCompletion() throws {
        let request = try request(), raw = receipt(request)
        let parsed = try HomeKeypadActivationReceipt(raw, request: request, boot: "boot", endpoint: "panel-key-3")
        guard case .accepted(let cue) = parsed else { Issue.record("Expected accepted cue"); return }
        #expect(cue.status == .pending && cue.color == .red)
        var unknown = raw
        unknown["indication"]["status"] = .string("unknown"); unknown["indication"]["color"] = .null
        guard case .accepted(let cue) = try HomeKeypadActivationReceipt(unknown, request: request, boot: "boot", endpoint: "panel-key-3") else {
            Issue.record("Unknown color must stay unknown"); return
        }
        #expect(cue.status == .unknown && cue.color == nil)
    }

    @Test(arguments: ["request_id", "boot", "revision", "keypad", "key", "gesture", "retry", "outcome"])
    func foreignOrMalformedReceiptCannotAffectMatchingKey(_ field: String) throws {
        let request = try request(); var raw = receipt(request)
        raw[field] = .string("foreign")
        #expect(throws: HomeServerError.self) { try HomeKeypadActivationReceipt(raw, request: request, boot: "boot", endpoint: "panel-key-3") }
    }

    @Test(arguments: ["request_id", "operation_sequence", "event_sequence", "endpoint_id", "source", "status", "color", "observed"])
    func malformedIndicationCannotBecomeKnownLED(_ field: String) throws {
        let request = try request(); var raw = receipt(request)
        raw["indication"][field] = field == "observed" ? .bool(true) : .string("foreign")
        #expect(throws: HomeServerError.self) { try HomeKeypadActivationReceipt(raw, request: request, boot: "boot", endpoint: "panel-key-3") }
        raw = receipt(request); raw["indication"]["status"] = .string("cancelled")
        #expect(throws: HomeServerError.self) { try HomeKeypadActivationReceipt(raw, request: request, boot: "boot", endpoint: "panel-key-3") }
    }

    @Test func uncertainExecutionCarriesNoCueAndNeverRequestsRetry() throws {
        let request = try request()
        var raw: HomeJSON = .object(["request_id": .string(request.requestID.uuidString), "outcome": .string("unknown"),
            "error": .string("execution_outcome_unknown"), "retry": .bool(false), "indication": .null])
        #expect(try HomeKeypadActivationReceipt(raw, request: request, boot: "boot", endpoint: "panel-key-3") == .unknown)
        raw["retry"] = .bool(true)
        #expect(throws: HomeServerError.self) { try HomeKeypadActivationReceipt(raw, request: request, boot: "boot", endpoint: "panel-key-3") }
    }

    @Test func newerPressWinsAndDuplicateReceiptCannotReapplyCue() throws {
        var state = HomeKeypadActivationState()
        let context = HomeKeypadActivationState.Context(generation: UUID(), boot: "boot", revision: "9007199254740993")
        state.reset(to: context)
        let older = try request(), newer = try request()
        try state.begin(older, endpoint: "panel-key-3", context: context)
        #expect(state.presentation("panel", key: 3)?.phase == .submitting)
        #expect(state.presentation("panel", key: 3)?.requestedColor == nil)
        try state.begin(newer, endpoint: "panel-key-3", context: context)
        let oldReply = try state.complete(receipt(older), request: older, context: context)
        let oldFailure = state.fail(older, context: context, uncertain: true)
        #expect(!oldReply && !oldFailure)
        #expect(state.presentation("panel", key: 3)?.phase == .submitting)
        let newReply = try state.complete(receipt(newer), request: newer, context: context)
        #expect(newReply)
        #expect(state.presentation("panel", key: 3)?.requestedColor == .red)
        let duplicate = try state.complete(receipt(newer), request: newer, context: context)
        let lateFailure = state.fail(newer, context: context, uncertain: true)
        #expect(!duplicate && !lateFailure)
    }

    @Test func changedSessionBootOrRevisionDropsOldResponses() throws {
        let original = HomeKeypadActivationState.Context(generation: UUID(), boot: "boot", revision: "9007199254740993")
        for next in [HomeKeypadActivationState.Context(generation: UUID(), boot: original.boot, revision: original.revision),
                     HomeKeypadActivationState.Context(generation: original.generation, boot: "other", revision: original.revision),
                     HomeKeypadActivationState.Context(generation: original.generation, boot: original.boot, revision: "9007199254740994")] {
            var state = HomeKeypadActivationState(); state.reset(to: original)
            let request = try request()
            try state.begin(request, endpoint: "panel-key-3", context: original)
            state.reset(to: original)
            #expect(state.presentation("panel", key: 3)?.phase == .submitting, "Routine same-context snapshots preserve pending progress")
            state.reset(to: next)
            #expect(state.presentation("panel", key: 3) == nil)
            let lateReply = try state.complete(receipt(request), request: request, context: original)
            let lateFailure = state.fail(request, context: original, uncertain: true)
            #expect(!lateReply && !lateFailure)
        }
    }

    @Test func lostReplyStaysUncertainAndRejectedRequestDoesNotInventColor() throws {
        var state = HomeKeypadActivationState()
        let context = HomeKeypadActivationState.Context(generation: UUID(), boot: "boot", revision: "9007199254740993")
        state.reset(to: context)
        let first = try request(), second = try request()
        try state.begin(first, endpoint: "panel-key-3", context: context)
        let lost = state.fail(first, context: context, uncertain: true)
        #expect(lost)
        #expect(state.presentation("panel", key: 3)?.phase == .uncertain)
        #expect(state.presentation("panel", key: 3)?.requestedColor == nil)
        try state.begin(second, endpoint: "panel-key-3", context: context)
        let rejected = state.fail(second, context: context, uncertain: false)
        #expect(rejected)
        #expect(state.presentation("panel", key: 3)?.phase == .rejected)
        #expect(state.presentation("panel", key: 3)?.requestedColor == nil)
        state.reset()
        #expect(state.presentation("panel", key: 3) == nil)
        #expect(throws: HomeServerError.self) { try state.begin(first, endpoint: "panel-key-3", context: context) }
    }

    @Test func buttonEventCueIsSeparateFromAuthenticatedClickReceipt() throws {
        let request = try request(); var raw = receipt(request)
        raw["indication"]["source"] = .string("button_event")
        raw["indication"]["event_sequence"] = .string("20")
        raw["indication"]["request_id"] = .string("wall-00000000000040008000000000000001-20")
        let event = try HomeKeypadActionIndication(raw["indication"], endpoint: "panel-key-3", boot: "00000000-0000-4000-8000-000000000001")
        #expect(event.source == .buttonEvent && event.eventSequence == 20)
        #expect(throws: HomeServerError.self) { try HomeKeypadActivationReceipt(raw, request: request, boot: "boot", endpoint: "panel-key-3") }
        for value in [HomeJSON.null, .integer(20), .string("0"), .string("-1"), .string("01"), .string("18446744073709551616")] {
            raw["indication"]["event_sequence"] = value
            #expect(throws: HomeServerError.self) { try HomeKeypadActionIndication(raw["indication"], endpoint: "panel-key-3", boot: "00000000-0000-4000-8000-000000000001") }
        }
        raw = receipt(request); raw["indication"]["event_sequence"] = .string("20")
        #expect(throws: HomeServerError.self) { try HomeKeypadActionIndication(raw["indication"], endpoint: "panel-key-3", boot: "00000000-0000-4000-8000-000000000001") }
    }
}
