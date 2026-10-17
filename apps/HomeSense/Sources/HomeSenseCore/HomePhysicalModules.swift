import Foundation

/// The administrator's physical inventory. No model is inferred from a light
/// name, a logical device alias, or a module address.
public struct HomePhysicalModule: Identifiable, Equatable, Sendable {
    public let raw: HomeJSON
    public let gateway: String
    public let address: String
    public let profile: String
    public let channelCount: Int
    public var moduleID: String? { raw["module_id"].text }
    public var id: String { "\(gateway.utf8.count):\(gateway):\(address)" }
    public var modelName: String? {
        switch profile {
        case "r8i8_din_pro_v2_7_7": "R818-DIN"
        case "ad8_din_rs485_v2_2_1": "AD8-DIN"
        case "c4_din_pro_v2_7_7": "C4-DIN"
        default: nil
        }
    }
    public var slots: [HomeJSON] { raw["slots"].values }
    public var supportsQuery: Bool {
        ["whole_module", "individual_channels"].contains(raw["query_mode"].text ?? "")
    }
    public init(_ raw: HomeJSON) throws {
        guard raw.isObject, let gateway = raw["gateway"].text, !gateway.isEmpty,
              let address = raw["address"].text, address.count == 2, UInt8(address, radix: 16) != nil,
              let profile = raw["profile"].text, !profile.isEmpty,
              let channels = raw["channels"].integerText.flatMap(Int.init), (1...255).contains(channels),
              case .array(let slots) = raw["slots"], slots.count == channels else { throw HomeServerError.invalidResponse }
        let numbers = slots.compactMap { $0["channel"].integerText.flatMap(Int.init) }
        guard numbers.count == channels, Set(numbers).count == channels,
              numbers.allSatisfy({ (1...channels).contains($0) }),
              raw["module_id"] == .null || raw["module_id"].text?.isEmpty == false else { throw HomeServerError.invalidResponse }
        self.raw = raw; self.gateway = gateway; self.address = address.uppercased()
        self.profile = profile; self.channelCount = channels
    }
    public func permits(channel: Int, command: HomeJSON) -> Bool {
        guard case .object(let fields) = command, let action = command["action"].text,
              let slot = slots.first(where: { $0["channel"].integerText == String(channel) }),
              raw["enabled"].flag, slot["binding"]["enabled"].flag,
              slot["binding"]["supply_for"].values.isEmpty,
              slot["binding"]["actions"].values.contains(.string(action)) else { return false }
        switch action {
        case "power":
            guard ["r8i8_din_pro_v2_7_7", "ad8_din_rs485_v2_2_1"].contains(profile),
                  Set(fields.keys) == ["action", "on"], case .bool = command["on"] else { return false }
            return true
        case "brightness":
            guard profile == "ad8_din_rs485_v2_2_1", Set(fields.keys) == ["action", "level"],
                  command["level"].text == nil, let level = command["level"].integerText.flatMap(Int.init) else { return false }
            return (0...100).contains(level)
        case "open", "close", "stop": return profile == "c4_din_pro_v2_7_7" && Set(fields.keys) == ["action"] && slot["binding"]["motor_calibrated"].flag
        default: return false
        }
    }
}
