#if os(iOS)
import SwiftUI
@preconcurrency import HomeKit
import HomeSenseCore

enum HomeKitDailyCategory: String, CaseIterable, Identifiable {
    case climate = "温控", lights = "灯", security = "安防", curtains = "窗帘", switches = "开关", sensors = "传感器"
    var id: Self { self }
    var symbol: String {
        switch self {
        case .climate: "thermometer.medium"
        case .lights: "lightbulb.fill"
        case .security: "shield.lefthalf.filled"
        case .curtains: "curtains.closed"
        case .switches: "power"
        case .sensors: "sensor.fill"
        }
    }
    @MainActor func contains(_ item: HomeKitDailyStore.Service) -> Bool {
        switch self {
        case .climate: [.fan, .temperature, .humidity].contains(item.kind)
        case .lights: item.kind == .light
        case .security: [.motion, .contact].contains(item.kind)
        case .curtains: item.kind == .curtain
        case .switches: item.kind == .switchControl
        case .sensors: ![.light, .fan, .switchControl, .curtain].contains(item.kind)
        }
    }
}

private enum HomeKitDailyRoute: Hashable {
    case home, favorites, category(HomeKitDailyCategory), room(UUID?)
}
private struct HomeKitServiceSelection: Identifiable { let id: UUID }

