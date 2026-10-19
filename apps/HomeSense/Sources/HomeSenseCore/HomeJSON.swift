import Foundation

/// Retain unknown configuration fields and exact integer values when deriving
/// drafts. In particular, never route revision tokens through Double.
public enum HomeJSON: Codable, Equatable, Sendable {
    case object([String: HomeJSON]), array([HomeJSON]), string(String)
    case integer(Int64), unsigned(UInt64), decimal(Decimal), bool(Bool), null

    public init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
        if value.decodeNil() { self = .null }
        else if let v = try? value.decode(Bool.self) { self = .bool(v) }
        else if let v = try? value.decode(Int64.self) { self = .integer(v) }
        else if let v = try? value.decode(UInt64.self) { self = .unsigned(v) }
        else if let v = try? value.decode(Decimal.self) { self = .decimal(v) }
        else if let v = try? value.decode(String.self) { self = .string(v) }
        else if let v = try? value.decode([String: HomeJSON].self) { self = .object(v) }
        else { self = .array(try value.decode([HomeJSON].self)) }
    }
    public func encode(to encoder: Encoder) throws {
        var value = encoder.singleValueContainer()
        switch self {
        case .object(let v): try value.encode(v)
        case .array(let v): try value.encode(v)
        case .string(let v): try value.encode(v)
        case .integer(let v): try value.encode(v)
        case .unsigned(let v): try value.encode(v)
        case .decimal(let v): try value.encode(v)
        case .bool(let v): try value.encode(v)
        case .null: try value.encodeNil()
        }
    }
    public subscript(_ key: String) -> HomeJSON {
        get { if case .object(let fields) = self { return fields[key] ?? .null }; return .null }
        set {
            if case .object(var fields) = self { fields[key] = newValue; self = .object(fields) }
        }
    }
    public var text: String? { if case .string(let v) = self { v } else { nil } }
    public var flag: Bool { if case .bool(let v) = self { v } else { false } }
    public var values: [HomeJSON] { if case .array(let v) = self { v } else { [] } }
    public var integerText: String? {
        switch self { case .integer(let v): String(v); case .unsigned(let v): String(v); case .string(let v): v; default: nil }
    }
    public var isObject: Bool { if case .object = self { true } else { false } }
    public func removing(_ keys: [String]) -> HomeJSON {
        guard case .object(var fields) = self else { return self }
        for key in keys { fields.removeValue(forKey: key) }
        return .object(fields)
    }
    public func data(pretty: Bool = false) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = pretty ? [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes] : [.sortedKeys, .withoutEscapingSlashes]
        return try encoder.encode(self)
    }
    public var formatted: String { (try? data(pretty: true)).flatMap { String(data: $0, encoding: .utf8) } ?? "null" }
    public static func parse(_ text: String) throws -> Self {
        guard let bytes = text.data(using: .utf8), bytes.count <= 256 * 1024 else { throw HomeServerError.invalidDocument }
        return try JSONDecoder().decode(Self.self, from: bytes)
    }
    /// Complete saved documents share the server's finite draft budget. Small
    /// inline editors keep the stricter `parse` limit.
    public static func parseConfiguration(_ text: String) throws -> Self {
        guard let bytes = text.data(using: .utf8), bytes.count <= 4 * 1024 * 1024 else { throw HomeServerError.invalidDocument }
        return try JSONDecoder().decode(Self.self, from: bytes)
    }
}

public struct HomeRecord: Identifiable, Equatable, Sendable {
    public let raw: HomeJSON
    public var id: String { raw["id"].text ?? "" }
    public var name: String { raw["name"].text ?? id }
    /// Runtime DTOs contain a room object; configuration metadata stores its ID.
    public var roomID: String? {
        let id = raw["room"]["id"].text ?? raw["room"].text
        return id.flatMap { !$0.isEmpty && $0 == $0.trimmingCharacters(in: .whitespacesAndNewlines) ? $0 : nil }
    }
    public init(_ raw: HomeJSON) { self.raw = raw }
}

/// A virtual scene card is presentation, never a physical keypad or LED source.
public struct HomeScenePanel: Identifiable, Equatable, Sendable {
    public let id: String
    public let title: String
    public let page: Int
    public let pageCount: Int
    public let scenes: [HomeRecord]
}

public enum HomeSceneCardAction: Equatable, Sendable {
    case execute, selection(Bool)
    public var on: Bool? { if case .selection(let value) = self { value } else { nil } }
    public var title: String {
        switch self { case .execute: "执行"; case .selection(true): "开启"; case .selection(false): "关闭" }
    }
}

public extension HomeRecord {
    /// Display-only state. Read-only activity must never become an Off intent.
    var sceneCardConfirmedActive: Bool? {
        if raw["switch_kind"].text == "selection" {
            if case .bool(let selected) = raw["selected"] { return selected }
            return nil
        }
        guard raw["switch_kind"].text == "momentary", raw["activity_kind"].text == "read_only" else { return nil }
        switch raw["activity"].text {
        case "Active": return true
        case "Inactive": return false
        default: return nil
        }
    }

    var sceneCardAction: HomeSceneCardAction {
        guard raw["switch_kind"].text == "selection" else { return .execute }
        // Unknown is an explicit On intent, never evidence of an Off state.
        return .selection(raw["selected"] != .bool(true))
    }
}

public enum HomeServerError: LocalizedError {
    case invalidEndpoint, notAuthenticated, denied, invalidDocument, invalidResponse
    case staleConfiguration, missingAutomation, revisionOverflow, server(String), expiredRequest
    public var errorDescription: String? {
        switch self {
        case .invalidEndpoint: "请输入完整 HTTPS 服务地址，例如 https://192.168.3.2:8444；不含路径、账号或查询参数。"
        case .notAuthenticated: "请连接 HomeSense 服务并输入访问令牌。"
        case .denied: "当前凭证没有配置管理权限。"
        case .invalidDocument: "配置内容无效，请核对名称、ID、引用和 JSON 格式。"
        case .invalidResponse: "服务端响应格式不符合当前 HomeSense API。"
        case .staleConfiguration: "活动配置已变化，请刷新、核对草稿基线并重新预览。"
        case .missingAutomation: "该配置没有自动化文档，不能通过此入口发布。"
        case .revisionOverflow: "配置版本已到达可用整数上限。"
        case .server(let message): "服务端：\(message)"
        case .expiredRequest: "连接已切换，已忽略旧响应。"
        }
    }
}
