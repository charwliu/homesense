import SwiftUI
import HomeSenseCore

private enum HomePage: String, CaseIterable, Identifiable {
    case overview = "家庭概览", floors = "楼层", rooms = "房间", devices = "设备", scenes = "场景", drafts = "草稿与发布", server = "服务连接"
    var id: String { rawValue }
    var icon: String {
        switch self {
        case .overview: "house"; case .floors: "building.2"; case .rooms: "door.left.hand.open"
        case .devices: "lightbulb"; case .scenes: "sparkles"; case .drafts: "doc.badge.gearshape"; case .server: "server.rack"
        }
    }
}
struct HomeEditRequest: Identifiable {
    let id = UUID()
    let kind: String
    var record: HomeRecord?
    var floor: String?
}

struct HomeManagementView: View {
    @ObservedObject var home: HomeServerStore
    @State private var page: HomePage? = .overview
    @State private var token = ""
    @State private var roomFilter = ""
    @State private var editor: HomeEditRequest?
    @State private var confirmPublication = false
    @State private var confirmReload = false
    @State private var removal: HomeEditRequest?
    @State private var compactColumn: NavigationSplitViewColumn = .sidebar

    var body: some View {
        NavigationSplitView(preferredCompactColumn: $compactColumn) {
            List(selection: $page) {
                Section {
                    Label {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("HomeSense").font(.title2.bold())
                            Text("HomeSense · 家庭中控").font(.caption).foregroundStyle(.secondary)
                        }
                    } icon: { Image(systemName: "house.circle.fill").font(.largeTitle).foregroundStyle(.teal) }
                    .padding(.vertical, 12)
                }
                Section("家庭") {
                    ForEach(visiblePages) { destination in
                        NavigationLink(value: destination) {
                            HStack {
                                Image(systemName: destination.icon).frame(width: 20)
                                    .foregroundStyle(page == destination ? Color.white : Color.secondary)
                                Text(destination == .overview && !managing ? "家庭" : destination.rawValue)
                                    .foregroundStyle(page == destination ? Color.white : Color.primary)
                            }
                        }
                        .listRowBackground(page == destination ? Color(red: 0.04, green: 0.38, blue: 0.4) : Color.clear)
                    }
                }
                Section(managing ? "服务端" : "家庭状态") {
                    Label(home.connected ? "已连接" : "未连接", systemImage: home.connected ? "checkmark.circle" : "circle")
                    if managing, !home.revision.isEmpty { LabeledContent("配置版本", value: home.revision) }
                    Text("\(home.devices.count) 个设备 · \(home.scenes.count) 个场景").font(.caption).foregroundStyle(.secondary)
                }
            }
            .navigationTitle("HomeSense")
            .navigationSplitViewColumnWidth(min: 210, ideal: 245, max: 300)
        } detail: {
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(managing ? "配置管理" : "我的家").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                        Text(pageTitle).font(.largeTitle.bold())
                        if managing { Text(home.message).font(.subheadline).foregroundStyle(.secondary) }
                        if home.connected { Label(home.receivingPush ? "状态实时更新" : "正在同步状态", systemImage: home.receivingPush ? "checkmark.circle" : "arrow.triangle.2.circlepath").font(.caption).foregroundStyle(.secondary) }
                        if let error = home.error { Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.red).textSelection(.enabled) }
                    }
                    if home.busy { ProgressView().controlSize(.small) }
                    if !home.connected, page != .server {
                        ContentUnavailableView("连接你的家庭", systemImage: "house", description: Text("连接 ngr 上的 HomeSense 服务，查看家庭状态和控制设备。现有直连测试在“本地调试”中保留。"))
                        Button("连接服务") { page = .server }.buttonStyle(.borderedProminent)
                    } else {
                        switch page ?? .overview {
                        case .overview: if managing { overview } else { dailyOverview }
                        case .floors: floors
                        case .rooms: rooms
                        case .devices: devices
                        case .scenes: scenes
                        case .drafts: if managing { configuration }
                        case .server: connection
                        }
                    }
                }
                .padding(24).frame(maxWidth: 950, alignment: .leading).frame(maxWidth: .infinity)
            }
            .background(.background.secondary)
            .navigationTitle(managing ? "配置管理" : "HomeSense")
            .toolbar {
                if home.canManage {
                    Button(managing ? "完成管理" : "管理", systemImage: managing ? "checkmark" : "slider.horizontal.3") {
                        home.selectInterfaceMode(managing ? .daily : .management)
                    }.accessibilityIdentifier("home.interfaceMode")
                }
                Button("服务连接", systemImage: "network") { page = .server; compactColumn = .detail }
                if home.connected {
                    Button("刷新", systemImage: "arrow.clockwise") { Task { await home.refresh() } }.disabled(home.busy)
                }
            }
        }
        .onChange(of: home.interfaceMode) { _, _ in preserveNavigation() }
        .onChange(of: home.canManage) { _, canManage in
            preserveNavigation()
            if !canManage {
                if let kind = editor?.kind, !kind.hasSuffix("-details") { editor = nil }
                confirmPublication = false; confirmReload = false; removal = nil
            }
        }
        .sheet(item: $editor, onDismiss: { home.resume() }) { request in
            switch request.kind {
            case "keypad-details": HomeKeypadDetails(home: home, keypadID: request.record?.id ?? "")
            case "device-details": HomeDeviceDetails(home: home, deviceID: request.record?.id ?? "")
            case "scene-details": HomeSceneDetails(home: home, sceneID: request.record?.id ?? "")
            case "keypad": HomeKeypadEditor(home: home, record: request.record).onAppear { home.pause() }
            default: HomeManagementEditor(home: home, request: request).onAppear { home.pause() }
            }
        }
        .alert("发布此配置到 HomeSense？", isPresented: $confirmPublication) {
            Button("确认发布", role: .destructive) { Task { await home.publishDraft() } }
            Button("取消", role: .cancel) { }
        } message: { Text("将发布版本 \(home.candidateRevision)，结束旧版本规则和未执行动作，并重新查询设备。已执行的设备动作不会回滚。") }
        .alert("重新读取活动配置？", isPresented: $confirmReload) {
            Button("重新读取", role: .destructive) { Task { await home.loadActiveConfiguration() } }
            Button("取消", role: .cancel) { }
        } message: { Text("将用服务端活动配置替换本机尚未保存的编辑；服务端已保存草稿会保留。") }
        .alert("从编辑稿中移除？", isPresented: Binding(get: { removal != nil }, set: { if !$0 { removal = nil } })) {
            Button("移除", role: .destructive) { removeSelection() }
            Button("取消", role: .cancel) { removal = nil }
        } message: { Text("此操作修改编辑稿，发布后才影响活动配置。移除楼层将保留房间并清除其楼层归属；被引用的房间、设备或场景需先处理引用。") }
    }

    private var managing: Bool { home.interfaceMode == .management && home.canManage }
    private var pageTitle: String {
        if page == .devices, !roomFilter.isEmpty, let room = home.rooms.first(where: { $0.id == roomFilter }) { return room.name }
        return page == .overview && !managing ? "家庭" : (page ?? .overview).rawValue
    }
    private var visiblePages: [HomePage] { HomePage.allCases.filter { managing || $0 != .drafts } }
    private func preserveNavigation() {
        if let page, !visiblePages.contains(page) { self.page = .overview }
    }
    private var displayRooms: [HomeRecord] { managing ? home.editableRooms : home.rooms }
    private var displayFloors: [String] { managing ? home.floors : home.activeFloors }
    private let cardColumns = [GridItem(.adaptive(minimum: 260), alignment: .top)]
    private var dailyOverview: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("房间").font(.title2.bold())
            LazyVGrid(columns: cardColumns, alignment: .leading, spacing: 16) {
                ForEach(home.rooms) { room in
                    Button { roomFilter = room.id; page = .devices; compactColumn = .detail } label: {
                        VStack(alignment: .leading, spacing: 10) {
                            Label(room.name, systemImage: "door.left.hand.open").font(.headline)
                            Text(room.raw["floor"].text ?? "未分配楼层").font(.caption)
                            Text("\(home.devices.filter { $0.raw["room"]["id"].text == room.id }.count) 个可见设备").font(.caption)
                        }.frame(maxWidth: .infinity, alignment: .leading).padding(20)
                            .background(.background, in: RoundedRectangle(cornerRadius: 16))
                            .overlay { RoundedRectangle(cornerRadius: 16).strokeBorder(.primary.opacity(0.07)) }
                    }.buttonStyle(.plain)
                }
            }
            if home.rooms.isEmpty { empty("暂无可见房间", "房间和设备清单由服务端授权决定。") }
            Text("设备").font(.title2.bold())
            activeDeviceCards
            Text("场景").font(.title2.bold())
            activeSceneCards
        }
    }
    private var activeDeviceCards: some View {
        LazyVGrid(columns: cardColumns, alignment: .leading, spacing: 16) {
            ForEach(filteredDevices) { device in
                if device.raw["kind"].text == "Keypad", device.raw["module_id"].text != nil {
                    if let keypad = try? HomeKeypad(record: device, current: home.snapshotIsCurrent) {
                        HomeKeypadCard(keypad: keypad, controlsAvailable: home.controlsAvailable,
                            activate: { number in Task { await home.activateKeypad(device.id, key: number) } },
                            details: { editor = HomeEditRequest(kind: "keypad-details", record: device) },
                            isActionAvailable: { home.canActivateKeypad(device.id, key: $0) })
                    } else { empty("面板数据暂不可用", "请刷新或核对服务端接口。") }
                } else {
                    Button { editor = HomeEditRequest(kind: "device-details", record: device) } label: {
                        HomeDeviceTile(device: device, live: home.snapshotIsCurrent)
                    }.buttonStyle(.plain)
                }
            }
        }
    }
    private var filteredDevices: [HomeRecord] { home.devices.filter { roomFilter.isEmpty || $0.raw["room"]["id"].text == roomFilter } }
    private var canEdit: Bool { home.canManage && home.deviceDocument != nil && !home.configurationStale && !home.busy }
    private var connection: some View {
        panel("HomeSense 管理服务", icon: "server.rack") {
            TextField("HTTPS 地址", text: $home.endpointText).textFieldStyle(.roundedBorder).disabled(home.connected || (home.busy && !home.restoringCredential))
                #if os(iOS)
                .textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL)
                #endif
            if !home.connected {
                SecureField("访问令牌", text: $token).textFieldStyle(.roundedBorder)
                Button("连接 HomeSense", systemImage: "link") {
                    let credential = token; token = ""
                    Task { await home.connect(token: credential) }
                }.buttonStyle(.borderedProminent).disabled(home.busy || home.restoringCredential || token.isEmpty)
                Button("使用此服务已保存的凭证连接") { Task { await home.restoreSavedConnection() } }
                    .disabled(home.busy || home.restoringCredential)
                if home.restoringCredential {
                    Button("取消自动连接") { home.disconnect() }.buttonStyle(.bordered)
                }
            } else {
                Text(home.canManage ? "管理员 · 可编辑配置草稿" : "设备与场景权限由服务端授权决定")
                Button("断开服务") { home.disconnect() }.buttonStyle(.bordered).disabled(home.busy && !home.restoringCredential)
            }
            Button("退出并移除本机凭证", role: .destructive) { Task { await home.removeSavedCredential() } }
                .disabled(home.busy && !home.restoringCredential)
            if !home.credentialStatus.isEmpty { Text(home.credentialStatus).font(.caption).foregroundStyle(.secondary) }
            Text("鉴权成功后，令牌按 HTTPS 服务地址保存在本机 Keychain，不参与 iCloud 同步，不写入配置或日志。HTTPS 证书须由系统信任且与服务地址匹配。")
                .font(.caption).foregroundStyle(.secondary)
            Text("楼层、房间、设备和场景共用服务端配置版本；修改需保存草稿、预览并发布。")
                .font(.caption).foregroundStyle(.secondary)
        }
    }
    private var overview: some View {
        VStack(spacing: 20) {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 130))], spacing: 12) {
                statistic("楼层", home.floors.count, "building.2")
                statistic("房间", home.editableRooms.count, "door.left.hand.open")
                statistic("设备", home.devices.count, "lightbulb")
                statistic("场景", home.scenes.count, "sparkles")
            }
            panel("家庭配置", icon: "house") {
                Text("活动版本 \(home.revision)")
                if let captured = home.capturedAt { Text("最近读取：\(captured.formatted(date: .omitted, time: .standard))").foregroundStyle(.secondary) }
                Text("控制以服务端实际反馈为准；请求受理和场景选择不表示负载已到位。")
                    .font(.caption).foregroundStyle(.secondary)
                if home.draftChanged { Label("有尚未保存的配置编辑", systemImage: "pencil.circle").foregroundStyle(.orange) }
                Button("管理草稿与发布") { page = .drafts }.buttonStyle(.bordered)
            }
        }
    }
    private var floors: some View {
        VStack(spacing: 16) {
            if managing { editNotice }
            if managing { Button("新增楼层", systemImage: "plus") { editor = HomeEditRequest(kind: "floor") }.disabled(!canEdit) }
            if displayFloors.isEmpty { empty("暂无楼层", "可独立新增楼层，再添加房间或调整房间归属。") }
            ForEach(displayFloors, id: \.self) { floor in
                panel(floor, icon: "building.2") {
                    Text(displayRooms.filter { $0.raw["floor"].text == floor }.map(\.name).joined(separator: "、").nonemptyOr("暂无房间"))
                        .foregroundStyle(.secondary)
                    if managing {
                        HStack {
                            Button("重命名") { editor = HomeEditRequest(kind: "floor", floor: floor) }
                            Button("移除楼层", role: .destructive) { removal = HomeEditRequest(kind: "floor", floor: floor) }
                        }.disabled(!canEdit)
                    }
                }
            }
        }
    }
    private var rooms: some View {
        VStack(spacing: 16) {
            if managing { editNotice }
            if managing { Button("新增房间", systemImage: "plus") { editor = HomeEditRequest(kind: "room") }.disabled(!canEdit) }
            if displayRooms.isEmpty { empty("暂无房间", "可为房间选择楼层，再把设备分配到房间。") }
            ForEach(displayRooms) { room in
                panel(room.name, icon: "door.left.hand.open") {
                    Text(room.raw["floor"].text ?? "未分配楼层").foregroundStyle(.secondary)
                    Text(managing
                         ? "\(home.editableDevices.filter { $0.raw["room"].text == room.id }.count) 个配置设备"
                         : "\(home.devices.filter { $0.raw["room"]["id"].text == room.id }.count) 个可见设备").font(.caption)
                    if managing {
                        HStack {
                            Button("编辑房间") { editor = HomeEditRequest(kind: "room", record: room) }
                            Button("移除", role: .destructive) { removal = HomeEditRequest(kind: "room", record: room) }
                        }.disabled(!canEdit)
                    }
                }
            }
        }
    }
    private var devices: some View {
        VStack(spacing: 16) {
            Picker("按房间查看", selection: $roomFilter) {
                Text("全部房间").tag("")
                ForEach(home.rooms) { Text($0.name).tag($0.id) }
            }
            activeDeviceCards
            if home.devices.isEmpty { empty("暂无可见设备", "设备清单与可控范围由服务端凭证决定。") }
            if managing {
                editNotice
                HStack {
                    Text("设备配置").font(.headline)
                    Spacer()
                    Button("新增设备", systemImage: "plus") { editor = HomeEditRequest(kind: "device") }.disabled(!canEdit)
                    Button("新增 Keypad", systemImage: "square.grid.2x2") { editor = HomeEditRequest(kind: "keypad") }.disabled(!canEdit)
                }
                ForEach(home.editableKeypads) { keypad in
                    panel(keypad.name, icon: "square.grid.2x2") {
                        Text("\(keypad.id) · \(keypad.raw["keys"].values.count) 个已配置按键").font(.caption).foregroundStyle(.secondary)
                        HStack {
                            Button("编辑面板与按键用途") { editor = HomeEditRequest(kind: "keypad", record: keypad) }
                            Button("移除面板", role: .destructive) { removal = HomeEditRequest(kind: "keypad", record: keypad) }
                        }.disabled(!canEdit)
                    }
                }
                ForEach(home.editableDevices) { device in
                    panel(device.name, icon: "switch.2") {
                        Text("\(device.id) · \(device.raw["enabled"].flag ? "已启用" : "未启用")").font(.caption).foregroundStyle(.secondary)
                        HStack {
                            Button("编辑设备") { editor = HomeEditRequest(kind: "device", record: device) }
                            Button("移除", role: .destructive) { removal = HomeEditRequest(kind: "device", record: device) }
                        }.disabled(!canEdit)
                    }
                }
            }
        }
    }
    private var scenes: some View {
        VStack(spacing: 16) {
            activeSceneCards
            if managing {
                editNotice
                Button("新增场景", systemImage: "plus") { editor = HomeEditRequest(kind: "scene") }.disabled(!canEdit)
                ForEach(home.editableScenes) { scene in
                    panel(scene.name, icon: "doc.text") {
                        Text("\(scene.id) · \(scene.raw["enabled"].flag ? "已启用" : "未启用")").font(.caption).foregroundStyle(.secondary)
                        HStack {
                            Button("编辑场景") { editor = HomeEditRequest(kind: "scene", record: scene) }
                            Button("移除", role: .destructive) { removal = HomeEditRequest(kind: "scene", record: scene) }
                        }.disabled(!canEdit)
                    }
                }
            }
            if home.scenes.isEmpty && (!managing || home.editableScenes.isEmpty) { empty("暂无场景", managing ? "可在配置草稿中新增场景动作，并在审核后发布。" : "场景清单与可控范围由服务端授权决定。") }
        }
    }
    private var activeSceneCards: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 155), alignment: .top)], alignment: .leading, spacing: 14) {
            ForEach(home.scenes) { scene in
                Button { editor = HomeEditRequest(kind: "scene-details", record: scene) } label: {
                    VStack(alignment: .leading, spacing: 12) {
                    Image(systemName: "sparkles").font(.title2).foregroundStyle(.teal)
                    Text(scene.name).font(.headline)
                    Text("负载反馈：\(scene.raw["activity"].text ?? "未知")").foregroundStyle(.secondary)
                    if scene.raw["switch_kind"].text == "selection" {
                        Text("场景选择：\(selectionDescription(scene.raw["selected"]))").font(.caption)
                    }
                    }.padding(18).frame(maxWidth: .infinity, minHeight: 140, alignment: .topLeading)
                        .background(.background, in: RoundedRectangle(cornerRadius: 20))
                        .overlay { RoundedRectangle(cornerRadius: 20).strokeBorder(.primary.opacity(0.07)) }
                }.buttonStyle(.plain)
            }
        }
    }
    private var configuration: some View {
        VStack(spacing: 16) {
            if !home.canManage { empty("需要管理员权限", "当前凭证不能管理配置。") }
            else {
                panel("配置编辑稿", icon: "doc.badge.gearshape") {
                    Text("活动版本 \(home.revision) · 编辑基线 \(home.baseRevision.isEmpty ? "未读取" : home.baseRevision) → \(home.candidateRevision)")
                    if home.configurationStale { Label("编辑基线已过期，请重新读取并核对修改。", systemImage: "exclamationmark.triangle").foregroundStyle(.orange) }
                    if home.draftChanged { Text("有未保存修改").foregroundStyle(.orange) }
                    if let id = home.draftID { Text("服务端草稿：\(id)").font(.caption.monospaced()).textSelection(.enabled) }
                    Button("读取活动配置") { if home.draftChanged { confirmReload = true } else { Task { await home.loadActiveConfiguration() } } }
                    Button("验证并保存到服务端") { Task { await home.saveDraft() } }.disabled(!canEdit)
                    Button("预览发布影响") { Task { await home.previewDraft() } }.disabled(!canEdit || home.draftID == nil || home.draftChanged)
                    if let preview = home.preview {
                        Text(preview["impact"].formatted).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                        Button("发布已预览的草稿") { confirmPublication = true }.buttonStyle(.borderedProminent).disabled(home.busy || home.configurationStale)
                    }
                    Text("保存和编辑只影响草稿。发布后手机与服务端共用新版本；服务端将执行校验、版本比对、备份和连接恢复。")
                        .font(.caption).foregroundStyle(.secondary)
                }.disabled(home.busy)
                panel("服务端草稿", icon: "tray.full") {
                    Button("读取我的草稿") { Task { await home.loadDrafts() } }.disabled(home.busy)
                    ForEach(home.drafts) { draft in
                        VStack(alignment: .leading) {
                            Text("版本 \(draft.raw["expected_revision"].integerText ?? "?") → \(draft.raw["revision"].integerText ?? "?")")
                            Text(draft.id).font(.caption.monospaced()).foregroundStyle(.secondary)
                            Button("读取此草稿") { Task { await home.loadDraft(draft.id) } }.disabled(home.busy || home.draftChanged)
                        }
                    }
                }
            }
        }
    }
    private var editNotice: some View {
        Group {
            if home.canManage {
                Label(home.deviceDocument == nil ? "先在“草稿与发布”读取活动配置。" : "正在编辑配置草稿；保存并发布后同步生效。", systemImage: "pencil.circle")
                    .font(.caption).foregroundStyle(home.configurationStale ? Color.orange : Color.secondary)
            }
        }
    }
    private func removeSelection() {
        guard let selected = removal else { return }
        defer { removal = nil }
        do {
            switch selected.kind {
            case "floor": if let floor = selected.floor { try home.renameFloor(floor, to: nil) }
            case "room": if let row = selected.record { try home.removeRoom(row.id) }
            case "device": if let row = selected.record { try home.removeDevice(row.id) }
            case "keypad": if let row = selected.record { try home.removeKeypad(row.id) }
            case "scene": if let row = selected.record { try home.removeScene(row.id, collection: row.raw["collection"].text ?? "immediate_scenes") }
            default: break
            }
        } catch { home.error = error.localizedDescription }
    }
    private func selectionDescription(_ value: HomeJSON) -> String {
        if case .bool(let selected) = value { return selected ? "开启" : "关闭" }; return "未知"
    }
    private func empty(_ title: String, _ detail: String) -> some View { ContentUnavailableView(title, systemImage: "house", description: Text(detail)) }
    private func statistic(_ title: String, _ count: Int, _ icon: String) -> some View {
        VStack(alignment: .leading, spacing: 10) { Label(title, systemImage: icon).foregroundStyle(.secondary); Text("\(count)").font(.largeTitle.bold()) }
            .padding(20).frame(maxWidth: .infinity, alignment: .leading).background(.background, in: RoundedRectangle(cornerRadius: 16))
    }
    private func panel<Content: View>(_ title: String, icon: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 14) { Label(title, systemImage: icon).font(.headline); content() }
            .padding(20).frame(maxWidth: .infinity, alignment: .leading).background(.background, in: RoundedRectangle(cornerRadius: 16))
    }
}

