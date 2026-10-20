import Foundation

/// Pure planning only. Native HomeKit discovery and writes belong to the App.
/// A serial number is scoped to an explicitly selected bridge, never global.
public enum HomeKitRoomPlacement {
    /// Ordinary HomeKit clients can read HMAccessory.model, but not its old
    /// serial characteristic. Both wire fields encode the same stable AID.
    /// A vendor AID, when available, must agree with an explicit model AID.
    public static func nativeIdentity(manufacturer: String?, model: String?, vendorAID: UInt64? = nil) -> String? {
        guard manufacturer == "HomeSense" else { return nil }
        let prefix = "HomeSense-AID:"
        var modelAID: UInt64?
        if let model, model.hasPrefix(prefix) {
            let value = String(model.dropFirst(prefix.count))
            guard let aid = UInt64(value), aid > 1, String(aid) == value else { return nil }
            modelAID = aid
        }
        if let vendorAID {
            guard vendorAID > 1, modelAID == nil || modelAID == vendorAID else { return nil }
            return "HS-\(vendorAID)"
        }
        return modelAID.map { "HS-\($0)" }
    }
    public enum Kind: String, Codable, Hashable, Sendable { case light, scene, curtain }

    public struct Source: Codable, Hashable, Sendable {
        private enum CodingKeys: String, CodingKey { case bridgeID = "bridge_id", kind, id }
        public let bridgeID: String
        public let kind: Kind
        public let id: String
        public init(bridgeID: String, kind: Kind, id: String) {
            self.bridgeID = bridgeID; self.kind = kind; self.id = id
        }
    }

    public struct PublishedAccessory: Codable, Equatable, Sendable {
        private enum CodingKeys: String, CodingKey {
            case kind, id, aid, serialNumber = "serial_number", roomID = "room_id"
        }
        public let kind: Kind
        public let id: String
        public let aid: UInt64
        public let serialNumber: String
        public let roomID: String?
        public init(kind: Kind, id: String, aid: UInt64, serialNumber: String, roomID: String?) {
            self.kind = kind; self.id = id; self.aid = aid
            self.serialNumber = serialNumber; self.roomID = roomID
        }
    }

    public struct PublishedBridge: Codable, Equatable, Sendable {
        public let id: String
        public let name: String
        public let available: Bool
        public let accessories: [PublishedAccessory]
        public init(id: String, name: String, available: Bool, accessories: [PublishedAccessory]) {
            self.id = id; self.name = name; self.available = available; self.accessories = accessories
        }
    }

    public struct Publication: Codable, Equatable, Sendable {
        public let boot: String
        public let revision: String
        public let generation: String
        public let bridges: [PublishedBridge]
        public init(boot: String, revision: String, generation: String, bridges: [PublishedBridge]) {
            self.boot = boot; self.revision = revision; self.generation = generation; self.bridges = bridges
        }
    }

    public struct Accessory: Equatable, Sendable {
        public let id: UUID
        public let serialNumber: String?
        public let manufacturer: String?
        public let roomID: UUID?
        public init(id: UUID, serialNumber: String?, manufacturer: String?, roomID: UUID?) {
            self.id = id; self.serialNumber = serialNumber; self.manufacturer = manufacturer; self.roomID = roomID
        }
    }

    public struct Bridge: Equatable, Sendable {
        public let id: UUID
        /// These must come from this native bridge's `bridgedAccessories`.
        public let accessories: [Accessory]
        public init(id: UUID, accessories: [Accessory]) { self.id = id; self.accessories = accessories }
    }

    public struct Room: Equatable, Sendable {
        public let id: UUID
        public let name: String
        public init(id: UUID, name: String) { self.id = id; self.name = name }
    }

    public struct Inventory: Equatable, Sendable {
        public let homeID: UUID
        public let bridges: [Bridge]
        public let rooms: [Room]
        public init(homeID: UUID, bridges: [Bridge], rooms: [Room]) {
            self.homeID = homeID; self.bridges = bridges; self.rooms = rooms
        }
    }

    public enum Destination: Hashable, Sendable { case existing(UUID), create(name: String) }
    public enum Reason: String, Sendable {
        case bridgeUnavailable, bridgeNotSelected, bridgeNotDiscovered
        case accessoryNotDiscovered, unassignedRoom, roomNotSelected, roomNotDiscovered
    }
    public struct Issue: Equatable, Sendable {
        public let source: Source
        public let reason: Reason
    }
    public struct Assignment: Equatable, Sendable {
        public let source: Source
        public let published: PublishedAccessory
        public let nativeBridgeID: UUID
        public let accessoryID: UUID
        public let previousRoomID: UUID?
        public let destination: Destination
        public var alreadyAssigned: Bool {
            if case .existing(let id) = destination { return id == previousRoomID }
            return false
        }
    }
    public struct Preview: Equatable, Sendable {
        public let publication: Publication
        public let homeID: UUID
        public let assignments: [Assignment]
        public let issues: [Issue]
        public var pendingAssignments: [Assignment] { assignments.filter { !$0.alreadyAssigned } }
        public var roomsToCreate: [String: String] {
            assignments.reduce(into: [:]) { rooms, assignment in
                if let room = assignment.published.roomID, case .create(let name) = assignment.destination {
                    rooms[room] = name
                }
            }
        }
    }
    public enum Failure: Error, Equatable {
        case invalidPublication, invalidInventory, duplicateBridgeBinding
        case ambiguousAccessory(Source), invalidRoomName, duplicateRoomName, stalePreview
    }

