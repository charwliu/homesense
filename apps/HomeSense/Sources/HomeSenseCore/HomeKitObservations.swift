import Foundation

/// Values confirmed by a HomeKit read or notification, never by a write intent.
public enum HomeKitObservedValue: Equatable, Sendable {
    case boolean(Bool), number(Double), text(String)
}

/// A home/subscription generation fences callbacks arriving after pause,
/// authorization revocation, or a different home selection.
public struct HomeKitObservations: Sendable {
    public struct ReadTicket: Sendable {
        fileprivate let characteristic: UUID
        fileprivate let generation: UUID
        fileprivate let ownerEpoch: UUID
        fileprivate let request: UUID
    }
    public private(set) var generation = UUID()
    public private(set) var values: [UUID: HomeKitObservedValue] = [:]
    public private(set) var failures: [UUID: String] = [:]
    public private(set) var revisions: [UUID: UInt64] = [:]
    private var sequence: UInt64 = 0
    private var members: [UUID: UUID] = [:]
    private var ownerEpochs: [UUID: UUID] = [:]
    private var pendingReads: [UUID: UUID] = [:]
    public init() {}
    public mutating func reset(members: [UUID: UUID] = [:]) {
        generation = UUID(); self.members = members
        values = [:]; failures = [:]; revisions = [:]; sequence = 0
        ownerEpochs = Dictionary(uniqueKeysWithValues: Set(members.values).map { ($0, UUID()) })
        pendingReads = [:]
    }
    public mutating func beginRead(for id: UUID) -> ReadTicket? {
        guard let owner = members[id], let epoch = ownerEpochs[owner] else { return nil }
        let request = UUID(); pendingReads[id] = request
        return ReadTicket(characteristic: id, generation: generation, ownerEpoch: epoch, request: request)
    }
    public mutating func receivedRead(_ value: HomeKitObservedValue?, ticket: ReadTicket, error: String? = nil) {
        guard ticket.generation == generation, let owner = members[ticket.characteristic],
            ownerEpochs[owner] == ticket.ownerEpoch, pendingReads[ticket.characteristic] == ticket.request else { return }
        pendingReads[ticket.characteristic] = nil
        apply(value, for: ticket.characteristic, error: error)
    }
    public mutating func received(_ value: HomeKitObservedValue?, for id: UUID, generation: UUID, error: String? = nil) {
        guard generation == self.generation, members[id] != nil else { return }
        // A new SDK notification supersedes any read that started before it.
        pendingReads[id] = nil
        apply(value, for: id, error: error)
    }
    private mutating func apply(_ value: HomeKitObservedValue?, for id: UUID, error: String?) {
        values[id] = error == nil ? value : nil
        failures[id] = error
        sequence &+= 1; revisions[id] = sequence
    }
    public mutating func unreachable(_ accessory: UUID) {
        ownerEpochs[accessory] = UUID()
        for (id, owner) in members where owner == accessory {
            pendingReads[id] = nil
            values[id] = nil; failures[id] = "设备未响应"
            sequence &+= 1; revisions[id] = sequence
        }
    }
}
