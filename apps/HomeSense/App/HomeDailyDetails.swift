import SwiftUI
import HomeSenseCore

struct HomeModuleDetails: View {
    @ObservedObject var home: HomeServerStore
    let moduleID: String
    @State private var levels: [Int: Double] = [:]
    private var module: HomePhysicalModule? { home.moduleInventory.first { $0.id == moduleID } }
    var body: some View {
        NavigationStack {
            Form {
                if let module {
                    Section("物理模块") {
                        LabeledContent("型号", value: module.modelName ?? module.profile)
                        LabeledContent("总线地址（HEX）", value: module.address)
                        LabeledContent("网关", value: module.gateway)
                        LabeledContent("通道数", value: String(module.channelCount))
                        Text("\(module.profile)").font(.caption.monospaced()).foregroundStyle(.secondary)
                        Button("重新读取清单") { Task { await home.loadPhysicalModules() } }.disabled(home.loadingModules || !home.connected)
                    }
                    if module.supportsQuery {
                        Section("状态查询") {
                            Button("发送整模块查询") { Task { await home.queryPhysicalModule(moduleID) } }
                                .disabled(!home.canQueryPhysicalModule(moduleID))
                            Text("查询经服务端队列发送；请重新读取清单核对实际反馈。")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    ForEach(module.slots.indices, id: \.self) { index in
                        channel(module, slot: module.slots[index])
                    }
                } else { Text("模块清单已失效，请返回设备页重新读取。") }
                if !home.moduleInventoryMessage.isEmpty { Text(home.moduleInventoryMessage).font(.caption).foregroundStyle(.secondary) }
                Text(home.message).font(.caption).foregroundStyle(.secondary)
                if let error = home.error { Text(error).foregroundStyle(.red) }
            }
            .navigationTitle(module?.modelName ?? "模块设置与测试")
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("完成") { dismiss() } } }
        }
        #if os(macOS)
        .frame(minWidth: 520, minHeight: 620)
        #endif
    }
    @Environment(\.dismiss) private var dismiss
    @ViewBuilder private func channel(_ module: HomePhysicalModule, slot: HomeJSON) -> some View {
        if let number = slot["channel"].integerText.flatMap(Int.init) {
            Section("通道 \(number)") {
                if let name = slot["binding"]["name"].text { Text(name).font(.headline) }
                Text("读取时反馈：\(slot["actual"].formatted) · \(slot["quality"].text ?? "未知")")
                    .font(.caption).foregroundStyle(.secondary)
                if module.supportsQuery {
                    Button("查询此通道") { Task { await home.queryPhysicalModule(moduleID, channel: number) } }
                        .disabled(!home.canQueryPhysicalModule(moduleID))
                }
                let actions = slot["binding"]["actions"].values.compactMap(\.text)
                if actions.contains("power") {
                    HStack {
                        command("测试开启", number, .object(["action": .string("power"), "on": .bool(true)]))
                        command("测试关闭", number, .object(["action": .string("power"), "on": .bool(false)]))
                    }
                }
                if actions.contains("brightness") {
                    Slider(value: Binding(get: { levels[number] ?? 50 }, set: { levels[number] = $0 }), in: 0...100, step: 1)
                    command("设置亮度 \(Int(levels[number] ?? 50))%", number,
                        .object(["action": .string("brightness"), "level": .integer(Int64(levels[number] ?? 50))]))
                }
                if actions.contains("open") || actions.contains("close") || actions.contains("stop") {
                    HStack {
                        ForEach([("打开", "open"), ("停止", "stop"), ("关闭", "close")], id: \.1) { title, action in
                            if actions.contains(action) { command("测试\(title)", number, .object(["action": .string(action)])) }
                        }
                    }
                }
                if !actions.isEmpty { Text("测试指令会控制此通道的实际负载。").font(.caption).foregroundStyle(.secondary) }
            }
        }
    }
    private func command(_ title: String, _ channel: Int, _ command: HomeJSON) -> some View {
        Button(title) { Task { await home.controlPhysicalModule(moduleID, channel: channel, command: command) } }
            .disabled(!home.canControlPhysicalModule(moduleID, channel: channel, command: command))
    }
}

