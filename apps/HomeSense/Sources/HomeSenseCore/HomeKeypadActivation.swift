import Foundation

struct HomeKeypadActionCapability {
    static func supported(_ raw: HomeJSON) -> Bool {
        raw["version"] == .integer(1) && raw["path"] == .string(HomeKeypadActivationRequest.path)
            && raw["gestures"] == .array([.string("activate")]) && raw["indication_schema"] == .integer(1)
    }
    static func available(_ raw: HomeJSON, panel: String, key: Int, revision: String) -> Bool {
        raw["version"] == .integer(1) && raw["path"] == .string(HomeKeypadActivationRequest.path)
            && raw["gestures"] == .array([.string("activate")]) && raw["revision"] == .string(revision)
            && raw["keypad"] == .string(panel) && raw["key"] == .integer(Int64(key)) && raw["available"] == .bool(true)
    }
}

struct HomeKeypadActionHTTPFailure: LocalizedError {
    let status: Int
    let rejected: Bool
    let code: String
    var errorDescription: String? { rejected ? "按键请求未受理：\(code)" : "按键执行结果未知，请核对设备；未自动重试。" }
}

/// A complete virtual press. Targets and LED colors are resolved by the server.
struct HomeKeypadActivationRequest: Equatable, Sendable {
    static let path = "/api/v1/keypad-actions"
    let revision: String
    let keypad: String
    let key: Int
    let requestID: UUID

    init(revision: String, keypad: String, key: Int, requestID: UUID = UUID()) throws {
        guard let version = UInt64(revision), version > 0, String(version) == revision,
              !keypad.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              keypad.utf8.count <= 250, (1...6).contains(key) else { throw HomeServerError.invalidDocument }
        self.revision = revision; self.keypad = keypad; self.key = key; self.requestID = requestID
    }

    var body: HomeJSON {
        .object(["revision": .string(revision), "keypad": .string(keypad), "key": .integer(Int64(key)),
                 "request_id": .string(requestID.uuidString), "gesture": .string("activate")])
    }
}

/// An accepted cue remains separate from observed LED and physical load state.
struct HomeKeypadActionIndication: Equatable, Sendable {
    enum Status: String, Sendable { case pending, written, mappingGap = "mapping_gap", unknown, superseded, cancelled, retired }
    enum Source: String, Sendable { case authenticatedClick = "authenticated_click", buttonEvent = "button_event" }
    let requestID: String
    let operationSequence: UInt64
    let source: Source
    let eventSequence: UInt64?
    let status: Status
    let color: HomeKeypadLEDColor?

    init(_ raw: HomeJSON, request: HomeKeypadActivationRequest, endpoint: String) throws {
        try self.init(raw, endpoint: endpoint)
        guard requestID == request.requestID.uuidString, source == .authenticatedClick, eventSequence == nil else { throw HomeServerError.invalidResponse }
    }
    init(_ raw: HomeJSON, endpoint: String, boot: String? = nil) throws {
        guard raw.isObject, let requestID = raw["request_id"].text, !requestID.isEmpty, requestID.utf8.count <= 128,
              requestID.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || $0 == 45 || $0 == 95 }),
              let token = raw["operation_sequence"].text, let sequence = UInt64(token), sequence > 0, String(sequence) == token,
              raw["endpoint_id"] == .string(endpoint), !endpoint.isEmpty,
              let source = raw["source"].text.flatMap(Source.init(rawValue:)), raw["observed"] == .bool(false),
              let status = raw["status"].text.flatMap(Status.init(rawValue:)) else { throw HomeServerError.invalidResponse }
        let eventSequence: UInt64?
        switch source {
        case .authenticatedClick:
            guard raw["event_sequence"] == .null else { throw HomeServerError.invalidResponse }
            eventSequence = nil
        case .buttonEvent:
            guard let token = raw["event_sequence"].text, let value = UInt64(token), value > 0, String(value) == token else {
                throw HomeServerError.invalidResponse
            }
            guard let boot, let uuid = UUID(uuidString: boot),
                  requestID == "wall-\(uuid.uuidString.replacingOccurrences(of: "-", with: "").lowercased())-\(value)" else {
                throw HomeServerError.invalidResponse
            }
            eventSequence = value
        }
        let color: HomeKeypadLEDColor?
        switch raw["color"] {
        case .null: color = nil
        case .string("red"): color = .red
        case .string("green"): color = .green
        case .string("off"): color = .off
        default: throw HomeServerError.invalidResponse
        }
        guard ![Status.superseded, .cancelled, .retired].contains(status) || color == nil else { throw HomeServerError.invalidResponse }
        self.requestID = requestID; operationSequence = sequence; self.status = status; self.color = color
        self.source = source; self.eventSequence = eventSequence
    }

    var progress: Int {
        switch status {
        case .unknown: color == nil ? 0 : 3
        case .mappingGap: 0
        case .pending: 1
        case .written: 2
        case .superseded, .cancelled: 3
        case .retired: 4
        }
    }
}

