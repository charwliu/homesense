import Foundation

/// Local editing state, independent of the server's actual device feedback.
public struct HomeBrightnessDraft: Equatable, Sendable {
    public struct Submission: Equatable, Identifiable, Sendable {
        public let id: UUID
        public let level: Int64
        fileprivate let feedbackTimestamp: UInt64?
    }
    public private(set) var value: Double = 0
    public private(set) var isEditing = false
    public private(set) var submission: Submission?
    public private(set) var unconfirmedTarget: Int64?
    private var unconfirmedFeedbackTimestamp: UInt64?
    public init() { }
    public mutating func setValue(_ value: Double) {
        guard value.isFinite else { return }
        self.value = min(100, max(0, value.rounded()))
    }
    public mutating func beginEditing() {
        isEditing = true
        submission = nil; unconfirmedTarget = nil; unconfirmedFeedbackTimestamp = nil
    }
    public mutating func endEditing(feedbackTimestamp: UInt64?) -> Submission? {
        guard isEditing else { return nil }
        isEditing = false
        let next = Submission(id: UUID(), level: Int64(value), feedbackTimestamp: feedbackTimestamp)
        submission = next
        return next
    }
    public mutating func complete(_ id: UUID, accepted: Bool) {
        guard let pending = submission, pending.id == id, !isEditing else { return }
        // An accepted receipt never confirms the level or clears the draft.
        if !accepted { retainUnconfirmed(pending) }
    }
    public mutating func expire(_ id: UUID) {
        guard let pending = submission, pending.id == id, !isEditing else { return }
        retainUnconfirmed(pending)
    }
    public mutating func cancel() {
        isEditing = false; submission = nil; unconfirmedTarget = nil; unconfirmedFeedbackTimestamp = nil
    }
    public mutating func observe(level: Int64?, confirmed: Bool, timestamp: UInt64?) {
        guard !isEditing, let level, (0...100).contains(level) else { return }
        if let target = submission?.level ?? unconfirmedTarget {
            // Ending the request wait does not make intermediate feedback the
            // user's target. Actual feedback is displayed separately by the UI.
            let baseline = submission?.feedbackTimestamp ?? unconfirmedFeedbackTimestamp
            guard confirmed, level == target, let timestamp,
                  baseline.map({ timestamp > $0 }) ?? true else { return }
            submission = nil; unconfirmedTarget = nil; unconfirmedFeedbackTimestamp = nil
        }
        setValue(Double(level))
    }
    private mutating func retainUnconfirmed(_ pending: Submission) {
        unconfirmedTarget = pending.level
        unconfirmedFeedbackTimestamp = pending.feedbackTimestamp
        submission = nil
    }
}