struct HomeDeviceTile: View {
    let device: HomeRecord
    var live = true
    var showsBackground = true
    private var confirmed: Bool { device.feedbackIsConfirmed(current: live) }
    private var isOn: Bool { device.raw["observed"]["on"] == .bool(true) && confirmed }
    private var icon: String {
        switch device.raw["kind"].text {
        case "Motor": "blinds.horizontal.closed"
        case "Dimmer": "lightbulb.fill"
        case "Keypad": "square.grid.2x2"
        default: "powerplug.fill"
        }
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Image(systemName: icon).font(.system(size: 30)).foregroundStyle(isOn ? Color.orange : Color.secondary)
                Spacer()
                Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary)
            }
            Text(device.name).font(.title3.weight(.semibold)).lineLimit(2)
            if let status = device.targetAvailability.description { Text(status).font(.caption).foregroundStyle(.secondary) }
            if let room = device.raw["room"]["name"].text { Text(room).font(.caption).foregroundStyle(.secondary) }
            (Text(status).font(.title3.weight(.medium)).foregroundColor(confirmed ? .primary : .secondary)
                + Text(device.raw["state_authority"].text == "pending_target" ? " · 目标待确认" : "")
                    .font(.caption).foregroundColor(.orange))
                .lineLimit(1).minimumScaleFactor(0.65)
                .frame(height: 25, alignment: .leading)
        }
        .padding(.horizontal, 14).padding(.vertical, 18)
        .frame(maxWidth: .infinity, minHeight: 170, alignment: .topLeading)
        .background(showsBackground ? (isOn ? Color.orange.opacity(0.09) : Color.primary.opacity(0.025)) : Color.clear, in: RoundedRectangle(cornerRadius: 20))
        .accessibilityElement(children: .combine)
    }
    private var status: String {
        if device.raw["kind"].text == "Motor" { return device.motorStatusDescription(current: live) }
        let prefix = confirmed ? "实测 · " : "历史 · "
        if let level = device.raw["observed"]["level"].integerText { return prefix + level + "%" }
        if device.raw["observed"]["on"] == .bool(true) { return prefix + "开启" }
        if device.raw["observed"]["on"] == .bool(false) { return prefix + "关闭" }
        return "实测状态未知"
    }
}

struct HomeQuickDeviceCard: View {
    @ObservedObject var home: HomeServerStore
    let device: HomeRecord
    let openDetails: () -> Void
    var body: some View {
        VStack(spacing: 0) {
            Button(action: openDetails) {
                HomeDeviceTile(device: device, live: home.snapshotIsCurrent, showsBackground: false)
            }.buttonStyle(.plain)
                .accessibilityIdentifier("device.\(device.id).details")
                .accessibilityHint("查看设备详情")
            if ["Motor", "Dimmer", "Switch"].contains(device.raw["kind"].text ?? "") {
                Divider().padding(.horizontal, 14)
                Group {
                    if device.raw["kind"].text == "Motor" { HomeMotorControls(home: home, device: device) }
                    else { HomeLightPowerControls(home: home, device: device) }
                }.padding(.horizontal, 12).padding(.vertical, 14)
            }
        }
        .background(.background, in: RoundedRectangle(cornerRadius: 20))
        .overlay { RoundedRectangle(cornerRadius: 20).strokeBorder(.primary.opacity(0.07)) }
    }
}

struct HomeMotorControls: View {
    @ObservedObject var home: HomeServerStore
    let device: HomeRecord
    var body: some View {
        HStack(spacing: 8) {
            action("打开", "arrow.up", .open)
            action("停止", "stop.fill", .stop)
            action("关闭", "arrow.down", .close)
        }
    }
    private func action(_ title: String, _ icon: String, _ action: HomeMotorControlAction) -> some View {
        Button {
            let operation = home.beginDeviceControl(device.id)
            Task { await home.controlDevice(device.id, command: action.command, operation: operation) }
        } label: {
            ViewThatFits(in: .horizontal) {
                Label(title, systemImage: icon).fixedSize(horizontal: true, vertical: false)
                Text(title).lineLimit(1).minimumScaleFactor(0.8)
            }
            .frame(maxWidth: .infinity, minHeight: 28)
        }
            .buttonStyle(.bordered)
            .disabled(action == .stop ? !home.canStopMotorDevice(device.id) : !home.canControlDevice(device.id))
            .accessibilityLabel("\(device.name)\(title)")
            .accessibilityIdentifier("device.\(device.id).\(action.rawValue)")
    }
}