struct HomeKitDailyView: View {
    @ObservedObject var store: HomeKitDailyStore
    @State private var route: HomeKitDailyRoute? = .home
    @State private var compactColumn: NavigationSplitViewColumn = .detail
    @State private var selectedService: HomeKitServiceSelection?
    @State private var showHiddenAccessories = false
    @Environment(\.horizontalSizeClass) private var sizeClass
    @Environment(\.dynamicTypeSize) private var typeSize
    private var title: String {
        switch route ?? .home {
        case .home: store.homeName
        case .favorites: "常用配件"
        case .category(let category): category.rawValue
        case .room(let id): store.rooms.first(where: { $0.id == id })?.name ?? "未分配房间"
        }
    }
    private var isHome: Bool { route == .home || route == nil }
    private var visibleServices: [HomeKitDailyStore.Service] {
        store.displayedServices.filter { item in
            switch route ?? .home {
            case .home: true
            case .favorites: store.favoriteIDs.contains(item.id)
            case .category(let category): category.contains(item)
            case .room(let id): item.roomID == id
            }
        }
    }
    private var columns: [GridItem] {
        [GridItem(.adaptive(minimum: typeSize.isAccessibilitySize ? 260 : 150, maximum: 230), spacing: 12, alignment: .top)]
    }
    var body: some View {
        NavigationSplitView(preferredCompactColumn: $compactColumn) {
            List(selection: $route) {
                Section {
                    sidebar("家庭", symbol: "house.fill", route: .home)
                    sidebar("常用配件", symbol: "star.fill", route: .favorites)
                }
                Section("类别") {
                    ForEach(HomeKitDailyCategory.allCases) { category in
                        if store.displayedServices.contains(where: category.contains) {
                            sidebar(category.rawValue, symbol: category.symbol, route: .category(category))
                        }
                    }
                }
                Section("房间") {
                    ForEach(store.rooms) { room in sidebar(room.name, symbol: "door.left.hand.open", route: .room(room.id)) }
                    if store.displayedServices.contains(where: { $0.roomID == nil }) {
                        sidebar("未分配房间", symbol: "square.grid.2x2", route: .room(nil))
                    }
                }
            }
            .listStyle(.sidebar).navigationTitle("家庭")
            .navigationSplitViewColumnWidth(min: 190, ideal: 220, max: 280)
        } detail: {
            NavigationStack {
                ScrollView {
                    VStack(alignment: .leading, spacing: 26) {
                        header
                        if isHome { categories }
                        if !store.authorized {
                            ContentUnavailableView("连接 Apple 家庭", systemImage: "house", description: Text(store.status))
                        } else if store.selectedHomeID == nil {
                            ContentUnavailableView("选择家庭", systemImage: "house", description: Text("请从右上角的家庭菜单选择要使用的家庭。"))
                        } else if store.services.isEmpty {
                            ContentUnavailableView("暂无配件", systemImage: "lightbulb", description: Text(store.status))
                        } else if store.displayedServices.isEmpty && (!isHome || store.scenes.isEmpty) {
                            ContentUnavailableView {
                                Label("配件已隐藏", systemImage: "eye.slash")
                            } description: {
                                Text("可在隐藏配件设置中恢复显示。")
                            } actions: {
                                Button("管理隐藏配件") { showHiddenAccessories = true }
                            }
                        } else if isHome {
                            if !store.favoriteIDs.isEmpty {
                                section("常用配件", items: store.displayedServices.filter { store.favoriteIDs.contains($0.id) }, showRoom: true)
                            }
                            if !store.scenes.isEmpty { scenes }
                            ForEach(store.rooms) { room in
                                section(room.name, items: store.displayedServices.filter { $0.roomID == room.id }, destination: .room(room.id))
                            }
                            section("未分配房间", items: store.displayedServices.filter { $0.roomID == nil }, destination: .room(nil))
                        } else {
                            if visibleServices.isEmpty {
                                ContentUnavailableView(route == .favorites ? "还没有常用配件" : "暂无配件", systemImage: route == .favorites ? "star" : "square.grid.2x2", description: Text(route == .favorites ? "长按配件卡片，将常用的灯或窗帘加入这里。" : "此房间或类别暂时没有可显示的配件。"))
                            }
                            grid(visibleServices, showRoom: { if case .room = route { return false }; return true }())
                        }
                        if let error = store.error { Label(error, systemImage: "exclamationmark.triangle").font(.callout).foregroundStyle(.orange) }
                        let identity = HomeBuildIdentity(info: Bundle.main.infoDictionary ?? [:])
                        Text("HomeSense \(identity.version) Build \(identity.displayBuild)")
                            .font(.caption2).foregroundStyle(.secondary).accessibilityIdentifier("home.appBuild")
                    }
                    .padding(.horizontal, sizeClass == .compact ? 18 : 28).padding(.vertical, 24)
                    .frame(maxWidth: 1300, alignment: .leading).frame(maxWidth: .infinity, alignment: .top)
                }
                .background(HomeKitCanvas())
                .navigationTitle(isHome ? "家庭" : title).navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    if sizeClass == .compact {
                        ToolbarItem(placement: .topBarLeading) {
                            Button("房间", systemImage: "sidebar.left") { compactColumn = .sidebar }.accessibilityIdentifier("homekit.rooms")
                        }
                    }
                    ToolbarItem(placement: .primaryAction) {
                        Menu {
                            Picker("家庭", selection: Binding(get: { store.selectedHomeID }, set: { store.selectHome($0); route = .home })) {
                                Text("选择家庭").tag(UUID?.none)
                                ForEach(store.homes) { Text($0.name).tag(Optional($0.id)) }
                            }
                            Button("隐藏配件", systemImage: "eye.slash") { showHiddenAccessories = true }
                                .disabled(store.selectedHomeID == nil)
                            Button("重新读取状态", systemImage: "arrow.clockwise") { store.synchronize() }
                        } label: { Image(systemName: "ellipsis.circle").font(.title3) }
                        .accessibilityLabel("家庭与状态设置")
                    }
                }
                .sheet(item: $selectedService) { selection in
                    if let item = store.services.first(where: { $0.id == selection.id }) {
                        HomeKitAccessoryDetails(store: store, item: item)
                            .presentationDetents([.large]).presentationDragIndicator(.visible)
                    }
                }
                .sheet(isPresented: $showHiddenAccessories) {
                    HomeKitHiddenAccessoriesView(store: store)
                }
            }
        }
        .navigationSplitViewStyle(.balanced)
        .onChange(of: route) { _, _ in compactColumn = .detail }
        .onChange(of: store.selectedHomeID) { _, _ in route = .home; selectedService = nil }
        .onChange(of: store.observations.generation) { _, _ in selectedService = nil }
    }
    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.largeTitle.bold()).accessibilityAddTraits(.isHeader)
            HStack(spacing: 6) {
                Text(store.authorized ? (isHome ? "\(store.rooms.count) 个房间 · \(store.displayedServices.count) 个配件" : "\(visibleServices.count) 个配件") : store.status)
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }
    private func sidebar(_ name: String, symbol: String, route destination: HomeKitDailyRoute) -> some View {
        NavigationLink(value: destination) { Label(name, systemImage: symbol) }
    }
    private var categories: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 10) {
                ForEach(HomeKitDailyCategory.allCases) { category in
                    if store.displayedServices.contains(where: category.contains) {
                        Button { route = .category(category) } label: {
                            HStack(spacing: 10) {
                                Image(systemName: category.symbol).font(.title3).foregroundStyle(category == .lights ? Color.orange : .primary)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(category.rawValue).font(.subheadline.weight(.semibold))
                                    Text(summary(category)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                                }
                            }.padding(.horizontal, 17).padding(.vertical, 12)
                                .background(.regularMaterial, in: Capsule())
                        }.buttonStyle(.plain).accessibilityIdentifier("homekit.category.\(category.id)")
                    }
                }
            }
        }
    }
    private func summary(_ category: HomeKitDailyCategory) -> String {
        let items = store.displayedServices.filter(category.contains)
        let values = items.map(store.presentation)
        if category == .lights { return HomeAccessoryPresentation.lightSummary(values) }
        if category == .climate {
            let temperatures = values.filter { $0.kind == .temperature }.compactMap(\.number)
            if let low = temperatures.min(), let high = temperatures.max() {
                let format = FloatingPointFormatStyle<Double>.number.precision(.fractionLength(0...1))
                return low == high ? low.formatted(format) + "°" : low.formatted(format) + "–" + high.formatted(format) + "°"
            }
        }
        return "\(items.count) 个配件"
    }
    @ViewBuilder private func section(_ name: String, items: [HomeKitDailyStore.Service], showRoom: Bool = false, destination: HomeKitDailyRoute? = nil) -> some View {
        if !items.isEmpty {
            VStack(alignment: .leading, spacing: 12) {
                if let destination {
                    Button { route = destination } label: {
                        HStack { Text(name).font(.title2.bold()); Image(systemName: "chevron.right").font(.subheadline.weight(.semibold)).foregroundStyle(.secondary) }
                    }.buttonStyle(.plain)
                } else { Text(name).font(.title2.bold()) }
                grid(items, showRoom: showRoom)
            }
        }
    }
    private func grid(_ items: [HomeKitDailyStore.Service], showRoom: Bool) -> some View {
        LazyVGrid(columns: columns, alignment: .leading, spacing: 12) {
            ForEach(items) { item in
                HomeKitAccessoryTile(store: store, item: item, showsRoom: showRoom) { selectedService = HomeKitServiceSelection(id: item.id) }
            }
        }
    }
    private var scenes: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("场景").font(.title2.bold())
            LazyVGrid(columns: columns, alignment: .leading, spacing: 12) {
                ForEach(store.scenes, id: \.uniqueIdentifier) { scene in
                    Button { Task { await store.execute(scene) } } label: {
                        HStack(spacing: 12) {
                            Image(systemName: "sparkles").font(.title3).foregroundStyle(.orange)
                            Text(scene.name).font(.subheadline.weight(.semibold)).multilineTextAlignment(.leading)
                            Spacer(minLength: 0)
                            if store.busy.contains(scene.uniqueIdentifier) { ProgressView().controlSize(.mini) }
                        }.padding(16).frame(maxWidth: .infinity, minHeight: 70, alignment: .leading)
                            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 20))
                    }.buttonStyle(.plain).disabled(store.busy.contains(scene.uniqueIdentifier))
                }
            }
        }
    }
}

