#if os(iOS)
import SwiftUI
@preconcurrency import HomeKit
import HomeSenseCore

struct HomeKitAccessoryDetails: View {
    @ObservedObject var store: HomeKitDailyStore
    let item: HomeKitDailyStore.Service
    @Environment(\.dismiss) private var dismiss
    @State private var showInformation = false
    @State private var showRename = false
    var body: some View {
        Group {
            switch item.kind {
            case .light: adaptive { HomeKitLightDetails(store: store, item: item, info: { showInformation = true }, close: { dismiss() }) }
            case .curtain: adaptive { HomeKitCurtainDetails(store: store, item: item, info: { showInformation = true }, close: { dismiss() }) }
            default: otherDetails
            }
        }
        .sheet(isPresented: $showInformation) { information }
    }
    private func adaptive<Content: View>(@ViewBuilder content: @escaping () -> Content) -> some View {
        GeometryReader { geometry in
            ScrollView {
                content().frame(height: max(620, geometry.size.height))
            }.scrollDisabled(geometry.size.height >= 620)
        }
    }
    private var otherDetails: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 24) {
                    Image(systemName: item.kind.symbol).font(.system(size: 58, weight: .light)).foregroundStyle(.orange)
                        .frame(width: 120, height: 120).background(.regularMaterial, in: Circle())
                    VStack(spacing: 6) {
                        Text(item.room).font(.subheadline).foregroundStyle(.secondary)
                        Text(item.name).font(.title.bold()).multilineTextAlignment(.center)
                        Text(store.presentation(item).status).font(.system(size: 36, weight: .medium, design: .rounded)).monospacedDigit()
                    }
                    if let power = item.power {
                        HStack(spacing: 16) {
                            action("开启", symbol: "power", enabled: store.canWrite(power, in: item)) { await store.write(true, to: power, in: item) }
                            action("关闭", symbol: "power", enabled: store.canWrite(power, in: item)) { await store.write(false, to: power, in: item) }
                        }
                    }
                    VStack(spacing: 14) {
                        ForEach(item.characteristics.filter { HomeKitDailyStore.visibleTypes.contains($0.characteristicType) }, id: \.uniqueIdentifier) { characteristic in
                            HomeKitCharacteristicRow(store: store, item: item, characteristic: characteristic)
                        }
                    }.padding(20).background(.regularMaterial, in: RoundedRectangle(cornerRadius: 22))
                    if let error = store.error { Text(error).font(.callout).foregroundStyle(.orange) }
                }.padding(28).frame(maxWidth: 560).frame(maxWidth: .infinity)
            }.background(HomeKitCanvas())
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) { Button("完成") { dismiss() } }
                    ToolbarItem(placement: .primaryAction) { Button("配件信息", systemImage: "gearshape") { showInformation = true } }
                }
        }
    }
    private func action(_ title: String, symbol: String, enabled: Bool, run: @escaping @MainActor () async -> Void) -> some View {
        Button { Task { await run() } } label: { Label(title, systemImage: symbol).frame(maxWidth: .infinity, minHeight: 44) }
            .buttonStyle(.borderedProminent).tint(.orange).disabled(!enabled)
    }
    private var information: some View {
        NavigationStack {
            Form {
                Section("配件") {
                    Button { showRename = true } label: {
                        HStack {
                            Text("名称").foregroundStyle(.primary)
                            Spacer()
                            Text(item.name).foregroundStyle(.secondary)
                            Image(systemName: "pencil").font(.caption)
                        }
                    }.disabled(!store.canRename(item)).accessibilityIdentifier("homekit.rename.open")
                    LabeledContent("房间", value: item.room)
                    LabeledContent("设备", value: item.accessory.name)
                    LabeledContent("服务名称", value: item.service.name)
                    if let manufacturer = item.accessory.manufacturer { LabeledContent("制造商", value: manufacturer) }
                    if let model = item.accessory.model { LabeledContent("型号", value: model) }
                    LabeledContent("状态", value: store.presentation(item).status)
                    if !store.canRename(item) {
                        Text("改名需要 Apple 家庭的编辑权限；已有名称请求未完成时也不能重复保存。")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                Section {
                    Toggle("加入常用配件", isOn: Binding(get: { store.favoriteIDs.contains(item.id) }, set: { desired in
                        if desired != store.favoriteIDs.contains(item.id) { store.toggleFavorite(item.id) }
                    }))
                    Text("常用配件保存在本机，不改变 Apple 家庭中的收藏设置。").font(.caption).foregroundStyle(.secondary)
                }
                Section("状态与访问") {
                    Text("状态和控制权限由 Apple 家庭提供。配件未响应时保留未知状态，操作不会自动重发。")
                    if item.kind == .curtain { Text("当前位置为配件报告的位置；部分窗帘的位置可能由配件估算。") }
                    Text("HomeSense 的设备绑定、按键配方和配置发布，请使用“配置管理”。")
                }
            }.navigationTitle("配件信息")
                .toolbar { ToolbarItem(placement: .confirmationAction) { Button("完成") { showInformation = false } } }
                .sheet(isPresented: $showRename) { HomeKitRenameView(store: store, item: item) }
        }
    }
}

private struct HomeKitRenameView: View {
    @ObservedObject var store: HomeKitDailyStore
    let item: HomeKitDailyStore.Service
    @StateObject private var editor: HomeAccessoryNameEditor
    @Environment(\.dismiss) private var dismiss
    init(store: HomeKitDailyStore, item: HomeKitDailyStore.Service) {
        self.store = store; self.item = item
        _editor = StateObject(wrappedValue: HomeAccessoryNameEditor(name: item.name))
    }
    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("配件名称", text: $editor.name).disabled(editor.saving)
                        .accessibilityIdentifier("homekit.rename.name")
                } header: {
                    Text("名称")
                } footer: {
                    Text("保存到 Apple 家庭，其他设备由系统同步。此操作不会改变 HomeSense 服务端配置或操作灯光、窗帘。")
                }
                if let error = editor.error { Section { Text(error).foregroundStyle(.orange).textSelection(.enabled) } }
                if editor.saving { ProgressView("正在保存到 Apple 家庭…") }
            }.navigationTitle("修改名称")
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) { Button("取消") { dismiss() }.disabled(editor.saving) }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("保存") {
                            Task { if await editor.save(using: { try await store.rename($0, in: item) }) { dismiss() } }
                        }.disabled(!editor.canSave || !store.canRename(item)).accessibilityIdentifier("homekit.rename.save")
                    }
                }
                .interactiveDismissDisabled(editor.saving)
        }
    }
}

