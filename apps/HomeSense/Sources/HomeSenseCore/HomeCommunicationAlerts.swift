import Foundation

/// A server-pushed communication error is presentation, never a retry request.
public enum HomeCommunicationAlerts {
    public static func message(for devices: [HomeRecord]) -> String? {
        let names = devices.filter {
            $0.raw["communication_error"]["code"].text == "module_feedback_timeout"
                && ($0.raw["communication_error"]["consecutive_cycles"].integerText.flatMap(Int.init) ?? 0) >= 3
        }.map(\.name)
        guard !names.isEmpty else { return nil }
        let visible = names.prefix(3).joined(separator: "、")
        let suffix = names.count > 3 ? "等\(names.count)项" : ""
        return "设备反馈连续多轮超时：\(visible)\(suffix)。服务已延长查询间隔，收到有效反馈后自动恢复。"
    }
}