private struct HomeKitHiddenAccessoriesView: View {
    @ObservedObject var store: HomeKitDailyStore
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Toggle("隐藏“灯带”开头的配件", isOn: Binding(
                        get: { store.visibility.hidesLightStrips },
                        set: { store.setHidesLightStrips($0) }))
                        .accessibilityIdentifier("homekit.hidden.lightStrips")
                } footer: {
                    Text("自动隐藏名称以“灯带”开头的配件，新加入的同名配件也适用。可在下方单独恢复；重新开启此规则会再次隐藏全部匹配项。")
                }
                Section("已隐藏的配件（\(store.hiddenServices.count)）") {
                    if store.hiddenServices.isEmpty {
                        Text("没有已隐藏的配件").foregroundStyle(.secondary)
                    }
                    ForEach(store.hiddenServices) { item in
                        HStack {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(item.name)
                                Text(item.room).font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Button("恢复显示") { store.setHidden(false, id: item.id) }
                                .buttonStyle(.borderless)
                                .accessibilityLabel("恢复显示\(item.name)")
                        }
                    }
                }
                Section {
                    Button("恢复显示全部配件") { store.restoreAllHidden() }
                        .disabled(store.visibility == HomeAccessoryVisibility())
                } footer: {
                    Text("仅影响本机当前家庭的显示，不改变Apple家庭、场景或墙面板控制。其他设备需分别设置。隐藏不会清除常用配件，恢复后仍保留。")
                }
            }
            .navigationTitle("隐藏配件").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("完成") { dismiss() } } }
        }
    }
}