struct HomeLightPowerControls: View {
    @ObservedObject var home: HomeServerStore
    let device: HomeRecord
    var beforeAction: () -> Void = { }
    private var confirmedOn: Bool? {
        guard device.feedbackIsConfirmed(current: home.snapshotIsCurrent), case .bool(let value) = device.raw["observed"]["on"] else { return nil }
        return value
    }
    var body: some View {
        HStack {
            if let on = confirmedOn { power(on ? "关闭" : "开启", on: !on) }
            else { power("开", on: true); power("关", on: false) }
        }.disabled(!home.canControlDevice(device.id))
    }
    private func power(_ title: String, on: Bool) -> some View {
        Button {
            beforeAction()
            let operation = home.beginDeviceControl(device.id)
            Task { await home.controlDevice(device.id, command: .object(["action": .string("power"), "on": .bool(on)]), operation: operation) }
        } label: { Label(title, systemImage: "power").frame(maxWidth: .infinity) }
            .buttonStyle(.bordered)
            .accessibilityLabel("\(device.name)\(title)")
            .accessibilityIdentifier("device.\(device.id).power.\(on ? "on" : "off")")
    }
}

extension HomeRecord {
    var sceneActivityDescription: String {
        switch raw["activity"].text {
        case "Active": "匹配"
        case "Inactive": "未匹配"
        case "Pending": "待确认"
        default: "未知"
        }
    }
    func motorStatusDescription(current: Bool) -> String {
        let estimate = raw["motor_estimate"]
        let prefix = current && targetAvailable ? "估计 · " : "历史估计 · "
        let movement: String
        switch estimate["movement"].text {
        case "Opening": movement = "正在打开"
        case "Closing": movement = "正在关闭"
        case "Stopped": movement = "已停止"
        default: return current && targetAvailable ? "位置与运动状态未知" : "状态未同步 · 位置与运动状态未知"
        }
        if let text = estimate["percent_hundredths"].integerText, let value = Int(text) { return prefix + "\(movement) · \(value / 100)%" }
        return prefix + "\(movement) · 位置未知"
    }
}

/// A native light surface: the local target follows the gesture while actual
/// feedback has a stable, separate line. No network code lives in this view.
struct HomeLightControlSurface: View {
    let name: String
    let level: Double
    let levelDescription: String
    let feedback: String
    let available: Bool
    let adjustable: Bool
    let editing: Bool
    var notice: String? = nil
    let change: (Double) -> Void
    let editingChanged: (Bool) -> Void
    let toggle: () -> Void
    let settings: () -> Void
    let close: () -> Void
    @State private var dragStart: Double?

