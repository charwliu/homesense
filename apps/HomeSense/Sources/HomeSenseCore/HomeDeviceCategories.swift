import Foundation

/// Navigation over the existing authorized catalog, never a new control scope.
public enum HomeDeviceCategory: String, CaseIterable, Identifiable, Sendable {
    case climate, lights
    public var id: String { rawValue }
    public var title: String { self == .climate ? "温控" : "灯" }
    public var englishTitle: String { self == .climate ? "Climate" : "Lights" }
    public var icon: String { self == .climate ? "thermometer.medium" : "lightbulb" }

    public func devices(from records: [HomeRecord]) -> [HomeRecord] {
        var seen = Set<String>()
        return records.filter { record in
            let matches = switch record.raw["kind"].text {
            case "Motor": self == .climate
            case "Switch", "Dimmer": self == .lights
            default: false
            }
            return matches && !record.id.isEmpty && record.raw["catalog_visible"] != .bool(false)
                && seen.insert(record.id).inserted
        }
    }

    public func summary(devices: [HomeRecord], current: Bool) -> String {
        let rows = self.devices(from: devices)
        guard !rows.isEmpty else { return self == .climate ? "暂无窗帘" : "暂无灯" }
        if self == .climate { return "\(rows.count) 扇窗帘" }
        guard current else { return "\(rows.count) 盏灯 · 状态未同步" }
        var on = 0, unknown = 0
        for row in rows {
            guard row.feedbackIsConfirmed(current: current) else { unknown += 1; continue }
            if case .bool(let value) = row.raw["observed"]["on"] {
                if value { on += 1 }
            } else { unknown += 1 }
        }
        if unknown > 0 { return on > 0 ? "\(on) 盏开启 · \(unknown) 盏待同步" : "\(unknown) 盏待同步" }
        return on > 0 ? "\(on) 盏开启" : "全部关闭"
    }
}

public struct HomeCategoryRoom: Identifiable, Equatable, Sendable {
    public let id: String
    public let name: String
    public let floor: String?
    public let devices: [HomeRecord]
}

public extension HomeDeviceCategory {
    /// Use active room IDs only. Unassigned or non-visible rooms never acquire
    /// a room name from a device label or an unpublished configuration draft.
    func rooms(devices: [HomeRecord], rooms: [HomeRecord]) -> [HomeCategoryRoom] {
        let rows = self.devices(from: devices)
        var sections: [HomeCategoryRoom] = [], seen = Set<String>()
        for room in rooms where !room.id.isEmpty && seen.insert(room.id).inserted {
            let members = rows.filter { $0.roomID == room.id }
            if !members.isEmpty {
                sections.append(HomeCategoryRoom(id: "room:\(room.id)", name: room.name,
                    floor: room.raw["floor"].text, devices: members))
            }
        }
        let unassigned = rows.filter { $0.roomID.map { !seen.contains($0) } ?? true }
        if !unassigned.isEmpty {
            sections.append(HomeCategoryRoom(id: "unassigned", name: "未分配房间", floor: nil, devices: unassigned))
        }
        return sections
    }
}