struct HomeKitCanvas: View {
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        ZStack {
            Color(uiColor: .systemGroupedBackground)
            LinearGradient(colors: [Color.orange.opacity(scheme == .dark ? 0.08 : 0.05), .clear, Color.blue.opacity(0.04)], startPoint: .topLeading, endPoint: .bottomTrailing)
        }.ignoresSafeArea()
    }
}

private struct HomeKitAccessoryTile: View {
    @ObservedObject var store: HomeKitDailyStore
    let item: HomeKitDailyStore.Service
    let showsRoom: Bool
    let open: () -> Void
    @Environment(\.colorScheme) private var scheme
    @Environment(\.dynamicTypeSize) private var typeSize
    private var presentation: HomeAccessoryPresentation { store.presentation(item) }
    private var active: Bool { presentation.isActive }
    private var busy: Bool { item.characteristics.contains { store.busy.contains($0.uniqueIdentifier) } }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Button(action: primaryAction) {
                    Image(systemName: item.kind.symbol).font(.system(size: 23, weight: .regular))
                        .foregroundStyle(active ? Color.orange : Color.secondary)
                        .frame(width: 45, height: 45).background(active ? Color.orange.opacity(0.14) : Color.secondary.opacity(0.10), in: Circle())
                }.buttonStyle(.plain)
                    .accessibilityLabel(item.name + (item.power != nil ? "开关" : "详情"))
                Spacer(minLength: 4)
                if busy { ProgressView().controlSize(.mini).tint(active ? .black : .primary) }
                else if !item.accessory.isReachable { Image(systemName: "exclamationmark.circle").foregroundStyle(.orange) }
            }
            Button(action: open) {
                VStack(alignment: .leading, spacing: 3) {
                    if showsRoom { Text(item.room).font(.caption).foregroundStyle(active ? .black.opacity(0.55) : .secondary).lineLimit(1) }
                    Text(item.name).font(.subheadline.weight(.semibold)).foregroundStyle(active ? .black : .primary).lineLimit(2).frame(maxWidth: .infinity, alignment: .leading)
                    Text(presentation.status).font(.subheadline).foregroundStyle(active ? .black.opacity(0.58) : .secondary).lineLimit(1)
                }.frame(maxWidth: .infinity, minHeight: 46, alignment: .leading).contentShape(Rectangle())
            }.buttonStyle(.plain)
        }
        .padding(16).frame(maxWidth: .infinity, alignment: .topLeading)
        .frame(height: typeSize.isAccessibilitySize ? nil : showsRoom ? 164 : 148, alignment: .topLeading)
        .background(active ? Color.white : Color(uiColor: scheme == .dark ? .secondarySystemGroupedBackground : .systemBackground), in: RoundedRectangle(cornerRadius: 22, style: .continuous))
        .shadow(color: .black.opacity(scheme == .dark ? 0 : 0.025), radius: 8, y: 3)
        .contextMenu {
            Button("打开控制", systemImage: "slider.horizontal.3", action: open)
            Button(store.favoriteIDs.contains(item.id) ? "移出常用配件" : "加入常用配件", systemImage: store.favoriteIDs.contains(item.id) ? "star.slash" : "star") { store.toggleFavorite(item.id) }
            Button("隐藏配件", systemImage: "eye.slash") { store.setHidden(true, id: item.id) }
        }
        .accessibilityIdentifier("homekit.tile.\(item.id)")
    }
    private func primaryAction() {
        if let power = item.power, let on = presentation.on, store.canWrite(power, in: item) {
            Task { await store.write(!on, to: power, in: item) }
        } else { open() }
    }
}

