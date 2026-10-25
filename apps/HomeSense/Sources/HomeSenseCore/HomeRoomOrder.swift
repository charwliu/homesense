import Foundation

/// Device-local room order. Room identity survives renames and HomeKit refreshes.
public struct HomeRoomOrder: Codable, Equatable, Sendable {
    public private(set) var roomIDs: [UUID] = []

    public init() {}

    public mutating func setOrder(_ ids: [UUID]) {
        var seen: Set<UUID> = []
        roomIDs = ids.filter { seen.insert($0).inserted }
    }

    /// Keep known rooms in the saved order, then append new rooms in default order.
    /// Missing rooms are omitted without discarding their saved preference.
    public func orderedIDs(in available: [UUID]) -> [UUID] {
        let current = Set(available)
        var seen: Set<UUID> = []
        return (roomIDs.filter { current.contains($0) } + available)
            .filter { seen.insert($0).inserted }
    }

    public static func load(homeID: UUID, defaults: UserDefaults) -> Self {
        guard let data = defaults.data(forKey: key(homeID)),
              let value = try? JSONDecoder().decode(Self.self, from: data) else { return Self() }
        return value
    }

    public func save(homeID: UUID, defaults: UserDefaults) {
        guard let data = try? JSONEncoder().encode(self) else { return }
        defaults.set(data, forKey: Self.key(homeID))
    }

    private static func key(_ homeID: UUID) -> String { "homekit.daily.roomOrder.\(homeID)" }
}
