import Foundation
import Combine

public enum HomeInterfaceMode: String, CaseIterable, Identifiable, Sendable {
    case daily = "日常控制", management = "配置管理"
    public var id: String { rawValue }
}

/// Admission acknowledgement only; physical state remains server feedback.
public enum HomeDeviceCommandResult: Equatable, Sendable {
    case notSent
    case accepted(requestID: String)
    case unconfirmed(requestID: String)
    public var accepted: Bool { if case .accepted = self { return true }; return false }
}

/// Shared device-card and detail intent. The server decides whether a repeated
/// direction stops movement; the client never derives it from cached feedback.
public enum HomeMotorControlAction: String, CaseIterable, Sendable {
    case open, stop, close

    public var command: HomeJSON {
        switch self {
        case .open, .close:
            .object(["action": .string("motor_press"), "direction": .string(rawValue)])
        case .stop:
            .object(["action": .string("stop")])
        }
    }
}

@MainActor
public final class HomeServerStore: ObservableObject {
    @Published public var endpointText: String {
        didSet {
            if (try? HomeServerEndpoint(oldValue).origin) != (try? HomeServerEndpoint(endpointText).origin) {
                disconnect()
            }
        }
    }
    @Published public private(set) var connected = false
    public let diagnostics: HomeDiagnostics
    private var loggedKeypadGates: [String: [HomeKeypadGateState]] = [:]
    @Published private var ordinaryBusy = false
    public var busy: Bool { ordinaryBusy || !pendingKeypadActivations.isEmpty || !priorityMotorStops.isEmpty }
    @Published public private(set) var canManage = false
    @Published public private(set) var interfaceMode: HomeInterfaceMode = .daily
    @Published public private(set) var devices: [HomeRecord] = []
    @Published public private(set) var rooms: [HomeRecord] = []
    @Published public private(set) var scenes: [HomeRecord] = []
    @Published public private(set) var awayOperations: [HomeAwayOperation] = []
    @Published private var sceneActionHints: [String: HomeSceneActionPresentation] = [:]
    private var sceneCueVersions: [String: UInt64] = [:]
    public var awayStatus: HomeAwayOperation? { receivingPush ? awayOperations.first : nil }
    public func sceneActionPresentation(_ id: String) -> HomeSceneActionPresentation? {
        snapshotIsCurrent ? sceneActionHints[id] : nil
    }
    private func showSceneAction(_ id: String, requestID: String, selected: Bool?) {
        guard scenes.contains(where: { $0.id == id }) else { return }
        let hint = HomeSceneActionPresentation(requestID: requestID, selected: selected)
        sceneActionHints[id] = hint
        let generation = epoch
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(15))
            guard !Task.isCancelled, let self, self.epoch == generation, self.sceneActionHints[id] == hint else { return }
            self.sceneActionHints.removeValue(forKey: id)
        }
    }
    private func clearSceneAction(_ requestID: String) {
        sceneActionHints = sceneActionHints.filter { $0.value.requestID != requestID }
    }
    private func reconcileSceneActions() {
        sceneActionHints = sceneActionHints.filter { id, hint in
            guard let scene = scenes.first(where: { $0.id == id }) else { return false }
            return hint.selected.map { scene.raw["selected"] != .bool($0) } ?? true
        }
    }
    @Published public private(set) var moduleInventory: [HomePhysicalModule] = []
    @Published public private(set) var loadingModules = false
    @Published public private(set) var moduleInventoryMessage = ""
    private var moduleLoadID = UUID()
    private var moduleInventoryRevision = ""
    private var moduleInventoryBoot = ""
    private var moduleInventoryReady = false
    @Published public private(set) var unifiedKeypadActionsSupported = false
    @Published private var keypadActivationState = HomeKeypadActivationState()
    private var keypadActivationContext: HomeKeypadActivationState.Context?
    private struct PendingKeypadActivation {
        let operation: HomeControlOperation
        let request: HomeKeypadActivationRequest
        let context: HomeKeypadActivationState.Context
        let controlGeneration: UUID
    }
    @Published private var pendingKeypadActivations: [UUID: PendingKeypadActivation] = [:]
    private var priorityStopGeneration = UUID()
    @Published private var priorityMotorStops: Set<UUID> = []
    private var latestPriorityMessage: String?
    private var keypadEventVersions: [String: UInt64] = [:]
    private var keypadEventStates: [String: Bool] = [:]
    @Published public private(set) var revision = ""
    @Published public private(set) var capturedAt: Date?
    @Published public private(set) var message = "连接 HomeSense 服务，读取家庭、设备与场景。"
    @Published public var error: String?
    @Published public private(set) var deviceDocument: HomeJSON?
    @Published public private(set) var automationDocument: HomeJSON?
    @Published public private(set) var baseRevision = ""
    @Published public private(set) var candidateRevision = ""
    @Published public private(set) var draftID: String?
    @Published public private(set) var draftChanged = false
    @Published public private(set) var drafts: [HomeRecord] = []
    @Published public private(set) var preview: HomeJSON?
    private var endpoint: HomeServerEndpoint?
    private var token: String?
    private var epoch = UUID()
    private let transport: any HomeHTTPTransport
    private let defaults: UserDefaults?
    private let credentialStore: any HomeCredentialStore
    @Published public private(set) var credentialStatus = ""
    @Published public private(set) var restoringCredential = false
    @Published private var savedCredentialOrigin: String?
    private var restoreGeneration = UUID()
    // Only a credential explicitly restored for this exact Origin may recover
    // an initially unauthenticated connection. Never replay a control request.
    private var savedRecoveryOrigin: String?
    @Published public private(set) var receivingPush = false
    @Published public private(set) var synchronization = "未连接"
    @Published public private(set) var serverFloors: [String] = []
    private let eventTransport: any HomeEventTransport
    private var eventTask: Task<Void, Never>?
    private var eventsWanted = true
    private var eventEpoch = UUID()
    private var stateSerial: UInt64 = 0
    private var snapshotBoot = ""
    private var eventCursor: HomeJSON?

    public init(transport: any HomeHTTPTransport = HomeURLSessionTransport(), eventTransport: any HomeEventTransport = HomeWebSocketTransport(), defaults: UserDefaults? = .standard,
                credentialStore: any HomeCredentialStore = HomeKeychainCredentialStore(), diagnostics: HomeDiagnostics? = nil) {
        self.diagnostics = diagnostics ?? HomeDiagnostics()
        self.transport = transport; self.eventTransport = eventTransport; self.defaults = defaults
        self.credentialStore = credentialStore
        endpointText = defaults?.string(forKey: "home.server") ?? "https://192.168.3.2:8444"
    }
    public var controlsAvailable: Bool {
        snapshotIsCurrent && !busy
    }
    /// A server-owned presentation hint, never a permission or polling filter.
    /// Keep the complete authorized store for scene feedback and admission.
    public var dailyDevices: [HomeRecord] { devices.filter { $0.raw["catalog_visible"] != .bool(false) } }
    public var dailyPanels: [HomeRecord] {
        dailyDevices.filter { $0.raw["kind"].text == "Keypad" && $0.raw["module_id"].text != nil }
    }
    public var physicalHardwareModules: [HomePhysicalModule] { moduleInventory.filter { $0.modelName != nil } }
    public var moduleInventoryContext: String { "\(epoch)-\(revision)-\(snapshotBoot)-\(canManage)" }
    public func physicalModule(for panel: HomeRecord) -> HomePhysicalModule? {
        guard let id = panel.raw["module_id"].text else { return nil }
        return moduleInventory.first { $0.moduleID == id }
    }
    /// Room/overview controls show lights and curtains. Keypads remain intact
    /// in the device catalog, including their key actions and LED state.
    public var dailyActuators: [HomeRecord] {
        dailyDevices.filter { ["Switch", "Dimmer", "Motor"].contains($0.raw["kind"].text ?? "") }
    }
    public func roomDevices(inRoom id: String) -> [HomeRecord] {
        guard rooms.contains(where: { $0.id == id }) else { return [] }
        return dailyActuators.filter { $0.roomID == id }
    }
    /// Only explicit, currently visible room associations affect navigation.
    /// Missing/unknown rooms stay reachable in the household overview.
    public var householdScenes: [HomeRecord] {
        let ids = Set(rooms.map(\.id))
        return scenes.filter { scene in scene.roomID.map { !ids.contains($0) } ?? true }
    }
    public func scenes(inRoom id: String) -> [HomeRecord] {
        guard rooms.contains(where: { $0.id == id }) else { return [] }
        return scenes.filter { $0.roomID == id }
    }
    public func scenePanels(for records: [HomeRecord]) -> [HomeScenePanel] {
        let roomNames = Dictionary(uniqueKeysWithValues: rooms.map { ($0.id, $0.name) })
        var order: [String] = [], groups: [String: [HomeRecord]] = [:], seen = Set<String>()
        for scene in records where !scene.id.isEmpty && seen.insert(scene.id).inserted {
            let room = scene.roomID.flatMap { roomNames[$0] == nil ? nil : $0 } ?? ""
            if groups[room] == nil { order.append(room) }
            groups[room, default: []].append(scene)
        }
        return order.flatMap { room -> [HomeScenePanel] in
            let rows = groups[room] ?? [], pageCount = (rows.count + 5) / 6
            return stride(from: 0, to: rows.count, by: 6).map { start in
                HomeScenePanel(id: "\(room.isEmpty ? "household" : "room:" + room):page:\(start / 6)",
                    title: roomNames[room].map { "\($0)场景" } ?? "全屋与未分配场景",
                    page: start / 6 + 1, pageCount: pageCount,
                    scenes: Array(rows[start..<min(start + 6, rows.count)]))
            }
        }
    }
    /// Deduplicate only against valid six-key cards rendered on the same page.
    /// This is presentation only: never trim the authorized catalog or infer
    /// bindings from nested scene programs. Ignore transient busy/offline state.
    public func independentScenes(_ candidates: [HomeRecord], alongside visibleDevices: [HomeRecord]) -> [HomeRecord] {
        let visibleIDs = Set(visibleDevices.map(\.id))
        var represented = Set<String>()
        for device in dailyDevices where visibleIDs.contains(device.id) {
            guard let panel = try? HomeKeypad(record: device) else { continue }
            for key in panel.keys where key.canActivateCard && key.cardAction == .absolute {
                guard let id = key.actionScene else { continue }
                represented.insert(id)
            }
        }
        return candidates.filter { !represented.contains($0.id) }
    }
    public var snapshotIsCurrent: Bool {
        connected && (eventsWanted ? receivingPush : capturedAt.map { Date().timeIntervalSince($0) < 10 } == true)
    }
    public var availableInterfaceModes: [HomeInterfaceMode] { canManage ? [.daily, .management] : [.daily] }
    public var hasSavedCredential: Bool {
        guard let origin = try? HomeServerEndpoint(endpointText).origin else { return false }
        return savedCredentialOrigin == origin
    }
    public func restoreSavedConnection() async {
        guard !busy, !connected, !restoringCredential else { return }
        let generation = epoch, restore = UUID()
        restoreGeneration = restore; restoringCredential = true
        defer { if restoreGeneration == restore { restoringCredential = false } }
        do {
            let origin = try HomeServerEndpoint(endpointText).origin
            savedRecoveryOrigin = origin
            let saved = try await credentialStore.load(for: origin)
            guard generation == epoch, restoreGeneration == restore, !Task.isCancelled, (try? HomeServerEndpoint(endpointText).origin) == origin else { return }
            guard let saved else {
                savedRecoveryOrigin = nil; savedCredentialOrigin = nil; credentialStatus = "此服务尚未保存本机凭证。"; return
            }
            savedCredentialOrigin = origin; credentialStatus = "已从本机 Keychain 恢复此服务凭证。"
            await connect(token: saved, restoringOrigin: origin)
        } catch { if generation == epoch { savedRecoveryOrigin = nil; self.error = error.localizedDescription } }
    }
    public func removeSavedCredential() async {
        guard !busy || restoringCredential else { return }
        do {
            let origin = try HomeServerEndpoint(endpointText).origin
            disconnect()
            let generation = epoch
            ordinaryBusy = true
            defer { if generation == epoch { ordinaryBusy = false } }
            try await credentialStore.remove(for: origin)
            guard generation == epoch else { return }
            if savedCredentialOrigin == origin { savedCredentialOrigin = nil }
            credentialStatus = "已移除此服务的本机凭证，服务端令牌未撤销。"
            message = "已退出此服务；重新连接需输入令牌。"
        } catch { self.error = error.localizedDescription; credentialStatus = "未能移除本机凭证，请解锁设备后重试。" }
    }
    /// Presentation only. Preserve authentication, subscriptions and unsaved drafts.
    public func selectInterfaceMode(_ mode: HomeInterfaceMode) {
        guard availableInterfaceModes.contains(mode) else { return }
        interfaceMode = mode
    }
    public var activeFloors: [String] {
        Array(Set(serverFloors + rooms.compactMap { $0.raw["floor"].text })).sorted {
            $0.localizedStandardCompare($1) == .orderedAscending
        }
    }
    public var configurationStale: Bool { !baseRevision.isEmpty && baseRevision != revision }
    public var editableRooms: [HomeRecord] {
        deviceDocument.map { $0["rooms"].values.map(HomeRecord.init) } ?? rooms
    }
    public var editableDevices: [HomeRecord] { deviceDocument?["devices"].values.map(HomeRecord.init) ?? [] }
    public var editableKeypads: [HomeRecord] { deviceDocument?["keypads"].values.map(HomeRecord.init) ?? [] }
    public var editableActionEndpoints: [HomeRecord] {
        editableDevices + editableKeypads.flatMap { panel in
            panel.raw["keys"].values.map { key in
                var endpoint = key["led_endpoint"]
                endpoint["name"] = .string("\(panel.name) · \(key["label"].text ?? "按键") LED")
                endpoint["module_id"] = panel.raw["module_id"]; endpoint["channel"] = key["number"]
                return HomeRecord(endpoint)
            }
        }
    }
    public var floors: [String] {
        let catalog = deviceDocument.map { $0["floors"].values.compactMap(\.text) } ?? serverFloors
        return Array(Set(catalog + editableRooms.compactMap { $0.raw["floor"].text })).sorted {
            $0.localizedStandardCompare($1) == .orderedAscending
        }
    }
    public var editableScenes: [HomeRecord] {
        guard let automationDocument else { return [] }
        return ["immediate_scenes", "timed_scenes"].flatMap { key in
            automationDocument[key].values.map { scene in
                var row = scene; row["collection"] = .string(key)
                let metadata = automationDocument["scene_metadata"].values.first { $0["id"] == scene["id"] }
                row["name"] = metadata?["name"] ?? scene["id"]
                row["room"] = metadata?["room"] ?? .null
                return HomeRecord(row)
            }
        }
    }
    public var editableKeypadScenes: [HomeRecord] {
        let ids = Set((automationDocument?["scene_switch_groups"].values ?? []).flatMap {
            $0["members"].values.compactMap { $0["scene"].text }
        })
        return editableScenes.filter { ids.contains($0.id) && $0.raw["enabled"].flag
            && $0.raw["review_evidence"].text?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false }
    }

    public func connect(token credential: String) async {
        await connect(token: credential, restoringOrigin: nil)
    }
    private func connect(token credential: String, restoringOrigin: String?) async {
        guard !busy else { return }
        disconnect()
        savedRecoveryOrigin = restoringOrigin
        restoringCredential = restoringOrigin != nil
        ordinaryBusy = true; error = nil
        let generation = epoch
        defer { if epoch == generation { ordinaryBusy = false } }
        do {
            let endpoint = try HomeServerEndpoint(endpointText)
            guard restoringOrigin == nil || restoringOrigin == endpoint.origin else { throw HomeServerError.expiredRequest }
            guard !credential.isEmpty, !credential.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { throw HomeServerError.notAuthenticated }
            self.endpoint = endpoint; token = credential
            let capabilities = try await request("/api/v1/session")
            guard case .bool = capabilities["can_manage_configuration"] else { throw HomeServerError.invalidResponse }
            canManage = capabilities["can_manage_configuration"].flag
            updateUnifiedKeypadCapability(capabilities)
            defaults?.set(endpoint.origin, forKey: "home.server")
            if restoringOrigin != nil {
                // It is already saved. Recovery must not recreate a credential
                // after the user has explicitly removed it during an await.
                savedCredentialOrigin = endpoint.origin
            } else {
                do {
                    try await credentialStore.save(credential, for: endpoint.origin)
                    guard epoch == generation else { return }
                    savedCredentialOrigin = endpoint.origin
                    credentialStatus = "此服务凭证已保存到本机 Keychain，不参与 iCloud 同步。"
                } catch {
                    guard epoch == generation else { return }
                    credentialStatus = "本次会话可用，但凭证保存失败：\(error.localizedDescription)"
                }
            }
            connected = true
            try await readSnapshots()
            defaults?.set(endpoint.origin, forKey: "home.server")
            message = "已连接服务端 · 配置版本 \(revision)"
            if canManage {
                do { try await readActiveConfiguration() }
                catch { self.error = error.localizedDescription }
            }
            savedRecoveryOrigin = nil; restoringCredential = false
            startEvents()
        } catch {
            guard epoch == generation else { return }
            connected = false; canManage = false; capturedAt = nil
            self.error = error.localizedDescription
            if let origin = restoringOrigin, Self.canRetryConnection(error),
               (try? HomeServerEndpoint(endpointText).origin) == origin {
                savedRecoveryOrigin = origin
                restoringCredential = eventsWanted
                synchronization = eventsWanted ? "服务暂不可用，正在自动连接" : "已暂停同步"
                startEvents()
            } else {
                savedRecoveryOrigin = nil; restoringCredential = false; token = nil
            }
        }
    }
    public func disconnect() {
        clearModuleInventory()
        savedRecoveryOrigin = nil
        restoreGeneration = UUID(); restoringCredential = false
        epoch = UUID(); stopEvents(); interfaceMode = .daily
        token = nil; endpoint = nil; connected = false; canManage = false; ordinaryBusy = false
        priorityMotorStops = []; priorityStopGeneration = UUID(); latestPriorityMessage = nil
        unifiedKeypadActionsSupported = false
        devices = []; rooms = []; scenes = []; drafts = []; serverFloors = []; capturedAt = nil; revision = ""
        snapshotBoot = ""; synchronization = "未连接"
        deviceDocument = nil; automationDocument = nil; preview = nil; draftID = nil
        baseRevision = ""; candidateRevision = ""; draftChanged = false; error = nil
        message = "已断开服务端并清除本次会话令牌；本机 Keychain 凭证保留。"
    }
    private func stopEvents() {
        clearUnifiedKeypadState()
        eventEpoch = UUID(); eventTask?.cancel(); eventTask = nil
        receivingPush = false; eventCursor = nil
    }
    public func pause() {
        eventsWanted = false
        if savedRecoveryOrigin != nil {
            // Cancel initial lookup/HTTP completion as well as a retry wait.
            epoch = UUID(); restoreGeneration = UUID()
            restoringCredential = false; ordinaryBusy = false; connected = false; canManage = false
        }
        stopEvents(); capturedAt = nil; preview = nil; synchronization = "已暂停同步"
    }
    public func resume() {
        eventsWanted = true
        if savedRecoveryOrigin != nil { restoringCredential = true }
        startEvents()
    }
    private static func canRetryConnection(_ error: Error) -> Bool {
        if error is HomeRetryableResponse { return true }
        guard let error = error as? URLError else { return false }
        return [.cannotConnectToHost, .notConnectedToInternet, .networkConnectionLost,
                .timedOut, .cannotFindHost, .dnsLookupFailed].contains(error.code)
    }
    private func startEvents() {
        guard eventsWanted, connected || savedRecoveryOrigin != nil, eventTask == nil else { return }
        let generation = eventEpoch
        let recovering = !connected
        eventTask = Task { [weak self] in
            var attempts = recovering ? 1 : 0
            if recovering {
                do { try await Task.sleep(for: .seconds(2)) } catch { return }
            }
            while !Task.isCancelled {
                guard let self, self.eventEpoch == generation,
                      self.connected || self.savedRecoveryOrigin != nil else { return }
                self.synchronization = attempts == 0 ? "正在连接状态推送" : "状态推送断开，正在重连"
                do {
                    // Recovery is read-only, scoped to the saved Origin and foreground lifecycle.
                    if !self.connected {
                        guard let origin = self.savedRecoveryOrigin,
                              (try? HomeServerEndpoint(self.endpointText).origin) == origin else { return }
                        if self.token == nil {
                            let saved = try await self.credentialStore.load(for: origin)
                            guard self.eventEpoch == generation, !Task.isCancelled else { return }
                            guard let saved else { self.disconnect(); return }
                            self.endpoint = try HomeServerEndpoint(origin); self.token = saved
                            self.savedCredentialOrigin = origin
                        }
                    }
                    try await self.readSnapshots()
                    guard self.eventEpoch == generation, !Task.isCancelled else { return }
                    if !self.connected {
                        self.connected = true; self.savedRecoveryOrigin = nil; self.restoringCredential = false
                        self.error = nil; self.message = "已自动恢复服务连接；等待新的状态推送。"
                        if self.canManage {
                            do { try await self.readActiveConfiguration() }
                            catch { self.error = error.localizedDescription }
                        }
                    }
                    guard self.eventEpoch == generation, !Task.isCancelled,
                          let endpoint = self.endpoint, let token = self.token else { return }
                    for try await message in self.eventTransport.events(try endpoint.eventRequest(token: token)) {
                        guard self.eventEpoch == generation, !Task.isCancelled else { return }
                        switch message {
                        case .heartbeat: break // Transport liveness never fabricates device feedback.
                        case .state(let data):
                            try await self.applyEvent(data, generation: generation)
                            attempts = 0
                        }
                    }
                    throw URLError(.networkConnectionLost)
                } catch {
                    guard self.eventEpoch == generation, !Task.isCancelled,
                          self.connected || self.savedRecoveryOrigin != nil else { return }
                    if !self.connected && !Self.canRetryConnection(error) {
                        self.disconnect(); self.error = error.localizedDescription
                        self.synchronization = "自动连接已停止，请核对服务与凭证。"
                        return
                    }
                    self.receivingPush = false; self.eventCursor = nil; self.capturedAt = nil
                    self.clearUnifiedKeypadState()
                    if !self.connected { self.canManage = false; self.restoringCredential = true; self.error = error.localizedDescription }
                    self.synchronization = self.connected ? "状态推送断开，正在重连" : "服务暂不可用，正在自动连接"
                }
                attempts = min(attempts + 1, 4)
                do { try await Task.sleep(for: .seconds(min(10, 1 << attempts))) } catch { return }
            }
        }
    }
    private func applyEvent(_ data: Data, generation: UUID) async throws {
        guard data.count <= 512 * 1024 else { throw HomeServerError.invalidResponse }
        let frame = try JSONDecoder().decode(HomeJSON.self, from: data)
        if frame["type"].text == "unavailable" {
            clearUnifiedKeypadState()
            receivingPush = false; capturedAt = nil; eventCursor = nil
            synchronization = "服务端状态暂不可用"
            return
        }
        guard frame["type"].text == "state", let version = frame["revision"].integerText,
              let boot = frame["cursor"]["boot"].text, !boot.isEmpty,
              frame["cursor"]["revision"].integerText == version,
              frame["cursor"]["sequence"].integerText.flatMap(UInt64.init) != nil,
              frame["cursor"]["transport_epoch"].integerText.flatMap(UInt64.init) != nil,
              frame["cursor"]["scope"].text?.isEmpty == false else { throw HomeServerError.invalidResponse }
        let cursor = frame["cursor"]
        if let previous = eventCursor, previous["boot"] == cursor["boot"],
           previous["revision"] == cursor["revision"], previous["transport_epoch"] == cursor["transport_epoch"],
           previous["scope"] == cursor["scope"],
           UInt64(cursor["sequence"].integerText!)! < UInt64(previous["sequence"].integerText!)! {
            throw HomeServerError.invalidResponse
        }
        if eventCursor == nil || snapshotBoot != boot || revision != version
            || eventCursor?["transport_epoch"] != cursor["transport_epoch"]
            || eventCursor?["scope"] != cursor["scope"] {
            guard frame["reset"].flag else { throw HomeServerError.invalidResponse }
        }
        let incoming = try records(frame["devices"]), nextScenes = try records(frame["scenes"])
        if frame["reset"].flag { clearUnifiedKeypadState() }
        // A publication/restart changes room and permission metadata too.
        if revision != version || snapshotBoot != boot || (eventCursor != nil && eventCursor?["scope"] != cursor["scope"]) {
            try await readSnapshots()
            guard revision == version, snapshotBoot == boot else { throw HomeServerError.staleConfiguration }
        }
        guard eventEpoch == generation, !Task.isCancelled else { throw HomeServerError.expiredRequest }
        var next = frame["reset"].flag ? [] : devices
        for record in incoming {
            if let index = next.firstIndex(where: { $0.id == record.id }) { next[index] = record }
            else { next.append(record) }
        }
        let nextAway: [HomeAwayOperation]
        if frame["away_operations"] == .null { nextAway = [] }
        else {
            guard case .array(let rows) = frame["away_operations"], rows.count <= 8 else { throw HomeServerError.invalidResponse }
            nextAway = try rows.map { try HomeAwayOperation($0, boot: boot, revision: version) }
        }
        devices = next; scenes = nextScenes; eventCursor = cursor
        synchronizeUnifiedKeypadContext()
        updateUnifiedKeypadHints(incoming, seedEvents: frame["reset"].flag, clearCues: frame["reset"].flag)
        awayOperations = nextAway
        reconcileSceneActions()
        stateSerial &+= 1; capturedAt = Date(); receivingPush = true
        synchronization = "服务端实时推送"
    }
    public func refresh() async {
        guard connected else { return }
        let generation = epoch
        do { try await readSnapshots() }
        catch {
            if epoch == generation { self.error = error.localizedDescription; capturedAt = nil }
        }
    }
    private func readSnapshots() async throws {
        let serial = stateSerial
        let capabilities = try await request("/api/v1/session")
        guard case .bool = capabilities["can_manage_configuration"] else { throw HomeServerError.invalidResponse }
        canManage = capabilities["can_manage_configuration"].flag
        updateUnifiedKeypadCapability(capabilities)
        if !canManage {
            clearModuleInventory()
            interfaceMode = .daily
            deviceDocument = nil; automationDocument = nil; preview = nil; draftID = nil; drafts = []
            draftChanged = false; baseRevision = ""; candidateRevision = ""
        }
        let d = try await request("/api/v1/devices")
        let s = try await request("/api/v1/scenes")
        let r = try await request("/api/v1/rooms")
        guard let version = d["revision_text"].text ?? d["revision"].integerText,
              s["revision"].integerText == version, r["revision"].integerText == version,
              d["boot"] == r["boot"], !version.isEmpty else { throw HomeServerError.staleConfiguration }
        if revision != version { preview = nil }
        let nextDevices = try records(d["devices"]), nextRooms = try records(r["rooms"]), nextScenes = try records(s["scenes"])
        // A later pushed state wins over an older HTTP request that completed late.
        try Task.checkCancellation()
        guard serial == stateSerial else { return }
        if revision != version || snapshotBoot != d["boot"].text { clearModuleInventory(); clearUnifiedKeypadState() }
        revision = version; devices = nextDevices; rooms = nextRooms; scenes = nextScenes
        serverFloors = r["floors"].values.compactMap(\.text)
        snapshotBoot = d["boot"].text ?? ""
        synchronizeUnifiedKeypadContext()
        updateUnifiedKeypadHints(nextDevices, seedEvents: true)
        capturedAt = Date()
    }
    public func canControlDevice(_ id: String) -> Bool {
        controlsAvailable && devices.contains { $0.id == id && $0.raw["can_control"].flag && $0.targetAvailable }
    }
    public func canControlScene(_ id: String) -> Bool {
        controlsAvailable && scenes.contains { $0.id == id && $0.raw["can_control"].flag && $0.targetAvailable }
    }
    private func clearModuleInventory() {
        moduleLoadID = UUID(); loadingModules = false; moduleInventory = []
        moduleInventoryRevision = ""; moduleInventoryBoot = ""; moduleInventoryReady = false
        moduleInventoryMessage = ""
    }
    public func loadPhysicalModules() async {
        guard connected, canManage, !loadingModules else { return }
        let generation = epoch, load = UUID(), version = revision, boot = snapshotBoot
        guard UInt64(version) != nil, !boot.isEmpty else { return }
        moduleLoadID = load; loadingModules = true
        defer { if moduleLoadID == load { loadingModules = false } }
        do {
            var all: [HomePhysicalModule] = [], seen = Set<String>(), offset = 0, ready = true
            for _ in 0..<256 {
                let page = try await request("/api/v1/modules?offset=\(offset)&limit=64&revision=\(version)", optionalModuleInventory: true)
                guard epoch == generation, moduleLoadID == load, canManage,
                      revision == version, snapshotBoot == boot else { throw HomeServerError.expiredRequest }
                if page == .null {
                    moduleInventory = []; moduleInventoryReady = false
                    moduleInventoryMessage = "当前服务未提供可访问的硬件清单。"; return
                }
                guard page["revision"].integerText == version, page["boot"].text == boot,
                      case .bool(let pageReady) = page["ready"],
                      case .array(let rows) = page["modules"], rows.count <= 64 else { throw HomeServerError.invalidResponse }
                ready = ready && pageReady
                for row in rows {
                    let module = try HomePhysicalModule(row)
                    guard seen.insert(module.id).inserted else { throw HomeServerError.invalidResponse }
                    all.append(module)
                }
                if page["next_offset"] == .null {
                    moduleInventory = all; moduleInventoryRevision = version; moduleInventoryBoot = boot
                    moduleInventoryReady = ready; moduleInventoryMessage = "硬件清单已读取；通道状态为读取时的快照。"
                    return
                }
                guard let next = page["next_offset"].integerText.flatMap(Int.init),
                      next == offset + rows.count, next > offset, next <= 16320 else { throw HomeServerError.invalidResponse }
                offset = next
            }
            throw HomeServerError.invalidResponse
        } catch {
            guard epoch == generation, moduleLoadID == load, !Task.isCancelled else { return }
            moduleInventory = []; moduleInventoryReady = false
            moduleInventoryMessage = "无法读取完整硬件清单，请刷新后重试。"
        }
    }
    private var moduleCommandsAvailable: Bool {
        canManage && controlsAvailable && moduleInventoryReady && moduleInventoryRevision == revision && moduleInventoryBoot == snapshotBoot
    }
    public func canQueryPhysicalModule(_ id: String) -> Bool {
        moduleCommandsAvailable && moduleInventory.contains { $0.id == id && $0.moduleID != nil && $0.supportsQuery }
    }
    public func canControlPhysicalModule(_ id: String, channel: Int, command: HomeJSON) -> Bool {
        guard moduleCommandsAvailable, let module = moduleInventory.first(where: { $0.id == id && $0.moduleID != nil }),
              module.permits(channel: channel, command: command),
              let binding = module.slots.first(where: { $0["channel"].integerText == String(channel) })?["binding"]["id"].text else { return false }
        return canControlDevice(binding)
    }
    public func queryPhysicalModule(_ id: String, channel: Int? = nil) async {
        guard canQueryPhysicalModule(id), let module = moduleInventory.first(where: { $0.id == id }),
              channel.map({ (1...module.channelCount).contains($0) }) ?? true else { return }
        let scope: HomeJSON = channel.map { .object(["kind": .string("channel"), "channel": .integer(Int64($0))]) } ?? .object(["kind": .string("whole_module")])
        await submitModuleRequest("/api/v1/module-queries", module: module, extra: ["scope": scope], message: "查询已受理，等待模块反馈；可再次读取硬件清单核对。")
    }
    public func controlPhysicalModule(_ id: String, channel: Int, command: HomeJSON) async {
        guard canControlPhysicalModule(id, channel: channel, command: command), let module = moduleInventory.first(where: { $0.id == id }) else { return }
        await submitModuleRequest("/api/v1/module-commands", module: module,
            extra: ["channel": .integer(Int64(channel)), "command": command], message: "测试指令已受理，等待实际设备反馈。")
    }
    private func submitModuleRequest(_ path: String, module: HomePhysicalModule, extra: [String: HomeJSON], message: String) async {
        await traced(target: module.id) {
            await self.run {
                guard self.moduleInventoryRevision == self.revision, self.moduleInventoryBoot == self.snapshotBoot,
                      let id = module.moduleID else { throw HomeServerError.staleConfiguration }
                let requestID = UUID().uuidString
                var body = extra; body["module_id"] = .string(id); body["revision"] = .string(self.revision); body["request_id"] = .string(requestID)
                let reply = try await self.request(path, method: "POST", body: .object(body))
                self.message = reply["outcome"].text == "accepted" && reply["request_id"].text == requestID
                    ? message : "指令结果未知，请核对实际设备；未自动重试。"
            }
        }
    }
    public func beginDeviceControl(_ id: String) -> HomeControlOperation {
        let operation = diagnostics.operation(target: id)
        diagnostics.record(.buttonClick, operation: operation)
        return operation
    }
    @discardableResult
    public func controlDevice(_ id: String, command: HomeJSON, operation: HomeControlOperation? = nil) async -> HomeDeviceCommandResult {
        var outcome = HomeDeviceCommandResult.notSent
        await traced(operation, target: id) {
            if command == .object(["action": .string("stop")]) {
                guard self.canStopMotorDevice(id) else { self.diagnostics.record(.ignored, reason: .deviceUnavailable); return }
                outcome = await self.sendPriorityMotorStop(id)
                return
            }
            guard self.canControlDevice(id) else {
                self.diagnostics.record(.ignored, reason: !self.connected ? .disconnected : self.busy ? .busy : !self.snapshotIsCurrent ? .snapshotNotCurrent : .deviceUnavailable)
                return
            }
            outcome = await self.sendDeviceCommand(id, command: command, includeRevision: command["action"].text == "motor_press")
        }
        return outcome
    }
    public func canStopMotorDevice(_ id: String) -> Bool {
        snapshotIsCurrent && devices.contains { $0.id == id && $0.raw["kind"] == .string("Motor")
            && $0.raw["motor_calibrated"] == .bool(true) && $0.raw["can_control"] == .bool(true) && $0.targetAvailable }
    }
    private func retireMotorKeyRequests(_ device: String) {
        for row in devices {
            guard let panel = try? HomeKeypad(record: row) else { continue }
            for key in panel.keys where key.actionDevice == device { keypadActivationState.retireLocalRequest(panel.id, key: key.number) }
        }
    }
    private func sendPriorityMotorStop(_ id: String) async -> HomeDeviceCommandResult {
        let requestID = UUID(), generation = epoch, stopGeneration = UUID()
        priorityStopGeneration = stopGeneration; priorityMotorStops.insert(requestID)
        latestPriorityMessage = "停止请求正在提交。"; message = latestPriorityMessage!; error = nil
        defer { priorityMotorStops.remove(requestID) }
        do {
            let result = try await request("/api/v1/device-commands", method: "POST", body: .object([
                "device": .string(id), "request_id": .string(requestID.uuidString), "command": .object(["action": .string("stop")])]))
            guard epoch == generation, priorityStopGeneration == stopGeneration else { return .unconfirmed(requestID: requestID.uuidString) }
            let accepted = result["outcome"] == .string("accepted") && result["request_id"] == .string(requestID.uuidString)
            if accepted { retireMotorKeyRequests(id) }
            latestPriorityMessage = accepted ? "停止请求已受理，等待设备反馈。" : "停止结果未知，请核对设备；未自动重试。"
            message = latestPriorityMessage!
            if !receivingPush { try await readSnapshots() }
            return accepted ? .accepted(requestID: requestID.uuidString) : .unconfirmed(requestID: requestID.uuidString)
        } catch {
            diagnostics.record(.workFailed, error: error)
            if epoch == generation, priorityStopGeneration == stopGeneration {
                self.error = error.localizedDescription; latestPriorityMessage = "停止结果未知，请核对设备；未自动重试。"; message = latestPriorityMessage!
            }
            return .unconfirmed(requestID: requestID.uuidString)
        }
    }
    @discardableResult
    private func sendDeviceCommand(_ id: String, command: HomeJSON, includeRevision: Bool = false) async -> HomeDeviceCommandResult {
        var outcome = HomeDeviceCommandResult.notSent
        await run {
            let requestID = UUID().uuidString
            var body: HomeJSON = .object([
                "device": .string(id), "request_id": .string(requestID), "command": command
            ])
            if includeRevision { body["revision"] = .string(self.revision) }
            outcome = .unconfirmed(requestID: requestID)
            let result = try await self.request("/api/v1/device-commands", method: "POST", body: body)
            if result["outcome"].text == "accepted", result["request_id"].text == requestID {
                outcome = .accepted(requestID: requestID)
            }
            self.message = result["outcome"].text == "accepted" ? "控制已受理，等待实际设备反馈。" : "控制结果未知，请核对实际设备；未自动重试。"
            if !self.receivingPush { try await self.readSnapshots() }
        }
        return outcome
    }
    private func updateUnifiedKeypadCapability(_ session: HomeJSON) {
        let supported = HomeKeypadActionCapability.supported(session["capabilities"]["keypad_actions"])
        if unifiedKeypadActionsSupported && !supported { clearUnifiedKeypadState() }
        unifiedKeypadActionsSupported = supported
    }
    private func clearUnifiedKeypadState() {
        pendingKeypadActivations = [:]; keypadActivationContext = nil
        keypadActivationState.reset(); keypadEventVersions = [:]; keypadEventStates = [:]
        sceneActionHints = [:]; sceneCueVersions = [:]; awayOperations = []
    }
    private func synchronizeUnifiedKeypadContext() {
        guard unifiedKeypadActionsSupported, !snapshotBoot.isEmpty, !revision.isEmpty else { return }
        if keypadActivationContext?.boot != snapshotBoot || keypadActivationContext?.revision != revision {
            clearUnifiedKeypadState()
            let context = HomeKeypadActivationState.Context(generation: UUID(), boot: snapshotBoot, revision: revision)
            keypadActivationContext = context; keypadActivationState.reset(to: context)
        }
    }
    private func keypadEventID(_ panel: String, _ number: Int) -> String { "\(panel.utf8.count):\(panel):\(number)" }
    private func updateUnifiedKeypadHints(_ records: [HomeRecord], seedEvents: Bool, clearCues: Bool = false) {
        guard unifiedKeypadActionsSupported, let context = keypadActivationContext else { return }
        for panel in records where panel.raw["kind"].text == "Keypad" {
            for raw in panel.raw["keys"].values {
                guard let number = raw["number"].integerText.flatMap(Int.init), (1...6).contains(number),
                      let endpoint = raw["endpoint_id"].text, !endpoint.isEmpty else { continue }
                if raw["indication"] == .null {
                    keypadActivationState.clearHint(panel.id, key: number, context: context)
                } else if let cue = try? HomeKeypadActionIndication(raw["indication"], endpoint: endpoint, boot: context.boot) {
                    let merged = keypadActivationState.merge(cue, panel: panel.id, key: number, endpoint: endpoint, context: context)
                    let eventID = keypadEventID(panel.id, number)
                    if seedEvents { sceneCueVersions[eventID] = max(sceneCueVersions[eventID] ?? 0, cue.operationSequence) }
                    if !seedEvents, !clearCues, merged, cue.operationSequence >= (sceneCueVersions[eventID] ?? 0) {
                        sceneCueVersions[eventID] = cue.operationSequence
                        if let scene = raw["action"]["scene"].text {
                            if [.pending, .written].contains(cue.status) {
                                let selection = scenes.first(where: { $0.id == scene })?.raw["switch_kind"] == .string("selection")
                                showSceneAction(scene, requestID: cue.requestID, selected: selection ? cue.color.flatMap { $0 == .red ? true : $0 == .green ? false : nil } : nil)
                            } else { clearSceneAction(cue.requestID) }
                        }
                    } else if merged, ![.pending, .written].contains(cue.status) { clearSceneAction(cue.requestID) }
                    if clearCues { keypadActivationState.clearHint(panel.id, key: number, context: context) }
                } else {
                    keypadActivationState.clearHint(panel.id, key: number, context: context)
                }
                let eventID = keypadEventID(panel.id, number), event = raw["key_event"]
                if seedEvents { keypadEventStates.removeValue(forKey: eventID) }
                guard event["source"] == .string("button_feedback"), event["quality"] == .string("Confirmed"),
                      let version = event["version"].text.flatMap(UInt64.init), version > 0,
                      event["observed_at_ms"].text.flatMap(UInt64.init) != nil,
                      case .bool(let pressed) = event["pressed"] else { continue }
                if !seedEvents, version > (keypadEventVersions[eventID] ?? 0) { keypadEventStates[eventID] = pressed }
                keypadEventVersions[eventID] = max(keypadEventVersions[eventID] ?? 0, version)
            }
        }
    }
    public func keypadActionPresentation(_ panel: String, key: Int) -> HomeKeypadActionPresentation? {
        keypadActivationState.presentation(panel, key: key)
            ?? keypadEventStates[keypadEventID(panel, key)].map { HomeKeypadActionPresentation(phase: $0 ? .pressed : .released) }
    }
    public func keypadEventIsPressed(_ panel: String, key: Int) -> Bool {
        snapshotIsCurrent && keypadEventStates[keypadEventID(panel, key)] == true
    }
    public var unifiedKeypadSupportMessage: String? {
        connected && !unifiedKeypadActionsSupported ? "当前服务未提供兼容的统一按键接口，请更新服务后使用按键动作。" : nil
    }
    public func unifiedKeypadGate(_ id: String, key number: Int) -> HomeKeypadGateState {
        unifiedKeypadGate(id, key: number, owning: nil)
    }
    private func unifiedKeypadGate(_ id: String, key number: Int, owning operationID: UUID?) -> HomeKeypadGateState {
        var state = HomeKeypadGateState()
        state.connected = connected; state.busy = busy
        state.current = snapshotIsCurrent; state.receivingPush = receivingPush
        if !connected { state.reason = .disconnected; return state }
        if !snapshotIsCurrent { state.reason = .snapshotNotCurrent; return state }
        guard unifiedKeypadActionsSupported else { state.reason = .unifiedKeypadUnsupported; return state }
        guard let row = devices.first(where: { $0.id == id }) else { state.reason = .panelMissing; return state }
        guard let panel = try? HomeKeypad(record: row), let key = panel.keys.first(where: { $0.number == number }),
              key.endpointID != nil, let raw = row.raw["keys"].values.first(where: { $0["number"] == .integer(Int64(number)) }) else {
            state.reason = .panelInvalid; return state
        }
        state.configured = raw["configured"] == .bool(true)
        // Motor gestures must reach server-side repeat-to-stop/STOP admission
        // even while an unrelated request waits for its HTTP receipt.
        let motorGesture = key.actionDevice != nil && key.motorAction != nil
        state.busy = busy && operationID.flatMap({ pendingKeypadActivations[$0] }) == nil && !motorGesture
        state.canControl = HomeKeypadActionCapability.available(raw["activation"], panel: id, key: number, revision: revision)
        state.targetAvailable = row.targetAvailable && state.canControl
        if !state.configured || !state.canControl || !state.targetAvailable { state.reason = .deviceUnavailable }
        else if state.busy { state.reason = .busy }
        return state
    }
    public func canActivateUnifiedKeypad(_ id: String, key number: Int) -> Bool { unifiedKeypadGate(id, key: number).allowed }
    public func canDisplayKeypadActionCue(_ id: String, key number: Int) -> Bool {
        let state = unifiedKeypadGate(id, key: number)
        return state.connected && state.current && state.configured && state.canControl && state.targetAvailable
    }
    public func unifiedKeypadGateStates(_ id: String) -> [HomeKeypadGateState] { (1...6).map { unifiedKeypadGate(id, key: $0) } }
    public func recordUnifiedKeypadGates(_ id: String) {
        let scope = "unified:" + id, states = unifiedKeypadGateStates(id), previous = loggedKeypadGates["unified:" + id]
        for (index, state) in states.enumerated() where previous?[index] != state {
            diagnostics.record(.gateState, operation: diagnostics.operation(target: id, key: index + 1), gate: state)
        }
        loggedKeypadGates[scope] = states
    }
    /// Reserves the click synchronously, so the pending layer appears before Task scheduling.
    public func beginUnifiedKeypadClick(_ id: String, key number: Int) -> HomeControlOperation {
        let operation = diagnostics.operation(target: id, key: number), gate = unifiedKeypadGate(id, key: number)
        diagnostics.record(.buttonClick, operation: operation, gate: gate)
        guard gate.allowed, let context = keypadActivationContext,
              let row = devices.first(where: { $0.id == id }), let panel = try? HomeKeypad(record: row),
              let endpoint = panel.keys.first(where: { $0.number == number })?.endpointID,
              let request = try? HomeKeypadActivationRequest(revision: revision, keypad: id, key: number) else { return operation }
        let sceneKey = panel.keys.first(where: { $0.number == number })
        let scene = sceneKey?.actionScene.flatMap { id in scenes.first { $0.id == id } }
        let nextSelection: Bool? = scene?.raw["switch_kind"] == .string("selection") ? scene?.raw["selected"] != .bool(true) : nil
        do { try keypadActivationState.begin(request, endpoint: endpoint, context: context, color: nextSelection.map { $0 ? .red : .green }) }
        catch { return operation }
        if let scene { showSceneAction(scene.id, requestID: request.requestID.uuidString, selected: nextSelection) }
        if panel.keys.first(where: { $0.number == number })?.actionDevice != nil {
            priorityStopGeneration = UUID()
            latestPriorityMessage = "窗帘按键请求正在提交。"; message = latestPriorityMessage!
        }
        pendingKeypadActivations[operation.id] = PendingKeypadActivation(operation: operation, request: request, context: context,
            controlGeneration: priorityStopGeneration)
        error = nil
        return operation
    }
    public func activateUnifiedKeypad(_ id: String, key number: Int, operation supplied: HomeControlOperation? = nil) async {
        let operation = supplied ?? beginUnifiedKeypadClick(id, key: number)
        await traced(operation, target: id, key: number) {
            let gate = self.unifiedKeypadGate(id, key: number, owning: operation.id)
            self.diagnostics.record(.gateCheck, gate: gate)
            guard let pending = self.pendingKeypadActivations[operation.id],
                  pending.request.keypad == id, pending.request.key == number else {
                self.diagnostics.record(.ignored, gate: gate); return
            }
            defer {
                self.pendingKeypadActivations.removeValue(forKey: operation.id)
            }
            guard gate.allowed else { self.keypadActivationState.fail(pending.request, context: pending.context, uncertain: false); self.clearSceneAction(pending.request.requestID.uuidString); return }
            let stopGeneration = pending.controlGeneration
            do {
                let result = try await self.request(HomeKeypadActivationRequest.path, method: "POST", body: pending.request.body)
                guard self.keypadActivationContext == pending.context, stopGeneration == self.priorityStopGeneration else { return }
                let applied = try self.keypadActivationState.complete(result, request: pending.request, context: pending.context)
                guard applied, stopGeneration == self.priorityStopGeneration else { return }
                if result["outcome"] != .string("accepted") { self.clearSceneAction(pending.request.requestID.uuidString) }
                else if let row = self.devices.first(where: { $0.id == id }), let panel = try? HomeKeypad(record: row),
                        let scene = panel.keys.first(where: { $0.number == number })?.actionScene {
                    if let cue = self.keypadActivationState.indication(id, key: number), cue.requestID == pending.request.requestID.uuidString,
                       [.pending, .written].contains(cue.status) {
                        let selection = self.scenes.first(where: { $0.id == scene })?.raw["switch_kind"] == .string("selection")
                        self.showSceneAction(scene, requestID: cue.requestID, selected: selection ? cue.color.flatMap { $0 == .red ? true : $0 == .green ? false : nil } : nil)
                    } else { self.clearSceneAction(pending.request.requestID.uuidString) }
                }
                self.message = result["outcome"] == .string("accepted")
                    ? "按键请求已受理；本键提示与实际设备反馈分别显示。" : "按键执行结果未知，请核对设备；未自动重试。"
                if self.latestPriorityMessage != nil { self.latestPriorityMessage = self.message }
                if !self.receivingPush { try await self.readSnapshots() }
            } catch {
                self.diagnostics.record(.workFailed, error: error)
                guard self.keypadActivationContext == pending.context, stopGeneration == self.priorityStopGeneration else { return }
                let rejected = (error as? HomeKeypadActionHTTPFailure)?.rejected == true
                let applied = self.keypadActivationState.fail(pending.request, context: pending.context, uncertain: !rejected)
                guard applied, stopGeneration == self.priorityStopGeneration else { return }
                self.clearSceneAction(pending.request.requestID.uuidString)
                self.error = error.localizedDescription
                self.message = rejected ? "按键请求未受理；没有改用旧接口。" : "按键执行结果未知，请核对设备；未自动重试。"
                if self.latestPriorityMessage != nil { self.latestPriorityMessage = self.message }
                if !rejected { self.capturedAt = nil }
            }
        }
    }

    /// Compatibility API; real six-key UI uses activateUnifiedKeypad exclusively.
    public func keypadGate(_ id: String, key number: Int, card: Bool = false) -> HomeKeypadGateState {
        var state = HomeKeypadGateState()
        state.connected = connected; state.busy = busy
        state.current = snapshotIsCurrent; state.receivingPush = receivingPush
        let factRow = devices.first { $0.id == id }
        let factPanel = factRow.flatMap { try? HomeKeypad(record: $0) }
        let factKey = factPanel?.keys.first { $0.number == number && $0.canActivate }
        let factTarget = factKey?.actionScene.flatMap { scene in scenes.first { $0.id == scene } }
            ?? factKey?.actionDevice.flatMap { device in devices.first { $0.id == device } }
        state.configured = factKey != nil
        state.canControl = factTarget?.raw["can_control"].flag == true
        let pressNeedsParent = card && factKey?.cardAction == HomeKeypadCardAction.motorPress
        state.targetAvailable = factTarget?.targetAvailable == true && (!pressNeedsParent || factRow?.targetAvailable == true)
        if factKey?.cardAction == .awayOff {
            state.canControl = factKey?.awayControlAllowed == true
            state.targetAvailable = factKey?.awayTargetAvailable == true && factRow?.targetAvailable == true
        }
        if case .bool(let value) = factTarget?.raw["selected"] { state.selected = value }
        state.relativeUnknownAllowed = factTarget?.raw["relative_unknown_allowed"].flag == true
        if !connected { state.reason = .disconnected; return state }
        if busy { state.reason = .busy; return state }
        if !snapshotIsCurrent { state.reason = .snapshotNotCurrent; return state }
        guard let row = devices.first(where: { $0.id == id }) else { state.reason = .panelMissing; return state }
        guard let panel = try? HomeKeypad(record: row) else { state.reason = .panelInvalid; return state }
        guard let key = panel.keys.first(where: { $0.number == number && $0.canActivate }) else { state.reason = .actionMissing; return state }
        state.configured = true
        if card && (!key.canActivateCard || ([HomeKeypadCardAction.motorPress, .awayOff].contains(key.cardAction) && UInt64(revision).map { $0 > 0 } != true)) {
            state.reason = .cardActionInvalid; return state
        }
        if key.cardAction == .awayOff {
            // The server's per-key gate covers the complete reviewed Away
            // scope. It is independent of scene selection or parent grants.
            if !card { state.reason = .cardActionInvalid }
            else if !state.canControl || !state.targetAvailable { state.reason = .deviceUnavailable }
            return state
        }
        if let device = key.actionDevice {
            state.canControl = canControlDevice(device)
            state.targetAvailable = devices.first { $0.id == device }?.targetAvailable == true && (!pressNeedsParent || row.targetAvailable)
            if !state.canControl || !state.targetAvailable { state.reason = .deviceUnavailable }
            else if !devices.contains(where: { $0.id == device && $0.raw["kind"].text == "Motor" && $0.raw["motor_calibrated"].flag }) { state.reason = .motorNotCalibrated }
            return state
        }
        guard let sceneID = key.actionScene, let scene = scenes.first(where: { $0.id == sceneID }) else { state.reason = .sceneUnavailable; return state }
        state.canControl = canControlScene(sceneID); state.targetAvailable = scene.targetAvailable
        if case .bool(let value) = scene.raw["selected"] { state.selected = value }
        state.relativeUnknownAllowed = scene.raw["relative_unknown_allowed"].flag
        if !state.canControl { state.reason = .sceneUnavailable }
        else if !key.executesScene && state.selected == nil && !state.relativeUnknownAllowed { state.reason = .selectionUnknown }
        return state
    }
    public func canActivateKeypad(_ id: String, key number: Int, card: Bool = false) -> Bool { keypadGate(id, key: number, card: card).allowed }
    public func keypadGateStates(_ id: String, card: Bool = false) -> [HomeKeypadGateState] { (1...6).map { keypadGate(id, key: $0, card: card) } }
    public func recordKeypadGateStates(_ id: String, card: Bool = false) {
        let scope = (card ? "card:" : "explicit:") + id
        let states = keypadGateStates(id, card: card), previous = loggedKeypadGates[scope]
        for (index, state) in states.enumerated() where previous?[index] != state {
            diagnostics.record(.gateState, operation: diagnostics.operation(target: id, key: index + 1), gate: state)
        }
        loggedKeypadGates[scope] = states
    }
    /// Called synchronously by the actual SwiftUI Button action, before its Task.
    /// A disabled Button dispatches no action; only gate.state can describe it.
    public func beginKeypadClick(_ id: String, key number: Int, card: Bool = false) -> HomeControlOperation {
        let operation = diagnostics.operation(target: id, key: number)
        diagnostics.record(.buttonClick, operation: operation, gate: keypadGate(id, key: number, card: card))
        return operation
    }
    private func traced(_ supplied: HomeControlOperation? = nil, target: String, key: Int? = nil, work: () async -> Void) async {
        if HomeControlContext.operation != nil { await work(); return }
        let operation = supplied ?? diagnostics.operation(target: target, key: key)
        await HomeControlContext.$operation.withValue(operation) {
            diagnostics.record(.operationBegin)
            defer { diagnostics.record(.operationComplete) }
            await work()
        }
    }
    public func activateKeypad(_ id: String, key number: Int, operation: HomeControlOperation? = nil) async {
        await traced(operation, target: id, key: number) { await self.activateKeypadAction(id, key: number) }
    }
    /// Card-only conditional press. The server, never a cached estimate or LED,
    /// decides whether the reviewed direction becomes STOP in Admission.
    public func activateKeypadCard(_ id: String, key number: Int, operation: HomeControlOperation? = nil) async {
        await traced(operation, target: id, key: number) { await self.activateKeypadAction(id, key: number, card: true) }
    }
    private func activateKeypadAction(_ id: String, key number: Int, card: Bool = false) async {
        let gate = keypadGate(id, key: number, card: card)
        diagnostics.record(.gateCheck, gate: gate)
        guard gate.allowed else { diagnostics.record(.ignored, gate: gate); return }
        guard controlsAvailable, let row = devices.first(where: { $0.id == id }),
              let panel = try? HomeKeypad(record: row),
              let key = panel.keys.first(where: { $0.number == number && $0.canActivate }) else { return }
        if card && key.cardAction == .awayOff {
            await run {
                let result = try await self.request("/api/v1/away-off", method: "POST", body: .object([
                    "revision": .string(self.revision), "request_id": .string(UUID().uuidString),
                    "command": .object(["action": .string("keypad_press"), "keypad": .string(id), "key": .integer(Int64(number))])
                ]))
                self.message = result["outcome"].text == "accepted"
                    ? "离家请求已受理，等待服务端执行及实际设备反馈。"
                    : "离家结果未知，请核对实际设备；未自动重试。"
                if !self.receivingPush { try await self.readSnapshots() }
            }
            return
        }
        if let device = key.actionDevice, let action = key.motorAction {
            guard canControlDevice(device), devices.contains(where: { $0.id == device && $0.raw["kind"].text == "Motor"
                && $0.raw["motor_calibrated"].flag }) else { return }
            if card && key.cardAction == .motorPress {
                await sendDeviceCommand(device, command: .object([
                    "action": .string("keypad_press"), "keypad": .string(id), "key": .integer(Int64(number))
                ]), includeRevision: true)
            } else {
                await sendDeviceCommand(device, command: .object(["action": .string(action)]))
            }
            return
        }
        guard let scene = key.actionScene, canControlScene(scene) else { return }
        if key.executesScene {
            await controlScene(scene, on: true)
            return
        }
        if let record = scenes.first(where: { $0.id == scene }), case .bool(let selected) = record.raw["selected"] {
            await controlScene(scene, on: !selected)
            return
        }
        diagnostics.record(.decision) // Selected state is unknown; existing relative path is unchanged.
        await run {
            let result = try await self.request("/api/v1/scene-relative-actions", method: "POST", body: .object([
                "scene": .string(scene), "request_id": .string(UUID().uuidString)
            ]))
            self.message = "场景映射请求：\(result["status"].text ?? result["outcome"].text ?? "结果未知")；请查看后续反馈，未自动重试。"
            if !self.receivingPush { try await self.readSnapshots() }
        }
    }
    public func canWriteKeypadLED(_ id: String, key number: Int) -> Bool {
        guard controlsAvailable, let row = devices.first(where: { $0.id == id }), row.targetAvailable,
              let panel = try? HomeKeypad(record: row) else { return false }
        return panel.keys.contains { $0.number == number && $0.canWriteLED && $0.endpointID != nil }
    }
    public func canWriteKeypadSetting(_ id: String, field: String) -> Bool {
        guard controlsAvailable, canManage, ["backlight", "sound", "work_mode"].contains(field),
              let row = devices.first(where: { $0.id == id }), row.targetAvailable,
              let panel = try? HomeKeypad(record: row), panel.keys.allSatisfy({ $0.endpointID != nil }) else { return false }
        return panel.settings[field]["can_write"].flag
    }
    public func writeKeypadLED(_ id: String, key number: Int, color: HomeKeypadLEDColor) async {
        guard canWriteKeypadLED(id, key: number), color != .unknown,
              let row = devices.first(where: { $0.id == id }), let panel = try? HomeKeypad(record: row),
              panel.ledColors.contains(color),
              let key = panel.keys.first(where: { $0.number == number && $0.canWriteLED }),
              let endpoint = key.endpointID else { return }
        let command: HomeJSON = color == .off ? .object(["action": .string("panel_led_off")])
            : .object(["action": .string("panel_led"), "on": .bool(color == .red)])
        await sendDeviceCommand(endpoint, command: command)
    }
    public func writeKeypadSetting(_ id: String, command: HomeJSON) async {
        guard controlsAvailable, canManage, let row = devices.first(where: { $0.id == id }),
              let panel = try? HomeKeypad(record: row), panel.keys.allSatisfy({ $0.endpointID != nil }),
              let action = command["action"].text else { return }
        let setting: String
        switch action {
        case "read_backlight": setting = "backlight"
        case "backlight":
            guard ["dim_after_ten_seconds", "bright", "dim"].contains(command["mode"].text ?? "") else { return }
            setting = "backlight"
        case "read_sound": setting = "sound"
        case "sound":
            guard case .bool = command["enabled"] else { return }; setting = "sound"
        case "read_work_mode": setting = "work_mode"
        case "work_mode":
            guard ["manual", "self_reset"].contains(command["mode"].text ?? ""),
                  command["mode"] == panel.settings["work_mode"]["configured"] else { return }
            setting = "work_mode"
        default: return
        }
        guard canWriteKeypadSetting(id, field: setting) else { return }
        await run {
            let result = try await self.request("/api/v1/keypad-settings", method: "POST", body: .object([
                "keypad": .string(id), "revision": .string(self.revision), "request_id": .string(UUID().uuidString), "command": command
            ]))
            self.message = "参数请求：\(result["outcome"].text ?? "结果未知")；实测值仍以已核实读回为准，未自动重试。"
            if !self.receivingPush { try await self.readSnapshots() }
        }
    }
    public func controlScene(_ id: String, on: Bool?) async {
        await traced(target: id) { await self.controlSceneAction(id, on: on) }
    }
    private func controlSceneAction(_ id: String, on: Bool?) async {
        diagnostics.record(.decision, on: on)
        guard canControlScene(id) else {
            diagnostics.record(.ignored, reason: !connected ? .disconnected : busy ? .busy : !snapshotIsCurrent ? .snapshotNotCurrent : .sceneUnavailable)
            return
        }
        let requestID = UUID().uuidString
        showSceneAction(id, requestID: requestID, selected: on)
        await run {
            var body: HomeJSON = .object(["scene": .string(id), "request_id": .string(requestID)])
            if let on { body["on"] = .bool(on) }
            let result: HomeJSON
            do { result = try await self.request("/api/v1/scene-actions", method: "POST", body: body) }
            catch { self.clearSceneAction(requestID); throw error }
            if result["outcome"].text != "accepted" { self.clearSceneAction(requestID) }
            self.message = result["outcome"].text == "accepted" ? "场景请求已受理，请查看后续状态。" : "场景结果未知，请刷新核对；未自动重试。"
            if !self.receivingPush { try await self.readSnapshots() }
        }
    }

    public func loadActiveConfiguration() async { await run { try await self.readActiveConfiguration() } }
    private func readActiveConfiguration() async throws {
        guard canManage else { throw HomeServerError.denied }
        let catalog = try await request("/api/v1/config/versions?limit=1")
        guard let active = catalog["active_revision"].text, active == revision else { throw HomeServerError.staleConfiguration }
        let exported = try await request("/api/v1/config/versions/\(active)/export")
        guard exported["active_revision"].text == active, exported["revision"].text == active,
              exported["devices"].isObject, exported["automation"].isObject else { throw HomeServerError.missingAutomation }
        guard let base = Int64(active), base > 0, base < Int64.max else { throw HomeServerError.revisionOverflow }
        deviceDocument = exported["devices"]; automationDocument = exported["automation"]
        baseRevision = active; candidateRevision = String(base + 1)
        draftID = nil; draftChanged = false; preview = nil
    }
    public func loadDrafts() async {
        await run {
            guard self.canManage else { throw HomeServerError.denied }
            self.drafts = try self.records(try await self.request("/api/v1/config/drafts")["drafts"])
        }
    }
    public func loadDraft(_ id: String) async {
        await run {
            guard self.canManage, UUID(uuidString: id) != nil else { throw HomeServerError.denied }
            let d = try await self.request("/api/v1/config/drafts/\(id)/source")
            guard let devices = d["devices"].text, let automation = d["automation"].text,
                  let base = d["expected_revision"].text, let revision = d["revision"].text else { throw HomeServerError.invalidResponse }
            self.deviceDocument = try HomeJSON.parse(devices); self.automationDocument = try HomeJSON.parse(automation)
            self.baseRevision = base; self.candidateRevision = revision; self.draftID = id
            self.draftChanged = false; self.preview = nil
            self.message = "已读取服务端草稿，尚未发布。"
        }
    }
    public func saveDraft() async {
        await run {
            try self.requireEditable()
            guard var devices = self.deviceDocument, var automation = self.automationDocument,
                  let base = Int64(self.baseRevision), let next = Int64(self.candidateRevision), next > base else {
                throw HomeServerError.invalidDocument
            }
            devices["revision"] = .integer(next); automation["configuration_revision"] = .integer(next)
            let id = UUID().uuidString
            _ = try await self.request("/api/v1/config/drafts/\(id)", method: "POST", body: .object([
                "expected_revision": .integer(base), "revision": .integer(next), "devices": devices, "automation": automation
            ]))
            self.deviceDocument = devices; self.automationDocument = automation
            self.draftID = id; self.draftChanged = false; self.preview = nil
            self.message = "草稿已通过服务端校验并保存，尚未发布。"
        }
    }
    public func previewDraft() async {
        await run {
            try self.requireEditable()
            guard let id = self.draftID, !self.draftChanged else { throw HomeServerError.invalidDocument }
            self.preview = nil
            let result = try await self.request("/api/v1/config/previews/\(id)")
            guard result["draft"].text?.lowercased() == id.lowercased(), result["impact"]["boot"].text != nil,
                  result["impact"]["active_revision"].integerText == self.baseRevision,
                  result["impact"]["candidate_revision"].integerText == self.candidateRevision else { throw HomeServerError.staleConfiguration }
            self.preview = result
        }
    }
    public func publishDraft() async {
        await run {
            try self.requireEditable()
            guard let preview = self.preview, let id = self.draftID, !self.draftChanged,
                  preview["draft"].text?.lowercased() == id.lowercased(),
                  let boot = preview["impact"]["boot"].text else { throw HomeServerError.staleConfiguration }
            self.preview = nil
            let result = try await self.request("/api/v1/config/activations/\(id)", method: "POST", body: .object([
                "request_id": .string(UUID().uuidString), "boot": .string(boot)
            ]))
            self.message = result["outcome"].text == "activated" ? "新配置已发布，正在刷新服务端状态。" : "发布结果未知，请刷新核对活动版本；未自动重试。"
            try await self.readSnapshots()
            if result["outcome"].text == "activated" { try await self.readActiveConfiguration() }
        }
    }

    public func addFloor(_ name: String) throws {
        try requireEditable(); try validateLabel(name)
        guard !floors.contains(name) else { throw HomeServerError.server("楼层已存在") }
        var document = try requireDevices()
        document["floors"] = .array((floors + [name]).map(HomeJSON.string))
        deviceDocument = document; edited()
    }
    public func upsertRoom(id: String, name: String, floor: String?) throws {
        try requireEditable(); try validateLabel(id); try validateLabel(name)
        if let floor { try validateLabel(floor) }
        var document = try requireDevices()
        var rows = document["rooms"].values
        let row: HomeJSON = .object(["id": .string(id), "name": .string(name), "floor": floor.map(HomeJSON.string) ?? .null])
        if let index = rows.firstIndex(where: { $0["id"].text == id }) { rows[index] = row } else { rows.append(row) }
        document["rooms"] = .array(rows)
        if let floor, !floors.contains(floor) { document["floors"] = .array((floors + [floor]).map(HomeJSON.string)) }
        deviceDocument = document; edited()
    }
    public func removeRoom(_ id: String) throws {
        try requireEditable(); var document = try requireDevices()
        guard !document["devices"].values.contains(where: { $0["room"].text == id }) else { throw HomeServerError.server("room_in_use：请先调整设备归属") }
        guard !editableScenes.contains(where: { $0.roomID == id }) else { throw HomeServerError.server("room_in_use：请先调整场景归属") }
        document["rooms"] = .array(document["rooms"].values.filter { $0["id"].text != id })
        deviceDocument = document; edited()
    }
    public func renameFloor(_ old: String, to next: String?) throws {
        try requireEditable(); if let next { try validateLabel(next) }
        guard floors.contains(old) else { throw HomeServerError.invalidDocument }
        if let next, next != old, floors.contains(next) { throw HomeServerError.server("楼层已存在") }
        var document = try requireDevices()
        document["floors"] = .array((floors.filter { $0 != old } + (next.map { [$0] } ?? [])).map(HomeJSON.string))
        document["rooms"] = .array(document["rooms"].values.map { room in
            var room = room
            if room["floor"].text == old { room["floor"] = next.map(HomeJSON.string) ?? .null }
            return room
        })
        deviceDocument = document; edited()
    }
    public func upsertDevice(_ row: HomeJSON, originalID: String? = nil) throws {
        try requireEditable(); guard row.isObject, let id = row["id"].text, let name = row["name"].text else { throw HomeServerError.invalidDocument }
        try validateLabel(id); try validateLabel(name)
        guard originalID == nil || originalID == id else { throw HomeServerError.invalidDocument }
        var document = try requireDevices(); var rows = document["devices"].values
        if let index = rows.firstIndex(where: { $0["id"].text == id }) {
            guard originalID == id else { throw HomeServerError.invalidDocument }
            let previous = rows[index]
            let changedBinding = ["module_id", "gateway", "address", "channel"].contains { previous[$0] != row[$0] }
            rows[index] = changedBinding ? row.removing(["motor_calibration", "curtain_state"]) : row
        } else { rows.append(row) }
        document["devices"] = .array(rows); deviceDocument = document; edited()
    }
    public func removeDevice(_ id: String) throws {
        try requireEditable(); var document = try requireDevices()
        document["devices"] = .array(document["devices"].values.filter { $0["id"].text != id })
        deviceDocument = document; edited()
    }
    public func upsertKeypad(_ row: HomeJSON, originalID: String? = nil) throws {
        try requireEditable()
        guard row.isObject, let id = row["id"].text, let name = row["name"].text,
              let module = row["module_id"].text, originalID == nil || originalID == id,
              deviceDocument?["schema_version"].integerText == "2" else { throw HomeServerError.invalidDocument }
        try validateLabel(id); try validateLabel(name); try validateLabel(module)
        let keys = row["keys"].values
        let numbers = keys.compactMap { $0["number"].integerText.flatMap(Int.init) }
        let endpoints = keys.compactMap { $0["led_endpoint"]["id"].text }
        guard !keys.isEmpty, keys.count <= 6, numbers.count == keys.count,
              numbers.allSatisfy({ (1...6).contains($0) }), Set(numbers).count == keys.count,
              endpoints.count == keys.count, Set(endpoints).count == keys.count else { throw HomeServerError.invalidDocument }
        for key in keys {
            try validateLabel(key["label"].text ?? "")
            try validateLabel(key["led_endpoint"]["id"].text ?? "")
            try validateLabel(key["led_endpoint"]["name"].text ?? "")
            if key["action"] != .null {
                switch key["action"]["kind"].text {
                case "scene_toggle": try validateLabel(key["action"]["scene"].text ?? "")
                case "motor":
                    try validateLabel(key["action"]["device"].text ?? "")
                    guard ["open", "stop", "close"].contains(key["action"]["action"].text ?? "") else { throw HomeServerError.invalidDocument }
                case "away_off":
                    // Preserve an existing reviewed server-owned scope. The
                    // editor cannot create/copy an Away action to another key.
                    let previous = deviceDocument?["keypads"].values.first { $0["id"].text == originalID }?["keys"].values.first { $0["number"] == key["number"] }
                    guard originalID == id, previous?["action"] == key["action"] else { throw HomeServerError.invalidDocument }
                default: throw HomeServerError.invalidDocument
                }
                let review = key["action"]["review_evidence"].text ?? ""
                guard !review.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                      review.utf8.count <= 4096 else { throw HomeServerError.invalidDocument }
            }
        }
        var document = try requireDevices(), rows = document["keypads"].values
        if let index = rows.firstIndex(where: { $0["id"].text == id }) {
            guard originalID == id else { throw HomeServerError.invalidDocument }
            rows[index] = row
        } else { rows.append(row) }
        document["keypads"] = .array(rows); deviceDocument = document; edited()
    }
    public func removeKeypad(_ id: String) throws {
        try requireEditable(); var document = try requireDevices()
        document["keypads"] = .array(document["keypads"].values.filter { $0["id"].text != id })
        deviceDocument = document; edited()
    }
    public func upsertScene(_ row: HomeJSON, collection: String, name: String, originalID: String? = nil, room: String? = nil) throws {
        try requireEditable(); try validateLabel(name)
        guard ["immediate_scenes", "timed_scenes"].contains(collection), row.isObject,
              let id = row["id"].text, var document = automationDocument else { throw HomeServerError.invalidDocument }
        try validateLabel(id)
        guard originalID == nil || originalID == id else { throw HomeServerError.invalidDocument }
        if originalID == nil, editableScenes.contains(where: { $0.id == id }) { throw HomeServerError.invalidDocument }
        if let room, !room.isEmpty {
            try validateLabel(room)
            guard editableRooms.contains(where: { $0.id == room }) else { throw HomeServerError.invalidDocument }
        }
        var rows = document[collection].values
        // Presentation fields belong only to scene_metadata, never to programs.
        let definition = row.removing(["name", "collection", "room"])
        if let index = rows.firstIndex(where: { $0["id"].text == id }) { rows[index] = definition } else { rows.append(definition) }
        document[collection] = .array(rows)
        var entry = document["scene_metadata"].values.first { $0["id"].text == id } ?? .object([:])
        entry["id"] = .string(id); entry["name"] = .string(name)
        // Omission preserves an existing association for older callers;
        // the editor passes an empty string to explicitly clear it.
        if let room {
            if room.isEmpty { entry = entry.removing(["room"]) }
            else { entry["room"] = .string(room) }
        }
        var metadata = document["scene_metadata"].values.filter { $0["id"].text != id }
        metadata.append(entry)
        document["scene_metadata"] = .array(metadata); automationDocument = document; edited()
    }
    public func removeScene(_ id: String, collection: String) throws {
        try requireEditable(); guard var document = automationDocument else { throw HomeServerError.missingAutomation }
        document[collection] = .array(document[collection].values.filter { $0["id"].text != id })
        document["scene_metadata"] = .array(document["scene_metadata"].values.filter { $0["id"].text != id })
        automationDocument = document; edited()
    }
    private func edited() { draftChanged = true; draftID = nil; preview = nil; message = "修改已加入本机编辑稿，请保存到服务端并预览发布。" }
    private func requireEditable() throws {
        guard connected else { throw HomeServerError.notAuthenticated }
        guard canManage else { throw HomeServerError.denied }
        guard !configurationStale else { throw HomeServerError.staleConfiguration }
        guard deviceDocument != nil, automationDocument != nil else { throw HomeServerError.invalidDocument }
    }
    private func requireDevices() throws -> HomeJSON { guard let d = deviceDocument else { throw HomeServerError.invalidDocument }; return d }
    private func validateLabel(_ label: String) throws {
        guard !label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, label.utf8.count <= 256,
              !label.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { throw HomeServerError.invalidDocument }
    }
    private func records(_ json: HomeJSON) throws -> [HomeRecord] {
        guard case .array(let rows) = json, rows.allSatisfy({ $0["id"].text?.isEmpty == false }),
              Set(rows.compactMap { $0["id"].text }).count == rows.count else { throw HomeServerError.invalidResponse }
        let records = rows.map(HomeRecord.init)
        for record in records where record.raw["kind"].text == "Keypad" && record.raw["module_id"].text != nil {
            _ = try HomeKeypad(record: record)
        }
        return records
    }
    private func request(_ path: String, method: String = "GET", body: HomeJSON? = nil, optionalModuleInventory: Bool = false) async throws -> HomeJSON {
        guard let endpoint, let token else { throw HomeServerError.notAuthenticated }
        let generation = epoch
        let stopGeneration = priorityStopGeneration
        let outgoing = try endpoint.request(path: path, method: method, token: token, body: body)
        let trace = HomeControlContext.operation ?? diagnostics.operation(target: "http")
        let requestID = body?["request_id"].text
        diagnostics.record(.requestSend, operation: trace, method: method, path: path, requestID: requestID)
        let response: HomeHTTPResponse
        do { response = try await transport.send(outgoing) }
        catch {
            diagnostics.record(.requestFailed, operation: trace, method: method, path: path, requestID: requestID, error: error)
            if method != "GET", generation == epoch, stopGeneration == priorityStopGeneration {
                message = "请求结果未知，服务端可能已执行；请刷新核对，不自动重试。"
            }
            throw error
        }
        diagnostics.record(.response, operation: trace, method: method, path: path, requestID: requestID, status: response.status, responseData: response.data)
        guard generation == epoch, !Task.isCancelled else { throw HomeServerError.expiredRequest }
        if optionalModuleInventory, [403, 404].contains(response.status) { return .null }
        if response.status == 401 {
            let origin = endpoint.origin
            disconnect(); error = HomeServerError.notAuthenticated.localizedDescription
            message = "凭证或会话已失效，请重新连接服务端。"
            let clearedGeneration = epoch
            do {
                let saved = try await credentialStore.load(for: origin)
                guard epoch == clearedGeneration else { throw HomeServerError.expiredRequest }
                let removed = saved == token
                if removed { try await credentialStore.remove(for: origin) }
                if epoch == clearedGeneration {
                    if removed || saved == nil {
                        if savedCredentialOrigin == origin { savedCredentialOrigin = nil }
                        credentialStatus = "服务端已明确拒绝此凭证，对应的本机记录已清除。"
                    } else {
                        savedCredentialOrigin = origin
                        credentialStatus = "本次输入的凭证被拒绝；此服务原先保存的其他凭证已保留。"
                    }
                }
            } catch {
                if epoch == clearedGeneration { credentialStatus = "凭证已失效，但移除本机记录失败，请解锁设备后重试。" }
            }
            throw HomeServerError.notAuthenticated
        }
        if method == "POST", stopGeneration != priorityStopGeneration,
           ["/api/v1/device-commands", "/api/v1/scene-actions", "/api/v1/scene-relative-actions", "/api/v1/keypad-actions",
            "/api/v1/module-commands", "/api/v1/module-queries", "/api/v1/away-off", "/api/v1/keypad-settings"].contains(path) {
            throw HomeServerError.expiredRequest
        }
        if method == "GET", [408, 500, 502, 503, 504].contains(response.status) {
            let body = try? JSONDecoder().decode(HomeJSON.self, from: response.data)
            throw HomeRetryableResponse(detail: body?["error"].text ?? "HTTP \(response.status)")
        }
        let value = try JSONDecoder().decode(HomeJSON.self, from: response.data)
        if path == HomeKeypadActivationRequest.path, !(200...299).contains(response.status) {
            let matches = value["request_id"] == body?["request_id"] && value["retry"] == .bool(false)
            if response.status == 503, matches, value["outcome"] == .string("unknown") { return value }
            throw HomeKeypadActionHTTPFailure(status: response.status,
                rejected: matches && value["outcome"] == .string("rejected"), code: value["error"].text ?? "HTTP \(response.status)")
        }
        guard (200...299).contains(response.status) else { throw HomeServerError.server(value["error"].text ?? "HTTP \(response.status)") }
        return value
    }
    private func run(_ work: () async throws -> Void) async {
        guard !busy, connected else { diagnostics.record(.ignored, reason: !connected ? .disconnected : .busy); return }
        let generation = epoch
        let stopGeneration = priorityStopGeneration
        ordinaryBusy = true; error = nil
        defer { if generation == epoch { ordinaryBusy = false } }
        do {
            try await work()
            if generation == epoch, stopGeneration != priorityStopGeneration, let latestPriorityMessage { message = latestPriorityMessage }
        }
        catch {
            diagnostics.record(.workFailed, error: error)
            guard generation == epoch else { return }
            if stopGeneration != priorityStopGeneration {
                if let latestPriorityMessage { message = latestPriorityMessage }
                return
            }
            self.error = error.localizedDescription
            if case HomeServerError.staleConfiguration = error { preview = nil }
            capturedAt = nil
            if error is URLError { preview = nil }
        }
    }
}

private struct HomeRetryableResponse: LocalizedError {
    let detail: String
    var errorDescription: String? { "服务端：\(detail)" }
}
