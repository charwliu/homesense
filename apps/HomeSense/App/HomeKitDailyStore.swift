#if os(iOS)
import Foundation
import Combine
@preconcurrency import HomeKit
import HomeSenseCore

@MainActor final class HomeKitDailyStore: NSObject, ObservableObject, HMHomeManagerDelegate, HMHomeDelegate {
    struct Choice: Identifiable, Hashable { let id: UUID; let name: String }
    struct Service: Identifiable {
        let accessory: HMAccessory
        let service: HMService
        let related: [HMService]
        init(accessory: HMAccessory, service: HMService, related: [HMService] = []) {
            self.accessory = accessory; self.service = service; self.related = related
        }
        var characteristics: [HMCharacteristic] {
            var seen: Set<UUID> = []
            return ([service] + related).flatMap(\.characteristics).filter { seen.insert($0.uniqueIdentifier).inserted }
        }
        var id: UUID { service.uniqueIdentifier }
        // Apple Home names tiles by service, including a grouped sensor's primary service.
        var name: String { service.name }
        var room: String { accessory.room?.name ?? "未分配房间" }
        var roomID: UUID? { accessory.room?.uniqueIdentifier }
    }
    @Published private(set) var homes: [Choice] = []
    @Published private(set) var rooms: [Choice] = []
    @Published private(set) var roomOrder = HomeRoomOrder()
    @Published private(set) var favoriteIDs: Set<UUID> = []
    @Published private(set) var visibility = HomeAccessoryVisibility()
    @Published private(set) var services: [Service] = []
    @Published private(set) var scenes: [HMActionSet] = []
    @Published private(set) var observations = HomeKitObservations()
    @Published private(set) var status = "允许访问 Apple 家庭后，即可查看和控制家庭设备。"
    @Published private(set) var error: String?
    @Published private(set) var busy: Set<UUID> = []
    @Published private(set) var renaming: Set<UUID> = []
    @Published private(set) var authorized = false
    @Published private(set) var selectedHomeID: UUID?
    private var manager: HMHomeManager?
    private var active = false
    private var accessoryDelegate: HomeKitDailyAccessoryDelegate?
    private var subscriptions: [HMCharacteristic] = []
    private var readers: [Task<Void, Never>] = []
    private var subscriptionErrors: [UUID: String] = [:]
    private var knownHome: HMHome? { manager?.homes.first { $0.uniqueIdentifier == selectedHomeID } }
    private let defaults = UserDefaults.standard
    var homeName: String { homes.first(where: { $0.id == selectedHomeID })?.name ?? "家庭" }
    var displayedServices: [Service] { services.filter { !isHidden($0) } }
    var hiddenServices: [Service] { services.filter(isHidden) }
    func isHidden(_ item: Service) -> Bool {
        visibility.isHidden(id: item.id, name: item.name)
    }
    func setHidden(_ hidden: Bool, id: UUID) {
        guard let homeID = selectedHomeID, services.contains(where: { $0.id == id }) else { return }
        visibility.setHidden(hidden, id: id)
        visibility.save(homeID: homeID, defaults: defaults)
    }
    func setHidesLightStrips(_ hidden: Bool) {
        guard let homeID = selectedHomeID else { return }
        visibility.setHidesLightStrips(hidden)
        visibility.save(homeID: homeID, defaults: defaults)
    }
    func restoreAllHidden() {
        guard let homeID = selectedHomeID else { return }
        visibility = HomeAccessoryVisibility()
        visibility.save(homeID: homeID, defaults: defaults)
    }
    func setRoomOrder(_ ids: [UUID]) {
        guard let homeID = selectedHomeID, ids.count == rooms.count,
              Set(ids) == Set(rooms.map(\.id)) else { return }
        roomOrder.setOrder(ids)
        roomOrder.save(homeID: homeID, defaults: defaults)
        applyRoomOrder()
    }
    func restoreDefaultRoomOrder() {
        guard let homeID = selectedHomeID else { return }
        roomOrder = .init()
        roomOrder.save(homeID: homeID, defaults: defaults)
        applyRoomOrder()
    }
    private func applyRoomOrder() {
        let sorted = rooms.sorted {
            let comparison = $0.name.localizedStandardCompare($1.name)
            return comparison == .orderedSame ? $0.id.uuidString < $1.id.uuidString : comparison == .orderedAscending
        }
        let byID = Dictionary(uniqueKeysWithValues: sorted.map { ($0.id, $0) })
        rooms = roomOrder.orderedIDs(in: sorted.map(\.id)).compactMap { byID[$0] }
    }
    func toggleFavorite(_ id: UUID) {
        guard let homeID = selectedHomeID, services.contains(where: { $0.id == id }) else { return }
        if !favoriteIDs.insert(id).inserted { favoriteIDs.remove(id) }
        defaults.set(favoriteIDs.map(\.uuidString).sorted(), forKey: "homekit.daily.favorites.\(homeID)")
    }