private struct HomeKitLightDetails: View {
    @ObservedObject var store: HomeKitDailyStore
    let item: HomeKitDailyStore.Service
    let info: () -> Void
    let close: () -> Void
    @State private var draft = HomeBrightnessDraft()
    @State private var choosePower = false
    private var presentation: HomeAccessoryPresentation { store.presentation(item) }
    private var adjustable: Bool { item.brightness?.properties.contains(HMCharacteristicPropertyWritable) == true }
    private var available: Bool {
        if adjustable, let brightness = item.brightness { return store.canWrite(brightness, in: item) }
        if let power = item.power { return store.canWrite(power, in: item) }
        return false
    }
    private var observedLevel: Int64? {
        guard let on = presentation.on else { return nil }
        if !on { return 0 }
        return Int64((store.number(item.brightness) ?? 100).rounded())
    }
    private var observationRevision: UInt64? {
        [item.power, item.brightness].compactMap { $0.flatMap { store.observations.revisions[$0.uniqueIdentifier] } }.max()
    }
    private var pending: Bool { draft.submission != nil || draft.unconfirmedTarget != nil }
    private var levelDescription: String {
        if adjustable && (draft.isEditing || pending) { return "\(Int(draft.value))%" }
        return presentation.status
    }
    var body: some View {
        HomeLightControlSurface(name: item.name, level: adjustable ? draft.value : presentation.on == true ? 100 : 0,
            levelDescription: levelDescription, feedback: "当前 · \(presentation.status)" + (pending ? " · 目标待确认" : ""),
            available: available, adjustable: adjustable, editing: draft.isEditing,
            notice: store.error, change: { draft.setValue($0) }, editingChanged: { editing in
                if editing { draft.beginEditing() } else { submit() }
            }, toggle: toggle, settings: info, close: close)
            .onAppear(perform: synchronize)
            .onChange(of: store.observations.revisions) { _, _ in synchronize() }
            .task(id: draft.submission?.id) {
                guard let id = draft.submission?.id else { return }
                do { try await Task.sleep(for: .seconds(10)) } catch { return }
                draft.expire(id)
            }
            .onDisappear { draft.cancel() }
            .confirmationDialog("设置灯光开关", isPresented: $choosePower) {
                Button("开启") { setPower(true) }
                Button("关闭") { setPower(false) }
                Button("取消", role: .cancel) {}
            }
    }
    private func synchronize() { draft.observe(level: observedLevel, confirmed: observedLevel != nil, timestamp: observationRevision) }
    private func submit() {
        guard let brightness = item.brightness,
            let submission = draft.endEditing(feedbackTimestamp: observationRevision) else { return }
        Task {
            let accepted = await store.write(NSNumber(value: submission.level), to: brightness, in: item)
            draft.complete(submission.id, accepted: accepted)
            synchronize()
        }
    }
    private func toggle() {
        guard item.power != nil else { return }
        if let on = presentation.on { setPower(!on) } else { choosePower = true }
    }
    private func setPower(_ on: Bool) {
        guard let power = item.power else { return }
        draft.cancel()
        Task { await store.write(on, to: power, in: item); synchronize() }
    }
}

