import Foundation

/// Scene membership is server-authored; current values come from the existing
/// pushed device directory. Opening a scene neither infers room membership nor
/// asks the hardware for another sample.
public struct HomeSceneLightState: Identifiable, Equatable, Sendable {
    public let id: String
    public let name: String
    public let isOn: Bool?
    public let isConfirmed: Bool
    public let status: String

    public init(id: String, name: String, device: HomeRecord?, current: Bool) {
        self.id = id
        self.name = device?.name ?? name
        guard let device else {
            isOn = nil; isConfirmed = false; status = "等待状态同步"; return
        }
        let raw = device.raw
        isConfirmed = device.feedbackIsConfirmed(current: current)
        let on: Bool? = if case .bool(let value) = raw["observed"]["on"] { value } else { nil }
        isOn = isConfirmed ? on : nil
        let level = raw["observed"]["level"].integerText.flatMap(Int.init).flatMap { (0...100).contains($0) ? $0 : nil }
        let value: String = if on == false { level == 0 ? "关闭 · 0%" : "关闭" }
            else if let level { "\(level)%" }
            else if on == true { "开启" } else { "状态未知" }
        var text = isConfirmed ? "实测 · \(value)" : "上次 · \(value)"
        if !current { text = "离线 · 上次\(value)" }
        else if let reason = device.targetAvailability.description { text = "\(reason) · 上次\(value)" }
        let pending = raw["state_authority"] == .string("pending_target") || raw["quality"] == .string("Pending")
            || (raw["pending"] != .null && raw["pending"] != .bool(false))
        if pending { text += " · 目标待确认" }
        status = text
    }
}

public extension HomeRecord {
    var sceneLightMembershipAvailable: Bool {
        if case .array = raw["lights"] { true } else { false }
    }

    /// A partial grant remains partial; never fill missing members from a room,
    /// scene title, draft configuration, or another scene's light footprint.
    func sceneLightStates(devices: [HomeRecord], current: Bool) -> [HomeSceneLightState] {
        let records = Dictionary(devices.map { ($0.id, $0) }, uniquingKeysWith: { _, latest in latest })
        var seen = Set<String>()
        return raw["lights"].values.compactMap { member in
            guard let id = member["id"].text, !id.isEmpty, seen.insert(id).inserted else { return nil }
            let device = records[id]
            if let kind = device?.raw["kind"].text, kind != "Switch" && kind != "Dimmer" { return nil }
            return HomeSceneLightState(id: id, name: member["name"].text ?? "灯光", device: device, current: current)
        }
    }
}
