import Foundation
import Combine

public enum HomeInterfaceMode: String, CaseIterable, Identifiable, Sendable {
    case daily = "日常控制", management = "配置管理"
    public var id: String { rawValue }
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
    @Published public private(set) var busy = false
    @Published public private(set) var canManage = false
    @Published public private(set) var interfaceMode: HomeInterfaceMode = .daily
    @Published public private(set) var devices: [HomeRecord] = []
    @Published public private(set) var rooms: [HomeRecord] = []
    @Published public private(set) var scenes: [HomeRecord] = []
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
            busy = true
            defer { if generation == epoch { busy = false } }
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
                let name = automationDocument["scene_metadata"].values.first { $0["id"] == scene["id"] }?["name"]
                row["name"] = name ?? scene["id"]
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
        busy = true; error = nil
        let generation = epoch
        defer { if epoch == generation { busy = false } }
        do {
            let endpoint = try HomeServerEndpoint(endpointText)
            guard restoringOrigin == nil || restoringOrigin == endpoint.origin else { throw HomeServerError.expiredRequest }
            guard !credential.isEmpty, !credential.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { throw HomeServerError.notAuthenticated }
            self.endpoint = endpoint; token = credential
            let capabilities = try await request("/api/v1/session")
            guard case .bool = capabilities["can_manage_configuration"] else { throw HomeServerError.invalidResponse }
            canManage = capabilities["can_manage_configuration"].flag
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
        savedRecoveryOrigin = nil
        restoreGeneration = UUID(); restoringCredential = false
        epoch = UUID(); stopEvents(); interfaceMode = .daily
        token = nil; endpoint = nil; connected = false; canManage = false; busy = false
        devices = []; rooms = []; scenes = []; drafts = []; serverFloors = []; capturedAt = nil; revision = ""
        snapshotBoot = ""; synchronization = "未连接"
        deviceDocument = nil; automationDocument = nil; preview = nil; draftID = nil
        baseRevision = ""; candidateRevision = ""; draftChanged = false; error = nil
        message = "已断开服务端并清除本次会话令牌；本机 Keychain 凭证保留。"
    }
    private func stopEvents() {
        eventEpoch = UUID(); eventTask?.cancel(); eventTask = nil
        receivingPush = false; eventCursor = nil
    }
    public func pause() {
        eventsWanted = false
        if savedRecoveryOrigin != nil {
            // Cancel initial lookup/HTTP completion as well as a retry wait.
            epoch = UUID(); restoreGeneration = UUID()
            restoringCredential = false; busy = false; connected = false; canManage = false
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
        devices = next; scenes = nextScenes; eventCursor = cursor
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
        if !canManage {
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
        revision = version; devices = nextDevices; rooms = nextRooms; scenes = nextScenes
        serverFloors = r["floors"].values.compactMap(\.text)
        snapshotBoot = d["boot"].text ?? ""
        capturedAt = Date()
    }
    public func canControlDevice(_ id: String) -> Bool {
        controlsAvailable && devices.contains { $0.id == id && $0.raw["can_control"].flag && $0.targetAvailable }
    }
    public func canControlScene(_ id: String) -> Bool {
        controlsAvailable && scenes.contains { $0.id == id && $0.raw["can_control"].flag && $0.targetAvailable }
    }
    public func controlDevice(_ id: String, command: HomeJSON) async {
        guard canControlDevice(id) else { return }
        await sendDeviceCommand(id, command: command)
    }
    private func sendDeviceCommand(_ id: String, command: HomeJSON, includeRevision: Bool = false) async {
        await run {
            var body: HomeJSON = .object([
                "device": .string(id), "request_id": .string(UUID().uuidString), "command": command
            ])
            if includeRevision { body["revision"] = .string(self.revision) }
            let result = try await self.request("/api/v1/device-commands", method: "POST", body: body)
            self.message = result["outcome"].text == "accepted" ? "控制已受理，等待实际设备反馈。" : "控制结果未知，请核对实际设备；未自动重试。"
            if !self.receivingPush { try await self.readSnapshots() }
        }
    }
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
        if case .bool(let value) = factTarget?.raw["selected"] { state.selected = value }
        state.relativeUnknownAllowed = factTarget?.raw["relative_unknown_allowed"].flag == true
        if !connected { state.reason = .disconnected; return state }
        if busy { state.reason = .busy; return state }
        if !snapshotIsCurrent { state.reason = .snapshotNotCurrent; return state }
        guard let row = devices.first(where: { $0.id == id }) else { state.reason = .panelMissing; return state }
        guard let panel = try? HomeKeypad(record: row) else { state.reason = .panelInvalid; return state }
        guard let key = panel.keys.first(where: { $0.number == number && $0.canActivate }) else { state.reason = .actionMissing; return state }
        state.configured = true
        if card && (!key.canActivateCard || (key.cardAction == .motorPress && UInt64(revision).map { $0 > 0 } != true)) {
            state.reason = .cardActionInvalid; return state
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
        else if state.selected == nil && !state.relativeUnknownAllowed { state.reason = .selectionUnknown }
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
        await run {
            var body: HomeJSON = .object(["scene": .string(id), "request_id": .string(UUID().uuidString)])
            if let on { body["on"] = .bool(on) }
            let result = try await self.request("/api/v1/scene-actions", method: "POST", body: body)
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
    public func upsertScene(_ row: HomeJSON, collection: String, name: String, originalID: String? = nil) throws {
        try requireEditable(); try validateLabel(name)
        guard ["immediate_scenes", "timed_scenes"].contains(collection), row.isObject,
              let id = row["id"].text, var document = automationDocument else { throw HomeServerError.invalidDocument }
        try validateLabel(id)
        guard originalID == nil || originalID == id else { throw HomeServerError.invalidDocument }
        if originalID == nil, editableScenes.contains(where: { $0.id == id }) { throw HomeServerError.invalidDocument }
        var rows = document[collection].values
        if let index = rows.firstIndex(where: { $0["id"].text == id }) { rows[index] = row } else { rows.append(row) }
        document[collection] = .array(rows)
        var metadata = document["scene_metadata"].values.filter { $0["id"].text != id }
        metadata.append(.object(["id": .string(id), "name": .string(name)]))
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
    private func request(_ path: String, method: String = "GET", body: HomeJSON? = nil) async throws -> HomeJSON {
        guard let endpoint, let token else { throw HomeServerError.notAuthenticated }
        let generation = epoch
        let outgoing = try endpoint.request(path: path, method: method, token: token, body: body)
        let trace = HomeControlContext.operation ?? diagnostics.operation(target: "http")
        let requestID = body?["request_id"].text
        diagnostics.record(.requestSend, operation: trace, method: method, path: path, requestID: requestID)
        let response: HomeHTTPResponse
        do { response = try await transport.send(outgoing) }
        catch {
            diagnostics.record(.requestFailed, operation: trace, method: method, path: path, requestID: requestID, error: error)
            if method != "GET", generation == epoch {
                message = "请求结果未知，服务端可能已执行；请刷新核对，不自动重试。"
            }
            throw error
        }
        diagnostics.record(.response, operation: trace, method: method, path: path, requestID: requestID, status: response.status, responseData: response.data)
        guard generation == epoch, !Task.isCancelled else { throw HomeServerError.expiredRequest }
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
        if method == "GET", [408, 500, 502, 503, 504].contains(response.status) {
            let body = try? JSONDecoder().decode(HomeJSON.self, from: response.data)
            throw HomeRetryableResponse(detail: body?["error"].text ?? "HTTP \(response.status)")
        }
        let value = try JSONDecoder().decode(HomeJSON.self, from: response.data)
        guard (200...299).contains(response.status) else { throw HomeServerError.server(value["error"].text ?? "HTTP \(response.status)") }
        return value
    }
    private func run(_ work: () async throws -> Void) async {
        guard !busy, connected else { diagnostics.record(.ignored, reason: !connected ? .disconnected : .busy); return }
        let generation = epoch
        busy = true; error = nil
        defer { if generation == epoch { busy = false } }
        do { try await work() }
        catch {
            diagnostics.record(.workFailed, error: error)
            guard generation == epoch else { return }
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