    private static func normalized(_ name: String) -> String {
        name.trimmingCharacters(in: .whitespacesAndNewlines).precomposedStringWithCanonicalMapping.lowercased()
    }

    public static func preview(publication: Publication, inventory: Inventory,
                               bridgeBindings: [String: UUID], roomBindings: [String: Destination],
                               unassignedRoom: UUID? = nil) throws -> Preview {
        guard !publication.boot.isEmpty, !publication.revision.isEmpty, !publication.generation.isEmpty,
              !publication.bridges.isEmpty, publication.bridges.count <= 8,
              Set(publication.bridges.map(\.id)).count == publication.bridges.count else {
            throw Failure.invalidPublication
        }
        let roomIDs = Set(inventory.rooms.map(\.id))
        guard Set(inventory.bridges.map(\.id)).count == inventory.bridges.count,
              roomIDs.count == inventory.rooms.count else { throw Failure.invalidInventory }
        let selected = publication.bridges.compactMap { bridgeBindings[$0.id] }
        guard Set(selected).count == selected.count else { throw Failure.duplicateBridgeBinding }
        var assignments: [Assignment] = [], issues: [Issue] = []
        var nativeIDs = Set<UUID>()
        for publishedBridge in publication.bridges {
            guard !publishedBridge.id.isEmpty, publishedBridge.accessories.count <= 4095 else {
                throw Failure.invalidPublication
            }
            var aids = Set<UInt64>(), serials = Set<String>(), sources = Set<Source>()
            for published in publishedBridge.accessories {
                let source = Source(bridgeID: publishedBridge.id, kind: published.kind, id: published.id)
                guard published.aid > 1, !published.id.isEmpty, !published.serialNumber.isEmpty,
                      aids.insert(published.aid).inserted, serials.insert(published.serialNumber).inserted,
                      sources.insert(source).inserted else { throw Failure.invalidPublication }
                let reason: Reason?
                let nativeBridge = bridgeBindings[publishedBridge.id].flatMap { selected in
                    inventory.bridges.first { $0.id == selected }
                }
                if !publishedBridge.available { reason = .bridgeUnavailable }
                else if bridgeBindings[publishedBridge.id] == nil { reason = .bridgeNotSelected }
                else if nativeBridge == nil { reason = .bridgeNotDiscovered }
                else { reason = nil }
                if let reason { issues.append(Issue(source: source, reason: reason)); continue }
                guard let nativeBridge else { throw Failure.invalidInventory }
                let matches = nativeBridge.accessories.filter {
                    $0.serialNumber == published.serialNumber && $0.manufacturer == "HomeSense"
                }
                guard matches.count <= 1 else { throw Failure.ambiguousAccessory(source) }
                guard let accessory = matches.first else {
                    issues.append(Issue(source: source, reason: .accessoryNotDiscovered)); continue
                }
                guard nativeIDs.insert(accessory.id).inserted else { throw Failure.invalidInventory }
                let destination: Destination
                if let room = published.roomID {
                    guard let selected = roomBindings[room] else {
                        issues.append(Issue(source: source, reason: .roomNotSelected)); continue
                    }
                    destination = selected
                } else if let unassignedRoom { destination = .existing(unassignedRoom) }
                else { issues.append(Issue(source: source, reason: .unassignedRoom)); continue }
                if case .existing(let id) = destination, !roomIDs.contains(id) {
                    issues.append(Issue(source: source, reason: .roomNotDiscovered)); continue
                }
                assignments.append(Assignment(source: source, published: published, nativeBridgeID: nativeBridge.id,
                                              accessoryID: accessory.id, previousRoomID: accessory.roomID, destination: destination))
            }
        }
        let preview = Preview(publication: publication, homeID: inventory.homeID, assignments: assignments, issues: issues)
        var creationNames = Set<String>()
        let existingNames = Set(inventory.rooms.map { normalized($0.name) })
        for name in preview.roomsToCreate.values {
            let key = normalized(name)
            guard !key.isEmpty else { throw Failure.invalidRoomName }
            guard !existingNames.contains(key), creationNames.insert(key).inserted else { throw Failure.duplicateRoomName }
        }
        return preview
    }

    /// Preflight immediately before starting a batch. After partial completion,
    /// build a new preview from rediscovered inventory; the old preview includes
    /// original rooms and intentionally becomes stale after successful moves.
    /// A caller must also use native HomeKit objects from this selected home.
    public static func validate(_ preview: Preview, publication: Publication, inventory: Inventory) throws {
        guard preview.publication == publication, preview.homeID == inventory.homeID else { throw Failure.stalePreview }
        let rooms = Set(inventory.rooms.map(\.id))
        guard rooms.count == inventory.rooms.count,
              Set(inventory.bridges.map(\.id)).count == inventory.bridges.count else { throw Failure.invalidInventory }
        let existingNames = Set(inventory.rooms.map { normalized($0.name) })
        for assignment in preview.assignments {
            guard let bridge = inventory.bridges.first(where: { $0.id == assignment.nativeBridgeID }),
                  let child = bridge.accessories.first(where: { $0.id == assignment.accessoryID }),
                  child.manufacturer == "HomeSense", child.serialNumber == assignment.published.serialNumber,
                  child.roomID == assignment.previousRoomID,
                  bridge.accessories.filter({ $0.serialNumber == assignment.published.serialNumber && $0.manufacturer == "HomeSense" }).count == 1 else {
                throw Failure.stalePreview
            }
            switch assignment.destination {
            case .existing(let id): guard rooms.contains(id) else { throw Failure.stalePreview }
            case .create(let name): guard !existingNames.contains(normalized(name)) else { throw Failure.stalePreview }
            }
        }
    }
}
