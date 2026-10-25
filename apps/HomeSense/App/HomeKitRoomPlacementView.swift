#if os(iOS)
import SwiftUI
import HomeSenseCore

struct HomeKitRoomPlacementView: View {
    private typealias P = HomeKitRoomPlacement
    @ObservedObject var home: HomeServerStore
    @StateObject private var directory = HomeKitRoomDirectory.shared
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var phase
    @State private var selectedHome: UUID?
    @State private var publication: P.Publication?
    @State private var inventory: P.Inventory?
    @State private var bridgeBindings: [String: UUID] = [:]
    @State private var roomBindings: [String: P.Destination] = [:]
    @State private var unassignedRoom: UUID?
    @State private var preview: P.Preview?
    @State private var report: HomeKitRoomPlacementExecution.Report?
    @State private var operation: Task<Void, Never>?
    @State private var busy = false
    @State private var message = ""

    var body: some View {
        NavigationStack {
            Form {
                Section("Apple家庭") {
                    Button("读取Apple家庭并授权") { directory.startDiscovery() }.disabled(busy)
                    Text(directory.status).font(.caption).foregroundStyle(.secondary)
                    Picker("家庭", selection: $selectedHome) {
                        Text("请选择家庭").tag(UUID?.none)
                        ForEach(directory.homes) { Text($0.name).tag(Optional($0.id)) }
                    }.disabled(busy)
                    Button("读取发布清单与房间") { run { try await reload() } }
                        .disabled(busy || selectedHome == nil || !home.connected || !home.canManage)
                    Text("先将HomeSense桥加入所选Apple家庭，再选择桥和目标房间。")
                        .font(.caption).foregroundStyle(.secondary)
                }
                if let publication, let inventory {
                    bridgeSection(publication)
                    roomSection(publication, inventory)
                    Section {
                        Button("预览房间归属") {
                            do {
                                preview = try P.preview(publication: publication, inventory: inventory,
                                                        bridgeBindings: bridgeBindings, roomBindings: roomBindings, unassignedRoom: unassignedRoom)
                                report = nil; message = ""
                            } catch { message = failureText(error) }
                        }.disabled(busy)
                    }
                }
                if let preview { previewSection(preview) }
                if let report {
                    Section("迁移结果") {
                        Text("已确认迁移\(report.assigned.count)项；原归属正确\(report.alreadyAssigned)项；待处理\(report.unresolved)项。")
                        if let stopped = report.stopped {
                            Text(stopText(stopped)).foregroundStyle(.orange)
                        } else if report.complete { Label("房间归属已完成", systemImage: "checkmark.circle") }
                        Text("重新读取并预览后，可继续处理剩余配件。已实际到位的配件会跳过。")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                if !message.isEmpty { Section { Text(message).foregroundStyle(.orange) } }
                if directory.pendingRequest && !busy {
                    Section { Text(HomeKitRoomDirectory.Failure.pending.localizedDescription).foregroundStyle(.orange) }
                }
                if busy { Section { ProgressView("正在处理房间归属…") } }
            }
            .navigationTitle("Apple家庭房间")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(busy ? "停止后续操作" : "完成") {
                        if busy { operation?.cancel() } else { dismiss() }
                    }
                }
            }
        }
        .interactiveDismissDisabled(busy)
        .onChange(of: selectedHome) { _, _ in
            operation?.cancel(); publication = nil; inventory = nil; preview = nil; report = nil
            bridgeBindings = [:]; roomBindings = [:]; unassignedRoom = nil
        }
        .onChange(of: bridgeBindings) { _, _ in preview = nil }
        .onChange(of: roomBindings) { _, _ in preview = nil }
        .onChange(of: unassignedRoom) { _, _ in preview = nil }
        .onChange(of: home.revision) { _, _ in preview = nil; operation?.cancel() }
        .onChange(of: home.canManage) { _, allowed in if !allowed { preview = nil; operation?.cancel() } }
        .onChange(of: phase) { _, phase in if phase != .active { operation?.cancel() } }
        .onDisappear { operation?.cancel() }
    }

