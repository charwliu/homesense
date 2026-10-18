import Foundation

/// Position-based taps use ten-percent stops; dragging keeps its finer scale.
public enum HomeBrightnessInteraction {
    public static func tappedLevel(verticalPosition: Double, height: Double) -> Double? {
        guard verticalPosition.isFinite, height.isFinite, height > 0 else { return nil }
        let fraction = min(1, max(0, verticalPosition / height))
        return ((1 - fraction) * 10).rounded() * 10
    }
}