    var body: some View {
        GeometryReader { geometry in
            let width = min(132.0, geometry.size.width * 0.30)
            let height = min(360.0, geometry.size.height * 0.45)
            ZStack {
                LinearGradient(colors: [Color(red: 0.28, green: 0.46, blue: 0.59), Color(red: 0.54, green: 0.54, blue: 0.48), Color(red: 0.54, green: 0.29, blue: 0.08)], startPoint: .top, endPoint: .bottom)
                RadialGradient(colors: [.orange.opacity(level > 0 ? 0.34 : 0.05), .clear], center: .bottomTrailing, startRadius: 0, endRadius: geometry.size.height * 0.75)
                Color.black.opacity(level > 0 ? 0.08 : 0.28)
                VStack(spacing: 0) {
                    HStack {
                        Spacer()
                        Button(action: close) { Image(systemName: "xmark").font(.system(size: 15, weight: .semibold)).frame(width: 36, height: 36).background(.black.opacity(0.15), in: Circle()) }
                            .buttonStyle(.plain).accessibilityLabel("关闭灯光详情")
                    }.padding(.horizontal, 22).padding(.top, 14)
                    Spacer(minLength: 12)
                    VStack(spacing: 8) {
                        Text(name).font(.system(size: 32, weight: .semibold)).lineLimit(2).multilineTextAlignment(.center).minimumScaleFactor(0.7)
                        Text(levelDescription).font(.system(size: 24, weight: .semibold, design: .rounded)).monospacedDigit().foregroundStyle(Color(red: 0.73, green: 0.86, blue: 0.95))
                        Text(feedback).font(.caption).foregroundStyle(.white.opacity(0.8))
                            .lineLimit(1).minimumScaleFactor(0.65).frame(height: 20)
                    }.padding(.horizontal, 24)
                    Spacer(minLength: 24)
                    RoundedRectangle(cornerRadius: width * 0.35, style: .continuous)
                        .fill(.white.opacity(0.20))
                        .overlay(alignment: .bottom) {
                            Rectangle().fill(Color(red: 1, green: 0.84, blue: 0))
                                .frame(height: max(0, min(1, level / 100)) * height)
                        }
                        .overlay(alignment: .bottom) {
                            Image(systemName: "lightbulb.fill").font(.system(size: 36, weight: .regular))
                                .foregroundStyle(level > 0 ? .white : .white.opacity(0.65))
                                .padding(.bottom, 23)
                        }
                        .clipShape(RoundedRectangle(cornerRadius: width * 0.35, style: .continuous))
                        .frame(width: width, height: height)
                        .shadow(color: .black.opacity(0.08), radius: 12, y: 6)
                        .contentShape(RoundedRectangle(cornerRadius: width * 0.35))
                        .gesture(SpatialTapGesture().onEnded { gesture in
                            guard available else { return }
                            if adjustable {
                                guard let target = HomeBrightnessInteraction.tappedLevel(
                                    verticalPosition: Double(gesture.location.y), height: Double(height)) else { return }
                                editingChanged(true); change(target); editingChanged(false)
                            } else { toggle() }
                        }.exclusively(before: DragGesture(minimumDistance: 3)
                            .onChanged { gesture in
                                guard available, adjustable else { return }
                                if dragStart == nil { dragStart = level; editingChanged(true) }
                                change(min(100, max(0, (dragStart ?? level) - Double(gesture.translation.height / height) * 100)))
                            }
                            .onEnded { _ in
                                guard dragStart != nil else { return }
                                dragStart = nil; editingChanged(false)
                            }))
                        .animation(editing ? nil : .easeOut(duration: 0.18), value: level)
                        .opacity(available ? 1 : 0.60)
                        .accessibilityElement(children: .ignore)
                        .accessibilityLabel(name + (adjustable ? "亮度" : "开关"))
                        .accessibilityValue(levelDescription + "，" + feedback)
                        .accessibilityHint(available ? (adjustable ? "按高度轻点选择百分之十档位；上下拖动精细调光，松手后设置" : "轻点切换开关") : "控制暂不可用")
                        .accessibilityAdjustableAction { direction in
                            guard available, adjustable else { return }
                            editingChanged(true)
                            switch direction {
                            case .increment: change(min(100, level + 5))
                            case .decrement: change(max(0, level - 5))
                            @unknown default: break
                            }
                            editingChanged(false)
                        }
                        .accessibilityAction(named: Text("开关")) { if available { toggle() } }
                        .accessibilityIdentifier("home.light.brightness")
                    Spacer(minLength: 24)
                    HStack {
                        if let notice { Text(notice).font(.caption).foregroundStyle(.white).lineLimit(2) }
                        else if !available { Text("控制暂不可用").font(.caption).foregroundStyle(.white.opacity(0.8)) }
                        Spacer()
                        Button(action: settings) {
                            Image(systemName: "gearshape.fill").font(.system(size: 26)).frame(width: 54, height: 54)
                                .background(.ultraThinMaterial.opacity(0.75), in: Circle())
                                .overlay { Circle().strokeBorder(.white.opacity(0.35), lineWidth: 1) }
                        }.buttonStyle(.plain).accessibilityLabel("灯光设置")
                    }.padding(24)
                }
            }.foregroundStyle(.white).ignoresSafeArea(edges: .bottom)
        }
    }
}