    private func bridgeSection(_ publication: P.Publication) -> some View {
        Section("对应的HomeSense桥") {
            ForEach(publication.bridges, id: \.id) { bridge in
                Picker("\(bridge.name) · \(bridge.accessories.count)配件", selection: Binding<UUID?>(
                    get: { bridgeBindings[bridge.id] }, set: { bridgeBindings[bridge.id] = $0 })) {
                    Text("不选择").tag(UUID?.none)
                    ForEach(directory.bridges) { native in
                        let count = inventory?.bridges.first { $0.id == native.id }?.accessories.count ?? 0
                        Text("\(native.name) · \(count)配件").tag(Optional(native.id))
                    }
                }.disabled(busy || !bridge.available)
                if !bridge.available { Text("该桥当前不可用").font(.caption).foregroundStyle(.secondary) }
            }
        }
    }
    private func roomSection(_ publication: P.Publication, _ inventory: P.Inventory) -> some View {
        Section("目标房间") {
            ForEach(roomIDs(publication), id: \.self) { id in
                let name = roomName(id)
                Picker(name, selection: Binding<P.Destination?>(get: { roomBindings[id] }, set: { roomBindings[id] = $0 })) {
                    Text("暂不分配").tag(P.Destination?.none)
                    ForEach(inventory.rooms, id: \.id) { Text($0.name).tag(Optional(P.Destination.existing($0.id))) }
                    if home.rooms.contains(where: { $0.id == id }) {
                        Text("新建：\(newRoomName(id))").tag(Optional(P.Destination.create(name: newRoomName(id))))
                    }
                }.disabled(busy)
            }
            if publication.bridges.flatMap(\.accessories).contains(where: { $0.roomID == nil }) {
                Picker("未归属／全屋场景", selection: $unassignedRoom) {
                    Text("暂不分配").tag(UUID?.none)
                    ForEach(inventory.rooms, id: \.id) { Text($0.name).tag(Optional($0.id)) }
                }.disabled(busy)
            }
        }
    }
    private func previewSection(_ preview: P.Preview) -> some View {
        Section("预览") {
            Text("将迁移\(preview.pendingAssignments.count)项，原归属正确\(preview.assignments.count - preview.pendingAssignments.count)项，新建\(preview.roomsToCreate.count)个房间。")
            ForEach(Array(preview.assignments.enumerated()), id: \.offset) { _, assignment in
                LabeledContent(sourceName(assignment.source), value: destinationName(assignment.destination))
                    .font(.caption)
            }
            ForEach(Array(preview.issues.enumerated()), id: \.offset) { _, issue in
                Text("\(sourceName(issue.source))：\(issueText(issue.reason))").font(.caption).foregroundStyle(.orange)
            }
            Button("应用\(preview.pendingAssignments.count)项房间归属") {
                run {
                    let result = await HomeKitRoomPlacementExecution.execute(preview, driver: directory) {
                        try await home.homeKitPlacementPublication()
                    }
                    report = result; self.preview = nil
                }
            }
            .disabled(busy || directory.pendingRequest || preview.pendingAssignments.isEmpty || preview.issues.contains(where: {
                bridgeBindings[$0.source.bridgeID] != nil && $0.reason != .unassignedRoom && $0.reason != .roomNotSelected
            }))
        }
    }
    private func run(_ job: @escaping @MainActor () async throws -> Void) {
        guard !busy else { return }
        busy = true; message = ""
        operation = Task { @MainActor in
            defer { busy = false; operation = nil }
            do { try await job() }
            catch is CancellationError { message = "已停止后续操作，请重新读取并预览。" }
            catch { message = failureText(error) }
        }
    }
    private func reload() async throws {
        guard let id = selectedHome else { throw HomeKitRoomDirectory.Failure.missingHome }
        let publication = try await home.homeKitPlacementPublication()
        let inventory = try await directory.inventory(homeID: id)
        try Task.checkCancellation()
        guard selectedHome == id else { throw HomeServerError.expiredRequest }
        self.publication = publication; self.inventory = inventory; preview = nil; report = nil
        bridgeBindings = bridgeBindings.filter { key, value in
            publication.bridges.contains { $0.id == key } && inventory.bridges.contains { $0.id == value }
        }
        roomBindings = [:]
        for room in roomIDs(publication) {
            let name = newRoomName(room), matches = inventory.rooms.filter { normalized($0.name) == normalized(name) }
            if matches.count == 1 { roomBindings[room] = .existing(matches[0].id) }
            else if matches.isEmpty && home.rooms.contains(where: { $0.id == room }) { roomBindings[room] = .create(name: name) }
        }
    }
    private func roomIDs(_ publication: P.Publication) -> [String] {
        Set(publication.bridges.flatMap(\.accessories).compactMap(\.roomID)).sorted { roomName($0) < roomName($1) }
    }
    private func normalized(_ name: String) -> String { name.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() }
    private func roomName(_ id: String) -> String {
        guard let row = home.rooms.first(where: { $0.id == id }) else { return "房间资料未读取" }
        return [row.raw["floor"].text, row.name].compactMap { $0 }.joined(separator: " · ")
    }
    private func newRoomName(_ id: String) -> String {
        guard let row = home.rooms.first(where: { $0.id == id }) else { return "" }
        if home.rooms.filter({ $0.name == row.name }).count > 1 { return roomName(id) }
        return row.name
    }
    private func sourceName(_ source: P.Source) -> String {
        switch source.kind {
        case .light, .fan, .curtain: return home.devices.first { $0.id == source.id }?.name ?? "配件"
        case .scene: return home.scenes.first { $0.id == source.id }?.name ?? "场景"
        }
    }
    private func destinationName(_ destination: P.Destination) -> String {
        switch destination {
        case .existing(let id): inventory?.rooms.first { $0.id == id }?.name ?? "房间已不可用"
        case .create(let name): "新建：\(name)"
        }
    }
    private func issueText(_ reason: P.Reason) -> String {
        switch reason {
        case .bridgeUnavailable: "服务端桥不可用"
        case .bridgeNotSelected: "未选择对应的桥"
        case .bridgeNotDiscovered: "尚未在所选家庭发现该桥"
        case .accessoryNotDiscovered: "尚未在所选桥中确认配件标识"
        case .unassignedRoom: "请选择未归属配件的目标房间"
        case .roomNotSelected: "尚未选择目标房间"
        case .roomNotDiscovered: "目标房间已不可用"
        }
    }
    private func failureText(_ error: Error) -> String {
        if error is P.Failure { return "预览已失效或配件归属不明确，请重新读取并检查桥和房间选择。" }
        return error.localizedDescription
    }
    private func stopText(_ stop: HomeKitRoomPlacementExecution.Stop) -> String {
        switch stop {
        case .cancelled: "已停止后续迁移。"
        case .stalePreview: "配置或房间归属已变化，请重新预览。"
        case .unavailable: "暂时无法读取家庭数据，请重新读取核对。"
        case .operationFailed: "部分操作未完成，请重新读取核对。"
        case .verificationFailed: "部分归属未获确认，请重新读取核对。"
        }
    }
}
#endif