private struct HomeKitCurtainDetails: View {
    @ObservedObject var store: HomeKitDailyStore
    let item: HomeKitDailyStore.Service
    let info: () -> Void
    let close: () -> Void
    @State private var draft = HomeBrightnessDraft()
    private var actual: Double? { store.number(item.currentPosition).flatMap { (0...100).contains($0) ? $0 : nil } }
    private var revision: UInt64? { item.currentPosition.flatMap { store.observations.revisions[$0.uniqueIdentifier] } }
    private var pending: Bool { draft.submission != nil || draft.unconfirmedTarget != nil }
    private var canSet: Bool { item.targetPosition.map { store.canWrite($0, in: item) } ?? false }
    private var canStop: Bool { item.holdPosition.map { store.canWrite($0, in: item) } ?? false }
    var body: some View {
        HomeCurtainControlSurface(name: item.name, room: item.room, level: draft.value,
            levelDescription: draft.isEditing || pending ? "\(Int(draft.value))%" : store.presentation(item).status,
            feedback: "当前位置 · \(store.presentation(item).status)" + (pending ? " · 目标待确认" : ""),
            available: canSet, stopAvailable: canStop, editing: draft.isEditing, notice: store.error,
            change: { draft.setValue($0) }, editingChanged: { editing in
                if editing { draft.beginEditing() } else { submit() }
            }, open: { setPosition(100) }, stop: stop, shut: { setPosition(0) }, information: info, close: close)
            .onAppear(perform: synchronize)
            .onChange(of: store.observations.revisions) { _, _ in synchronize() }
            .task(id: draft.submission?.id) {
                guard let id = draft.submission?.id else { return }
                do { try await Task.sleep(for: .seconds(15)) } catch { return }
                draft.expire(id)
            }
            .onDisappear { draft.cancel() }
    }
    private func synchronize() { draft.observe(level: actual.map { Int64($0.rounded()) }, confirmed: actual != nil, timestamp: revision) }
    private func setPosition(_ value: Double) { draft.beginEditing(); draft.setValue(value); submit() }
    private func submit() {
        guard let target = item.targetPosition, let submission = draft.endEditing(feedbackTimestamp: revision) else { return }
        Task {
            let accepted = await store.write(NSNumber(value: submission.level), to: target, in: item)
            draft.complete(submission.id, accepted: accepted); synchronize()
        }
    }
    private func stop() {
        guard let hold = item.holdPosition else { return }
        draft.cancel(); synchronize()
        Task { await store.write(true, to: hold, in: item) }
    }
}

