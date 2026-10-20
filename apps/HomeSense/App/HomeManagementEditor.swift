import SwiftUI
import HomeSenseCore

struct HomeManagementEditor: View {
    @ObservedObject var home: HomeServerStore
    let request: HomeEditRequest
    @Environment(\.dismiss) private var dismiss
    @State private var id: String
    @State private var name: String
    @State private var floor: String
    @State private var room: String
    @State private var sceneScope: String
    @State private var enabled: Bool
    @State private var module: String
    @State private var gateway: String
    @State private var address: String
    @State private var channel: String
    @State private var review: String
    @State private var definition: String
    @State private var selectedDevice = ""
    @State private var actionKind = "switch_on"
    @State private var brightness = 50.0
    @State private var failure: String?

    init(home: HomeServerStore, request: HomeEditRequest) {
        self.home = home; self.request = request
        let raw = request.record?.raw ?? .object([:])
        let newID = "mobile-\(request.kind)-\(UUID().uuidString.lowercased().prefix(8))"
        _id = State(initialValue: raw["id"].text ?? newID)
        _name = State(initialValue: request.floor ?? raw["name"].text ?? "")
        _floor = State(initialValue: raw["floor"].text ?? "")
        _room = State(initialValue: raw["room"].text ?? "")
        _sceneScope = State(initialValue: raw["room"].text != nil ? "room" : (raw["scope"].text == "household" ? "household" : "unassigned"))
        _enabled = State(initialValue: raw["enabled"].flag)
        _module = State(initialValue: raw["module_id"].text ?? "")
        _gateway = State(initialValue: raw["gateway"].text ?? "")
        _address = State(initialValue: raw["address"].text ?? "")
        _channel = State(initialValue: raw["channel"].integerText ?? "1")
        _review = State(initialValue: raw["review_evidence"].text ?? "")
        let scene: HomeJSON = request.record == nil
            ? .object(["id": .string(newID), "enabled": .bool(false), "review_evidence": .string(""), "activate": .array([])])
            : raw.removing(["name", "collection", "room", "scope"])
        _definition = State(initialValue: scene.formatted)
    }
    private var collection: String { request.record?.raw["collection"].text ?? "immediate_scenes" }
    private var title: String {
        let noun = ["floor": "楼层", "room": "房间", "device": "设备", "scene": "场景"][request.kind] ?? "配置"
        return (request.record != nil || request.floor != nil ? "编辑" : "新增") + noun
    }
    var body: some View {
        NavigationStack {
            Form {
                if let failure { Section { Text(failure).foregroundStyle(.red) } }
                Section("基本信息") {
                    if request.kind != "floor" {
                        TextField("稳定 ID", text: $id).disabled(request.record != nil)
                            .font(.system(.body, design: .monospaced))
                    }
                    TextField("名称", text: $name)
                }
                switch request.kind {
                case "floor":
                    Section { Text("楼层可独立保存，暂时没有房间也可以添加。").font(.caption) }
                case "room":
                    Section("所属楼层") {
                        TextField("楼层名称，留空为未分配", text: $floor)
                        if !home.floors.isEmpty {
                            Picker("选择已有楼层", selection: $floor) {
                                Text("未分配").tag("")
                                if !floor.isEmpty && !home.floors.contains(floor) { Text(floor).tag(floor) }
                                ForEach(home.floors, id: \.self) { Text($0).tag($0) }
                            }
                        }
                    }
                case "device": deviceFields
                case "scene": sceneFields
                default: EmptyView()
                }
                Section {
                    Text("保存到当前编辑稿；在“草稿与发布”验证、预览并发布后，修改才在服务端生效。")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            .disabled(home.busy)
            .navigationTitle(title)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("取消") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) { Button("加入编辑稿") { save() }.disabled(home.busy) }
            }
        }
        #if os(macOS)
        .frame(minWidth: 560, minHeight: 620)
        #endif
    }
    private var deviceFields: some View {
        Group {
            Section("归属与启用") {
                Picker("房间", selection: $room) {
                    Text("未分配").tag("")
                    ForEach(home.editableRooms) { Text($0.name).tag($0.id) }
                }
                Toggle("启用设备", isOn: $enabled)
            }
            Section("物理绑定") {
                if home.deviceDocument?["schema_version"].integerText == "2" {
                    Picker("模块", selection: $module) {
                        Text("请选择模块").tag("")
                        ForEach(registeredModules) { Text("\($0.id) · \($0.raw["address"].text ?? "")").tag($0.id) }
                    }
                } else {
                    TextField("网关 ID", text: $gateway)
                    TextField("模块地址 · HEX", text: $address)
                }
                TextField("通道", text: $channel)
                    #if os(iOS)
                    .keyboardType(.numberPad)
                    #endif
                Text("只能绑定服务端已有模块。更换物理绑定会清除旧电机校准和行程估计配置，发布前需要重新审核。")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }
    private var registeredModules: [HomeRecord] {
        (home.deviceDocument?["gateways"].values ?? []).flatMap { $0["modules"].values.map(HomeRecord.init) }.filter { !$0.id.isEmpty }
    }
    private var sceneFields: some View {
        Group {
            Section("场景归属") {
                Picker("归属类型", selection: $sceneScope) {
                    Text("对应房间").tag("room")
                    Text("全屋场景").tag("household").disabled(!home.scenePlacementSupported)
                    Text("待分配场景").tag("unassigned")
                }
                if sceneScope == "room" {
                    Picker("房间", selection: $room) {
                        Text("请选择房间").tag("")
                        if !room.isEmpty && !home.editableRooms.contains(where: { $0.id == room }) {
                            Text("原房间不可用（\(room)）").tag(room)
                        }
                        ForEach(home.editableRooms) { Text($0.name).tag($0.id) }
                    }
                }
                Text("明确的房间场景放入对应房间；全屋场景与尚未确认的场景分别展示。")
                    .font(.caption).foregroundStyle(.secondary)
                if !home.scenePlacementSupported {
                    Text("当前服务暂不支持设置全屋归属。").font(.caption).foregroundStyle(.secondary)
                }
            }
            Section("场景设置") {
                Toggle("启用场景", isOn: $enabled)
                TextField("审核依据", text: $review, axis: .vertical)
                Text("新增场景默认未启用。动作、引用和审核资料由服务端统一校验。")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if collection == "immediate_scenes" {
                Section("追加设备动作") {
                    Picker("设备", selection: $selectedDevice) {
                        Text("请选择设备").tag("")
                        ForEach(home.editableActionEndpoints) { Text($0.name).tag($0.id) }
                    }
                    Picker("动作", selection: $actionKind) {
                        Text("开灯 / 开关开启").tag("switch_on")
                        Text("关灯 / 开关关闭").tag("switch_off")
                        Text("设置调光亮度（渐变）").tag("level")
                        Text("面板红灯").tag("led_on")
                        Text("面板绿灯").tag("led_off")
                        Text("窗帘打开").tag("motor_open")
                        Text("窗帘关闭").tag("motor_close")
                        Text("窗帘停止").tag("motor_stop")
                    }
                    if actionKind == "level" { Slider(value: $brightness, in: 0...100, step: 1); Text("\(Int(brightness))%") }
                    Button("追加到开启动作") { appendAction() }.disabled(selectedDevice.isEmpty)
                }
            }
            Section(collection == "timed_scenes" ? "定时场景定义" : "场景动作定义") {
                TextEditor(text: $definition).font(.system(.caption, design: .monospaced)).frame(minHeight: 260)
                Text("保留已有条件、变量与关闭动作；可在此编辑完整定义。ID、启用状态及审核依据以本表单为准。")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }
    private func appendAction() {
        do {
            var scene = try HomeJSON.parse(definition)
            guard scene.isObject else { throw HomeServerError.invalidDocument }
            var action: HomeJSON
            if actionKind.hasPrefix("motor_") {
                guard !review.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw HomeServerError.server("请先填写电机映射审核依据") }
                action = .object(["kind": .string("motor"), "device": .string(selectedDevice),
                                  "action": .string(String(actionKind.dropFirst(6))), "mapping_review": .string(review)])
            } else {
                let command: HomeJSON
                switch actionKind {
                case "level": command = .object(["kind": .string("level"), "value": .integer(Int64(brightness)), "ramp": .bool(true)])
                case "led_on", "led_off": command = .object(["kind": .string("panel_led"), "on": .bool(actionKind == "led_on")])
                default:
                    let device = home.editableDevices.first { $0.id == selectedDevice }
                    let profile = registeredModules.first { $0.id == device?.raw["module_id"].text }?.raw["profile"].text ?? ""
                    let dimmer = home.devices.first { $0.id == selectedDevice }?.raw["kind"].text == "Dimmer"
                        || ["ad8", "gt_d8", "gt_l8", "gt_e4"].contains { profile.hasPrefix($0) }
                    command = dimmer
                        ? .object(["kind": .string("level"), "value": .integer(actionKind == "switch_on" ? 100 : 0), "ramp": .bool(true)])
                        : .object(["kind": .string("switch"), "on": .bool(actionKind == "switch_on")])
                }
                action = .object(["kind": .string("command"), "device": .string(selectedDevice), "command": command])
            }
            var rows = scene["activate"].values
            rows.append(.object(["source": .string("mobile:\(UUID().uuidString)"), "action": action]))
            scene["activate"] = .array(rows); definition = scene.formatted; failure = nil
        } catch { failure = error.localizedDescription }
    }
    private func save() {
        do {
            switch request.kind {
            case "floor":
                if let old = request.floor { try home.renameFloor(old, to: name) }
                else {
                    try home.addFloor(name)
                }
            case "room": try home.upsertRoom(id: id, name: name, floor: floor.isEmpty ? nil : floor)
            case "device":
                guard let value = Int64(channel), (1...255).contains(value) else { throw HomeServerError.invalidDocument }
                var row = request.record?.raw ?? .object([:])
                row["id"] = .string(id); row["name"] = .string(name); row["enabled"] = .bool(enabled)
                row["room"] = room.isEmpty ? .null : .string(room); row["channel"] = .integer(value)
                if home.deviceDocument?["schema_version"].integerText == "2" {
                    guard !module.isEmpty else { throw HomeServerError.invalidDocument }
                    row = row.removing(["gateway", "address"]); row["module_id"] = .string(module)
                } else {
                    let target = try BusAddress(hex: address)
                    guard !gateway.isEmpty else { throw HomeServerError.invalidDocument }
                    row = row.removing(["module_id"]); row["gateway"] = .string(gateway); row["address"] = .string(target.description)
                }
                try home.upsertDevice(row, originalID: request.record?.id)
            case "scene":
                var scene = try HomeJSON.parse(definition)
                guard scene.isObject else { throw HomeServerError.invalidDocument }
                scene["id"] = .string(id); scene["enabled"] = .bool(enabled)
                if !review.isEmpty { scene["review_evidence"] = .string(review) }
                else if collection == "immediate_scenes" { throw HomeServerError.server("请填写场景审核依据") }
                else { scene = scene.removing(["review_evidence"]) }
                try home.upsertScene(scene, collection: collection, name: name, originalID: request.record?.id,
                                     room: sceneScope == "room" ? room : "", scope: sceneScope)
            default: throw HomeServerError.invalidDocument
            }
            dismiss()
        } catch { failure = error.localizedDescription }
    }
}
