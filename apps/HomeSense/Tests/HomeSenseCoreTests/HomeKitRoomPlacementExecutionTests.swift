import Foundation
import Testing
@testable import HomeSenseCore

@MainActor private final class PlacementDriver: HomeKitRoomPlacementDriver {
    typealias P = HomeKitRoomPlacement
    enum Failure: Error { case lostResponse }
    let home = UUID(), bridge = UUID(), oldRoom = UUID(), target = UUID()
    let first = UUID(), second = UUID(), unrelated = UUID()
    var publication: P.Publication
    var state: P.Inventory
    var moves: [UUID] = [], creations = 0
    var failAfterFirstMove = false
    var mutateCompletedAfterConfirmation = false
    private var readsAfterFirstMove = 0
    init() {
        publication = P.Publication(boot: "boot", revision: "1", generation: "graph", bridges: [
            P.PublishedBridge(id: "bridge", name: "HomeSense", available: true, accessories: [
                .init(kind: .light, id: "first", aid: 2, serialNumber: "HS-2", roomID: "room"),
                .init(kind: .curtain, id: "second", aid: 3, serialNumber: "HS-3", roomID: "room")])])
        state = P.Inventory(homeID: home, bridges: [P.Bridge(id: bridge, accessories: [
            .init(id: first, serialNumber: "HS-2", manufacturer: "HomeSense", roomID: oldRoom),
            .init(id: second, serialNumber: "HS-3", manufacturer: "HomeSense", roomID: oldRoom),
            .init(id: unrelated, serialNumber: "OTHER", manufacturer: "Other", roomID: oldRoom)])],
            rooms: [.init(id: oldRoom, name: "Default"), .init(id: target, name: "Bedroom")])
    }
    func preview(create: Bool = false) throws -> P.Preview {
        try P.preview(publication: publication, inventory: state, bridgeBindings: ["bridge": bridge],
                      roomBindings: ["room": create ? .create(name: "New room") : .existing(target)])
    }
    func move(_ id: UUID, to room: UUID) {
        state = P.Inventory(homeID: home, bridges: state.bridges.map { bridge in
            P.Bridge(id: bridge.id, accessories: bridge.accessories.map {
                .init(id: $0.id, serialNumber: $0.serialNumber, manufacturer: $0.manufacturer, roomID: $0.id == id ? room : $0.roomID)
            })
        }, rooms: state.rooms)
    }
    func inventory(homeID: UUID) async throws -> P.Inventory {
        #expect(homeID == home)
        if moves.count == 1 {
            readsAfterFirstMove += 1
            if mutateCompletedAfterConfirmation && readsAfterFirstMove == 2 { move(first, to: oldRoom) }
        }
        return state
    }
    func createRoom(named: String, homeID: UUID) async throws -> P.Room {
        #expect(homeID == home)
        let room = P.Room(id: UUID(), name: named); creations += 1
        state = P.Inventory(homeID: home, bridges: state.bridges, rooms: state.rooms + [room])
        return room
    }
    func assign(_ assignment: P.Assignment, to roomID: UUID, homeID: UUID) async throws {
        #expect(homeID == home && assignment.nativeBridgeID == bridge)
        moves.append(assignment.accessoryID); move(assignment.accessoryID, to: roomID)
        if failAfterFirstMove && moves.count == 1 { throw Failure.lostResponse }
    }
}

@Suite @MainActor struct HomeKitRoomPlacementExecutionTests {
    @Test func movesOnlyReviewedChildrenAndVerifiesActualRooms() async throws {
        let driver = PlacementDriver(), preview = try driver.preview()
        let result = await HomeKitRoomPlacementExecution.execute(preview, driver: driver) { driver.publication }
        #expect(result.complete && result.assigned.count == 2 && result.createdRooms.isEmpty)
        #expect(driver.moves == [driver.first, driver.second])
        #expect(driver.state.bridges[0].accessories.first { $0.id == driver.unrelated }?.roomID == driver.oldRoom)
    }

    @Test func oneCreatedRoomServesAllItsAccessoriesWithoutInvalidatingOwnProgress() async throws {
        let driver = PlacementDriver(), preview = try driver.preview(create: true)
        let result = await HomeKitRoomPlacementExecution.execute(preview, driver: driver) { driver.publication }
        #expect(result.complete && result.assigned.count == 2 && driver.creations == 1)
        #expect(result.createdRooms.count == 1)
        #expect(driver.state.bridges[0].accessories.filter { $0.id != driver.unrelated }.allSatisfy { $0.roomID == result.createdRooms[0].id })
    }

    @Test func lostAssignmentResponseStopsAndFreshPreviewSkipsTheActuallyMovedAccessory() async throws {
        let driver = PlacementDriver(), preview = try driver.preview()
        driver.failAfterFirstMove = true
        let first = await HomeKitRoomPlacementExecution.execute(preview, driver: driver) { driver.publication }
        #expect(!first.complete && first.stopped == .operationFailed && driver.moves.count == 1)
        #expect(first.assigned.isEmpty && first.unresolved == 2)
        driver.failAfterFirstMove = false
        let refreshed = try driver.preview()
        let second = await HomeKitRoomPlacementExecution.execute(refreshed, driver: driver) { driver.publication }
        #expect(second.complete && second.alreadyAssigned == 1 && second.assigned.count == 1)
        #expect(driver.moves == [driver.first, driver.second])
    }

    @Test func changedPublicationRejectsTheWholeBatchBeforeCreatingOrAssigning() async throws {
        let driver = PlacementDriver(), preview = try driver.preview(create: true)
        let changed = HomeKitRoomPlacement.Publication(boot: "new", revision: "1", generation: "new", bridges: driver.publication.bridges)
        let result = await HomeKitRoomPlacementExecution.execute(preview, driver: driver) { changed }
        #expect(result.stopped == .stalePreview && driver.moves.isEmpty && driver.creations == 0)
        #expect(result.unresolved == 2)
    }

    @Test func manualMoveOfAnAlreadyConfirmedTargetStopsBeforeTheNextAccessory() async throws {
        let driver = PlacementDriver(), preview = try driver.preview()
        driver.mutateCompletedAfterConfirmation = true
        let result = await HomeKitRoomPlacementExecution.execute(preview, driver: driver) { driver.publication }
        #expect(result.stopped != nil && result.assigned.count == 1 && driver.moves == [driver.first])
    }

    @Test func cancellationDuringProgressNeverReplaysOrContinuesToAnotherAccessory() async throws {
        let driver = PlacementDriver(), preview = try driver.preview()
        let result = await HomeKitRoomPlacementExecution.execute(preview, driver: driver) {
            if !driver.moves.isEmpty { throw CancellationError() }
            return driver.publication
        }
        #expect(result.stopped == .cancelled && driver.moves.count == 1)
        #expect(result.assigned.isEmpty && result.unresolved == 2)
    }

    @Test func cancellationAfterConfirmationCountsOnlyTheRemainingAccessory() async throws {
        let driver = PlacementDriver(), preview = try driver.preview()
        var readsAfterMove = 0
        let result = await HomeKitRoomPlacementExecution.execute(preview, driver: driver) {
            if !driver.moves.isEmpty {
                readsAfterMove += 1
                if readsAfterMove == 2 { throw CancellationError() }
            }
            return driver.publication
        }
        #expect(result.stopped == .cancelled && driver.moves.count == 1)
        #expect(result.assigned.count == 1 && result.unresolved == 1)
    }
}