/// Pure gesture surface, with no network actions except the explicit callbacks.
struct HomeCurtainControlSurface: View {
    let name: String
    let room: String
    let level: Double
    let levelDescription: String
    let feedback: String
    let available: Bool
    let stopAvailable: Bool
    let editing: Bool
    let notice: String?
    let change: (Double) -> Void
    let editingChanged: (Bool) -> Void
    let open: () -> Void
    let stop: () -> Void
    let shut: () -> Void
    let information: () -> Void
    let close: () -> Void
    @State private var dragStart: Double?
    var body: some View {
        GeometryReader { geometry in
            let height = min(320.0, max(130.0, geometry.size.height - 490))
            let width = min(230.0, geometry.size.width * 0.56)
            ZStack {
                LinearGradient(colors: [Color(red: 0.17, green: 0.27, blue: 0.35), Color(red: 0.29, green: 0.42, blue: 0.48), Color(red: 0.49, green: 0.51, blue: 0.48)], startPoint: .top, endPoint: .bottom)
                VStack(spacing: 0) {
                    HStack { Spacer(); closeButton }.padding(.horizontal, 22).padding(.top, 14)
                    Spacer(minLength: 12)
                    VStack(spacing: 7) {
                        Text(room).font(.subheadline).foregroundStyle(.white.opacity(0.7))
                        Text(name).font(.system(size: 30, weight: .semibold)).multilineTextAlignment(.center)
                        Text(levelDescription).font(.system(size: 26, weight: .medium, design: .rounded)).monospacedDigit()
                        Text(feedback).font(.caption).foregroundStyle(.white.opacity(0.75)).lineLimit(2)
                    }.padding(.horizontal, 24)
                    Spacer(minLength: 20)
                    curtain(width: width, height: height)
                    Spacer(minLength: 24)
                    HStack(spacing: 14) {
                        control("打开", symbol: "arrow.up", enabled: available, action: open)
                        control("停止", symbol: "stop.fill", enabled: stopAvailable, action: stop)
                        control("关闭", symbol: "arrow.down", enabled: available, action: shut)
                    }.padding(.horizontal, 28)
                    HStack {
                        if let notice { Text(notice).font(.caption).lineLimit(2) }
                        else if !available { Text("位置控制暂不可用").font(.caption).foregroundStyle(.white.opacity(0.75)) }
                        Spacer()
                        Button(action: information) { Image(systemName: "gearshape.fill").font(.title2).frame(width: 50, height: 50).background(.ultraThinMaterial, in: Circle()) }
                            .buttonStyle(.plain).accessibilityLabel("窗帘信息")
                    }.padding(24)
                }
            }.foregroundStyle(.white).ignoresSafeArea(edges: .bottom)
        }
    }
    private var closeButton: some View {
        Button(action: close) { Image(systemName: "xmark").font(.system(size: 15, weight: .semibold)).frame(width: 36, height: 36).background(.black.opacity(0.15), in: Circle()) }
            .buttonStyle(.plain).accessibilityLabel("关闭窗帘详情")
    }
    private func control(_ title: String, symbol: String, enabled: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) { VStack(spacing: 7) { Image(systemName: symbol).font(.title3); Text(title).font(.caption.weight(.semibold)) }.frame(maxWidth: .infinity, minHeight: 64).background(.white.opacity(0.14), in: RoundedRectangle(cornerRadius: 18)) }
            .buttonStyle(.plain).disabled(!enabled).opacity(enabled ? 1 : 0.4)
    }
    private func curtain(width: Double, height: Double) -> some View {
        let openFraction = min(1, max(0, level / 100))
        let panelWidth = max(12, width * (1 - openFraction) / 2)
        return ZStack {
            RoundedRectangle(cornerRadius: 30).fill(.white.opacity(0.12))
            LinearGradient(colors: [.white.opacity(0.04), .white.opacity(0.24)], startPoint: .top, endPoint: .bottom)
            HStack(spacing: 0) { panel(width: panelWidth); Spacer(minLength: 0); panel(width: panelWidth) }
            VStack { Capsule().fill(.white.opacity(0.6)).frame(height: 5); Spacer(); Image(systemName: "curtains.closed").font(.title).foregroundStyle(.white.opacity(0.75)).padding(.bottom, 20) }.padding(.horizontal, 12).padding(.top, 12)
        }
        .frame(width: width, height: height).clipShape(RoundedRectangle(cornerRadius: 30))
        .overlay { RoundedRectangle(cornerRadius: 30).strokeBorder(.white.opacity(0.16), lineWidth: 1) }
        .contentShape(RoundedRectangle(cornerRadius: 30))
        .gesture(SpatialTapGesture().onEnded { gesture in
            guard available, let target = HomeBrightnessInteraction.tappedLevel(verticalPosition: Double(gesture.location.y), height: height) else { return }
            editingChanged(true); change(target); editingChanged(false)
        }.exclusively(before: DragGesture(minimumDistance: 3).onChanged { gesture in
            guard available else { return }
            if dragStart == nil { dragStart = level; editingChanged(true) }
            change(min(100, max(0, (dragStart ?? level) - Double(gesture.translation.height) / height * 100)))
        }.onEnded { _ in
            guard dragStart != nil else { return }
            dragStart = nil; editingChanged(false)
        }))
        .animation(editing ? nil : .easeOut(duration: 0.2), value: level)
        .opacity(available ? 1 : 0.55)
        .accessibilityElement(children: .ignore).accessibilityLabel(name + "打开比例").accessibilityValue(levelDescription + "，" + feedback)
        .accessibilityAdjustableAction { direction in
            guard available else { return }
            editingChanged(true)
            switch direction {
            case .increment: change(min(100, level + 5))
            case .decrement: change(max(0, level - 5))
            @unknown default: break
            }
            editingChanged(false)
        }.accessibilityIdentifier("homekit.curtain.position")
    }
    private func panel(width: Double) -> some View {
        HStack(spacing: 0) {
            ForEach(0..<6, id: \.self) { _ in LinearGradient(colors: [.white.opacity(0.75), .white.opacity(0.35)], startPoint: .leading, endPoint: .trailing) }
        }.frame(width: width)
    }
}