enum HomeKeypadActivationReceipt: Equatable, Sendable {
    case accepted(HomeKeypadActionIndication)
    case unknown

    init(_ raw: HomeJSON, request: HomeKeypadActivationRequest, boot: String, endpoint: String) throws {
        guard raw.isObject, raw["request_id"] == .string(request.requestID.uuidString),
              raw["retry"] == .bool(false) else { throw HomeServerError.invalidResponse }
        if raw["outcome"] == .string("unknown") {
            guard raw["indication"] == .null,
                  ["execution_outcome_unknown", "receipt_unavailable_outcome_unknown"].contains(raw["error"].text ?? "") else {
                throw HomeServerError.invalidResponse
            }
            self = .unknown
            return
        }
        guard raw["outcome"] == .string("accepted"), !boot.isEmpty,
              raw["boot"] == .string(boot), raw["revision"] == .string(request.revision),
              raw["keypad"] == .string(request.keypad), raw["key"] == .integer(Int64(request.key)),
              raw["gesture"] == .string("activate"), raw["execution"].isObject,
              ["scene", "motor", "away"].contains(raw["execution"]["kind"].text ?? "") else {
            throw HomeServerError.invalidResponse
        }
        self = .accepted(try HomeKeypadActionIndication(raw["indication"], request: request, endpoint: endpoint))
    }
}

/// Request progress has its own presentation; it never replaces HomeKeypadLED.
public struct HomeKeypadActionPresentation: Equatable, Sendable {
    public enum Phase: Equatable, Sendable { case submitting, pending, written, mappingGap, unresolved, uncertain, rejected, superseded, cancelled, retired, pressed, released }
    public let phase: Phase
    public let requestedColor: HomeKeypadLEDColor?

    init(phase: Phase, requestedColor: HomeKeypadLEDColor? = nil) {
        self.phase = phase
        self.requestedColor = [.pending, .written].contains(phase) && requestedColor != .unknown ? requestedColor : nil
    }
    public var title: String {
        switch phase {
        case .submitting: "提交中"
        case .pending: requestedColor.map { "请求\($0.title)灯" } ?? "已受理"
        case .written: "已写出"
        case .mappingGap: "映射待补"
        case .unresolved: "灯色未知"
        case .uncertain: "结果未知"
        case .rejected: "未受理"
        case .superseded: "已被替代"
        case .cancelled: "已取消"
        case .retired: "提示已更新"
        case .pressed: "按键按下"
        case .released: "按键释放"
        }
    }
    public var description: String {
        switch phase {
        case .submitting: "正在提交按键请求"
        case .pending: requestedColor.map { "已受理，请求\($0.title)灯；等待执行反馈" } ?? "按键请求已受理，等待执行反馈"
        case .written: requestedColor.map { "本键\($0.title)灯指令已写出；实测状态另行显示" } ?? "本键 LED 指令已写出；实测状态另行显示"
        case .mappingGap: "按键请求已受理，缺少本键 LED 映射"
        case .unresolved: "按键请求已受理，目标灯色尚未确定"
        case .uncertain: "执行结果未知，请核对设备；未自动重试"
        case .rejected: "按键请求未受理"
        case .superseded: "此按键请求已被较新操作替代"
        case .cancelled: "此按键请求已取消"
        case .retired: "已有较新 LED 写出，旧请求提示已结束"
        case .pressed: "收到有效按键按下反馈；不表示 LED 实测状态"
        case .released: "收到有效按键释放反馈；不重复发送控制"
        }
    }
    init(_ indication: HomeKeypadActionIndication) {
        let phase: Phase
        switch indication.status {
        case .pending: phase = .pending
        case .written: phase = .written
        case .mappingGap: phase = .mappingGap
        case .unknown: phase = indication.color == nil ? .unresolved : .uncertain
        case .superseded: phase = .superseded
        case .cancelled: phase = .cancelled
        case .retired: phase = .retired
        }
        self.init(phase: phase, requestedColor: indication.color)
    }
}

/// Correlates local requests only. WSS decoding and transport remain separate.
struct HomeKeypadActivationState {
    struct Context: Equatable, Sendable {
        let generation: UUID
        let boot: String
        let revision: String
    }
    private struct Key: Hashable { let panel: String; let number: Int }
    private struct Entry {
        var request: HomeKeypadActivationRequest?
        let endpoint: String
        var handled = false
        var replyFinished = false
        var latest: HomeKeypadActionIndication?
        var cleared = false
        var presentation: HomeKeypadActionPresentation?
        var retiredRequests: Set<String> = []
    }
    private var context: Context?
    private var entries: [Key: Entry] = [:]