struct HomeLightDeviceDetails: View {
    @ObservedObject var home: HomeServerStore
    let device: HomeRecord
    let close: () -> Void
    @State private var brightness = HomeBrightnessDraft()
    @State private var showingSettings = false
    private var adjustable: Bool { device.raw["kind"].text == "Dimmer" }
    private var actualOn: Bool? {
        guard device.feedbackIsConfirmed(current: home.snapshotIsCurrent), case .bool(let on) = device.raw["observed"]["on"] else { return nil }
        return on
    }
    private var pending: Bool { brightness.submission != nil || brightness.unconfirmedTarget != nil || device.raw["state_authority"].text == "pending_target" }
    private var feedback: String {
        let prefix = device.feedbackIsConfirmed(current: home.snapshotIsCurrent) ? "实测 · " : "历史 · "
        let actual = device.raw["observed"]["level"].integerText.map { $0 + "%" } ?? actualOn.map { $0 ? "开启" : "关闭" } ?? "未知"
        return prefix + actual + (pending ? " · 目标待确认" : "")
    }
    var body: some View {
        HomeLightControlSurface(name: device.name, level: adjustable ? brightness.value : actualOn == true ? 100 : 0,
            levelDescription: adjustable ? (!brightness.isEditing && !pending && device.raw["observed"]["level"].integerText == nil ? "状态未知" : "\(Int(brightness.value))%") : actualOn.map { $0 ? "开启" : "关闭" } ?? "状态未知",
            feedback: feedback, available: home.canControlDevice(device.id), adjustable: adjustable, editing: brightness.isEditing, notice: home.error,
            change: { brightness.setValue($0) }, editingChanged: { editing in
                if editing { brightness.beginEditing() } else { submitBrightness() }
            }, toggle: toggle, settings: { showingSettings = true }, close: close)
            .onAppear { syncLevel() }
            .onChange(of: device.raw) { _, _ in syncLevel() }
            .onChange(of: home.snapshotIsCurrent) { _, current in
                if !current { brightness.cancel() }
                syncLevel()
            }
            .task(id: brightness.submission?.id) {
                guard let id = brightness.submission?.id else { return }
                do { try await Task.sleep(for: .seconds(10)) } catch { return }
                brightness.expire(id); syncLevel()
            }
            .sheet(isPresented: $showingSettings) {
                NavigationStack {
                    Form {
                        Section("灯光") {
                            LabeledContent("名称", value: device.name)
                            LabeledContent("房间", value: device.raw["room"]["name"].text ?? "未分配")
                            Text(feedback)
                            if let status = device.targetAvailability.description { Text(status) }
                        }
                        Section("控制") { HomeLightPowerControls(home: home, device: device, beforeAction: { brightness.cancel(); syncLevel() }) }
                        if let error = home.error { Section { Text(error).foregroundStyle(.red) } }
                    }.navigationTitle("灯光设置").toolbar { Button("完成") { showingSettings = false } }
                }.presentationDetents([.medium, .large])
            }
    }
    private var feedbackTimestamp: UInt64? { device.raw["observed_at_ms"].integerText.flatMap(UInt64.init) }
    private func syncLevel() {
        brightness.observe(level: device.raw["observed"]["level"].integerText.flatMap(Int64.init),
            confirmed: device.feedbackIsConfirmed(current: home.snapshotIsCurrent), timestamp: feedbackTimestamp)
    }
    private func submitBrightness() {
        guard home.canControlDevice(device.id) else { brightness.cancel(); syncLevel(); return }
        guard let submission = brightness.endEditing(feedbackTimestamp: feedbackTimestamp) else { return }
        let operation = home.beginDeviceControl(device.id)
        Task {
            let result = await home.controlDevice(device.id, command: .object(["action": .string("brightness"), "level": .integer(submission.level)]), operation: operation)
            brightness.complete(submission.id, accepted: result.accepted); syncLevel()
        }
    }
    private func toggle() {
        guard home.canControlDevice(device.id) else { return }
        let on = actualOn != true
        brightness.cancel(); syncLevel()
        let operation = home.beginDeviceControl(device.id)
        Task { await home.controlDevice(device.id, command: .object(["action": .string("power"), "on": .bool(on)]), operation: operation) }
    }
}