private struct HomeKitCharacteristicRow: View {
    @ObservedObject var store: HomeKitDailyStore
    let item: HomeKitDailyStore.Service
    let characteristic: HMCharacteristic
    @State private var target: Double?
    private var type: String { characteristic.characteristicType }
    private var name: String {
        switch type {
        case HMCharacteristicTypePowerState: "开关"
        case HMCharacteristicTypeCurrentTemperature: "温度"
        case HMCharacteristicTypeTargetTemperature: "目标温度"
        case HMCharacteristicTypeCurrentRelativeHumidity: "湿度"
        case HMCharacteristicTypeMotionDetected: "移动"
        case HMCharacteristicTypeOccupancyDetected: "有人"
        case HMCharacteristicTypeCurrentLightLevel: "光照"
        case HMCharacteristicTypeContactState: "门窗状态"
        case HMCharacteristicTypeBatteryLevel: "电量"
        case HMCharacteristicTypeStatusLowBattery: "低电量"
        default: characteristic.localizedDescription
        }
    }
    private var formatted: String {
        guard let number = store.number(characteristic) else { return "未知" }
        if type == HMCharacteristicTypePowerState { return number == 0 ? "关闭" : "开启" }
        if [HMCharacteristicTypeMotionDetected, HMCharacteristicTypeOccupancyDetected].contains(type) { return number == 0 ? "无" : "有" }
        if type == HMCharacteristicTypeStatusLowBattery { return number == 0 ? "正常" : "电量低" }
        if type == HMCharacteristicTypeContactState { return number == 0 ? "关闭" : "打开" }
        let unit = [HMCharacteristicTypeCurrentRelativeHumidity, HMCharacteristicTypeBatteryLevel].contains(type) ? "%" : [HMCharacteristicTypeCurrentTemperature, HMCharacteristicTypeTargetTemperature].contains(type) ? " °C" : type == HMCharacteristicTypeCurrentLightLevel ? " lx" : ""
        return number.formatted(.number.precision(.fractionLength(0...1))) + unit
    }
    var body: some View {
        VStack(spacing: 8) {
            LabeledContent(name, value: formatted)
            if type == HMCharacteristicTypeTargetTemperature, characteristic.properties.contains(HMCharacteristicPropertyWritable) {
                let minimum = characteristic.metadata?.minimumValue?.doubleValue ?? 10
                let maximum = max(minimum + 1, characteristic.metadata?.maximumValue?.doubleValue ?? 35)
                Slider(value: Binding(get: { min(maximum, max(minimum, target ?? store.number(characteristic) ?? minimum)) }, set: { target = $0 }), in: minimum...maximum, step: max(0.1, characteristic.metadata?.stepValue?.doubleValue ?? 0.5)) { editing in
                    if !editing, let target { Task { await store.write(NSNumber(value: target), to: characteristic, in: item) } }
                }.disabled(!store.canWrite(characteristic, in: item))
            }
            if let failure = store.failure(characteristic) { Text(failure).font(.caption).foregroundStyle(.orange).frame(maxWidth: .infinity, alignment: .leading) }
        }
    }
}
#endif
