import SwiftUI
import HomeSenseCore

struct HomeKeypadCard: View {
    let keypad: HomeKeypad
    let controlsAvailable: Bool
    let activate: (Int) -> Void
    let details: () -> Void
    var isActionAvailable: ((Int) -> Bool)? = nil

    var body: some View {
        GeometryReader { geometry in
            VStack(spacing: 12) {
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 3) {
                        Text(keypad.name).font(.headline).lineLimit(1).minimumScaleFactor(0.7)
                        Text(keypad.roomName ?? "未分配房间").font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                    Spacer(minLength: 6)
                    Button(action: details) { Image(systemName: "info.circle").font(.title3) }
                        .buttonStyle(.plain).accessibilityLabel("\(keypad.name)详情与设置")
                }
                VStack(spacing: 8) {
                    ForEach(0..<3) { row in
                        HStack(spacing: 8) {
                            key(keypad.keys[row * 2], compact: geometry.size.width < 340)
                            key(keypad.keys[row * 2 + 1], compact: geometry.size.width < 340)
                        }.frame(maxHeight: .infinity)
                    }
                }
            }
            .padding(min(18, geometry.size.width * 0.05))
            .frame(width: geometry.size.width, height: geometry.size.height)
        }
        .aspectRatio(1, contentMode: .fit)
        .background(.background, in: RoundedRectangle(cornerRadius: 20))
        .overlay { RoundedRectangle(cornerRadius: 20).strokeBorder(.primary.opacity(0.08)) }
        .accessibilityIdentifier("keypad.\(keypad.id)")
    }

    private func key(_ key: HomeKeypadKey, compact: Bool) -> some View {
        Button { activate(key.number) } label: {
            VStack(alignment: .leading, spacing: compact ? 2 : 4) {
                HStack {
                    Text("\(key.number)").font(.caption.monospacedDigit().weight(.semibold)).foregroundStyle(.secondary)
                    Spacer(minLength: 4)
                    ZStack {
                        Circle().fill(key.led.displayColor == .red ? Color.red : key.led.displayColor == .green ? Color.green : Color.secondary.opacity(0.2))
                        if key.led.displayColor == .unknown { Text("?").font(.system(size: 8, weight: .bold)) }
                        if key.led.displayColor == .off { Circle().strokeBorder(.secondary, lineWidth: 1) }
                    }.frame(width: 11, height: 11).accessibilityHidden(true)
                }
                Text(key.name).font(compact ? .caption.weight(.medium) : .subheadline.weight(.medium)).lineLimit(1).minimumScaleFactor(0.65)
                Text(key.led.displayDescription).font(.caption2).foregroundStyle(.secondary).lineLimit(1).minimumScaleFactor(0.7)
            }
            .padding(compact ? 6 : 10).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
            .background(keyColor(key.led.displayColor), in: RoundedRectangle(cornerRadius: 10))
            .overlay { RoundedRectangle(cornerRadius: 10).strokeBorder(.primary.opacity(key.led.displayColor == .unknown ? 0.12 : 0.04)) }
            .contentShape(RoundedRectangle(cornerRadius: 10))
        }
        .buttonStyle(KeypadPressStyle()).disabled(!controlsAvailable || !key.canActivateCard || isActionAvailable?(key.number) == false)
        .accessibilityLabel("按键 \(key.number)，\(key.name)，\(key.led.actualDescription)，\(key.led.historyDescription ?? "")")
        .accessibilityHint(key.actionDescription ?? "没有已授权的按键动作")
        .accessibilityIdentifier("keypad.\(keypad.id).key.\(key.number)")
    }

    private func keyColor(_ color: HomeKeypadLEDColor) -> Color {
        switch color {
        case .red: .red.opacity(0.23)
        case .green: .green.opacity(0.23)
        case .off: .secondary.opacity(0.16)
        case .unknown: .primary.opacity(0.025)
        }
    }
}

private struct KeypadPressStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(Color.primary)
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .overlay { RoundedRectangle(cornerRadius: 10).strokeBorder(.primary.opacity(configuration.isPressed ? 0.4 : 0), lineWidth: 2) }
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}
