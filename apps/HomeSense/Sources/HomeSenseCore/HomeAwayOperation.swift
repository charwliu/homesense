import Foundation

/// Server-owned progress. Admission and physical success are different states;
/// the client never retries or manufactures missing module feedback.
public struct HomeAwayOperation: Equatable, Sendable, Identifiable {
    public let id: String
    public let state: String
    public let rounds: Int
    public let confirmed: Int
    public let total: Int
    public let missing: [String]
    public let reason: String?
    public var failed: Bool { state == "failed" }
    public var message: String {
        switch state {
        case "confirmed": "离家已确认：\(total) 路全部关闭"
        case "failed": (rounds >= 3 ? "离家多次尝试后仍未确认关闭：" : "离家执行超时，仍未确认关闭：") + missing.joined(separator: "；")
        case "cancelled": "离家执行已取消，后续重试已停止"
        case "retrying": "离家正在重试（第 \(rounds)/3 轮），已确认 \(confirmed)/\(total) 路关闭"
        default: "离家正在确认，已确认 \(confirmed)/\(total) 路关闭"
        }
    }
    init(_ raw: HomeJSON, boot: String, revision: String) throws {
        guard let id = raw["operation"].text, id.hasPrefix(boot + "-"), id.utf8.count <= 128,
              raw["boot"].text == boot, raw["revision"].integerText == revision,
              let state = raw["state"].text, ["pending", "confirming", "retrying", "confirmed", "failed", "cancelled"].contains(state),
              let rounds = raw["rounds"].integerText.flatMap(Int.init), (0...3).contains(rounds),
              raw["max_rounds"] == .integer(3),
              let total = raw["total_outputs"].integerText.flatMap(Int.init), (8...1024).contains(total), total % 8 == 0,
              let confirmed = raw["confirmed_outputs"].integerText.flatMap(Int.init), (0...total).contains(confirmed),
              case .array(let rows) = raw["missing"], rows.count <= 128 else { throw HomeServerError.invalidResponse }
        var ids = Set<String>(), missing: [String] = [], count = 0
        for row in rows {
            guard let module = row["module_id"].text, !module.isEmpty, module.utf8.count <= 250, ids.insert(module).inserted,
                  case .array(let channels) = row["channels"], !channels.isEmpty, channels.count <= 8 else { throw HomeServerError.invalidResponse }
            let ports = channels.compactMap { $0.integerText.flatMap(Int.init) }
            guard ports.count == channels.count, Set(ports).count == ports.count, ports.allSatisfy({ (1...8).contains($0) }) else { throw HomeServerError.invalidResponse }
            count += ports.count
            missing.append("\(module) [\(ports.map(String.init).joined(separator: ","))]")
        }
        guard count + confirmed == total, state != "confirmed" || confirmed == total,
              state != "failed" || count > 0 else { throw HomeServerError.invalidResponse }
        self.id = id; self.state = state; self.rounds = rounds; self.confirmed = confirmed; self.total = total
        self.missing = missing; reason = raw["reason"].text
    }
}

/// A pending visual layer, separate from selection and raw load observations.
public struct HomeSceneActionPresentation: Equatable, Sendable {
    public let requestID: String
    public let selected: Bool?
    public var title: String { selected.map { $0 ? "开启待确认" : "关闭待确认" } ?? "执行待确认" }
}
