import SwiftUI
import HomeSenseCore

private struct KeypadKeyEdit: Identifiable {
    let number: Int
    let original: HomeJSON
    var configured: Bool
    var label: String
    var endpoint: String
    var enabled: Bool
    var scene: String
    var mappingKind: String
    var motorDevice: String
    var motorAction: String
    var review: String
    var id: Int { number }
}

struct HomeKeypadEditor: View {
    @ObservedObject var home: HomeServerStore
    let record: HomeRecord?
    @Environment(\.dismiss) private var dismiss
    @State private var id: String
    @State private var name: String
    @State private var room: String
    @State private var module: String
    @State private var enabled: Bool
    @State private var keys: [KeypadKeyEdit]
    @State private var failure: String?
    init(home: HomeServerStore, record: HomeRecord?) {
        self.home = home; self.record = record
        let raw = record?.raw ?? .object([:])
        let id = record?.id ?? "mobile-keypad-\(UUID().uuidString.lowercased().prefix(8))"
        _id = State(initialValue: id); _name = State(initialValue: raw["name"].text ?? "")
        _room = State(initialValue: raw["room"].text ?? "")
        _module = State(initialValue: raw["module_id"].text ?? "")
        _enabled = State(initialValue: raw["enabled"].flag)
        _keys = State(initialValue: (1...6).map { number in
            let row = raw["keys"].values.first { $0["number"].integerText == String(number) } ?? .object([:])
            return KeypadKeyEdit(number: number, original: row, configured: row["number"] != .null,
                label: row["label"].text ?? "按键 \(number)",
                endpoint: row["led_endpoint"]["id"].text ?? "\(id)-key-\(number)-led",
                enabled: row["led_endpoint"]["enabled"].flag,
                scene: row["action"]["scene"].text ?? "", mappingKind: row["action"]["kind"].text ?? "none",
                motorDevice: row["action"]["device"].text ?? "", motorAction: row["action"]["action"].text ?? "stop",
                review: row["action"]["review_evidence"].text ?? "")
        })
    }
    var body: some View {
        NavigationStack {
            Form {
                if let failure { Text(failure).foregroundStyle(.red) }
                Section("面板") {
                    TextField("稳定 ID", text: $id).disabled(record != nil)
                    TextField("面板名称", text: $name)
                    Picker("房间", selection: $room) {
                        Text("未分配").tag("")
                        ForEach(home.editableRooms) { Text($0.name).tag($0.id) }
                    }
                    Picker("TS6 模块", selection: $module) {
                        Text("请选择模块").tag("")
                        ForEach(modules) { Text($0.id).tag($0.id) }
                    }
                    Toggle("启用面板", isOn: $enabled)
                }
                ForEach($keys) { $key in
                    Section("按键 \(key.number)") {
                        Toggle("配置此按键", isOn: $key.configured)
                        if key.configured {
                            TextField("用途名称", text: $key.label)
                            TextField("LED 稳定子端点 ID", text: $key.endpoint)
                                .disabled(key.original["number"] != .null)
                            Toggle("启用 LED 子端点", isOn: $key.enabled)
                            Picker("按键用途", selection: $key.mappingKind) {
                                Text("未映射，不发送动作").tag("none")
                                Text("场景切换").tag("scene_toggle")
                                Text("窗帘动作").tag("motor")
                                if key.original["action"]["kind"].text == "away_off" {
                                    Text("离家（保留现有审核映射）").tag("away_off")
                                }
                            }
                            if key.mappingKind == "scene_toggle" {
                                Picker("目标场景", selection: $key.scene) {
                                    Text("请选择场景").tag("")
                                    if !key.scene.isEmpty, !home.editableKeypadScenes.contains(where: { $0.id == key.scene }) {
                                        Text("\(key.scene)（当前映射，需核对）").tag(key.scene)
                                    }
                                    ForEach(home.editableKeypadScenes) { Text($0.name).tag($0.id) }
                                }
                                Text("可选用途来自已审核、已启用的场景选择组。")
                                    .font(.caption).foregroundStyle(.secondary)
                            } else if key.mappingKind == "motor" {
                                Picker("已校准窗帘", selection: $key.motorDevice) {
                                    Text("请选择窗帘").tag("")
                                    ForEach(motors) { Text($0.name).tag($0.id) }
                                }
                                Picker("窗帘动作", selection: $key.motorAction) {
                                    Text("打开").tag("open"); Text("停止").tag("stop"); Text("关闭").tag("close")
                                }
                            }
                            if key.mappingKind == "away_off" {
                                Text("保留已发布的离家映射；执行范围由服务端审核配置决定。此处不创建或扩大全屋控制范围。")
                                    .font(.caption).foregroundStyle(.secondary)
                            } else if key.mappingKind != "none" {
                                TextField("映射审核依据", text: $key.review, axis: .vertical)
                                Text("点击此键只执行经过审核的映射，不注入物理按键事件。目标还需具备独立控制权限。")
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                }
                Section {
                    Text("只修改配置编辑稿。已有逐键 ID、规则、权限与 Apple 家庭身份由服务端校验保留；已有独立 LED 设备须先通过后端迁移，不能在此重复建父设备。保存、预览并显式发布后生效。")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            .navigationTitle(record == nil ? "新增 Keypad" : "编辑 Keypad")
            .disabled(home.busy || !home.canManage)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("取消") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) { Button("加入编辑稿") { save() }.disabled(home.busy || !home.canManage) }
            }
        }.frame(idealWidth: 560, idealHeight: 680)
    }
    private var modules: [HomeRecord] {
        (home.deviceDocument?["gateways"].values ?? []).flatMap { $0["modules"].values.map(HomeRecord.init) }
            .filter { $0.raw["profile"].text?.hasPrefix("ts6") == true || $0.id == module }
    }
    private var motors: [HomeRecord] {
        home.editableDevices.filter { device in
            device.raw["enabled"].flag && (device.raw["motor_calibration"].isObject
                || home.devices.contains { $0.id == device.id && $0.raw["kind"].text == "Motor" && $0.raw["motor_calibrated"].flag })
        }
    }
    private func save() {
        do {
            var row = record?.raw ?? .object([:])
            row["id"] = .string(id); row["name"] = .string(name); row["module_id"] = .string(module)
            row["room"] = room.isEmpty ? .null : .string(room); row["enabled"] = .bool(enabled)
            row["keys"] = .array(try keys.filter(\.configured).map { key in
                var raw = key.original
                raw["number"] = .integer(Int64(key.number)); raw["label"] = .string(key.label)
                var led = raw["led_endpoint"].isObject ? raw["led_endpoint"] : .object([:])
                led["id"] = .string(key.endpoint); led["enabled"] = .bool(key.enabled)
                if led["name"].text == nil { led["name"] = .string(key.label + " LED") }
                raw["led_endpoint"] = led
                switch key.mappingKind {
                case "scene_toggle": raw["action"] = .object(["kind": .string("scene_toggle"), "scene": .string(key.scene), "review_evidence": .string(key.review)])
                case "motor": raw["action"] = .object(["kind": .string("motor"), "device": .string(key.motorDevice), "action": .string(key.motorAction), "review_evidence": .string(key.review)])
                case "away_off":
                    guard key.original["action"]["kind"].text == "away_off" else { throw HomeServerError.invalidDocument }
                    raw["action"] = key.original["action"]
                case "none": raw = raw.removing(["action"])
                default: throw HomeServerError.invalidDocument
                }
                return raw
            })
            try home.upsertKeypad(row, originalID: record?.id)
            dismiss()
        } catch { failure = error.localizedDescription }
    }
}
