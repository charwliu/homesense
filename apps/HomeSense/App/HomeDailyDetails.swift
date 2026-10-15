import SwiftUI
import HomeSenseCore

struct HomeDeviceTile: View {
    let device: HomeRecord
    var live = true
    private var confirmed: Bool { live && device.raw["quality"].text == "Confirmed" }
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
            if let room = device.raw["room"]["name"].text { Text(room).font(.caption).foregroundStyle(.secondary) }
            Text(status).font(.title3.weight(.medium)).foregroundStyle(confirmed ? Color.primary : Color.secondary)
            if device.raw["state_authority"].text == "pending_target" {
                Text("目标待确认").font(.caption).foregroundStyle(.orange)
            }
        }
        .padding(18).frame(maxWidth: .infinity, minHeight: 170, alignment: .topLeading)
        .background(isOn ? Color.orange.opacity(0.09) : Color.primary.opacity(0.025), in: RoundedRectangle(cornerRadius: 20))
        .background(.background, in: RoundedRectangle(cornerRadius: 20))
        .accessibilityElement(children: .combine)
    }
    private var status: String {
        let prefix = confirmed ? "实测 · " : "历史 · "
        if let level = device.raw["observed"]["level"].integerText { return prefix + level + "%" }
        if device.raw["observed"]["on"] == .bool(true) { return prefix + "开启" }
        if device.raw["observed"]["on"] == .bool(false) { return prefix + "关闭" }
        return "实测状态未知"
    }
}

struct HomeDeviceDetails: View {
    @ObservedObject var home: HomeServerStore
    let deviceID: String
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            ScrollView {
                if let device = home.devices.first(where: { $0.id == deviceID }) {
                    HomeDeviceCard(home: home, device: device).padding()
                } else { ContentUnavailableView("设备暂不可用", systemImage: "lightbulb") }
                if let error = home.error { Text(error).foregroundStyle(.red).padding() }
                Text(home.message).font(.caption).foregroundStyle(.secondary).padding()
            }
            .navigationTitle(home.devices.first(where: { $0.id == deviceID })?.name ?? "设备")
            .toolbar { Button("完成") { dismiss() } }
        }.frame(idealWidth: 460, idealHeight: 500)
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
                        Text("负载反馈：\(scene.raw["activity"].text ?? "未知")")
                        if scene.raw["switch_kind"].text == "selection" {
                            Text("选择状态：\(scene.raw["selected"] == .bool(true) ? "开启" : scene.raw["selected"] == .bool(false) ? "关闭" : "未知")")
                        }
                        Text("场景选择与负载反馈分别显示，受理不表示设备已到位。")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    Section("控制") {
                        if scene.raw["switch_kind"].text == "selection" {
                            Button("开启场景") { Task { await home.controlScene(sceneID, on: true) } }
                            Button("关闭场景") { Task { await home.controlScene(sceneID, on: false) } }
                        } else { Button("执行场景") { Task { await home.controlScene(sceneID, on: nil) } } }
                    }.disabled(!home.controlsAvailable || !scene.raw["can_control"].flag)
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
                        Text("按键动作使用已审核映射；LED 设置是独立操作。")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    ForEach(keypad.keys) { key in
                        Section("\(key.number) · \(key.name)") {
                            Text(key.led.actualDescription)
                            if let history = key.led.historyDescription { Text(history).foregroundStyle(.secondary) }
                            if let raw = record?.raw["keys"].values.first(where: { $0["number"].integerText == String(key.number) }) {
                                if raw["key_event"]["pressed"] != .null {
                                    Text("按键事件：\(raw["key_event"]["pressed"] == .bool(true) ? "按下" : "释放") · \(raw["key_event"]["quality"].text ?? "未知")")
                                        .font(.caption).foregroundStyle(.secondary)
                                }
                                if let status = raw["led"]["last_request"]["status"].text {
                                    Text("最近请求：\(status)").font(.caption).foregroundStyle(.secondary)
                                }
                            }
                            if let action = key.actionDescription {
                                Button(action) { Task { await home.activateKeypad(keypadID, key: key.number) } }
                                    .disabled(!home.canActivateKeypad(keypadID, key: key.number))
                            } else { Text("未配置已授权的按键动作").font(.caption).foregroundStyle(.secondary) }
                            HStack {
                                Button("设置红灯") { Task { await home.writeKeypadLED(keypadID, key: key.number, color: .red) } }
                                Button("设置绿灯") { Task { await home.writeKeypadLED(keypadID, key: key.number, color: .green) } }
                                if keypad.ledColors.contains(.off) {
                                    Button("灭灯") { Task { await home.writeKeypadLED(keypadID, key: key.number, color: .off) } }
                                }
                            }.disabled(!home.controlsAvailable || !key.canWriteLED)
                        }
                    }
                    Section("工作模式") {
                        LabeledContent("配置工作模式", value: mode(keypad.settings["work_mode"]["configured"].text))
                        settingStatus(keypad, field: "work_mode")
                        Group {
                            Button("查询工作模式") { setting(.object(["action": .string("read_work_mode")])) }
                            Button("写入已发布的工作模式") { setting(.object(["action": .string("work_mode"), "mode": keypad.settings["work_mode"]["configured"]])) }
                        }.disabled(!canSet(keypad, field: "work_mode"))
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
                        }.disabled(!canSet(keypad, field: "backlight"))
                    }
                    Section("按键声音") {
                        settingStatus(keypad, field: "sound")
                        Group {
                            Button("开启声音") { setting(.object(["action": .string("sound"), "enabled": .bool(true)])) }
                            Button("关闭声音") { setting(.object(["action": .string("sound"), "enabled": .bool(false)])) }
                            Button("查询声音") { setting(.object(["action": .string("read_sound")])) }
                        }.disabled(!canSet(keypad, field: "sound"))
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
    }
    private func mode(_ value: String?) -> String {
        switch value { case "manual": "手动灯控"; case "self_reset": "自复位"; default: "未知" }
    }
    private func canSet(_ keypad: HomeKeypad, field: String) -> Bool {
        home.controlsAvailable && home.canManage && keypad.settings[field]["can_write"].flag
            && keypad.keys.allSatisfy { $0.endpointID != nil }
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
