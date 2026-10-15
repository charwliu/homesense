import SwiftUI
import UniformTypeIdentifiers
import HomeSenseCore

enum ToolPage: String, CaseIterable, Identifiable {
    case address = "面板地址", test = "指令测试", log = "通信日志", connection = "网关连接"
    var id: String { rawValue }
    var icon: String {
        switch self {
        case .address: "number.square"
        case .test: "slider.horizontal.3"
        case .log: "terminal"
        case .connection: "network"
        }
    }
}

struct ContentView: View {
    @ObservedObject var session: CommissioningSession
    @State private var selected: ToolPage? = .connection
    @State private var key = 1
    @State private var backlight = 1
    @State private var confirmChange = false
    @State private var exporting = false
    @State private var exportDocument = LogDocument(text: "")

    var body: some View {
        NavigationSplitView {
            List(selection: $selected) {
                Section {
                    Label {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("HomeSense · 本地调试").font(.headline)
                            Text("TS6-86 · 面板调试").font(.caption).foregroundStyle(.secondary)
                        }
                    } icon: {
                        Image(systemName: "wave.3.right.circle.fill").font(.title).foregroundStyle(.teal)
                    }
                    .padding(.vertical, 12)
                }
                Section("工具") {
                    ForEach(ToolPage.allCases) { page in
                        NavigationLink(value: page) { Label(page.rawValue, systemImage: page.icon) }
                    }
                }
                Section("通信状态") {
                    Label(statusText, systemImage: session.status == .connected ? "circle.fill" : "circle")
                        .foregroundStyle(session.status == .connected ? Color.teal : Color.secondary)
                    LabeledContent("目标地址", value: session.address.uppercased())
                    Text("TCP 串口 · 9600 / 8N1")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            .navigationTitle("HomeSense")
            .navigationSplitViewColumnWidth(min: 220, ideal: 250, max: 300)
        } detail: {
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    heading
                    if let error = session.error {
                        Label(error, systemImage: "exclamationmark.triangle.fill")
                            .foregroundStyle(.red).padding()
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(.red.opacity(0.08), in: RoundedRectangle(cornerRadius: 12))
                    }
                    switch selected ?? .connection {
                    case .connection: connectionPage
                    case .address: addressPage
                    case .test: testPage
                    case .log: logPage
                    }
                }
                .padding(24)
                .frame(maxWidth: 920, alignment: .leading)
                .frame(maxWidth: .infinity)
            }
            .background(.background.secondary)
            .navigationTitle((selected ?? .connection).rawValue)
            .toolbar {
                if session.status != .disconnected {
                    Button("断开", systemImage: "xmark.circle") { session.disconnect() }
                }
            }
        }
        .alert("修改 TS6-86 硬件地址", isPresented: $confirmChange) {
            Button("发送改址指令", role: .destructive) { session.changeAddress() }
            Button("取消", role: .cancel) { }
        } message: {
            Text("目标 \(session.address.uppercased()) → \(session.newAddress.uppercased())\n\(session.changePreview)\n请保持长按面板按键①。新地址不能与总线其他设备重复。写出后需重新查询核对。")
        }
        .fileExporter(isPresented: $exporting, document: exportDocument, contentType: .plainText,
                      defaultFilename: "HomeSense-log.txt") { result in
            if case .failure(let error) = result { session.error = error.localizedDescription }
        }
    }

    private var statusText: String {
        switch session.status {
        case .disconnected: "未连接"
        case .connecting: "连接中…"
        case .connected: "TCP 已连接"
        }
    }

    private var heading: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("TS6-86").font(.caption.weight(.semibold)).tracking(3).foregroundStyle(.teal)
            Text((selected ?? .connection).rawValue).font(.largeTitle.bold())
            Text(session.notice).font(.subheadline).foregroundStyle(.secondary).textSelection(.enabled)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var connectionPage: some View {
        VStack(spacing: 20) {
            card("TCP 串口 网关", icon: "network") {
                Text("Mac、iPhone / iPad 通过局域网连接 TCP Server 模式的透明网关。")
                    .foregroundStyle(.secondary)
                field("主机名 / IP", text: $session.host, prompt: "例如 192.168.3.7")
                field("TCP 端口", text: $session.port, prompt: "例如 8234")
                HStack {
                    if session.status == .disconnected {
                        Button("连接网关", systemImage: "link") { session.connect() }
                            .buttonStyle(.borderedProminent).disabled(session.busy)
                    } else {
                        Button("断开连接", systemImage: "link.badge.plus") { session.disconnect() }
                            .buttonStyle(.bordered)
                    }
                    if session.status == .connecting { ProgressView().controlSize(.small) }
                    Text(statusText).foregroundStyle(.secondary)
                }
            }
            card("连接前准备", icon: "cable.connector") {
                Text("网关串口设为 9600 波特率、8 数据位、无校验、1 停止位；确认 A/B 接线及面板供电。")
                Text("调试时由此 App 独占网关连接，暂停其他向同一总线发指令的控制程序。iPhone / iPad 首次连接需允许访问本地网络。")
                Text("连接后不会自动扫描或发指令；断线及进入后台会取消当前操作，重新连接后需手动查询。")
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var addressPage: some View {
        VStack(spacing: 20) {
            card("读取面板地址", icon: "magnifyingglass") {
                targetField
                hexLine(session.addressPreview)
                Button("查询当前地址", systemImage: "arrow.down.circle") {
                    session.send(.readAddress, label: "地址查询", expectsReply: true)
                }
                .buttonStyle(.borderedProminent).disabled(!session.canSend)
                Divider()
                Text("不知道当前地址？").font(.subheadline.bold())
                Toggle("已确认总线上只连接这一块面板", isOn: $session.isolatedPanel)
                Button("广播查询地址 · 00", systemImage: "antenna.radiowaves.left.and.right") {
                    session.queryUnknownAddress()
                }
                .buttonStyle(.bordered).disabled(!session.canSend || !session.isolatedPanel)
                Text("支持 TS6-86/B 的单字节地址回复，包括 00。00 为广播目标；仅单面板连接时可读取或改为 01–FF。")
                    .font(.caption).foregroundStyle(.secondary)
                if let address = session.queriedAddress {
                    Divider()
                    HStack {
                        Label("地址读回 \(address.description)", systemImage: "number.square.fill")
                            .font(.title3.bold()).foregroundStyle(.teal)
                        Spacer()
                        Button("选为目标") { session.useAddress(address.value) }.disabled(session.busy)
                    }
                }
                if !session.lastReply.isEmpty { hexLine(session.lastReply) }
            }
            card("修改硬件地址", icon: "pencil.circle") {
                field("新地址 · HEX", text: $session.newAddress, prompt: "01–FF")
                hexLine(session.changePreview)
                Text("发送时必须同时长按面板按键①。请为新地址选择总线上未占用的 01–FF；当前地址为 00 时，还须确认只有这一块面板连接。")
                Toggle("我正长按目标面板按键①", isOn: $session.holdingKeyOne)
                Button("修改面板地址", systemImage: "arrow.right.arrow.left") { confirmChange = true }
                    .buttonStyle(.borderedProminent)
                    .disabled(!session.canSend || !session.holdingKeyOne || !validChange)
                if let next = session.writtenAddress {
                    Divider()
                    Text("已写出改址指令 → \(next.description)，待核对").font(.subheadline.bold())
                    ViewThatFits(in: .horizontal) {
                        HStack { verifyButtons(next) }
                        VStack(alignment: .leading) { verifyButtons(next) }
                    }
                    Text("查询与收到报文均不等于改址成功确认；请结合新地址报文及面板按键操作核对。")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            card("本次连接观察到的地址", icon: "dot.radiowaves.left.and.right") {
                if session.observedAddresses.isEmpty {
                    Text("尚未收到带地址的完整帧。可以查询，或按下面板按键观察反馈。")
                        .foregroundStyle(.secondary)
                }
                ForEach(session.observedAddresses.keys.sorted(), id: \.self) { address in
                    HStack {
                        Text(String(format: "%02X", address)).font(.title3.monospaced().bold())
                        Text("收到原始帧").font(.caption).foregroundStyle(.secondary)
                        Spacer()
                        if let time = session.observedAddresses[address] {
                            Text(time, style: .time).font(.caption.monospacedDigit())
                        }
                        Button("选为目标") { session.useAddress(address) }.disabled(session.busy)
                    }
                }
                Text("帧来源地址只说明观察到通信，不识别唯一物理面板，也不检测同地址冲突。")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private func verifyButtons(_ next: BusAddress) -> some View {
        Button("查询新地址 \(next.description)") { session.queryWrittenAddress() }
            .buttonStyle(.bordered).disabled(!session.canSend)
        Button("将 \(next.description) 设为测试目标") { session.useAddress(next.value) }
            .buttonStyle(.bordered).disabled(session.busy)
    }

    private var validChange: Bool {
        (try? TS6Command.addressChange(fromHex: session.address,
                                     to: BusAddress(hex: session.newAddress),
                                     isolated: session.isolatedPanel)) != nil
    }

    private var testPage: some View {
        VStack(spacing: 20) {
            card("按键灯测试", icon: "lightbulb") {
                targetField
                Picker("按键", selection: $key) {
                    ForEach(1...6, id: \.self) { Text("\($0)").tag($0) }
                }.pickerStyle(.segmented)
                ViewThatFits(in: .horizontal) {
                    HStack { ledButtons }
                    VStack(alignment: .leading) { ledButtons }
                }
                Text("LED 测试要求反馈灯处于手动模式，可在下方切换。面板处于本地测试模式时，请先退出本地测试。")
                    .font(.caption).foregroundStyle(.secondary)
            }
            card("按键事件监视", icon: "hand.tap") {
                Toggle("按键联动 LED · 红／绿切换", isOn: Binding(
                    get: { session.keyLEDLinkEnabled },
                    set: { session.setKeyLEDLinkEnabled($0) }
                ))
                .disabled(!session.keyLEDLinkEnabled && !session.canSend)
                Text("启用后切换为手动灯控，并将六键设为绿灯。每次按下面板按键切换该键红／绿，松开保持灯色。请保持 App 在前台。")
                    .font(.caption).foregroundStyle(.secondary)
                Text("在实体面板上按键，显示当前目标地址的 D1–D6 / F1、F2 反馈。")
                    .font(.caption).foregroundStyle(.secondary)
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 110))], spacing: 12) {
                    ForEach(1...6, id: \.self) { key in
                        VStack(spacing: 8) {
                            Image(systemName: session.keyEvents[key]?.pressed == true ? "hand.tap.fill" : "hand.tap")
                                .font(.title2).foregroundStyle(.teal)
                            Text("按键 \(key)").font(.headline)
                            Text(session.keyEvents[key].map { $0.pressed ? "按下" : "已释放" } ?? "尚无事件")
                                .font(.caption).foregroundStyle(.secondary)
                            if let color = session.lastWrittenLEDColors[key] {
                                Label(color == .red ? "已写出红灯" : color == .green ? "已写出绿灯" : "已写出关灯",
                                      systemImage: "circle.fill")
                                    .font(.caption2)
                                    .foregroundStyle(color == .red ? Color.red : color == .green ? Color.green : Color.secondary)
                            }
                            if let event = session.keyEvents[key] {
                                Text(event.time, style: .time).font(.caption2.monospacedDigit())
                            }
                        }
                        .frame(maxWidth: .infinity).padding(.vertical, 14)
                        .background(.teal.opacity(0.06), in: RoundedRectangle(cornerRadius: 12))
                    }
                }
                Text("灯色标记表示已写出的指令，请观察实体 LED。断线、切换地址、改址或手动设置灯色／反馈模式会停止联动。")
                    .font(.caption).foregroundStyle(.secondary)
            }
            card("参数与版本", icon: "switch.2") {
                HStack {
                    Button("查询反馈灯模式") { send(.readFeedbackMode, "反馈灯模式查询", read: true) }
                    Button("查询按键声") { send(.readSound, "按键声查询", read: true) }
                }
                ViewThatFits(in: .horizontal) {
                    HStack { modeButtons }
                    VStack(alignment: .leading) { modeButtons }
                }
                HStack {
                    Button("开启按键声") { send(.setSound(true), "开启按键声") }
                    Button("关闭按键声") { send(.setSound(false), "关闭按键声") }
                }
                Divider()
                Picker("背光亮度模式", selection: $backlight) {
                    Text("10 秒后调暗").tag(1)
                    Text("保持最亮").tag(2)
                    Text("保持最暗").tag(3)
                }
                HStack {
                    Button("查询背光") { send(.readBacklight, "背光查询", read: true) }
                    Button("设置背光") { send(.setBacklight(backlight), "背光模式设置") }
                }
                Divider()
                Button("查询固件版本 · ASCII") { send(.firmware, "固件版本查询") }
                if !session.lastReply.isEmpty { hexLine(session.lastReply) }
                Text("版本返回为 ASCII，在通信日志 RX 中查看。参数设置仅单次发送，按 TS6-86 / TS6-86-B 手册执行。")
                    .font(.caption).foregroundStyle(.secondary)
            }
            .buttonStyle(.bordered).disabled(!session.canSend)
        }
    }

    private var ledButtons: some View {
        Group {
            Button("红灯 · 激活", systemImage: "circle.fill") { send(.led(key: key, color: .red), "按键 \(key) 红灯") }.tint(.red)
            Button("绿灯 · 非激活", systemImage: "circle.fill") { send(.led(key: key, color: .green), "按键 \(key) 绿灯") }.tint(.green)
            Button("背光全关", systemImage: "circle") { send(.led(key: key, color: .off), "按键 \(key) 背光关") }
        }.buttonStyle(.bordered).disabled(!session.canSend)
    }
    private var modeButtons: some View {
        Group {
            Button("切换为手动灯控") { send(.setManualFeedback(true), "切换手动灯控") }
            Button("启用自复位") { send(.setManualFeedback(false), "启用自复位") }
        }
    }
    private func send(_ command: TS6Command, _ label: String, read: Bool = false) {
        session.send(command, label: label, expectsReply: read)
    }

    private var logPage: some View {
        card("收发记录", icon: "terminal") {
            HStack {
                Text("最近 \(session.logs.count) / 600 条").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("导出", systemImage: "square.and.arrow.up") {
                    exportDocument = LogDocument(text: session.transcript); exporting = true
                }
                Button("清空", systemImage: "trash") { session.clearLogs() }
            }
            Text("TX 为 TCP 写出；RX 为原始字节；帧为固定 5 字节解码。未知帧保持原样，ASCII 列为字节预览。")
                .font(.caption).foregroundStyle(.secondary)
            if session.logs.isEmpty {
                ContentUnavailableView("暂无通信记录", systemImage: "terminal", description: Text("连接网关并手动发送查询后，收发报文会显示在这里。"))
            }
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(session.logs.reversed()) { entry in
                    VStack(alignment: .leading, spacing: 5) {
                        HStack {
                            Text(entry.kind.rawValue).font(.caption.bold())
                                .foregroundStyle(entry.kind == .tx ? Color.teal : entry.kind == .error ? Color.red : Color.secondary)
                            Text(entry.time, format: .dateTime.hour().minute().second().secondFraction(.fractional(3)))
                                .font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                        }
                        Text(entry.text).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                    }
                    .padding(.vertical, 10)
                    Divider()
                }
            }
        }
    }

    private var targetField: some View {
        field("目标地址 · HEX", text: $session.address, prompt: "00–FF，例如 10")
    }
    private func field(_ label: String, text: Binding<String>, prompt: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(label).font(.caption).foregroundStyle(.secondary)
            TextField(prompt, text: text).textFieldStyle(.roundedBorder)
                .font(.system(.body, design: .monospaced)).disabled(session.busy || (selected == .connection && session.status != .disconnected))
                #if os(iOS)
                .textInputAutocapitalization(.never).autocorrectionDisabled()
                #endif
        }
    }
    private func hexLine(_ text: String) -> some View {
        Text(text).font(.system(.subheadline, design: .monospaced)).textSelection(.enabled)
            .padding(12).frame(maxWidth: .infinity, alignment: .leading)
            .background(.teal.opacity(0.07), in: RoundedRectangle(cornerRadius: 8))
    }
    private func card<Content: View>(_ title: String, icon: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            Label(title, systemImage: icon).font(.headline)
            content()
        }
        .padding(20).frame(maxWidth: .infinity, alignment: .leading)
        .background(.background, in: RoundedRectangle(cornerRadius: 16))
        .overlay(RoundedRectangle(cornerRadius: 16).stroke(.quaternary, lineWidth: 1))
    }
}

struct LogDocument: FileDocument {
    static var readableContentTypes: [UTType] { [.plainText] }
    var text: String
    init(text: String) { self.text = text }
    init(configuration: ReadConfiguration) throws {
        text = String(data: configuration.file.regularFileContents ?? Data(), encoding: .utf8) ?? ""
    }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: Data(text.utf8))
    }
}
