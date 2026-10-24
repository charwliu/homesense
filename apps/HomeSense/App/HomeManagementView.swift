import SwiftUI
import UniformTypeIdentifiers
import HomeSenseCore

private enum HomePage: String, CaseIterable, Identifiable {
    case overview = "家庭概览", floors = "楼层", rooms = "房间", devices = "设备", scenes = "场景", drafts = "草稿与发布", server = "服务连接"
    case climate = "温控", lights = "灯"
    var id: String { rawValue }
    var icon: String {
        switch self {
        case .overview: "house"; case .floors: "building.2"; case .rooms: "door.left.hand.open"
        case .devices: "cpu"; case .scenes: "sparkles"; case .drafts: "doc.badge.gearshape"; case .server: "server.rack"
        case .climate: HomeDeviceCategory.climate.icon; case .lights: HomeDeviceCategory.lights.icon
        }
    }
    var category: HomeDeviceCategory? {
        switch self { case .climate: .climate; case .lights: .lights; default: nil }
    }
    static func categoryPage(_ category: HomeDeviceCategory) -> Self { category == .climate ? .climate : .lights }
}
private struct HomeRoomRoute: Hashable {
    let id: String
}

struct HomeEditRequest: Identifiable {
    let id = UUID()
    let kind: String
    var record: HomeRecord?
    var floor: String?
}

private struct HomeSyncStatus: View {
    @ObservedObject var home: HomeServerStore
    var body: some View {
        Label(home.connected ? (home.receivingPush ? "状态实时更新" : "正在同步状态") : "未连接",
              systemImage: home.connected && home.receivingPush ? "checkmark.circle" : "arrow.triangle.2.circlepath")
            .labelStyle(.titleAndIcon)
            .font(.caption).foregroundStyle(.secondary)
            .lineLimit(1).minimumScaleFactor(0.8)
            .accessibilityIdentifier("home.connectionStatus")
    }
}

struct HomeManagementView: View {
    @ObservedObject var home: HomeServerStore
    @State private var page: HomePage? = .overview
    @State private var token = ""
    @State private var exportingDiagnostics = false
    @State private var diagnosticExport = HomeDiagnosticExport(text: "")
    @State private var roomPath: [HomeRoomRoute] = []
    @State private var editor: HomeEditRequest?
    @State private var confirmPublication = false
    @State private var confirmReload = false
    @State private var removal: HomeEditRequest?
    @State private var compactColumn: NavigationSplitViewColumn = .sidebar
    #if os(iOS)
    @State private var homeKitPlacement = false
    #endif