struct HomeDeviceDetails: View {
    @ObservedObject var home: HomeServerStore
    let deviceID: String
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        Group {
            if let device = home.devices.first(where: { $0.id == deviceID }), ["Dimmer", "Switch"].contains(device.raw["kind"].text ?? "") {
                HomeLightDeviceDetails(home: home, device: device, close: { dismiss() })
            } else {
                NavigationStack {
                    ScrollView {
                        if let device = home.devices.first(where: { $0.id == deviceID }) { HomeDeviceCard(home: home, device: device).padding() }
                        else { ContentUnavailableView("设备暂不可用", systemImage: "lightbulb") }
                        if let error = home.error { Text(error).foregroundStyle(.red).padding() }
                        Text(home.message).font(.caption).foregroundStyle(.secondary).padding()
                    }.navigationTitle(home.devices.first(where: { $0.id == deviceID })?.name ?? "设备")
                    .toolbar { Button("完成") { dismiss() } }
                }
            }
        }.frame(idealWidth: 460, idealHeight: 780).presentationDetents([.large])
    }
}

struct HomeSceneDetails: View {
    @ObservedObject var home: HomeServerStore
    let sceneID: String
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            Form {
                if let scene = home.scenes.first(where: { $0.id == sceneID }) {
                    Section("场景状态") {
                        LabeledContent("归属", value: home.scenePlacementTitle(scene))
                        if let status = scene.targetAvailability.description { Text(status).foregroundStyle(.secondary) }
                        Text("\(home.snapshotIsCurrent && scene.targetAvailable ? "负载反馈" : "历史负载反馈")：\(scene.sceneActivityDescription)")
                        if scene.raw["switch_kind"].text == "selection" {
                            Text("\(home.snapshotIsCurrent && scene.targetAvailable ? "选择状态" : "历史选择")：\(scene.raw["selected"] == .bool(true) ? "开启" : scene.raw["selected"] == .bool(false) ? "关闭" : "未知")")
                        }
                        Text("场景选择与负载反馈分别显示，受理不表示设备已到位。")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    Section("控制") {
                        if scene.raw["switch_kind"].text == "selection" {
                            Button("开启场景") { Task { await home.controlScene(sceneID, on: true) } }
                            Button("关闭场景") { Task { await home.controlScene(sceneID, on: false) } }
                        } else { Button(scene.sceneCardAction.title) { Task { await home.controlScene(sceneID, on: nil) } } }
                    }.disabled(!home.canControlScene(sceneID))
                    Section("灯光状态") {
                        if scene.sceneLightMembershipAvailable {
                            let lights = scene.sceneLightStates(devices: home.devices, current: home.snapshotIsCurrent)
                            ForEach(lights) { light in
                                HStack(spacing: 12) {
                                    Image(systemName: light.isOn == true ? "lightbulb.fill" : "lightbulb")
                                        .foregroundStyle(light.isOn == true ? Color.orange : Color.secondary)
                                    VStack(alignment: .leading, spacing: 4) {
                                        Text(light.name)
                                        Text(light.status).font(.caption).foregroundStyle(.secondary)
                                            .lineLimit(1).minimumScaleFactor(0.75)
                                    }
                                }
                            }
                            if lights.isEmpty {
                                Text(scene.raw["lights_complete"] == .bool(true) ? "此场景不控制灯光" : "没有可查看的灯光")
                                    .foregroundStyle(.secondary)
                            }
                            if scene.raw["lights_complete"] == .bool(false) {
                                Text("部分灯光没有查看权限").font(.caption).foregroundStyle(.secondary)
                            }
                            Text("包含此场景会开启或关闭的灯光。状态随设备反馈更新。")
                                .font(.caption).foregroundStyle(.secondary)
                        } else {
                            Text("服务端尚未提供此场景的灯光清单").foregroundStyle(.secondary)
                        }
                    }
                } else { Text("场景暂不可用") }
                if let error = home.error { Text(error).foregroundStyle(.red) }
                Text(home.message).font(.caption).foregroundStyle(.secondary)
            }
            .navigationTitle(home.scenes.first(where: { $0.id == sceneID })?.name ?? "场景")
            .toolbar { Button("完成") { dismiss() } }
        }.frame(idealWidth: 460, idealHeight: 500)
    }
}