@MainActor extension HomeKitDailyStore.Service {
    func characteristic(_ type: String) -> HMCharacteristic? { characteristics.first { $0.characteristicType == type } }
    var power: HMCharacteristic? { characteristic(HMCharacteristicTypePowerState) }
    var brightness: HMCharacteristic? { characteristic(HMCharacteristicTypeBrightness) }
    var currentPosition: HMCharacteristic? { characteristic(HMCharacteristicTypeCurrentPosition) }
    var targetPosition: HMCharacteristic? { characteristic(HMCharacteristicTypeTargetPosition) }
    var holdPosition: HMCharacteristic? { characteristic(HMCharacteristicTypeHoldPosition) }
    var kind: HomeAccessoryKind {
        if service.serviceType == HMServiceTypeLightbulb { return .light }
        if service.serviceType == HMServiceTypeWindowCovering { return .curtain }
        if service.serviceType == HMServiceTypeFan { return .fan }
        if power != nil { return .switchControl }
        if service.characteristics.contains(where: { $0.characteristicType == HMCharacteristicTypeMotionDetected || $0.characteristicType == HMCharacteristicTypeOccupancyDetected }) { return .motion }
        if service.characteristics.contains(where: { $0.characteristicType == HMCharacteristicTypeContactState }) { return .contact }
        if characteristic(HMCharacteristicTypeCurrentTemperature) != nil { return .temperature }
        if characteristic(HMCharacteristicTypeCurrentRelativeHumidity) != nil { return .humidity }
        if characteristic(HMCharacteristicTypeMotionDetected) != nil || characteristic(HMCharacteristicTypeOccupancyDetected) != nil { return .motion }
        if characteristic(HMCharacteristicTypeContactState) != nil { return .contact }
        if characteristic(HMCharacteristicTypeCurrentLightLevel) != nil { return .illuminance }
        if characteristic(HMCharacteristicTypeBatteryLevel) != nil { return .battery }
        return .sensor
    }
    var observation: HMCharacteristic? {
        let type: String
        switch kind {
        case .light, .fan, .switchControl: return power
        case .curtain: return currentPosition
        case .temperature: type = HMCharacteristicTypeCurrentTemperature
        case .humidity: type = HMCharacteristicTypeCurrentRelativeHumidity
        case .motion: return characteristic(HMCharacteristicTypeMotionDetected) ?? characteristic(HMCharacteristicTypeOccupancyDetected)
        case .contact: type = HMCharacteristicTypeContactState
        case .illuminance: type = HMCharacteristicTypeCurrentLightLevel
        case .battery: type = HMCharacteristicTypeBatteryLevel
        case .sensor: return characteristics.first { HomeKitDailyStore.visibleTypes.contains($0.characteristicType) }
        }
        return characteristic(type)
    }
}

extension HomeKitDailyStore {
    func number(_ characteristic: HMCharacteristic?) -> Double? {
        guard let characteristic else { return nil }
        switch value(characteristic) {
        case .number(let value) where value.isFinite: return value
        case .boolean(let value): return value ? 1 : 0
        default: return nil
        }
    }
    func presentation(_ item: Service) -> HomeAccessoryPresentation {
        HomeAccessoryPresentation(kind: item.kind, value: item.observation.flatMap(value), brightness: number(item.brightness), reachable: item.accessory.isReachable)
    }
}
#endif
