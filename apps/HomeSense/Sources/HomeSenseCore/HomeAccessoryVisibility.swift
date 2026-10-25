import Foundation

/// Local presentation preferences, scoped to an Apple Home, never control policy.
public struct HomeAccessoryVisibility: Codable, Equatable, Sendable {
    public private(set) var hiddenIDs: Set<UUID> = []
    public private(set) var visibleOverrides: Set<UUID> = []
    public private(set) var hidesLightStrips = false

    public init() {}

    public func isHidden(id: UUID, name: String) -> Bool {
        hiddenIDs.contains(id) || (hidesLightStrips && !visibleOverrides.contains(id)
            && name.trimmingCharacters(in: .whitespacesAndNewlines).hasPrefix("灯带"))
    }

    public mutating func setHidden(_ hidden: Bool, id: UUID) {
        if hidden {
            hiddenIDs.insert(id); visibleOverrides.remove(id)
        } else {
            hiddenIDs.remove(id); visibleOverrides.insert(id)
        }
    }

    public mutating func setHidesLightStrips(_ hidden: Bool) {
        guard hidesLightStrips != hidden else { return }
        hidesLightStrips = hidden
        // Enabling the bulk rule applies it to all matching lights again.
        if hidden { visibleOverrides.removeAll() }
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

    private static func key(_ homeID: UUID) -> String { "homekit.daily.visibility.\(homeID)" }
}
