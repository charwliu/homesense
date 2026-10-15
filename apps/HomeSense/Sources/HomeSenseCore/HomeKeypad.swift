import Foundation

public enum HomeKeypadLEDColor: String, CaseIterable, Sendable {
    case red, green, off, unknown
    public var title: String {
        switch self { case .red: "红"; case .green: "绿"; case .off: "灭"; case .unknown: "未知" }
    }
}

/// Actual feedback and a successfully written target have separate authority.
public struct HomeKeypadLED: Equatable, Sendable {
    public let observed: HomeKeypadLEDColor
    public let confirmed: Bool
    public let lastWritten: HomeKeypadLEDColor
    public init(observed: HomeKeypadLEDColor = .unknown, confirmed: Bool = false,
                lastWritten: HomeKeypadLEDColor = .unknown) {
        self.observed = observed; self.confirmed = confirmed; self.lastWritten = lastWritten
    }
    public var actual: HomeKeypadLEDColor { confirmed ? observed : .unknown }
    public var displayColor: HomeKeypadLEDColor {
        actual != .unknown ? actual : lastWritten != .unknown ? lastWritten : observed
    }
    public var displayDescription: String {
        if actual != .unknown { return "实测：\(actual.title)" }
        if lastWritten != .unknown { return "最后写出：\(lastWritten.title)" }
        if observed != .unknown { return "历史：\(observed.title)" }
        return "实测：未知"
    }
    public var actualDescription: String { "实测：\(actual.title)" }
    public var historyDescription: String? {
        if lastWritten != .unknown { return "最后写出：\(lastWritten.title)" }
        if !confirmed, observed != .unknown { return "历史反馈：\(observed.title)" }
        return nil
    }
}

public struct HomeKeypadKey: Identifiable, Equatable, Sendable {
    public let number: Int
    public let name: String
    public let led: HomeKeypadLED
    public let canActivate: Bool
    public let actionDescription: String?
    public let endpointID: String?
    public let canWriteLED: Bool
    public let actionScene: String?
    public let actionDevice: String?
    public let motorAction: String?
    public var id: Int { number }
    public init(number: Int, name: String, led: HomeKeypadLED = HomeKeypadLED(),
                canActivate: Bool = false, actionDescription: String? = nil,
                endpointID: String? = nil, canWriteLED: Bool = false, actionScene: String? = nil,
                actionDevice: String? = nil, motorAction: String? = nil) {
        self.number = number; self.name = name; self.led = led
        self.canActivate = canActivate; self.actionDescription = actionDescription
        self.endpointID = endpointID; self.canWriteLED = canWriteLED; self.actionScene = actionScene
        self.actionDevice = actionDevice; self.motorAction = motorAction
    }
}

/// A presentation of one server-owned parent device, never inferred by grouping LEDs.
public struct HomeKeypad: Identifiable, Equatable, Sendable {
    public let id: String
    public let name: String
    public let roomName: String?
    public let moduleID: String
    public let keys: [HomeKeypadKey]
    public let settings: HomeJSON
    public let ledColors: [HomeKeypadLEDColor]
    public init(id: String, name: String, roomName: String?, moduleID: String, keys: [HomeKeypadKey], settings: HomeJSON = .null,
                ledColors: [HomeKeypadLEDColor] = [.red, .green]) throws {
        guard !id.isEmpty, !moduleID.isEmpty,
              keys.allSatisfy({ (1...6).contains($0.number) }),
              Set(keys.map(\.number)).count == keys.count else { throw HomeServerError.invalidResponse }
        self.id = id; self.name = name; self.roomName = roomName; self.moduleID = moduleID
        self.settings = settings
        self.ledColors = ledColors
        self.keys = (1...6).map { number in
            keys.first { $0.number == number } ?? HomeKeypadKey(number: number, name: "按键 \(number)")
        }
    }
    public init(record: HomeRecord, current: Bool = true) throws {
        let raw = record.raw
        guard raw["kind"].text == "Keypad", let module = raw["module_id"].text,
              case .array(let rows) = raw["keys"], rows.count == 6 else { throw HomeServerError.invalidResponse }
        let keys = try rows.map { row -> HomeKeypadKey in
            guard let number = row["number"].integerText.flatMap(Int.init) else { throw HomeServerError.invalidResponse }
            let endpoint = row["endpoint_id"].text
            let configured = row["configured"].flag && endpoint?.isEmpty == false
            let action = row["action"]
            let scene = configured && action["kind"].text == "scene_toggle"
                && ["/api/v1/scene-relative-actions", "/api/v1/scene-actions"].contains(action["path"].text ?? "")
                ? action["scene"].text.flatMap { $0.isEmpty ? nil : $0 } : nil
            let motor = configured && action["kind"].text == "motor"
                && action["path"].text == "/api/v1/device-commands"
                && ["open", "close", "stop"].contains(action["command"]["action"].text ?? "")
                ? action["device"].text.flatMap { $0.isEmpty ? nil : $0 } : nil
            let led = row["led"]
            // Button observations and the parent's quality never prove LED color.
            let measured = current && raw["capabilities"]["led_readback"].flag && led["quality"].text == "Confirmed"
            let color = raw["capabilities"]["led_readback"].flag
                ? HomeKeypadLEDColor(rawValue: led["observed"].text ?? "") ?? .unknown : .unknown
            let written = led["last_written"]
            let last = written["status"].text == "written"
                ? HomeKeypadLEDColor(rawValue: written["color"].text ?? "") ?? .unknown : .unknown
            return HomeKeypadKey(number: number, name: row["label"].text ?? "按键 \(number)",
                led: HomeKeypadLED(observed: color, confirmed: measured, lastWritten: last),
                canActivate: scene != nil || motor != nil,
                actionDescription: scene.map { "执行已审核场景映射：\($0)" } ?? motor.map { "执行已审核窗帘映射：\($0)" },
                endpointID: configured ? endpoint : nil, canWriteLED: configured && row["can_write_led"].flag,
                actionScene: scene, actionDevice: motor, motorAction: motor == nil ? nil : action["command"]["action"].text)
        }
        try self.init(id: record.id, name: record.name, roomName: raw["room"]["name"].text,
                      moduleID: module, keys: keys, settings: raw["settings"],
                      ledColors: raw["capabilities"]["led_colors"].values.compactMap { HomeKeypadLEDColor(rawValue: $0.text ?? "") }.filter { $0 != .unknown })
    }
}