    var body: some View {
        NavigationSplitView(preferredCompactColumn: $compactColumn) {
            List(selection: $page) {
                Section {
                    Label {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("知居").font(.title2.bold())
                            Text("知居 · 家庭中控").font(.caption).foregroundStyle(.secondary)
                        }
                    } icon: { Image(systemName: "house.circle.fill").font(.largeTitle).foregroundStyle(.teal) }
                    .padding(.vertical, 12)
                }
                Section("家庭") {
                    ForEach(visiblePages.filter { $0.category == nil }) { navigationRow($0) }
                }
                Section("类别") {
                    ForEach(HomeDeviceCategory.allCases) { category in
                        navigationRow(.categoryPage(category))
                    }
                }
                Section(managing ? "服务端" : "家庭状态") {
                    Label(home.connected ? "已连接" : "未连接", systemImage: home.connected ? "checkmark.circle" : "circle")
                    if managing, !home.revision.isEmpty { LabeledContent("配置版本", value: home.revision) }
                    Text("\(managing ? home.devices.count : home.dailyDevices.count) 个设备 · \(home.scenes.count) 个场景").font(.caption).foregroundStyle(.secondary)
                }
            }
            .navigationTitle("知居")
            .navigationSplitViewColumnWidth(min: 190, ideal: 210, max: 240)
        } detail: {
            NavigationStack(path: $roomPath) {
                ScrollView {
                    VStack(alignment: .leading, spacing: 20) {
                        VStack(alignment: .leading, spacing: 6) {
                            Text(managing ? "配置管理" : "我的家").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                            Text(pageTitle).font(.largeTitle.bold())
                            if managing { Text(home.message).font(.subheadline).foregroundStyle(.secondary) }
                            if let away = home.awayStatus {
                                Label(away.message, systemImage: away.failed ? "exclamationmark.triangle" : "house")
                                    .font(.caption).foregroundStyle(away.failed ? Color.red : Color.secondary)
                            }
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
                            case .climate: categoryContent(.climate)
                            case .lights: categoryContent(.lights)
                            }
                        }
                    }
                    .padding(.horizontal, 12).padding(.vertical, 24)
                    .frame(maxWidth: 1100, alignment: .leading).frame(maxWidth: .infinity)
                }
                .background(.background.secondary)
                .navigationTitle(managing ? "配置管理" : "知居")
                .toolbar {
                    ToolbarItemGroup(placement: .primaryAction) {
                        HomeSyncStatus(home: home)
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
                .navigationDestination(for: HomeRoomRoute.self) { route in
                    HomeRoomDetail(home: home, roomID: route.id, openDetails: { editor = $0 })
                }
            }
        }
        #if os(iOS)
        .sheet(isPresented: $homeKitPlacement) { HomeKitRoomPlacementView(home: home) }
        #endif
        .onChange(of: page) { _, _ in roomPath.removeAll() }
        .fileExporter(isPresented: $exportingDiagnostics, document: diagnosticExport, contentType: .plainText, defaultFilename: "HomeSense-diagnostics") { _ in }
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
            case "module-details": HomeModuleDetails(home: home, moduleID: request.record?.id ?? "")
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
    private var appIdentity: HomeBuildIdentity { HomeBuildIdentity(info: Bundle.main.infoDictionary ?? [:]) }
    private var appVersion: String { appIdentity.version }
    private var appBuild: String { appIdentity.displayBuild }
    private var pageTitle: String {
        return page == .overview && !managing ? "家庭" : (page ?? .overview).rawValue
    }
    private var visiblePages: [HomePage] { HomePage.allCases.filter { managing || $0 != .drafts } }
    private func preserveNavigation() {
        if let page, !visiblePages.contains(page) { self.page = .overview }
    }
    private var displayRooms: [HomeRecord] { managing ? home.editableRooms : home.rooms }
    private var displayFloors: [String] { managing ? home.floors : home.activeFloors }
    private let cardColumns = [GridItem(.adaptive(minimum: 260), alignment: .top)]
    private func navigationRow(_ destination: HomePage) -> some View {
        NavigationLink(value: destination) {
            HStack {
                Image(systemName: destination.icon).frame(width: 20)
                    .foregroundStyle(page == destination ? Color.white : Color.secondary)
                Text(destination == .overview && !managing ? "家庭" : destination.rawValue)
                    .foregroundStyle(page == destination ? Color.white : Color.primary)
            }
        }
        .listRowBackground(page == destination ? Color(red: 0.04, green: 0.38, blue: 0.4) : Color.clear)
        .accessibilityIdentifier(destination.category.map { "home.categoryLink.\($0.rawValue)" } ?? "home.page.\(destination.id)")
    }
    private var categoryShortcuts: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("类别").font(.title2.bold())
            HStack(spacing: 12) {
                ForEach(HomeDeviceCategory.allCases) { category in
                    Button {
                        page = .categoryPage(category); compactColumn = .detail
                    } label: {
                        VStack(alignment: .leading, spacing: 8) {
                            HStack {
                                Label(category.title, systemImage: category.icon)
                                    .font(.headline).foregroundStyle(category == .lights ? Color.orange : Color.teal)
                                Spacer(minLength: 4)
                                Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary)
                            }
                            Text(category.englishTitle).font(.caption).foregroundStyle(.secondary)
                            Text(category.summary(devices: home.dailyDevices, current: home.snapshotIsCurrent))
                                .font(.caption).foregroundStyle(.secondary).lineLimit(2)
                                .frame(maxWidth: .infinity, minHeight: 32, alignment: .topLeading)
                        }.padding(16).frame(maxWidth: .infinity, alignment: .leading)
                            .background(.background, in: RoundedRectangle(cornerRadius: 18))
                            .overlay { RoundedRectangle(cornerRadius: 18).strokeBorder(.primary.opacity(0.08)) }
                            .contentShape(RoundedRectangle(cornerRadius: 18))
                    }.buttonStyle(.plain)
                        .accessibilityHint("按房间查看\(category.title)设备")
                        .accessibilityIdentifier("home.category.\(category.id)")
                }
            }
        }
    }
    private func categoryContent(_ category: HomeDeviceCategory) -> some View {
        let groups = category.rooms(devices: home.dailyDevices, rooms: home.rooms)
        return VStack(alignment: .leading, spacing: 20) {
            Label(category.englishTitle, systemImage: category.icon).font(.headline).foregroundStyle(.secondary)
            Text(category.summary(devices: home.dailyDevices, current: home.snapshotIsCurrent)).foregroundStyle(.secondary)
            if groups.isEmpty {
                ContentUnavailableView(category == .climate ? "暂无窗帘" : "暂无灯",
                    systemImage: category.icon, description: Text("此类别中暂无当前凭证可见的设备。"))
            }
            ForEach(groups) { group in
                VStack(alignment: .leading, spacing: 12) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(group.name).font(.title2.bold())
                        if let floor = group.floor { Text(floor).font(.caption).foregroundStyle(.secondary) }
                    }
                    HomeDeviceCards(home: home, devices: group.devices, openDetails: { editor = $0 })
                }.accessibilityIdentifier("home.categoryRoom.\(category.id).\(group.id)")
            }
        }.accessibilityIdentifier("home.categoryPage.\(category.id)")
    }
    private var dailyOverview: some View {
        VStack(alignment: .leading, spacing: 20) {
            categoryShortcuts
            let scenes = home.householdScenes
            if !scenes.isEmpty {
                HomeSceneCards(home: home, scenes: scenes, openDetails: { editor = $0 })
            }
            Text("房间").font(.title2.bold())
            LazyVGrid(columns: cardColumns, alignment: .leading, spacing: 16) {
                ForEach(home.rooms) { room in
                    roomCard(room)
                }
            }
            if home.rooms.isEmpty { empty("暂无可见房间", "房间和设备清单由服务端授权决定。") }
            Text("灯与窗帘").font(.title2.bold())
            HomeDeviceCards(home: home, devices: home.dailyActuators, openDetails: { editor = $0 })
            if !home.unassignedScenes.isEmpty {
                Text("归属尚未确认，可在管理模式中核对并分配。").font(.caption).foregroundStyle(.secondary)
                HomeSceneCards(home: home, scenes: home.unassignedScenes, openDetails: { editor = $0 })
            }
        }
    }
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
            #if os(iOS)
            if home.connected && home.canManage {
                Button("Apple家庭房间归属", systemImage: "house.and.flag") { homeKitPlacement = true }
                    .accessibilityIdentifier("home.homeKitPlacement")
            }
            #endif
            Button("退出并移除本机凭证", role: .destructive) { Task { await home.removeSavedCredential() } }
                .disabled(home.busy && !home.restoringCredential)
            if !home.credentialStatus.isEmpty { Text(home.credentialStatus).font(.caption).foregroundStyle(.secondary) }
            Text("鉴权成功后，令牌按 HTTPS 服务地址保存在本机 Keychain，不参与 iCloud 同步，不写入配置或日志。HTTPS 证书须由系统信任且与服务地址匹配。")
                .font(.caption).foregroundStyle(.secondary)
            Button("导出诊断日志", systemImage: "square.and.arrow.up") {
                Task {
                    diagnosticExport = HomeDiagnosticExport(text: await home.diagnostics.exportIncludingPrevious())
                    exportingDiagnostics = true
                }
            }
            Text("日志仅保留最近的操作状态与请求结果；禁用按键的触摸不会触发点击日志。导出不会发送设备指令。")
                .font(.caption).foregroundStyle(.secondary)
            Text("楼层、房间、设备和场景共用服务端配置版本；修改需保存草稿、预览并发布。")
                .font(.caption).foregroundStyle(.secondary)
            Text("HomeSense \(appVersion) Build \(appBuild)")
                .font(.caption).foregroundStyle(.secondary)
                .accessibilityIdentifier("home.appBuild")
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
                    let floorRooms = displayRooms.filter { $0.raw["floor"].text == floor }
                    if floorRooms.isEmpty { Text("暂无房间").foregroundStyle(.secondary) }
                    ForEach(floorRooms) { room in
                        roomLink(room, showsFloor: false)
                    }
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
                roomCard(room)
            }
        }
    }
    private func roomLink(_ room: HomeRecord, showsFloor: Bool = true) -> some View {
        NavigationLink(value: HomeRoomRoute(id: room.id)) {
            HStack(spacing: 14) {
                Image(systemName: "door.left.hand.open").font(.title2).foregroundStyle(.teal)
                    .frame(width: 32)
                VStack(alignment: .leading, spacing: 5) {
                    Text(room.name).font(.headline).foregroundStyle(.primary)
                    if showsFloor { Text(room.raw["floor"].text ?? "未分配楼层").font(.caption).foregroundStyle(.secondary) }
                    Text(managing
                         ? "\(home.roomDevices(inRoom: room.id).count) 个灯与窗帘 · \(home.editableScenes.filter { $0.roomID == room.id }.count) 个编辑稿场景"
                         : "\(home.roomDevices(inRoom: room.id).count) 个灯与窗帘 · \(home.scenes(inRoom: room.id).count) 个场景")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Spacer(minLength: 8)
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(.tertiary)
            }
            .padding(showsFloor ? 20 : 10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityHint("进入房间，查看设备和场景")
        .accessibilityIdentifier("home.roomLink.\(room.id)")
    }
    private func roomCard(_ room: HomeRecord) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            roomLink(room)
            if managing {
                Divider().padding(.horizontal, 20)
                HStack {
                    Button("编辑房间") { editor = HomeEditRequest(kind: "room", record: room) }
                        .accessibilityIdentifier("home.editRoom.\(room.id)")
                    Button("移除", role: .destructive) { removal = HomeEditRequest(kind: "room", record: room) }
                }
                .disabled(!canEdit).padding(20)
            }
        }
        .background(.background, in: RoundedRectangle(cornerRadius: 16))
        .overlay { RoundedRectangle(cornerRadius: 16).strokeBorder(.primary.opacity(0.07)) }
    }
    private var devices: some View {
        VStack(spacing: 16) {
            Text("面板").font(.title2.bold()).frame(maxWidth: .infinity, alignment: .leading)
            HomeDeviceCards(home: home, devices: home.dailyPanels, openDetails: { editor = $0 })
            if let notice = home.unifiedKeypadSupportMessage { Text(notice).font(.caption).foregroundStyle(.secondary) }
            if home.dailyPanels.isEmpty { Text("暂无当前凭证可见的面板").foregroundStyle(.secondary) }
            HStack {
                Text("DIN 模块").font(.title2.bold())
                Spacer()
                Button("读取硬件清单", systemImage: "arrow.clockwise") { Task { await home.loadPhysicalModules() } }
                    .disabled(!home.connected || !home.canManage || home.loadingModules)
            }
            if home.loadingModules { ProgressView("正在读取硬件清单") }
            if !home.canManage { Text("DIN 模块清单需要管理员权限。").font(.caption).foregroundStyle(.secondary) }
            if !home.moduleInventoryMessage.isEmpty { Text(home.moduleInventoryMessage).font(.caption).foregroundStyle(.secondary) }
            LazyVGrid(columns: cardColumns, alignment: .leading, spacing: 16) {
                ForEach(home.physicalHardwareModules) { module in
                    if module.dinChannelIndicators.count == 8 {
                        HomeDINModuleCard(home: home, module: module) {
                            editor = HomeEditRequest(kind: "module-details", record: HomeRecord(.object(["id": .string(module.id)])))
                        }
                    } else {
                        panel(module.modelName ?? module.profile, icon: "cpu") {
                            Text("地址 \(module.address) · HEX").font(.title3.monospaced())
                            Text("\(module.gateway) · \(module.channelCount) 通道").font(.caption).foregroundStyle(.secondary)
                            Button("设置与测试", systemImage: "slider.horizontal.3") {
                                editor = HomeEditRequest(kind: "module-details", record: HomeRecord(.object(["id": .string(module.id)])))
                            }.accessibilityIdentifier("module.\(module.id).details")
                        }
                    }
                }
            }
            if managing {
                editNotice
                HStack {
                    Text("设备配置").font(.headline)
                    Spacer()
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
            }
        }
        .task(id: home.moduleInventoryContext) {
            await home.loadPhysicalModules()
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(15)) } catch { break }
                if home.snapshotIsCurrent { await home.loadPhysicalModules() }
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
                        Text(home.scenePlacementTitle(scene, editing: true))
                            .font(.caption).foregroundStyle(.secondary)
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
        // This catalog has no keypad cards; retain every authorized scene entry.
        HomeSceneCards(home: home, scenes: home.scenes, openDetails: { editor = $0 })
    }
    private var configuration: some View {
        VStack(spacing: 16) {
            if !home.canManage { empty("需要管理员权限", "当前凭证不能管理配置。") }
            else {
                DisclosureGroup("逻辑设备配置") {
                    Button("新增逻辑设备", systemImage: "plus") { editor = HomeEditRequest(kind: "device") }.disabled(!canEdit)
                    ForEach(home.editableDevices) { device in
                        panel(device.name, icon: "switch.2") {
                            Text(device.id).font(.caption).foregroundStyle(.secondary)
                            HStack {
                                Button("编辑设备") { editor = HomeEditRequest(kind: "device", record: device) }
                                Button("移除", role: .destructive) { removal = HomeEditRequest(kind: "device", record: device) }
                            }.disabled(!canEdit)
                        }
                    }
                }
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

private struct HomeDINModuleCard: View {
    @ObservedObject var home: HomeServerStore
    let module: HomePhysicalModule
    let openDetails: () -> Void
    private var hasFeedback: Bool { home.snapshotIsCurrent && module.dinHasFeedback }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .top, spacing: 8) {
                VStack(alignment: .leading, spacing: 4) {
                    Label(module.modelName ?? module.profile, systemImage: "cpu").font(.headline)
                    Text("地址 \(module.address) · HEX · \(module.gateway)")
                        .font(.caption.monospaced()).foregroundStyle(.secondary)
                }
                Spacer(minLength: 4)
                VStack(spacing: 3) {
                    Circle().fill(hasFeedback ? Color.red : Color.red.opacity(0.16))
                        .overlay { Circle().strokeBorder(.red.opacity(0.6)) }
                        .frame(width: 17, height: 17)
                    Text(hasFeedback ? "有响应" : "待核").font(.caption2)
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("模块状态：\(hasFeedback ? "已收到反馈" : "暂无有效反馈，供电状态待核")")
                .accessibilityIdentifier("module.\(module.id).powerLED")
            }
            HStack(alignment: .top, spacing: 4) {
                ForEach(module.dinChannelIndicators) { indicator in
                    channel(indicator).frame(maxWidth: .infinity)
                }
            }
            Text("按实际反馈显示；暂无有效反馈时供电状态待核。")
                .font(.caption2).foregroundStyle(.secondary)
            Button("设置与测试", systemImage: "slider.horizontal.3", action: openDetails)
                .accessibilityIdentifier("module.\(module.id).details")
        }
        .padding(16).frame(maxWidth: .infinity, alignment: .leading)
        .background(.background, in: RoundedRectangle(cornerRadius: 16))
        .overlay { RoundedRectangle(cornerRadius: 16).strokeBorder(.primary.opacity(0.07)) }
    }

    private func channel(_ indicator: HomeDINChannelIndicator) -> some View {
        let on = home.snapshotIsCurrent ? indicator.isOn : nil
        let level = home.snapshotIsCurrent ? indicator.brightness : nil
        return VStack(spacing: 4) {
            Text("\(indicator.number)").font(.caption2.monospacedDigit())
            Circle()
                .fill(on == true ? Color.green : on == false ? Color.green.opacity(0.15) : Color.gray.opacity(0.22))
                .overlay { Circle().strokeBorder(on == nil ? Color.gray.opacity(0.5) : Color.green.opacity(0.7)) }
                .frame(width: 17, height: 17)
            if module.profile == "ad8_din_rs485_v2_2_1" {
                Text(level.map { "\($0)%" } ?? "—")
                    .font(.system(size: 10, design: .rounded)).monospacedDigit()
                    .foregroundStyle(level == nil ? Color.secondary : Color.primary)
                    .lineLimit(1).minimumScaleFactor(0.8)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("通道 \(indicator.number)：\(on.map { $0 ? "开启" : "关闭" } ?? "无反馈")" + (level.map { "，亮度 \($0)%" } ?? ""))
        .accessibilityIdentifier("module.\(module.id).channel.\(indicator.number)")
    }
}

/// Scene groups use their actual entry count and the existing admission path.
private struct HomeSceneCards: View {
    @ObservedObject var home: HomeServerStore
    let scenes: [HomeRecord]
    let openDetails: (HomeEditRequest) -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            ForEach(home.scenePanels(for: scenes)) { panel in
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        Label(panel.title, systemImage: "sparkles").font(.headline)
                        Spacer(minLength: 4)
                        Text("\(panel.scenes.count) 个").font(.caption).foregroundStyle(.secondary)
                    }
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 160), alignment: .top)], alignment: .leading, spacing: 8) {
                        ForEach(panel.scenes) { scene in
                            sceneKey(scene).frame(height: 82)
                        }
                    }
                }
                .padding(16)
                .background(.background, in: RoundedRectangle(cornerRadius: 20))
                .overlay { RoundedRectangle(cornerRadius: 20).strokeBorder(.primary.opacity(0.08)) }
                .accessibilityIdentifier("home.scenePanel.\(panel.id)")
            }
        }
    }
    private func sceneKey(_ scene: HomeRecord) -> some View {
        let state = HomeSceneCardState(scene: scene, current: home.snapshotIsCurrent && scene.targetAvailable,
            available: home.canControlScene(scene.id), pending: home.sceneActionPresentation(scene.id))
        return HomeSceneKey(state: state, activate: {
            Task { await home.controlScene(state.id, on: state.action.on) }
        }, details: {
            // Resolve the current record on demand; presentation equality must
            // not leave the detail sheet using an old observation timestamp.
            guard let latest = home.scenes.first(where: { $0.id == state.id }) else { return }
            openDetails(HomeEditRequest(kind: "scene-details", record: latest))
        }).equatable()
    }
}