    mutating func reset(to next: Context? = nil) {
        guard context != next || next == nil else { return }
        context = next; entries = [:]
    }
    mutating func begin(_ request: HomeKeypadActivationRequest, endpoint: String, context expected: Context) throws {
        guard context == expected, !expected.boot.isEmpty, expected.revision == request.revision,
              !endpoint.isEmpty else { throw HomeServerError.expiredRequest }
        let key = Key(panel: request.keypad, number: request.key)
        let old = entries[key]
        entries[key] = Entry(request: request, endpoint: endpoint, latest: old?.latest, cleared: old?.cleared ?? false,
                             presentation: HomeKeypadActionPresentation(phase: .submitting), retiredRequests: old?.retiredRequests ?? [])
    }
    func presentation(_ panel: String, key: Int) -> HomeKeypadActionPresentation? {
        entries[Key(panel: panel, number: key)]?.presentation
    }
    @discardableResult
    mutating func complete(_ raw: HomeJSON, request: HomeKeypadActivationRequest, context expected: Context) throws -> Bool {
        let key = Key(panel: request.keypad, number: request.key)
        guard context == expected, var entry = entries[key], entry.request == request, !entry.replyFinished else { return false }
        let receipt = try HomeKeypadActivationReceipt(raw, request: request, boot: expected.boot, endpoint: entry.endpoint)
        entry.handled = true; entry.replyFinished = true; entries[key] = entry
        switch receipt {
        case .unknown:
            if entry.latest?.requestID != request.requestID.uuidString && !entry.cleared {
                entry.presentation = HomeKeypadActionPresentation(phase: .uncertain); entries[key] = entry
            } else if entry.latest == nil && !entry.cleared {
                entry.presentation = HomeKeypadActionPresentation(phase: .uncertain); entries[key] = entry
            }
        case .accepted(let indication):
            if !merge(indication, panel: request.keypad, key: request.key, endpoint: entry.endpoint, context: expected),
               let latest = entries[key]?.latest, !entry.cleared {
                entries[key]?.presentation = latest.status == .retired ? nil : HomeKeypadActionPresentation(latest)
            }
        }
        return true
    }
    /// Indication ordering is independent of the raw physical state cursor.
    @discardableResult
    mutating func merge(_ indication: HomeKeypadActionIndication, panel: String, key number: Int, endpoint: String, context expected: Context) -> Bool {
        guard context == expected else { return false }
        let key = Key(panel: panel, number: number)
        var entry = entries[key] ?? Entry(request: nil, endpoint: endpoint, handled: true)
        guard entry.endpoint == endpoint, !entry.retiredRequests.contains(indication.requestID) else { return false }
        if let latest = entry.latest {
            guard indication.operationSequence >= latest.operationSequence else { return false }
            if indication.operationSequence == latest.operationSequence {
                let colorResolved = indication.status == latest.status && [.pending, .written].contains(indication.status)
                    && latest.color == nil && indication.color != nil
                guard !entry.cleared, indication.requestID == latest.requestID,
                      indication.progress > latest.progress || colorResolved else { return false }
            }
        }
        entry.latest = indication; entry.cleared = false
        let matchingClick = indication.source == .authenticatedClick && entry.request?.requestID.uuidString == indication.requestID
        if entry.request == nil || entry.handled || matchingClick {
            entry.handled = true; entry.presentation = indication.status == .retired ? nil : HomeKeypadActionPresentation(indication)
        }
        entries[key] = entry
        return true
    }
    mutating func clearHint(_ panel: String, key number: Int, context expected: Context) {
        guard context == expected else { return }
        let key = Key(panel: panel, number: number)
        guard var entry = entries[key] else { return }
        if entry.handled, entry.presentation?.phase != .uncertain, entry.presentation?.phase != .rejected { entry.presentation = nil }
        entry.cleared = true; entries[key] = entry
    }
    mutating func retireLocalRequest(_ panel: String, key number: Int) {
        let key = Key(panel: panel, number: number)
        guard var entry = entries[key] else { return }
        if let request = entry.request { entry.retiredRequests.insert(request.requestID.uuidString) }
        entry.request = nil; entry.handled = true; entry.replyFinished = true; entry.presentation = nil; entry.cleared = true
        entries[key] = entry
    }
    @discardableResult
    mutating func fail(_ request: HomeKeypadActivationRequest, context expected: Context, uncertain: Bool) -> Bool {
        let key = Key(panel: request.keypad, number: request.key)
        guard context == expected, var entry = entries[key], entry.request == request, !entry.handled else { return false }
        entry.presentation = HomeKeypadActionPresentation(phase: uncertain ? .uncertain : .rejected)
        entry.handled = true; entry.replyFinished = true; entries[key] = entry
        return true
    }
}
