import Foundation
import Testing
@testable import HomeSenseCore

@Suite struct HomeKitRoomPlacementTests {
    private typealias P = HomeKitRoomPlacement
    private struct Fixture {
        let home = UUID(), firstBridge = UUID(), secondBridge = UUID()
        let first = UUID(), second = UUID(), firstRoom = UUID(), secondRoom = UUID()
        var publication: P.Publication {
            P.Publication(boot: "boot", revision: "1", generation: "graph-1", bridges: [
                P.PublishedBridge(id: "one", name: "Same bridge name", available: true, accessories: [
                    P.PublishedAccessory(kind: .light, id: "lamp", aid: 2, serialNumber: "HS-2", roomID: "room-one")]),
                P.PublishedBridge(id: "two", name: "Same bridge name", available: true, accessories: [
                    P.PublishedAccessory(kind: .light, id: "lamp", aid: 2, serialNumber: "HS-2", roomID: "room-two")])])
        }
        var inventory: P.Inventory {
            P.Inventory(homeID: home, bridges: [
                P.Bridge(id: firstBridge, accessories: [P.Accessory(id: first, serialNumber: "HS-2", manufacturer: "HomeSense", roomID: nil)]),
                P.Bridge(id: secondBridge, accessories: [P.Accessory(id: second, serialNumber: "HS-2", manufacturer: "HomeSense", roomID: nil)])],
                rooms: [P.Room(id: firstRoom, name: "Same room name"), P.Room(id: secondRoom, name: "Same room name")])
        }
        var bridgeBindings: [String: UUID] { ["one": firstBridge, "two": secondBridge] }
        var roomBindings: [String: P.Destination] { ["room-one": .existing(firstRoom), "room-two": .existing(secondRoom)] }
        func plan(_ inventory: P.Inventory? = nil) throws -> P.Preview {
            try P.preview(publication: publication, inventory: inventory ?? self.inventory,
                          bridgeBindings: bridgeBindings, roomBindings: roomBindings)
        }
    }

    @Test func fanPublicationRetainsItsBridgeScopedIdentityAndRoom() throws {
        let json = #"{"kind":"fan","id":"relay","aid":2,"serial_number":"HS-2","room_id":"equipment"}"#
        let accessory = try JSONDecoder().decode(P.PublishedAccessory.self, from: Data(json.utf8))
        #expect(accessory.kind == .fan && accessory.serialNumber == "HS-2" && accessory.roomID == "equipment")
        #expect(try JSONDecoder().decode(P.PublishedAccessory.self, from: JSONEncoder().encode(accessory)) == accessory)
    }

    @Test func identicalAIDsSerialsAndNamesRemainScopedToExplicitNativeBridgeAndRoomIDs() throws {
        let f = Fixture(), preview = try f.plan()
        #expect(preview.issues.isEmpty && preview.pendingAssignments.count == 2)
        #expect(preview.assignments.map(\.accessoryID) == [f.first, f.second])
        #expect(preview.assignments.map(\.nativeBridgeID) == [f.firstBridge, f.secondBridge])
        #expect(preview.assignments.map(\.destination) == [.existing(f.firstRoom), .existing(f.secondRoom)])
        try P.validate(preview, publication: f.publication, inventory: f.inventory)
    }

    @Test func unselectedGatewayWithSameSerialIsNeverIncluded() throws {
        let f = Fixture(), stranger = UUID()
        let extra = P.Bridge(id: UUID(), accessories: [P.Accessory(id: stranger, serialNumber: "HS-2", manufacturer: "HomeSense", roomID: nil)])
        let inventory = P.Inventory(homeID: f.home, bridges: f.inventory.bridges + [extra], rooms: f.inventory.rooms)
        #expect(try f.plan(inventory).assignments.allSatisfy { $0.accessoryID != stranger })
    }

    @Test func duplicateSerialWithinSelectedBridgeIsRejectedRatherThanGuessed() {
        let f = Fixture(), source = P.Source(bridgeID: "one", kind: .light, id: "lamp")
        let duplicate = P.Accessory(id: UUID(), serialNumber: "HS-2", manufacturer: "HomeSense", roomID: nil)
        let inventory = P.Inventory(homeID: f.home,
                                    bridges: [P.Bridge(id: f.firstBridge, accessories: f.inventory.bridges[0].accessories + [duplicate]), f.inventory.bridges[1]],
                                    rooms: f.inventory.rooms)
        #expect(throws: P.Failure.ambiguousAccessory(source)) { try f.plan(inventory) }
    }

