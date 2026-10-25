import Foundation

/// Position-based taps use ten-percent stops; dragging keeps its finer scale.
public enum HomeBrightnessInteraction {
    public enum Axis: Equatable, Sendable { case vertical, horizontal }
    /// One touch becomes either a position-based tap or a relative fine drag.
    /// Latch dragging so returning to the starting point cannot become a tap.
    public struct Touch: Equatable, Sendable {
        public let startLevel: Double
        public let axis: Axis
        public private(set) var hasMoved = false
        public private(set) var isDragging = false

        public init(level: Double, axis: Axis = .vertical) {
            startLevel = level; self.axis = axis
        }

        public mutating func move(horizontal: Double, vertical: Double, extent: Double) -> Double? {
            guard startLevel.isFinite, horizontal.isFinite, vertical.isFinite,
                extent.isFinite, extent > 0 else { return nil }
            hasMoved = hasMoved || abs(horizontal) >= 3 || abs(vertical) >= 3
            let translation = axis == .vertical ? -vertical : horizontal
            guard isDragging || abs(translation) >= 3 else { return nil }
            isDragging = true
            return min(100, max(0, startLevel + translation / extent * 100)).rounded()
        }
    }

    public static func tappedLevel(verticalPosition: Double, height: Double) -> Double? {
        tappedLevel(position: verticalPosition, extent: height, axis: .vertical)
    }

    public static func tappedLevel(position: Double, extent: Double, axis: Axis) -> Double? {
        guard position.isFinite, extent.isFinite, extent > 0 else { return nil }
        let fraction = min(1, max(0, position / extent))
        return ((axis == .vertical ? 1 - fraction : fraction) * 10).rounded() * 10
    }
}