/// No store subscription here: unrelated feedback and transport liveness may
/// update the parent, but only changed presentation inputs redraw this key.
private struct HomeSceneKey: View, Equatable {
    let state: HomeSceneCardState
    let activate: () -> Void
    let details: () -> Void
    nonisolated static func == (lhs: Self, rhs: Self) -> Bool { lhs.state == rhs.state }
    var body: some View {
        let available = state.available, current = state.current
        let pending = state.pending, selected = state.selected, action = state.action
        return ZStack(alignment: .topTrailing) {
            Button {
                activate()
            } label: {
                VStack(alignment: .leading, spacing: 3) {
                    Text(state.name).font(.caption.weight(.semibold)).lineLimit(1).minimumScaleFactor(0.65).padding(.trailing, 22)
                    HStack(spacing: 3) {
                        Text(pending?.title ?? (available ? action.title : "不可用")).font(.caption2.weight(.medium))
                        if let selection = state.selectionDescription {
                            Text(selection)
                                .font(.system(size: 9)).foregroundStyle(.secondary).lineLimit(1).minimumScaleFactor(0.7)
                        }
                    }
                    Text(state.activityDescription)
                        .font(.system(size: 9)).foregroundStyle(.secondary).lineLimit(1).minimumScaleFactor(0.7)
                }.padding(8).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
                    .background(pending != nil && current ? (selected ? Color.teal.opacity(0.24) : Color.blue.opacity(0.13)) : available ? (selected && current ? Color.teal.opacity(0.17) : Color.primary.opacity(0.025)) : Color.gray.opacity(0.16), in: RoundedRectangle(cornerRadius: 10))
                    .overlay { RoundedRectangle(cornerRadius: 10).strokeBorder(available ? Color.primary.opacity(0.08) : Color.gray.opacity(0.35)) }
                    .contentShape(RoundedRectangle(cornerRadius: 10))
            }.buttonStyle(.plain).disabled(!available)
                .accessibilityLabel("\(state.name)，\(action.title)")
                .accessibilityHint(available ? "执行场景动作；等待服务端反馈" : "控制暂不可用，保留历史状态")
                .accessibilityIdentifier("home.sceneAction.\(state.id)")
            Button { details() } label: {
                Image(systemName: "info.circle").font(.caption).frame(width: 34, height: 30).contentShape(Rectangle())
            }.buttonStyle(.plain)
                .accessibilityLabel("\(state.name)详情")
                .accessibilityIdentifier("home.sceneDetails.\(state.id)")
        }.frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// Shared by the overview, device catalog and room destination so every control
/// observes the same store and keeps its existing admission and LED behavior.
private struct HomeDeviceCards: View {
    @ObservedObject var home: HomeServerStore
    let devices: [HomeRecord]
    let openDetails: (HomeEditRequest) -> Void
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    private var minimumCardWidth: CGFloat { sizeClass == .regular ? 300 : 260 }
    #else
    private let minimumCardWidth: CGFloat = 300
    #endif
    private var cardColumns: [GridItem] { [GridItem(.adaptive(minimum: minimumCardWidth), alignment: .top)] }
    var body: some View {
        LazyVGrid(columns: cardColumns, alignment: .leading, spacing: 16) {
            ForEach(devices) { device in
                if device.raw["kind"].text == "Keypad", device.raw["module_id"].text != nil {
                    if let keypad = try? HomeKeypad(record: device, current: home.snapshotIsCurrent) {
                        HomeKeypadCard(keypad: keypad, controlsAvailable: home.snapshotIsCurrent,
                            activate: { number in
                                let operation = home.beginUnifiedKeypadClick(device.id, key: number)
                                Task { await home.activateUnifiedKeypad(device.id, key: number, operation: operation) }
                            },
                            details: { openDetails(HomeEditRequest(kind: "keypad-details", record: device)) },
                            addressDescription: home.physicalModule(for: device).map { "地址 \($0.address) · HEX" },
                            isActionAvailable: { home.canActivateUnifiedKeypad(device.id, key: $0) },
                            actionPresentation: { home.keypadActionPresentation(device.id, key: $0) },
                            isPhysicallyPressed: { home.keypadEventIsPressed(device.id, key: $0) },
                            canShowActionPresentation: { home.canDisplayKeypadActionCue(device.id, key: $0) })
                            .onAppear { home.recordUnifiedKeypadGates(device.id) }
                            .onChange(of: home.unifiedKeypadGateStates(device.id)) { _, _ in home.recordUnifiedKeypadGates(device.id) }
                    } else { ContentUnavailableView("面板数据暂不可用", systemImage: "square.grid.2x2", description: Text("请刷新或核对服务端接口。")) }
                } else {
                    HomeQuickDeviceCard(home: home, device: device) {
                        openDetails(HomeEditRequest(kind: "device-details", record: device))
                    }
                }
            }
        }
    }
}

private struct HomeRoomDetail: View {
    @ObservedObject var home: HomeServerStore
    let roomID: String
    let openDetails: (HomeEditRequest) -> Void
    private var managing: Bool { home.interfaceMode == .management && home.canManage }
    private var activeRoom: HomeRecord? { home.rooms.first { $0.id == roomID } }
    private var room: HomeRecord? {
        activeRoom ?? (managing ? home.editableRooms.first { $0.id == roomID } : nil)
    }
    private var devices: [HomeRecord] { home.roomDevices(inRoom: roomID) }
    // The room has no keypad cards. Every explicitly assigned scene needs its
    // own entry, even when a keypad elsewhere directly binds that scene.
    private var scenes: [HomeRecord] { home.scenes(inRoom: roomID) }
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                if let room {
                    VStack(alignment: .leading, spacing: 6) {
                        Label(room.raw["floor"].text ?? "未分配楼层", systemImage: "building.2")
                            .font(.caption).foregroundStyle(.secondary)
                        Text(room.name).font(.largeTitle.bold())
                        Text("\(devices.count) 个灯与窗帘 · \(scenes.count) 个场景").font(.subheadline).foregroundStyle(.secondary)
                    }
                    if let away = home.awayStatus {
                        Label(away.message, systemImage: away.failed ? "exclamationmark.triangle" : "house")
                            .font(.caption).foregroundStyle(away.failed ? Color.red : Color.secondary)
                    }
                    if activeRoom == nil {
                        Text("此房间尚未发布；日常设备和场景以活动配置为准。").font(.subheadline).foregroundStyle(.secondary)
                    }
                    if !devices.isEmpty {
                        Text("灯与窗帘").font(.title2.bold())
                        HomeDeviceCards(home: home, devices: devices, openDetails: openDetails)
                    }
                    if !scenes.isEmpty {
                        Text("场景").font(.title2.bold())
                        HomeSceneCards(home: home, scenes: scenes, openDetails: openDetails)
                    }
                    if devices.isEmpty && scenes.isEmpty {
                        ContentUnavailableView("暂无可见控制", systemImage: "lightbulb", description: Text("此房间尚无当前凭证可见的灯、窗帘或场景。"))
                    }
                } else {
                    ContentUnavailableView("房间暂不可见", systemImage: "door.left.hand.open", description: Text("房间可能已调整，或当前凭证无权查看。"))
                }
            }
            .padding(.horizontal, 12).padding(.vertical, 24)
            .frame(maxWidth: 1100, alignment: .leading).frame(maxWidth: .infinity)
        }
        .background(.background.secondary)
        .navigationTitle(room?.name ?? "房间")
        .accessibilityIdentifier("home.roomDetail.\(roomID)")
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                HomeSyncStatus(home: home)
                if home.canManage {
                    Button(managing ? "完成管理" : "管理", systemImage: managing ? "checkmark" : "slider.horizontal.3") {
                        home.selectInterfaceMode(managing ? .daily : .management)
                    }
                }
            }
        }
    }
}

