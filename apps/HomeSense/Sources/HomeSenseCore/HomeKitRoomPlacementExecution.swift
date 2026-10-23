import Foundation

/// Native adapters only edit HomeKit's room database; no device control API.
@MainActor public protocol HomeKitRoomPlacementDriver: AnyObject {
    func inventory(homeID: UUID) async throws -> HomeKitRoomPlacement.Inventory
    func createRoom(named: String, homeID: UUID) async throws -> HomeKitRoomPlacement.Room
    func assign(_ assignment: HomeKitRoomPlacement.Assignment, to roomID: UUID, homeID: UUID) async throws
}

public enum HomeKitRoomPlacementExecution {
    private typealias P = HomeKitRoomPlacement
    public enum Stop: String, Sendable { case cancelled, stalePreview, unavailable, operationFailed, verificationFailed }
    public struct Report: Sendable {
        public let assigned: [HomeKitRoomPlacement.Source]
        public let alreadyAssigned: Int
        public let createdRooms: [HomeKitRoomPlacement.Room]
        public let unresolved: Int
        public let stopped: Stop?
        public let detail: String?
        public var complete: Bool { stopped == nil && unresolved == 0 }
    }

    /// Never replay a failed/unknown assignment. A new preview discovers what
    /// really happened and skips accessories already in their requested rooms.
    @MainActor public static func execute(
        _ preview: HomeKitRoomPlacement.Preview,
        driver: any HomeKitRoomPlacementDriver,
        currentPublication: @MainActor () async throws -> HomeKitRoomPlacement.Publication
    ) async -> Report {
        var assigned: [P.Source] = [], completed: [P.Source: UUID] = [:]
        var created: [String: P.Room] = [:]
        var stop: Stop?, detail: String?
        func validateCurrent() async throws {
            try Task.checkCancellation()
            let publication = try await currentPublication()
            let inventory = try await driver.inventory(homeID: preview.homeID)
            try Task.checkCancellation()
            // Account for this batch's own successful changes, while still
            // rejecting manual moves of both untouched and completed targets.
            let createdIDs = Set(created.values.map(\.id))
            guard createdIDs.count == created.count else { throw P.Failure.stalePreview }
            for (key, room) in created {
                guard let name = preview.roomsToCreate[key], room.name == name,
                      inventory.rooms.contains(room) else { throw P.Failure.stalePreview }
            }
            var bridges: [P.Bridge] = []
            for bridge in inventory.bridges {
                var children: [P.Accessory] = []
                for child in bridge.accessories {
                    if let assignment = preview.assignments.first(where: { $0.nativeBridgeID == bridge.id && $0.accessoryID == child.id }),
                       let destination = completed[assignment.source] {
                        guard child.roomID == destination else { throw P.Failure.stalePreview }
                        children.append(P.Accessory(id: child.id, serialNumber: child.serialNumber,
                                                    manufacturer: child.manufacturer, roomID: assignment.previousRoomID))
                    } else { children.append(child) }
                }
                bridges.append(P.Bridge(id: bridge.id, accessories: children))
            }
            let baseline = P.Inventory(homeID: inventory.homeID, bridges: bridges,
                                       rooms: inventory.rooms.filter { !createdIDs.contains($0.id) })
            try P.validate(preview, publication: publication, inventory: baseline)
        }
        do {
            try await validateCurrent()
            for key in preview.roomsToCreate.keys.sorted() {
                try await validateCurrent()
                guard let name = preview.roomsToCreate[key] else { throw P.Failure.invalidRoomName }
                let before = try await driver.inventory(homeID: preview.homeID)
                let room = try await driver.createRoom(named: name, homeID: preview.homeID)
                guard room.name == name, !before.rooms.contains(where: { $0.id == room.id }) else {
                    stop = .verificationFailed; throw P.Failure.invalidInventory
                }
                created[key] = room
                try await validateCurrent()
            }
            for assignment in preview.pendingAssignments {
                try await validateCurrent()
                let destination: UUID
                switch assignment.destination {
                case .existing(let id): destination = id
                case .create:
                    guard let room = assignment.published.roomID.flatMap({ created[$0] }) else { throw P.Failure.invalidInventory }
                    destination = room.id
                }
                try await driver.assign(assignment, to: destination, homeID: preview.homeID)
                completed[assignment.source] = destination
                // A successful SDK callback alone is not a confirmed move.
                do { try await validateCurrent() }
                catch { stop = .verificationFailed; throw error }
                assigned.append(assignment.source)
            }
            try await validateCurrent()
        } catch {
            if error is CancellationError { stop = .cancelled }
            else if stop == nil {
                if let failure = error as? P.Failure { stop = failure == .stalePreview ? .stalePreview : .verificationFailed }
                else { stop = .operationFailed }
            }
            detail = String(describing: error)
        }
        // Include moves not yet confirmed when a batch stops partway through.
        return Report(assigned: assigned, alreadyAssigned: preview.assignments.count - preview.pendingAssignments.count,
                      createdRooms: created.keys.sorted().compactMap { created[$0] },
                      unresolved: preview.issues.count + preview.pendingAssignments.count - assigned.count,
                      stopped: stop, detail: detail)
    }
}