    @Test func twoPublicationBridgesCannotBindToTheSameNativeBridge() {
        let f = Fixture()
        #expect(throws: P.Failure.duplicateBridgeBinding) {
            try P.preview(publication: f.publication, inventory: f.inventory,
                          bridgeBindings: ["one": f.firstBridge, "two": f.firstBridge], roomBindings: f.roomBindings)
        }
    }

    @Test func missingBindingsAndWrongManufacturerProduceIssuesWithoutAssignments() throws {
        let f = Fixture()
        let unbound = try P.preview(publication: f.publication, inventory: f.inventory, bridgeBindings: [:], roomBindings: f.roomBindings)
        #expect(unbound.assignments.isEmpty && unbound.issues.allSatisfy { $0.reason == .bridgeNotSelected })
        let wrong = P.Inventory(homeID: f.home, bridges: [P.Bridge(id: f.firstBridge, accessories: [
            P.Accessory(id: f.first, serialNumber: "HS-2", manufacturer: "Other", roomID: nil)]), f.inventory.bridges[1]], rooms: f.inventory.rooms)
        let preview = try f.plan(wrong)
        #expect(preview.assignments.map(\.accessoryID) == [f.second])
        #expect(preview.issues.map(\.reason) == [.accessoryNotDiscovered])
        let noRooms = try P.preview(publication: f.publication, inventory: f.inventory,
                                   bridgeBindings: f.bridgeBindings, roomBindings: [:])
        #expect(noRooms.assignments.isEmpty && noRooms.issues.allSatisfy { $0.reason == .roomNotSelected })
    }

    @Test func bootRevisionGenerationAndHomeChangesInvalidatePreview() throws {
        let f = Fixture(), preview = try f.plan()
        for publication in [
            P.Publication(boot: "new", revision: "1", generation: "graph-1", bridges: f.publication.bridges),
            P.Publication(boot: "boot", revision: "2", generation: "graph-1", bridges: f.publication.bridges),
            P.Publication(boot: "boot", revision: "1", generation: "graph-2", bridges: f.publication.bridges)] {
            #expect(throws: P.Failure.stalePreview) { try P.validate(preview, publication: publication, inventory: f.inventory) }
        }
        let otherHome = P.Inventory(homeID: UUID(), bridges: f.inventory.bridges, rooms: f.inventory.rooms)
        #expect(throws: P.Failure.stalePreview) { try P.validate(preview, publication: f.publication, inventory: otherHome) }
    }

    @Test func manualRoomMoveOrChangedBridgeMembershipInvalidatesPreview() throws {
        let f = Fixture(), preview = try f.plan()
        let moved = P.Inventory(homeID: f.home, bridges: [P.Bridge(id: f.firstBridge, accessories: [
            P.Accessory(id: f.first, serialNumber: "HS-2", manufacturer: "HomeSense", roomID: f.secondRoom)]), f.inventory.bridges[1]], rooms: f.inventory.rooms)
        #expect(throws: P.Failure.stalePreview) { try P.validate(preview, publication: f.publication, inventory: moved) }
        let noChild = P.Inventory(homeID: f.home, bridges: [P.Bridge(id: f.firstBridge, accessories: []), f.inventory.bridges[1]], rooms: f.inventory.rooms)
        #expect(throws: P.Failure.stalePreview) { try P.validate(preview, publication: f.publication, inventory: noChild) }
    }

    @Test func alreadyAssignedAccessoriesHaveNoPendingWrites() throws {
        let f = Fixture()
        let placed = P.Inventory(homeID: f.home, bridges: [
            P.Bridge(id: f.firstBridge, accessories: [P.Accessory(id: f.first, serialNumber: "HS-2", manufacturer: "HomeSense", roomID: f.firstRoom)]),
            P.Bridge(id: f.secondBridge, accessories: [P.Accessory(id: f.second, serialNumber: "HS-2", manufacturer: "HomeSense", roomID: f.secondRoom)])], rooms: f.inventory.rooms)
        let preview = try f.plan(placed)
        #expect(preview.assignments.count == 2 && preview.pendingAssignments.isEmpty)
        try P.validate(preview, publication: f.publication, inventory: placed)
    }

    @Test func newRoomsAreDeduplicatedByServerRoomIDAndCollisionsRequireExplicitResolution() throws {
        let f = Fixture()
        let sameRoom = P.Publication(boot: "boot", revision: "1", generation: "graph", bridges: [
            P.PublishedBridge(id: "one", name: "Bridge", available: true, accessories: [
                P.PublishedAccessory(kind: .light, id: "first", aid: 2, serialNumber: "HS-2", roomID: "new-room"),
                P.PublishedAccessory(kind: .scene, id: "second", aid: 3, serialNumber: "HS-3", roomID: "new-room")])])
        let inventory = P.Inventory(homeID: f.home, bridges: [P.Bridge(id: f.firstBridge, accessories: [
            P.Accessory(id: f.first, serialNumber: "HS-2", manufacturer: "HomeSense", roomID: nil),
            P.Accessory(id: f.second, serialNumber: "HS-3", manufacturer: "HomeSense", roomID: nil)])], rooms: f.inventory.rooms)
        let preview = try P.preview(publication: sameRoom, inventory: inventory, bridgeBindings: ["one": f.firstBridge],
                                    roomBindings: ["new-room": .create(name: "New room")])
        #expect(preview.pendingAssignments.count == 2 && preview.roomsToCreate == ["new-room": "New room"])
        #expect(throws: P.Failure.duplicateRoomName) {
            try P.preview(publication: f.publication, inventory: f.inventory, bridgeBindings: f.bridgeBindings,
                          roomBindings: ["room-one": .create(name: "New"), "room-two": .create(name: " NEW ")])
        }
        #expect(throws: P.Failure.duplicateRoomName) {
            try P.preview(publication: sameRoom, inventory: inventory, bridgeBindings: ["one": f.firstBridge],
                          roomBindings: ["new-room": .create(name: "Same room name")])
        }
    }

    @Test func malformedPublicationNeverCreatesAPartialPlan() {
        let f = Fixture()
        let invalid = P.Publication(boot: "boot", revision: "1", generation: "graph", bridges: [
            P.PublishedBridge(id: "one", name: "Bridge", available: true, accessories: [
                P.PublishedAccessory(kind: .light, id: "lamp", aid: 1, serialNumber: "HS-1", roomID: "room-one")])])
        #expect(throws: P.Failure.invalidPublication) {
            try P.preview(publication: invalid, inventory: f.inventory, bridgeBindings: f.bridgeBindings, roomBindings: f.roomBindings)
        }
    }

    @Test func serverPublicationUsesExplicitWireKeysAndPreservesBridgeLocalIdentifiers() throws {
        let data = Data(#"{"boot":"boot","revision":"45","generation":"graph","bridges":[{"id":"one","name":"HomeSense","available":true,"accessories":[{"kind":"light","id":"lamp","aid":2,"serial_number":"HS-2","room_id":"bedroom"}]}]}"#.utf8)
        let publication = try JSONDecoder().decode(P.Publication.self, from: data)
        let accessory = publication.bridges[0].accessories[0]
        #expect(accessory.serialNumber == "HS-2" && accessory.roomID == "bedroom")
        let encoded = try JSONSerialization.jsonObject(with: JSONEncoder().encode(publication)) as? [String: Any]
        let bridges = encoded?["bridges"] as? [[String: Any]]
        let accessories = bridges?[0]["accessories"] as? [[String: Any]]
        #expect(accessories?[0]["serial_number"] as? String == "HS-2")
        #expect(accessories?[0]["serialNumber"] == nil)
    }

    @Test func unassignedGlobalAccessoriesRequireAnExplicitExistingRoom() throws {
        let f = Fixture()
        let publication = P.Publication(boot: "boot", revision: "1", generation: "graph", bridges: [
            .init(id: "one", name: "HomeSense", available: true, accessories: [
                .init(kind: .scene, id: "whole-house", aid: 2, serialNumber: "HS-2", roomID: nil)])])
        let missing = try P.preview(publication: publication, inventory: f.inventory, bridgeBindings: f.bridgeBindings, roomBindings: [:])
        #expect(missing.assignments.isEmpty && missing.issues[0].reason == .unassignedRoom)
        let selected = try P.preview(publication: publication, inventory: f.inventory, bridgeBindings: f.bridgeBindings,
                                     roomBindings: [:], unassignedRoom: f.secondRoom)
        #expect(selected.issues.isEmpty && selected.assignments[0].destination == .existing(f.secondRoom))
        #expect(selected.assignments[0].published.roomID == nil && selected.roomsToCreate.isEmpty)
    }

    @Test func nativeIdentityUsesCanonicalPublicModelAndNeverGuessesOrIgnoresAConflict() {
        #expect(P.nativeIdentity(manufacturer: "HomeSense", model: "HomeSense-AID:42") == "HS-42")
        #expect(P.nativeIdentity(manufacturer: "HomeSense", model: "HomeSense", vendorAID: 42) == "HS-42")
        #expect(P.nativeIdentity(manufacturer: "HomeSense", model: "HomeSense-AID:42", vendorAID: 42) == "HS-42")
        #expect(P.nativeIdentity(manufacturer: "HomeSense", model: "HomeSense-AID:42", vendorAID: 43) == nil)
        #expect(P.nativeIdentity(manufacturer: "Other", model: "HomeSense-AID:42") == nil)
        for value in ["HomeSense", "HomeSense-AID:1", "HomeSense-AID:0", "HomeSense-AID:02", "HomeSense-AID:-2", "HomeSense-AID:2 extra", "HomeSense-AID:18446744073709551616"] {
            #expect(P.nativeIdentity(manufacturer: "HomeSense", model: value) == nil)
        }
    }
}
