import Foundation

/// Target-local health from the server. Absence retains the older API contract;
/// present but invalid metadata must not enable a command or confirm feedback.
public struct HomeTargetAvailability: Equatable, Sendable {
    public let isAvailable: Bool
    public let description: String?

    public init(_ raw: HomeJSON) {
        var reasons: [String] = []
        func inspect(_ view: HomeJSON) {
            guard case .object = view,
                  let gateway = view["gateway"].text, !gateway.isEmpty,
                  let epoch = view["connection_epoch"].text, UInt64(epoch) != nil,
                  let state = view["state"].text else {
                reasons.append("目标可用状态无效"); return
            }
            switch state {
            case "ready": break
            case "offline": reasons.append("网关离线")
            case "synchronizing": reasons.append("网关正在同步")
            default: reasons.append("目标暂不可用")
            }
        }
        if raw["availability"] != .null { inspect(raw["availability"]) }
        if raw["action_gate"] != .null {
            let gate = raw["action_gate"]
            if case .bool(let available) = gate["available"] {
                if !available { reasons.append("目标暂不可用") }
            } else { reasons.append("目标可用状态无效") }
            if gate["required_gateways"] != .null {
                if case .array(let views) = gate["required_gateways"] { views.forEach(inspect) }
                else { reasons.append("目标可用状态无效") }
            }
        }
        isAvailable = reasons.isEmpty
        description = reasons.first
    }
}

public extension HomeRecord {
    var targetAvailability: HomeTargetAvailability { HomeTargetAvailability(raw) }
    var targetAvailable: Bool { targetAvailability.isAvailable }
    func feedbackIsConfirmed(current: Bool) -> Bool {
        current && targetAvailable && raw["quality"].text == "Confirmed"
    }
}