struct HomeDeviceCard: View {
    @ObservedObject var home: HomeServerStore
    let device: HomeRecord
    @State private var brightness = HomeBrightnessDraft()
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(device.name).font(.headline)
            if let name = device.raw["room"]["name"].text { Text(name).font(.caption).foregroundStyle(.secondary) }
            if let status = device.targetAvailability.description { Text(status).font(.caption).foregroundStyle(.secondary) }
            Text(home.snapshotIsCurrent && device.targetAvailable ? "反馈质量：\(device.raw["quality"].text ?? "未知")" : "状态未同步，以下保留历史反馈").font(.caption).foregroundStyle(.secondary)
            if let on = device.raw["observed"]["on"].optionalBool {
                Text((device.feedbackIsConfirmed(current: home.snapshotIsCurrent) ? "实际反馈：" : "历史反馈：") + (on ? "开" : "关"))
            }
            else { Text("实际反馈：未知").foregroundStyle(.secondary) }
            if device.raw["state_authority"].text == "pending_target" { Text("有待确认的目标；以上仅显示实际反馈。").font(.caption).foregroundStyle(.orange) }
            if device.raw["kind"].text == "Motor" {
                Text(device.motorStatusDescription(current: home.snapshotIsCurrent)).font(.subheadline).foregroundStyle(.secondary)
                HomeMotorControls(home: home, device: device)
            } else if device.raw["kind"].text == "Keypad" {
                HStack {
                    action("红灯", .object(["action": .string("panel_led"), "on": .bool(true)]))
                    action("绿灯", .object(["action": .string("panel_led"), "on": .bool(false)]))
                }
            } else {
                HomeLightPowerControls(home: home, device: device, beforeAction: {
                    brightness.cancel(); syncLevel()
                })
            }
            if device.raw["kind"].text == "Dimmer" {
                if let actual = device.raw["observed"]["level"].integerText {
                    Text("\(device.feedbackIsConfirmed(current: home.snapshotIsCurrent) ? "读回亮度" : "历史读回亮度")：\(actual)%")
                        .font(.caption).foregroundStyle(.secondary)
                }
                VStack(spacing: 12) {
                    Text("\(Int(brightness.value))%")
                        .font(.system(size: 44, weight: .medium, design: .rounded)).monospacedDigit()
                        .accessibilityIdentifier("device.\(device.id).brightnessTarget")
                    Slider(value: Binding(get: { brightness.value }, set: { brightness.setValue($0) }), in: 0...100, step: 1, onEditingChanged: { editing in
                        if editing { brightness.beginEditing() } else { submitBrightness() }
                    })
                    .controlSize(.large).tint(.orange)
                    .frame(width: 240, height: 64).rotationEffect(.degrees(-90)).frame(width: 88, height: 250)
                    .background(.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 24))
                    .accessibilityLabel("亮度").accessibilityValue("\(Int(brightness.value))%")
                    .accessibilityIdentifier("device.\(device.id).brightness")
                    .accessibilityAdjustableAction { direction in
                        guard home.canControlDevice(device.id) else { return }
                        brightness.beginEditing()
                        switch direction {
                        case .increment: brightness.setValue(brightness.value + 5)
                        case .decrement: brightness.setValue(brightness.value - 5)
                        @unknown default: brightness.cancel(); return
                        }
                        submitBrightness()
                    }
                    .disabled(!home.canControlDevice(device.id))
                    if brightness.submission != nil {
                        Text("目标 \(Int(brightness.value))%，等待设备反馈").font(.caption).foregroundStyle(.secondary)
                    } else if let target = brightness.unconfirmedTarget {
                        Text("上次目标 \(target)% 尚未确认").font(.caption).foregroundStyle(.secondary)
                    }
                }.frame(maxWidth: .infinity)
            }
        }
        .padding(20).frame(maxWidth: .infinity, alignment: .leading).background(.background, in: RoundedRectangle(cornerRadius: 16))
        .onAppear { syncLevel() }
        .onChange(of: device.raw) { _, _ in syncLevel() }
        .onChange(of: home.snapshotIsCurrent) { _, _ in syncLevel() }
        .task(id: brightness.submission?.id) {
            guard let id = brightness.submission?.id else { return }
            do { try await Task.sleep(for: .seconds(10)) } catch { return }
            brightness.expire(id); syncLevel()
        }
    }
    private var feedbackTimestamp: UInt64? { device.raw["observed_at_ms"].integerText.flatMap(UInt64.init) }
    private func syncLevel() {
        brightness.observe(level: device.raw["observed"]["level"].integerText.flatMap(Int64.init),
                           confirmed: device.feedbackIsConfirmed(current: home.snapshotIsCurrent), timestamp: feedbackTimestamp)
    }
    private func submitBrightness() {
        guard let submission = brightness.endEditing(feedbackTimestamp: feedbackTimestamp) else { return }
        let operation = home.beginDeviceControl(device.id)
        Task {
            let result = await home.controlDevice(device.id, command: .object(["action": .string("brightness"), "level": .integer(submission.level)]), operation: operation)
            brightness.complete(submission.id, accepted: result.accepted)
            syncLevel()
        }
    }
    private func action(_ title: String, _ command: HomeJSON) -> some View {
        Button(title) {
            let operation = home.beginDeviceControl(device.id)
            Task { await home.controlDevice(device.id, command: command, operation: operation) }
        }
            .buttonStyle(.bordered).disabled(!home.canControlDevice(device.id))
    }
}
private extension HomeJSON {
    var optionalBool: Bool? { if case .bool(let value) = self { value } else { nil } }
}

private extension String {
    func nonemptyOr(_ alternative: String) -> String { isEmpty ? alternative : self }
}

private struct HomeDiagnosticExport: FileDocument {
    static var readableContentTypes: [UTType] { [.plainText] }
    var text: String
    init(text: String) { self.text = text }
    init(configuration: ReadConfiguration) throws {
        text = configuration.file.regularFileContents.flatMap { String(data: $0, encoding: .utf8) } ?? ""
    }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: Data(text.utf8))
    }
}