    override init() {
        selectedHomeID = defaults.string(forKey: "homekit.daily.home").flatMap(UUID.init(uuidString:))
        super.init()
    }
    func setActive(_ active: Bool) {
        guard self.active != active else { return }
        self.active = active
        if active {
            if manager == nil { manager = HMHomeManager(); manager?.delegate = self }
            synchronize()
        } else {
            stopObserving(); status = "已暂停家庭状态订阅"
        }
    }
    func selectHome(_ id: UUID?) {
        guard selectedHomeID != id else { return }
        selectedHomeID = id
        defaults.set(id?.uuidString, forKey: "homekit.daily.home")
        synchronize()
    }
    private func stopObserving() {
        readers.forEach { $0.cancel() }; readers = []
        // Leave no old confirmation visible after switching homes/background.
        observations.reset(); services = []; scenes = []; subscriptionErrors = [:]
        for characteristic in subscriptions { characteristic.enableNotification(false) { _ in } }
        subscriptions = []; accessoryDelegate = nil
    }
    func synchronize() {
        stopObserving(); error = nil
        guard let manager else { return }
        authorized = manager.authorizationStatus.contains(.authorized)
        guard authorized else {
            homes = []; rooms = []; roomOrder = .init(); favoriteIDs = []; visibility = .init(); status = manager.authorizationStatus.contains(.determined)
                ? "请在系统设置中允许 HomeSense 访问家庭。" : "等待系统家庭访问授权…"
            return
        }
        guard active else { return }
        homes = manager.homes.map { Choice(id: $0.uniqueIdentifier, name: $0.name) }
        if selectedHomeID == nil, homes.count == 1 { selectedHomeID = homes[0].id }
        guard let home = knownHome else {
            rooms = []; roomOrder = .init(); favoriteIDs = []; visibility = .init()
            status = homes.isEmpty ? "尚无 Apple 家庭，请先在家庭 App 中添加设备。" : "请选择 Apple 家庭。"
            return
        }
        defaults.set(home.uniqueIdentifier.uuidString, forKey: "homekit.daily.home")
        favoriteIDs = Set((defaults.stringArray(forKey: "homekit.daily.favorites.\(home.uniqueIdentifier)") ?? []).compactMap(UUID.init(uuidString:)))
        visibility = .load(homeID: home.uniqueIdentifier, defaults: defaults)
        roomOrder = .load(homeID: home.uniqueIdentifier, defaults: defaults)
        rooms = home.rooms.map { Choice(id: $0.uniqueIdentifier, name: $0.name) }.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        home.delegate = self
        var discovered: [Service] = []
        for accessory in home.accessories {
            let visible = accessory.services.filter { $0.characteristics.contains(where: { Self.visibleTypes.contains($0.characteristicType) }) }
            var sensors: [HMService] = []
            for service in visible {
                if service.characteristics.contains(where: { [HMCharacteristicTypePowerState, HMCharacteristicTypeCurrentPosition, HMCharacteristicTypeTargetPosition].contains($0.characteristicType) }) {
                    discovered.append(Service(accessory: accessory, service: service))
                } else { sensors.append(service) }
            }
            // A temperature/humidity/battery accessory is one physical sensor,
            // not three identical tiles. Keep every characteristic subscribed.
            let preferred = [HMCharacteristicTypeMotionDetected, HMCharacteristicTypeOccupancyDetected, HMCharacteristicTypeContactState, HMCharacteristicTypeCurrentTemperature, HMCharacteristicTypeCurrentRelativeHumidity]
            let primary = preferred.lazy.compactMap { type in sensors.first { $0.characteristics.contains { $0.characteristicType == type } } }.first ?? sensors.first
            if let primary { discovered.append(Service(accessory: accessory, service: primary, related: sensors.filter { $0 !== primary })) }
        }
        services = discovered.sorted { left, right in
            (left.room + left.name).localizedStandardCompare(right.room + right.name) == .orderedAscending
        }
        for accessory in home.accessories {
            if let room = accessory.room, !rooms.contains(where: { $0.id == room.uniqueIdentifier }) {
                rooms.append(Choice(id: room.uniqueIdentifier, name: room.name))
            }
        }
        applyRoomOrder()
        scenes = home.actionSets.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        let characteristics = services.flatMap(\.characteristics).filter {
            Self.visibleTypes.contains($0.characteristicType) && $0.properties.contains(HMCharacteristicPropertyReadable)
        }
        var members: [UUID: UUID] = [:]
        for item in services {
            for characteristic in item.characteristics where Self.visibleTypes.contains(characteristic.characteristicType) {
                members[characteristic.uniqueIdentifier] = item.accessory.uniqueIdentifier
            }
        }
        observations.reset(members: members)
        let generation = observations.generation
        let delegate = HomeKitDailyAccessoryDelegate(store: self, generation: generation)
        accessoryDelegate = delegate
        for accessory in home.accessories { accessory.delegate = delegate }
        subscriptions = characteristics.filter { $0.properties.contains(HMCharacteristicPropertySupportsEventNotification) }
        status = "已连接 \(home.name) · 状态由 Apple 家庭同步"
        // Bounded initial reads, once per subscription generation. No polling
        // timer, HTTP request, device scan, or automatic command retry.
        for worker in 0..<min(4, characteristics.count) {
            readers.append(Task { [weak self] in
                guard let self else { return }
                for index in stride(from: worker, to: characteristics.count, by: 4) {
                    guard !Task.isCancelled, self.observations.generation == generation else { return }
                    let characteristic = characteristics[index]
                    if characteristic.properties.contains(HMCharacteristicPropertySupportsEventNotification) {
                        do {
                            try await self.wait { done in
                                characteristic.enableNotification(true) { [weak self] error in
                                    Task { @MainActor in
                                        if self?.active != true || self?.subscriptions.contains(where: { $0 === characteristic }) != true {
                                            characteristic.enableNotification(false) { _ in }
                                        }
                                        done(error)
                                    }
                                }
                            }
                        } catch {
                            guard self.observations.generation == generation else { return }
                            self.subscriptionErrors[characteristic.uniqueIdentifier] = error.localizedDescription
                        }
                    }
                    guard !Task.isCancelled, self.observations.generation == generation else { return }
                    await self.read(characteristic, generation: generation)
                }
            })
        }
    }
    private func wait(_ start: (@escaping @Sendable (Error?) -> Void) -> Void) async throws {
        let completion = HomeCallbackCompletion()
        let deadline = Task {
            do { try await Task.sleep(for: .seconds(8)) } catch { return }
            completion.finish(.failure(URLError(.timedOut)))
        }
        defer { deadline.cancel() }
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (reply: CheckedContinuation<Void, Error>) in
                if Task.isCancelled { completion.finish(.failure(CancellationError())) }
                if completion.install(reply) {
                    start { error in Task { @MainActor in completion.finish(error.map { .failure($0) } ?? .success(())) } }
                }
            }
        } onCancel: { Task { @MainActor in completion.finish(.failure(CancellationError())) } }
    }
    private func read(_ characteristic: HMCharacteristic, generation: UUID) async {
        guard observations.generation == generation,
            let ticket = observations.beginRead(for: characteristic.uniqueIdentifier) else { return }
        do {
            try await wait { done in characteristic.readValue(completionHandler: done) }
            guard active, manager?.authorizationStatus.contains(.authorized) == true else {
                synchronize(); return
            }
            guard let accessory = characteristic.service?.accessory else {
                observations.receivedRead(nil, ticket: ticket, error: "配件已不可用"); return
            }
            guard accessory.isReachable else {
                if observations.generation == generation { observations.unreachable(accessory.uniqueIdentifier) }
                return
            }
            observations.receivedRead(Self.value(characteristic.value), ticket: ticket)
        } catch {
            observations.receivedRead(nil, ticket: ticket, error: error.localizedDescription)
        }
    }
    static func value(_ value: Any?) -> HomeKitObservedValue? {
        if let number = value as? NSNumber {
            if CFGetTypeID(number) == CFBooleanGetTypeID() { return .boolean(number.boolValue) }
            return .number(number.doubleValue)
        }
        if let value = value as? String { return .text(value) }
        return nil
    }
    func value(_ characteristic: HMCharacteristic) -> HomeKitObservedValue? { observations.values[characteristic.uniqueIdentifier] }
    func failure(_ characteristic: HMCharacteristic) -> String? {
        observations.failures[characteristic.uniqueIdentifier] ?? subscriptionErrors[characteristic.uniqueIdentifier]
    }
    func canWrite(_ characteristic: HMCharacteristic, in item: Service) -> Bool {
        active && authorized && manager?.authorizationStatus.contains(.authorized) == true && item.accessory.isReachable && services.contains { $0.id == item.id && $0.characteristics.contains(where: { $0 === characteristic }) }
            && characteristic.properties.contains(HMCharacteristicPropertyWritable)
            && !busy.contains(characteristic.uniqueIdentifier)
    }
    func canRename(_ item: Service) -> Bool {
        guard active, authorized, manager?.authorizationStatus.contains(.authorized) == true,
              let home = knownHome, home.homeAccessControl(for: home.currentUser).isAdministrator,
              home.accessories.contains(where: { $0 === item.accessory }),
              services.contains(where: { $0.id == item.id && $0.service === item.service }),
              !renaming.contains(item.accessory.uniqueIdentifier) else { return false }
        // Names are HomeKit metadata; an offline accessory can still be renamed.
        return true
    }
    func rename(_ name: String, in item: Service) async throws -> String {
        guard !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw NameFailure.empty
        }
        guard canRename(item), let homeID = selectedHomeID else { throw NameFailure.permissionOrPending }
        if name == item.name { return item.name }
        let id = item.accessory.uniqueIdentifier, generation = observations.generation
        renaming.insert(id)
        try await wait { done in
            let completion: @Sendable (Error?) -> Void = { [weak self] error in
                Task { @MainActor in
                    self?.renaming.remove(id)
                    if error == nil { self?.accessoryNamesChanged(item.accessory, generation: generation) }
                    done(error)
                }
            }
            // Keep display, write and confirmation on the same HomeKit service.
            // The device name and other grouped sensor service names remain independent.
            item.service.updateName(name, completionHandler: completion)
        }
        guard active, selectedHomeID == homeID, observations.generation == generation,
              manager?.authorizationStatus.contains(.authorized) == true,
              knownHome?.accessories.contains(where: { $0 === item.accessory }) == true else {
            throw NameFailure.contextChanged
        }
        accessoryNamesChanged(item.accessory, generation: generation)
        return item.name
    }
    private enum NameFailure: LocalizedError {
        case empty, permissionOrPending, contextChanged
        var errorDescription: String? {
            switch self {
            case .empty: "名称不能为空。"
            case .permissionOrPending: "需要 Apple 家庭的编辑权限，且上一次名称请求已完成。"
            case .contextChanged: "家庭或访问权限已变化，请在原家庭中重新核对名称。"
            }
        }
    }
    @discardableResult func write(_ value: Any, to characteristic: HMCharacteristic, in item: Service) async -> Bool {
        guard canWrite(characteristic, in: item) else { return false }
        let id = characteristic.uniqueIdentifier, generation = observations.generation
        busy.insert(id); error = nil
        do {
            try await wait { done in
                characteristic.writeValue(value) { [weak self] error in
                    Task { @MainActor in self?.busy.remove(id) }
                    done(error)
                }
            }
            // A write acknowledgement never substitutes for observed state.
            if observations.generation == generation, characteristic.properties.contains(HMCharacteristicPropertyReadable) {
                await read(characteristic, generation: generation)
            }
            return observations.generation == generation
        } catch {
            if observations.generation == generation { self.error = "操作尚未确认：\(error.localizedDescription)。不会自动重发。" }
            return false
        }
    }
    func execute(_ scene: HMActionSet) async {
        guard active, authorized, let home = knownHome, home.actionSets.contains(where: { $0.uniqueIdentifier == scene.uniqueIdentifier }), !busy.contains(scene.uniqueIdentifier) else { return }
        let id = scene.uniqueIdentifier, generation = observations.generation
        busy.insert(id); error = nil
        do {
            try await wait { done in home.executeActionSet(scene) { [weak self] error in
                Task { @MainActor in self?.busy.remove(id) }; done(error)
            } }
        } catch { if observations.generation == generation { self.error = "场景执行尚未确认：\(error.localizedDescription)。不会自动重发。" } }
    }
    nonisolated func homeManagerDidUpdateHomes(_ manager: HMHomeManager) { Task { @MainActor in self.synchronize() } }
    nonisolated func homeManager(_ manager: HMHomeManager, didUpdate status: HMHomeManagerAuthorizationStatus) { Task { @MainActor in self.synchronize() } }
    nonisolated func home(_ home: HMHome, didAdd accessory: HMAccessory) { Task { @MainActor in self.synchronize() } }
    nonisolated func home(_ home: HMHome, didRemove accessory: HMAccessory) { Task { @MainActor in self.synchronize() } }
    nonisolated func home(_ home: HMHome, didUpdateNameFor room: HMRoom) { Task { @MainActor in self.synchronize() } }
    nonisolated func homeDidUpdateName(_ home: HMHome) { Task { @MainActor in self.synchronize() } }
    nonisolated func home(_ home: HMHome, didAdd room: HMRoom) { Task { @MainActor in self.synchronize() } }
    nonisolated func home(_ home: HMHome, didRemove room: HMRoom) { Task { @MainActor in self.synchronize() } }
    nonisolated func home(_ home: HMHome, didUpdate room: HMRoom, for accessory: HMAccessory) { Task { @MainActor in self.synchronize() } }
    nonisolated func home(_ home: HMHome, didAdd actionSet: HMActionSet) { Task { @MainActor in self.synchronize() } }
    nonisolated func home(_ home: HMHome, didRemove actionSet: HMActionSet) { Task { @MainActor in self.synchronize() } }
    nonisolated func homeDidUpdateActionSets(_ home: HMHome) { Task { @MainActor in self.synchronize() } }
    nonisolated func homeDidUpdateAccessControl(forCurrentUser home: HMHome) { Task { @MainActor in self.objectWillChange.send() } }
    fileprivate func accessoryNamesChanged(_ accessory: HMAccessory, generation: UUID) {
        guard active, observations.generation == generation,
              knownHome?.accessories.contains(where: { $0 === accessory }) == true else { return }
        // Metadata updates must not clear confirmed device states or restart reads.
        services.sort { ($0.room + $0.name).localizedStandardCompare($1.room + $1.name) == .orderedAscending }
    }
    fileprivate func accessoryChanged(_ accessory: HMAccessory, generation: UUID, reachability: Bool) {
        guard active, observations.generation == generation,
            knownHome?.accessories.contains(where: { $0 === accessory }) == true else { return }
        if !reachability { synchronize(); return }
        if !accessory.isReachable { observations.unreachable(accessory.uniqueIdentifier) }
        else {
            readers.append(Task { [weak self] in
                guard let self else { return }
                for characteristic in accessory.services.flatMap(\.characteristics)
                    where Self.visibleTypes.contains(characteristic.characteristicType) && characteristic.properties.contains(HMCharacteristicPropertyReadable) {
                    guard !Task.isCancelled, self.observations.generation == generation else { return }
                    await self.read(characteristic, generation: generation)
                }
            })
        }
        // Reachability does not invalidate other accessories' confirmations.
        objectWillChange.send()
    }
    fileprivate func received(_ characteristic: HMCharacteristic, accessory: HMAccessory, generation: UUID) {
        guard active, authorized, manager?.authorizationStatus.contains(.authorized) == true, accessory.isReachable,
            knownHome?.accessories.contains(where: { $0 === accessory }) == true else { return }
        observations.received(Self.value(characteristic.value), for: characteristic.uniqueIdentifier, generation: generation)
    }
    static let visibleTypes: Set<String> = [
        HMCharacteristicTypePowerState, HMCharacteristicTypeBrightness,
        HMCharacteristicTypeCurrentPosition, HMCharacteristicTypeTargetPosition, HMCharacteristicTypeHoldPosition,
        HMCharacteristicTypeMotionDetected, HMCharacteristicTypeOccupancyDetected,
        HMCharacteristicTypeCurrentTemperature, HMCharacteristicTypeTargetTemperature,
        HMCharacteristicTypeCurrentRelativeHumidity, HMCharacteristicTypeCurrentLightLevel,
        HMCharacteristicTypeContactState, HMCharacteristicTypeSmokeDetected,
        HMCharacteristicTypeCarbonMonoxideDetected, HMCharacteristicTypeBatteryLevel,
        HMCharacteristicTypeStatusLowBattery, HMCharacteristicTypeLeakDetected,
    ]
}
/// Each SDK subscription carries its immutable generation into its callback.
/// A late callback cannot acquire a newer generation after a home switch.
@MainActor private final class HomeKitDailyAccessoryDelegate: NSObject, HMAccessoryDelegate {
    private weak var store: HomeKitDailyStore?
    private let generation: UUID
    init(store: HomeKitDailyStore, generation: UUID) { self.store = store; self.generation = generation }
    nonisolated func accessoryDidUpdateName(_ accessory: HMAccessory) {
        Task { @MainActor in self.store?.accessoryNamesChanged(accessory, generation: self.generation) }
    }
    nonisolated func accessory(_ accessory: HMAccessory, didUpdateNameFor service: HMService) {
        Task { @MainActor in self.store?.accessoryNamesChanged(accessory, generation: self.generation) }
    }
    nonisolated func accessoryDidUpdateServices(_ accessory: HMAccessory) {
        Task { @MainActor in self.store?.accessoryChanged(accessory, generation: self.generation, reachability: false) }
    }
    nonisolated func accessoryDidUpdateReachability(_ accessory: HMAccessory) {
        Task { @MainActor in self.store?.accessoryChanged(accessory, generation: self.generation, reachability: true) }
    }
    nonisolated func accessory(_ accessory: HMAccessory, service: HMService, didUpdateValueFor characteristic: HMCharacteristic) {
        Task { @MainActor in self.store?.received(characteristic, accessory: accessory, generation: self.generation) }
    }
}
#endif
