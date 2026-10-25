import Foundation

public enum HomeAccessoryKind: String, Sendable {
    case light, fan, switchControl, curtain, temperature, humidity, motion, contact, illuminance, battery, sensor
    public var symbol: String {
        switch self {
        case .light: "lightbulb.fill"
        case .fan: "fan.fill"
        case .switchControl: "power"
        case .curtain: "curtains.closed"
        case .temperature: "thermometer.medium"
        case .humidity: "humidity.fill"
        case .motion: "sensor.fill"
        case .contact: "door.left.hand.closed"
        case .illuminance: "sun.max.fill"
        case .battery: "battery.100percent"
        case .sensor: "sensor"
        }
    }
}

/// Presentation consumes observations only. A requested brightness/position
/// does not belong here and cannot turn an unknown tile into an active one.
public struct HomeAccessoryPresentation: Equatable, Sendable {
    public let kind: HomeAccessoryKind
    public let value: HomeKitObservedValue?
    public let brightness: Double?
    public let reachable: Bool
    public init(kind: HomeAccessoryKind, value: HomeKitObservedValue?, brightness: Double? = nil, reachable: Bool = true) {
        self.kind = kind; self.value = value; self.brightness = brightness; self.reachable = reachable
    }
    public var number: Double? {
        guard reachable else { return nil }
        switch value {
        case .number(let value) where value.isFinite: return value
        case .boolean(let value): return value ? 1 : 0
        default: return nil
        }
    }
    public var on: Bool? { number.map { $0 != 0 } }
    public var isActive: Bool {
        guard reachable else { return false }
        switch kind {
        case .light, .fan, .switchControl, .motion: return on == true
        case .curtain: return number.map { $0 > 0 && $0 <= 100 } ?? false
        default: return false
        }
    }
    public var status: String {
        guard reachable else { return "未响应" }
        guard let number else { return "状态未知" }
        switch kind {
        case .light:
            guard on == true else { return "关闭" }
            if let brightness, brightness.isFinite, (0...100).contains(brightness) { return "\(Int(brightness.rounded()))%" }
            return "开启"
        case .fan, .switchControl: return on == true ? "开启" : "关闭"
        case .curtain:
            guard (0...100).contains(number) else { return "状态未知" }
            if number == 0 { return "已关闭" }
            if number == 100 { return "已打开" }
            return "打开 \(Int(number.rounded()))%"
        case .temperature: return number.formatted(.number.precision(.fractionLength(0...1))) + "°"
        case .humidity, .battery: return "\(Int(number.rounded()))%"
        case .motion: return on == true ? "检测到移动" : "未检测到移动"
        case .contact: return on == true ? "打开" : "关闭"
        case .illuminance: return number.formatted(.number.precision(.fractionLength(0...1))) + " lx"
        case .sensor: return number.formatted(.number.precision(.fractionLength(0...1)))
        }
    }
    public static func lightSummary(_ lights: [Self]) -> String {
        guard !lights.isEmpty else { return "暂无灯光" }
        let unknown = lights.filter { $0.on == nil }.count
        let on = lights.filter { $0.on == true }.count
        if on > 0 { return "\(on) 盏开启" + (unknown > 0 ? " · \(unknown) 未知" : "") }
        return unknown > 0 ? "\(unknown) 盏状态未知" : "全部关闭"
    }
}