struct HomeKeypadDetails: View {
    @ObservedObject var home: HomeServerStore
    let keypadID: String
    @Environment(\.dismiss) private var dismiss
    @State private var backlightMode = "dim_after_ten_seconds"
    private var record: HomeRecord? { home.devices.first { $0.id == keypadID } }
    private var keypad: HomeKeypad? { record.flatMap { try? HomeKeypad(record: $0, current: home.snapshotIsCurrent) } }
    var body: some View {
        NavigationStack {
            Form {
                if let keypad {
                    Section("面板") {
                        LabeledContent("房间", value: keypad.roomName ?? "未分配")
                        if let record, let module = home.physicalModule(for: record) {
                            LabeledContent("总线地址（HEX）", value: module.address)
                            LabeledContent("网关", value: module.gateway)
                        }
                        if let status = record?.targetAvailability.description { Text(status).foregroundStyle(.secondary) }
                        Text("按键动作使用已审核映射；LED 设置是独立操作。")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    ForEach(keypad.keys) { key in
                        Section("\(key.number) · \(key.name)") {
                            let raw = record?.raw["keys"].values.first(where: { $0["number"].integerText == String(key.number) }) ?? .null
                            Text(key.led.actualDescription)
                            if let history = key.led.historyDescription { Text(history).foregroundStyle(.secondary) }
                            if raw.isObject {
                                if raw["key_event"]["pressed"] != .null {
                                    let current = home.snapshotIsCurrent && record?.targetAvailable == true && raw["key_event"]["quality"] == .string("Confirmed")
                                    let position = raw["key_event"]["pressed"] == .bool(true) ? "按下" : "释放"
                                    Text("\(current ? "按键事件" : "历史按键事件")：\(position) · \(raw["key_event"]["quality"].text ?? "未知")")
                                        .font(.caption).foregroundStyle(.secondary)
                                }
                                if let status = raw["led"]["last_request"]["status"].text {
                                    Text("最近请求：\(status)").font(.caption).foregroundStyle(.secondary)
                                }
                            }
                            if let progress = home.keypadActionPresentation(keypadID, key: key.number) {
                                Text(progress.description).font(.caption).foregroundStyle(cueColor(key.number))
                            }
                            if key.canActivate || raw["activation"].isObject {
                                Button(key.cardActionDescription ?? "执行按键动作") {
                                    let operation = home.beginUnifiedKeypadClick(keypadID, key: key.number)
                                    Task { await home.activateUnifiedKeypad(keypadID, key: key.number, operation: operation) }
                                }
                                    .buttonStyle(.bordered).tint(cueColor(key.number))
                                    .disabled(!home.canActivateUnifiedKeypad(keypadID, key: key.number))
                            } else { Text("未配置已授权的按键动作").font(.caption).foregroundStyle(.secondary) }
                            HStack {
                                Button("设置红灯") { Task { await home.writeKeypadLED(keypadID, key: key.number, color: .red) } }
                                Button("设置绿灯") { Task { await home.writeKeypadLED(keypadID, key: key.number, color: .green) } }
                                if keypad.ledColors.contains(.off) {
                                    Button("灭灯") { Task { await home.writeKeypadLED(keypadID, key: key.number, color: .off) } }
                                }
                            }.disabled(!home.canWriteKeypadLED(keypadID, key: key.number))
                        }
                    }
                    if let notice = home.unifiedKeypadSupportMessage { Text(notice).font(.caption).foregroundStyle(.secondary) }
                    Section("工作模式") {
                        LabeledContent("配置工作模式", value: mode(keypad.settings["work_mode"]["configured"].text))
                        settingStatus(keypad, field: "work_mode")
                        Group {
                            Button("查询工作模式") { setting(.object(["action": .string("read_work_mode")])) }
                            Button("写入已发布的工作模式") { setting(.object(["action": .string("work_mode"), "mode": keypad.settings["work_mode"]["configured"]])) }
                        }.disabled(!canSet(field: "work_mode"))
                        Text("工作模式须与已发布配置一致。更改模式前先审核模块配置和 LED 联动；打开详情不发送硬件命令。")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    Section("背光") {
                        settingStatus(keypad, field: "backlight")
                        Group {
                            Picker("目标背光", selection: $backlightMode) {
                                Text("10 秒后变暗").tag("dim_after_ten_seconds")
                                Text("常亮").tag("bright"); Text("常暗").tag("dim")
                            }
                            Button("设置背光") { setting(.object(["action": .string("backlight"), "mode": .string(backlightMode)])) }
                            Button("查询背光") { setting(.object(["action": .string("read_backlight")])) }
                        }.disabled(!canSet(field: "backlight"))
                    }
                    Section("按键声音") {
                        settingStatus(keypad, field: "sound")
                        Group {
                            Button("开启声音") { setting(.object(["action": .string("sound"), "enabled": .bool(true)])) }
                            Button("关闭声音") { setting(.object(["action": .string("sound"), "enabled": .bool(false)])) }
                            Button("查询声音") { setting(.object(["action": .string("read_sound")])) }
                        }.disabled(!canSet(field: "sound"))
                    }
                    Section {
                        Text("整板参数仅向具备完整六键权限的管理员开放。参数读回格式尚未核实，查询不表示已确认实际设置；最后写出也不是实测。")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                } else { Text("面板暂不可用") }
                if let error = home.error { Text(error).foregroundStyle(.red) }
                Text(home.message).font(.caption).foregroundStyle(.secondary)
            }
            .navigationTitle(keypad?.name ?? "Keypad")
            .toolbar { Button("完成") { dismiss() } }
        }.frame(idealWidth: 520, idealHeight: 650)
        .onAppear { home.recordUnifiedKeypadGates(keypadID) }
        .onChange(of: home.unifiedKeypadGateStates(keypadID)) { _, _ in home.recordUnifiedKeypadGates(keypadID) }
    }
    private func cueColor(_ number: Int) -> Color {
        let cue = home.keypadActionPresentation(keypadID, key: number)
        if cue?.requestedColor == .red { return .red }
        if cue?.requestedColor == .green { return .green }
        if cue?.phase == .uncertain || cue?.phase == .mappingGap { return .orange }
        return .blue
    }
    private func mode(_ value: String?) -> String {
        switch value { case "manual": "手动灯控"; case "self_reset": "自复位"; default: "未知" }
    }
    private func canSet(field: String) -> Bool {
        home.canWriteKeypadSetting(keypadID, field: field)
    }
    private func setting(_ command: HomeJSON) { Task { await home.writeKeypadSetting(keypadID, command: command) } }
    @ViewBuilder private func settingStatus(_ keypad: HomeKeypad, field: String) -> some View {
        Text("实测：未知").foregroundStyle(.secondary)
        if keypad.settings[field]["last_written"]["status"].text == "written" {
            LabeledContent("最后写出", value: settingValue(keypad.settings[field]["last_written"]["value"], field: field))
        }
    }
    private func settingValue(_ value: HomeJSON, field: String) -> String {
        if field == "work_mode" { return mode(value.text) }
        if field == "sound" { return value == .bool(true) ? "开启" : value == .bool(false) ? "关闭" : "未知" }
        switch value.text { case "dim_after_ten_seconds": return "10 秒后变暗"; case "bright": return "常亮"; case "dim": return "常暗"; default: return "未知" }
    }
}
