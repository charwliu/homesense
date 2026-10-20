#if os(iOS)
import Foundation
import Combine
@preconcurrency import HomeKit
import HomeSenseCore

@MainActor private final class HomeKitCall<Value: Sendable> {
    var result: Result<Value, Error>?
    private var continuation: CheckedContinuation<Value, Error>?
    private var timer: Task<Void, Never>?
    func begin(_ continuation: CheckedContinuation<Value, Error>) {
        if let result { continuation.resume(with: result); return }
        self.continuation = continuation
        timer = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(15)) } catch { return }
            self?.finish(.failure(HomeKitRoomDirectory.Failure.timeout))
        }
    }
    func finish(_ value: Result<Value, Error>) {
        guard result == nil else { return }
        result = value; timer?.cancel(); timer = nil
        continuation?.resume(with: value); continuation = nil
    }
}

/// Created only from the explicit Apple Home placement screen. Construction
/// alone does not instantiate HMHomeManager or request HomeKit authorization.
@MainActor final class HomeKitRoomDirectory: NSObject, ObservableObject, HMHomeManagerDelegate, HomeKitRoomPlacementDriver {
    static let shared = HomeKitRoomDirectory()
    typealias P = HomeKitRoomPlacement
    struct Choice: Identifiable { let id: UUID; let name: String }
    enum Failure: LocalizedError {
        case permission, missingHome, missingBridge, missingAccessory, missingRoom, invalidIdentity, timeout, pending
        var errorDescription: String? {
            switch self {
            case .permission: "尚未允许访问Apple家庭，请在系统设置中检查HomeSense的家庭权限。"
            case .missingHome: "所选Apple家庭已不可用，请重新读取。"
            case .missingBridge: "所选桥已不可用，请重新选择。"
            case .missingAccessory: "配件已不在所选桥中，请重新预览。"
            case .missingRoom: "目标房间已不可用，请重新预览。"
            case .invalidIdentity: "配件标识发生变化，请重新读取后核对。"
            case .timeout: "Apple家庭操作未在时限内确认；请重新读取核对，不会自动重发。"
            case .pending: "上一次Apple家庭请求仍待系统确认，暂不继续迁移；可以重新读取状态。"
            }
        }
    }
    @Published private(set) var homes: [Choice] = []
    @Published private(set) var bridges: [Choice] = []
    @Published private(set) var status = "读取Apple家庭后，请选择家庭和对应的HomeSense桥。"
    @Published private(set) var pendingRequest = false
    private var manager: HMHomeManager?

    func startDiscovery() {
        if manager == nil { manager = HMHomeManager(); manager?.delegate = self }
        refreshHomes()
    }
    nonisolated func homeManagerDidUpdateHomes(_ manager: HMHomeManager) {
        Task { @MainActor [weak self] in self?.refreshHomes() }
    }
    nonisolated func homeManager(_ manager: HMHomeManager, didUpdate status: HMHomeManagerAuthorizationStatus) {
        Task { @MainActor [weak self] in self?.refreshHomes() }
    }
    private func refreshHomes() {
        guard let manager else { return }
        guard manager.authorizationStatus.contains(.authorized) else {
            homes = []; bridges = []
            status = manager.authorizationStatus.contains(.determined) ? Failure.permission.localizedDescription : "等待系统家庭访问授权…"
            return
        }
        homes = manager.homes.map { Choice(id: $0.uniqueIdentifier, name: $0.name) }
        status = homes.isEmpty ? "尚未发现Apple家庭，请先在Apple家庭App中确认。" : "请选择要分配房间的家庭。"
    }
    private func selectedHome(_ id: UUID) throws -> HMHome {
        guard let manager, manager.authorizationStatus.contains(.authorized) else { throw Failure.permission }
        guard let home = manager.homes.first(where: { $0.uniqueIdentifier == id }) else { throw Failure.missingHome }
        return home
    }
    private func perform<Value: Sendable>(_ start: (@escaping @Sendable (Result<Value, Error>) -> Void) -> Void) async throws -> Value {
        try Task.checkCancellation()
        guard !pendingRequest else { throw Failure.pending }
        let call = HomeKitCall<Value>()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                call.begin(continuation)
                if Task.isCancelled { call.finish(.failure(CancellationError())) }
                if call.result == nil {
                    pendingRequest = true
                    start { value in
                        Task { @MainActor [self] in
                            // Cancellation/our timeout cannot cancel an SDK
                            // write. Keep its mutation fence until the real
                            // completion, including across sheet presentations.
                            pendingRequest = false; call.finish(value)
                        }
                    }
                }
            }
        } onCancel: { Task { @MainActor in call.finish(.failure(CancellationError())) } }
    }
    private func identity(_ accessory: HMAccessory) -> String? {
        var aid: UInt64?
        if #available(iOS 26.1, *), accessory.isVendorAccessory { aid = accessory.hapInstanceID }
        return P.nativeIdentity(manufacturer: accessory.manufacturer, model: accessory.model, vendorAID: aid)
    }
    func inventory(homeID: UUID) async throws -> P.Inventory {
        let home = try selectedHome(homeID)
        let nativeBridges = home.accessories.filter { !$0.isBridged && !$0.bridgedAccessories.isEmpty }
        bridges = nativeBridges.map { Choice(id: $0.uniqueIdentifier, name: $0.name) }
        var result: [P.Bridge] = []
        for bridge in nativeBridges {
            var children: [P.Accessory] = []
            for child in bridge.bridgedAccessories {
                try Task.checkCancellation()
                children.append(P.Accessory(id: child.uniqueIdentifier, serialNumber: identity(child), manufacturer: child.manufacturer, roomID: child.room?.uniqueIdentifier))
            }
            result.append(P.Bridge(id: bridge.uniqueIdentifier, accessories: children))
        }
        return P.Inventory(homeID: homeID, bridges: result, rooms: home.rooms.map { .init(id: $0.uniqueIdentifier, name: $0.name) })
    }
    func createRoom(named: String, homeID: UUID) async throws -> P.Room {
        let home = try selectedHome(homeID)
        let id: UUID = try await perform { done in
            home.addRoom(withName: named) { room, error in
                if let error { done(.failure(error)) }
                else if let room { done(.success(room.uniqueIdentifier)) }
                else { done(.failure(Failure.missingRoom)) }
            }
        }
        guard let room = home.rooms.first(where: { $0.uniqueIdentifier == id }) else { throw Failure.missingRoom }
        return P.Room(id: id, name: room.name)
    }
    func assign(_ assignment: P.Assignment, to roomID: UUID, homeID: UUID) async throws {
        let home = try selectedHome(homeID)
        guard let bridge = home.accessories.first(where: { $0.uniqueIdentifier == assignment.nativeBridgeID }) else { throw Failure.missingBridge }
        guard let child = bridge.bridgedAccessories.first(where: { $0.uniqueIdentifier == assignment.accessoryID }) else { throw Failure.missingAccessory }
        guard identity(child) == assignment.published.serialNumber else { throw Failure.invalidIdentity }
        guard let room = home.rooms.first(where: { $0.uniqueIdentifier == roomID }) else { throw Failure.missingRoom }
        let _: Bool = try await perform { done in
            home.assignAccessory(child, to: room) { error in done(error.map { .failure($0) } ?? .success(true)) }
        }
    }
}
#endif