struct HomeDeviceCard: View {
    @ObservedObject var home: HomeServerStore
    let device: HomeRecord
    @State private var level = 50.0
    @State private var editingLevel = false
    @State private var pendingLevel = false
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(device.name).font(.headline)
            if let name = device.raw["room"]["name"].text { Text(name).font(.caption).foregroundStyle(.secondary) }
            Text(home.snapshotIsCurrent ? "反馈质量：\(device.raw["quality"].text ?? "未知")" : "状态未同步，以下保留历史反馈").font(.caption).foregroundStyle(.secondary)
            if let on = device.raw["observed"]["on"].optionalBool {
                Text((home.snapshotIsCurrent && device.raw["quality"].text == "Confirmed" ? "实际反馈：" : "历史反馈：") + (on ? "开" : "关"))
            }
            else { Text("实际反馈：未知").foregroundStyle(.secondary) }
            if device.raw["state_authority"].text == "pending_target" { Text("有待确认的目标；以上仅显示实际反馈。").font(.caption).foregroundStyle(.orange) }
            HStack {
                if device.raw["kind"].text == "Motor" {
                    action("打开", .object(["action": .string("open")]))
                    action("关闭", .object(["action": .string("close")]))
                    action("停止", .object(["action": .string("stop")]))
                } else if device.raw["kind"].text == "Keypad" {
                    action("红灯", .object(["action": .string("panel_led"), "on": .bool(true)]))
                    action("绿灯", .object(["action": .string("panel_led"), "on": .bool(false)]))
                } else {
                    action("开", .object(["action": .string("power"), "on": .bool(true)]))
                    action("关", .object(["action": .string("power"), "on": .bool(false)]))
                }
            }
            if device.raw["kind"].text == "Dimmer" {
                if let actual = device.raw["observed"]["level"].integerText { Text("读回亮度：\(actual)%").font(.caption).foregroundStyle(.secondary) }
                HStack {
                    Slider(value: $level, in: 0...100, step: 1, onEditingChanged: { editing in
                        editingLevel = editing
                        if editing { pendingLevel = true }
                    })
                    Text("目标 \(Int(level))%").monospacedDigit()
                }
                Button("设置亮度") {
                    let target = Int64(level)
                    Task {
                        await home.controlDevice(device.id, command: .object(["action": .string("brightness"), "level": .integer(target)]))
                        if home.error == nil { pendingLevel = false }
                    }
                }.buttonStyle(.bordered).disabled(!home.controlsAvailable || !device.raw["can_control"].flag)
            }
        }
        .padding(20).frame(maxWidth: .infinity, alignment: .leading).background(.background, in: RoundedRectangle(cornerRadius: 16))
        .onAppear { syncLevel() }
        .onChange(of: device.raw["observed"]["level"]) { _, _ in syncLevel() }
    }
    private func syncLevel() {
        if !editingLevel, !pendingLevel, let text = device.raw["observed"]["level"].integerText, let value = Double(text) { level = value }
    }
    private func action(_ title: String, _ command: HomeJSON) -> some View {
        Button(title) { Task { await home.controlDevice(device.id, command: command) } }
            .buttonStyle(.bordered).disabled(!home.controlsAvailable || !device.raw["can_control"].flag)
    }
}
private extension HomeJSON {
    var optionalBool: Bool? { if case .bool(let value) = self { value } else { nil } }
}

private extension String {
    func nonemptyOr(_ alternative: String) -> String { isEmpty ? alternative : self }
}
