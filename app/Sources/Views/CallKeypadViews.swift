import SwiftUI

private struct CallDialKey {
    let symbol: String
    let letters: String
}

private let callDialKeys: [CallDialKey] = [
    .init(symbol: "1", letters: ""),
    .init(symbol: "2", letters: "ABC"),
    .init(symbol: "3", letters: "DEF"),
    .init(symbol: "4", letters: "GHI"),
    .init(symbol: "5", letters: "JKL"),
    .init(symbol: "6", letters: "MNO"),
    .init(symbol: "7", letters: "PQRS"),
    .init(symbol: "8", letters: "TUV"),
    .init(symbol: "9", letters: "WXYZ"),
    .init(symbol: "*", letters: ""),
    .init(symbol: "0", letters: "+"),
    .init(symbol: "#", letters: ""),
]

struct CallDialPad: View {
    @Environment(\.workspaceViewport) private var viewport
    @Binding var number: String
    let canDial: Bool
    let callReady: Bool
    let dialHelp: String
    let onDial: () -> Void

    var body: some View {
        VStack(spacing: viewport.isCompactHeight ? 8 : 14) {
            CallKeyGrid(accessibilityVerb: "输入") { key in
                number.append(key)
            }

            Button(action: onDial) {
                Label("拨打电话", systemImage: "phone.fill")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.proletariusPrimary)
            .controlSize(.large)
            .keyboardShortcut(.defaultAction)
            .disabled(!canDial)
            .help(dialHelp)

            if !callReady {
                Label("设备准备完成后即可拨号", systemImage: "lock.fill")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }
}

struct CallInCallKeypad: View {
    @Environment(\.workspaceViewport) private var viewport
    @Binding var sentDigits: String
    let onSend: (String) -> Void

    var body: some View {
        VStack(spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text("通话按键")
                    .font(.system(size: 14, weight: .semibold))
                Spacer()
                Text(sentDigits.isEmpty ? "等待输入" : String(sentDigits.suffix(18)))
                    .font(.callout.monospacedDigit())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }

            CallKeyGrid(accessibilityVerb: "发送通话按键") { key in
                sentDigits.append(key)
                onSend(key)
            }
        }
        .padding(viewport.isCompactHeight ? 12 : 16)
        .background(WorkspaceColors.inset, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .accessibilityElement(children: .contain)
    }
}

private struct CallKeyGrid: View {
    @Environment(\.workspaceViewport) private var viewport
    let accessibilityVerb: String
    let onKey: (String) -> Void

    var body: some View {
        Grid(horizontalSpacing: viewport.isCompactHeight ? 7 : 10, verticalSpacing: viewport.isVeryShort ? 6 : (viewport.isCompactHeight ? 8 : 10)) {
            ForEach(0..<4, id: \.self) { row in
                GridRow {
                    ForEach(0..<3, id: \.self) { column in
                        let key = callDialKeys[row * 3 + column]
                        Button {
                            onKey(key.symbol)
                        } label: {
                            VStack(spacing: 2) {
                                Text(key.symbol)
                                    .font(.system(size: viewport.isCompactHeight ? 20 : 23, weight: .medium, design: .rounded).monospacedDigit())
                                Text(key.letters.isEmpty ? " " : key.letters)
                                    .font(.system(size: 8, weight: .medium))
                                    .foregroundStyle(.secondary)
                                    .tracking(0.7)
                            }
                            .frame(maxWidth: .infinity, minHeight: viewport.isVeryShort ? 36 : (viewport.isCompactHeight ? 42 : 48))
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(CallKeyButtonStyle())
                        .accessibilityLabel("\(accessibilityVerb) \(key.symbol)")
                    }
                }
            }
        }
    }
}

private struct CallKeyButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(WorkspaceColors.accent)
            .background(
                WorkspaceColors.accent.opacity(configuration.isPressed ? 0.16 : 0.06),
                in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(WorkspaceColors.accent.opacity(configuration.isPressed ? 0.28 : 0.12), lineWidth: 0.8))
    }
}
