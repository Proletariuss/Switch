import AppKit
import ImageIO
import SwiftUI

// MARK: - Proletarius visual system

enum ProletariusLayout {
    static let pageInset: CGFloat = 20
    static let sectionSpacing: CGFloat = 16
    static let cardPadding: CGFloat = 16
    static let cardRadius: CGFloat = 14
    static let compactRadius: CGFloat = 9
    static let minimumPanelWidth: CGFloat = 340
    static let sidebarIconFrame: CGFloat = 20
    static let inlineIconFrame: CGFloat = 16
    static let controlSpacing: CGFloat = 8
    static let rowSpacing: CGFloat = 10
    static let controlHeight: CGFloat = 32
    static let toolbarButtonFrame: CGFloat = 28
}

enum WorkspaceHeaderMetrics {
    static let iconFrame: CGFloat = 40
}

/// New presentation is opt-in. Unstyled standalone surfaces keep the established
/// surfaces, controls, dimensions and colors through the legacy default.
enum WorkspaceVisualStyle {
    case legacy
    case nekoko
}

private struct WorkspaceVisualStyleKey: EnvironmentKey {
    static let defaultValue = WorkspaceVisualStyle.legacy
}

private struct WorkspaceViewportKey: EnvironmentKey {
    static let defaultValue = WorkspaceViewport()
}

extension EnvironmentValues {
    var workspaceViewport: WorkspaceViewport {
        get { self[WorkspaceViewportKey.self] }
        set { self[WorkspaceViewportKey.self] = newValue }
    }

    var workspaceVisualStyle: WorkspaceVisualStyle {
        get { self[WorkspaceVisualStyleKey.self] }
        set { self[WorkspaceVisualStyleKey.self] = newValue }
    }
}

enum WorkspaceColors {
    static let accent = adaptive(light: 0x2563EB, dark: 0x60A5FA)
    // White button text needs a darker blue even in dark appearance.
    static let primaryFill = adaptive(light: 0x2563EB, dark: 0x2563EB)
    static let canvas = adaptive(light: 0xF6F8FC, dark: 0x101722)
    static let surface = adaptive(light: 0xFFFFFF, dark: 0x182231)
    static let inset = adaptive(light: 0xEFF6FC, dark: 0x182230)
    static let sidebar = adaptive(light: 0xF8FAFD, dark: 0x18212E)
    static let outline = adaptive(light: 0xDEE8F2, dark: 0x344256)

    private static func adaptive(light: UInt32, dark: UInt32) -> Color {
        Color(nsColor: NSColor(name: nil) { appearance in
            let value = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? dark : light
            return NSColor(
                red: CGFloat((value >> 16) & 0xff) / 255,
                green: CGFloat((value >> 8) & 0xff) / 255,
                blue: CGFloat(value & 0xff) / 255,
                alpha: 1)
        })
    }
}

/// Vector artwork follows the system's text scaling and appearance. No
/// downloaded icon set or fixed-resolution illustration is needed.
struct WorkspaceSymbol: View {
    let systemName: String
    let tint: Color
    var size: CGFloat = 40

    var body: some View {
        Image(systemName: systemName)
            .font(.system(size: size * 0.48, weight: .medium))
            .symbolRenderingMode(.hierarchical)
            .foregroundStyle(tint)
            .frame(width: size, height: size)
            .background(tint.opacity(0.09), in: RoundedRectangle(cornerRadius: size * 0.28))
            .accessibilityHidden(true)
    }
}

struct WorkspaceHero: View {
    @Environment(\.workspaceViewport) private var viewport
    let title: String
    let subtitle: String
    let symbol: String
    var eyebrow: String = "WORKSPACE"

    var body: some View {
        HStack(alignment: .center, spacing: viewport.isCompactHeight ? 12 : 18) {
            WorkspaceSymbol(systemName: symbol, tint: WorkspaceColors.accent,
                            size: WorkspaceHeaderMetrics.iconFrame)
            VStack(alignment: .leading, spacing: viewport.isCompactHeight ? 3 : 6) {
                Text(eyebrow)
                    .font(.system(size: 10, weight: .semibold, design: .rounded))
                    .tracking(1.8)
                    .foregroundStyle(WorkspaceColors.accent)
                Text(title)
                    .font(.system(size: viewport.isCompactHeight ? 21 : 26, weight: .bold))
                    .fixedSize(horizontal: false, vertical: true)
                Text(subtitle)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(.vertical, viewport.isCompactHeight ? 4 : 10)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct WorkspaceSectionHeader: View {
    let title: String
    var detail: String? = nil

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.system(size: 16, weight: .semibold))
            if let detail, !detail.isEmpty {
                Text(detail).font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct WorkspaceCardModifier: ViewModifier {
    @Environment(\.workspaceVisualStyle) private var style
    @Environment(\.colorScheme) private var colorScheme
    let tint: Color?
    let cornerRadius: CGFloat

    func body(content: Content) -> some View {
        if style == .nekoko {
            let radius = cornerRadius == ProletariusLayout.cardRadius ? 14 : cornerRadius
            let shape = RoundedRectangle(cornerRadius: radius, style: .continuous)
            content
                .background(shape.fill(WorkspaceColors.surface))
                .overlay(shape.strokeBorder(WorkspaceColors.outline, lineWidth: 0.8))
                .shadow(color: .black.opacity(colorScheme == .dark ? 0.06 : 0.015), radius: 6, y: 2)
        } else if let tint {
            content
                .background(
                    RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                        .fill(Color(nsColor: .controlBackgroundColor))
                        .overlay(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous).fill(tint.opacity(0.07)))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                        .stroke(tint.opacity(0.22), lineWidth: 0.5)
                )
        } else {
            content
                .background(
                    RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                        .fill(Color(nsColor: .controlBackgroundColor))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                        .stroke(Color(nsColor: .separatorColor).opacity(0.7), lineWidth: 0.5)
                )
        }
    }
}

struct WorkspaceTextFieldStyle: TextFieldStyle {
    @Environment(\.controlSize) private var controlSize
    @Environment(\.isFocused) private var isFocused

    func _body(configuration: TextField<Self._Label>) -> some View {
        configuration
            .textFieldStyle(.plain)
            .font(.system(size: controlSize == .small ? 12 : 13))
            .padding(.horizontal, 10)
            .padding(.vertical, controlSize == .small ? 6 : 9)
            .background(WorkspaceColors.inset, in: RoundedRectangle(cornerRadius: 9))
            .overlay(RoundedRectangle(cornerRadius: 9).strokeBorder(
                isFocused ? WorkspaceColors.accent : WorkspaceColors.outline,
                lineWidth: isFocused ? 1.2 : 0.8))
    }
}

extension TextFieldStyle where Self == WorkspaceTextFieldStyle {
    static var workspace: WorkspaceTextFieldStyle { .init() }
}

extension View {
    func workspaceCard(tint: Color? = nil) -> some View {
        modifier(WorkspaceCardModifier(tint: tint, cornerRadius: ProletariusLayout.cardRadius))
    }

    /// Supply actual detail-area dimensions without adding a second Store or
    /// tying presentation changes to device/service lifecycle operations.
    func workspaceAdaptiveLayout(enabled: Bool = true) -> some View {
        modifier(WorkspaceViewportModifier(enabled: enabled))
    }
}

private struct WorkspaceViewportModifier: ViewModifier {
    let enabled: Bool

    @ViewBuilder
    func body(content: Content) -> some View {
        if enabled {
            GeometryReader { geometry in
                content
                    .environment(\.workspaceViewport, WorkspaceViewport(
                        width: max(0, geometry.size.width), height: max(0, geometry.size.height)))
                    .frame(width: geometry.size.width, height: geometry.size.height, alignment: .topLeading)
            }
        } else {
            content
        }
    }
}

private struct CopyableInformationModifier: ViewModifier {
    @EnvironmentObject private var store: DashboardStore
    @State private var copied = false
    let value: String
    let enabled: Bool

    @ViewBuilder
    func body(content: Content) -> some View {
        if enabled, !value.isEmpty, value != "-" {
            // Keep the original field as the layout node. Neither Button nor
            // ZStack may wrap arbitrary row content here: both can participate
            // in recursive intrinsic-size measurement in NavigationSplitView.
            Group {
                if copied {
                    content.foregroundStyle(.green)
                } else {
                    content
                }
            }
                .contentShape(Rectangle())
                .textSelection(.disabled)
                .onTapGesture {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(value, forType: .string)
                    copied = true
                    store.notifyCopied()
                    Task { @MainActor in
                        try? await Task.sleep(nanoseconds: 1_500_000_000)
                        copied = false
                    }
                }
                .help("点击复制此字段")
        } else {
            content
                .help(enabled ? "无可复制内容" : "关闭隐私遮罩后可复制")
        }
    }
}

extension View {
    /// Applies the same copy interaction to identity, network, SMS and call
    /// fields without changing their established visual style.
    func copyableInformation(_ value: String, enabled: Bool = true) -> some View {
        modifier(CopyableInformationModifier(value: value, enabled: enabled))
    }

    /// Apply macOS 15+ window toolbar chrome only where the host supports it.
    @ViewBuilder
    func compatibleWindowToolbarBackgroundHidden() -> some View {
        if #available(macOS 15.0, *) {
            toolbarBackgroundVisibility(.hidden, for: .windowToolbar)
        } else {
            toolbarBackground(.hidden, for: .windowToolbar)
        }
    }
}

extension ToolbarContent {
    /// Newer macOS releases expose shared toolbar backgrounds and spacers;
    /// older systems keep the same actions with native default spacing.
    @ToolbarContentBuilder
    func compatibleSharedToolbarBackgroundHidden() -> some ToolbarContent {
        if #available(macOS 26.0, *) {
            sharedBackgroundVisibility(.hidden)
        } else {
            self
        }
    }
}

/// Semantic accents: one identity per workspace, with green/orange/red reserved
/// for success, attention and failure states throughout the app.
enum ProletariusPalette {
    static let action = Color.blue
    static let activity = Color.blue
    static let module = Color.orange
    static let reader = Color.teal
    static let messages = Color.indigo
    static let calls = Color.green
    static let profiles = Color.purple
    static let diagnostics = Color.cyan
    static let about = Color.pink
}

private struct WorkspaceAccentKey: EnvironmentKey {
    static let defaultValue = ProletariusPalette.activity
}

extension EnvironmentValues {
    var workspaceAccent: Color {
        get { self[WorkspaceAccentKey.self] }
        set { self[WorkspaceAccentKey.self] = newValue }
    }
}

struct ProletariusButtonStyle: ButtonStyle {
    var prominent = false
    @Environment(\.workspaceAccent) private var accent
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.controlSize) private var controlSize
    @Environment(\.workspaceVisualStyle) private var visualStyle
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var hovered = false

    @ViewBuilder
    func makeBody(configuration: Configuration) -> some View {
        if visualStyle == .nekoko {
            workspaceBody(configuration: configuration)
        } else {
            legacyBody(configuration: configuration)
        }
    }

    private func legacyBody(configuration: Configuration) -> some View {
        let destructive = configuration.role == .destructive
        let tint: Color = destructive ? .red : ProletariusPalette.action
        let compact = controlSize == .mini || controlSize == .small
        let height: CGFloat = compact ? 28 : ProletariusLayout.controlHeight
        let shape = RoundedRectangle(cornerRadius: 8, style: .continuous)
        return configuration.label
            .font(.system(size: compact ? 12 : 13, weight: .medium))
            .padding(.horizontal, compact ? 9 : 12)
            .frame(minWidth: height, minHeight: height)
            .foregroundStyle(prominent ? Color.white : (destructive ? Color.red : Color.primary))
            .background {
                shape.fill(prominent ? tint : Color(nsColor: .controlBackgroundColor))
                shape.fill(tint.opacity(prominent ? 0 : (hovered ? 0.14 : 0.065)))
                if configuration.isPressed { shape.fill(Color.primary.opacity(0.10)) }
            }
            .overlay(shape.strokeBorder(tint.opacity(prominent ? 0 : (colorScheme == .dark ? 0.38 : 0.23)), lineWidth: 0.7))
            .contentShape(shape)
            .opacity(isEnabled ? 1 : 0.42)
            .onHover { hovered = $0 }
    }

    private func workspaceBody(configuration: Configuration) -> some View {
        let destructive = configuration.role == .destructive
        let tint: Color = destructive ? .red : WorkspaceColors.accent
        let compact = controlSize == .mini || controlSize == .small
        let shape = RoundedRectangle(cornerRadius: 8, style: .continuous)
        return configuration.label
            .font(.system(size: compact ? 12 : 13, weight: .semibold))
            .padding(.horizontal, compact ? 9 : 12)
            .frame(minWidth: compact ? 28 : 32, minHeight: compact ? 28 : 32)
            .foregroundStyle(prominent ? Color.white : tint)
            .background(shape.fill(prominent ? (destructive ? Color.red : WorkspaceColors.primaryFill) : tint.opacity(hovered ? 0.13 : 0.07)))
            .overlay(shape.strokeBorder(tint.opacity(prominent ? 0 : 0.14), lineWidth: 0.8))
            .brightness(configuration.isPressed ? -0.04 : 0)
            .contentShape(shape)
            .opacity(isEnabled ? 1 : 0.42)
            .onHover { hovered = $0 }
            .animation(reduceMotion ? nil : ProletariusMotion.quick, value: hovered)
    }
}

extension ButtonStyle where Self == ProletariusButtonStyle {
    static var proletarius: ProletariusButtonStyle { .init() }
    static var proletariusPrimary: ProletariusButtonStyle { .init(prominent: true) }
}

/// Top toolbar actions deliberately have no plate, border or capsule. The
/// invisible frame preserves a comfortable pointer target while the symbol is
/// the only visible element.
struct ProletariusToolbarButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 15, weight: .semibold))
            .frame(width: ProletariusLayout.toolbarButtonFrame,
                   height: ProletariusLayout.toolbarButtonFrame)
            .contentShape(Rectangle())
            .opacity(isEnabled ? (configuration.isPressed ? 0.48 : 1) : 0.34)
            .scaleEffect(configuration.isPressed ? 0.94 : 1)
            .animation(ProletariusMotion.quick, value: configuration.isPressed)
    }
}

extension ButtonStyle where Self == ProletariusToolbarButtonStyle {
    static var proletariusToolbar: ProletariusToolbarButtonStyle { .init() }
}

enum ProletariusIconStyle {
    static let sidebar = Font.system(size: 14, weight: .medium)
    static let inline = Font.callout.weight(.semibold)
    static let status = Font.caption2.weight(.semibold)
    static let emptyState = Font.system(size: 32, weight: .regular)
}

enum ProletariusMotion {
    static let quick = Animation.easeOut(duration: 0.12)
    static let standard = Animation.easeInOut(duration: 0.2)
    static let stateChange = Animation.easeInOut(duration: 0.25)
}

/// 主界面保留品牌 GIF 动效；侧栏使用透明车环图形。
struct SwitchBrandLogo: View {
    enum Placement {
        case notification
        case compactAction
        case sidebar

        fileprivate var size: CGSize {
            switch self {
            case .notification: return CGSize(width: 32, height: 24)
            case .compactAction: return CGSize(width: 36, height: 27)
            case .sidebar: return CGSize(width: 64, height: 32)
            }
        }

        fileprivate var usesTransparentCarLoop: Bool {
            if case .sidebar = self { return true }
            return false
        }
    }

    let placement: Placement

    @ViewBuilder
    var body: some View {
        if placement.usesTransparentCarLoop {
            Image("NWaitLoop")
                .resizable()
                .interpolation(.high)
                .scaledToFit()
                .frame(width: placement.size.width, height: placement.size.height)
                .accessibilityHidden(true)
        } else {
            AnimatedSwitchBrandImage()
                .frame(width: placement.size.width, height: placement.size.height)
                .accessibilityHidden(true)
        }
    }
}

private struct AnimatedSwitchBrandImage: View {
    private let frames = SwitchBrandGIF.frames

    var body: some View {
        TimelineView(.animation(minimumInterval: 0.08, paused: frames.count < 2)) { timeline in
            if let frame = SwitchBrandGIF.frame(at: timeline.date) {
                Image(decorative: frame, scale: 1, orientation: .up)
                    .resizable()
                    .interpolation(.high)
                    .scaledToFit()
            } else {
                Color.clear
            }
        }
        .aspectRatio(4.0 / 3.0, contentMode: .fit)
    }
}

private enum SwitchBrandGIF {
    struct Frame {
        let image: CGImage
        let duration: TimeInterval
    }

    static let frames: [Frame] = loadFrames()
    private static let totalDuration = frames.reduce(0) { $0 + $1.duration }

    static func frame(at date: Date) -> CGImage? {
        guard !frames.isEmpty, totalDuration > 0 else { return nil }
        let elapsed = date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: totalDuration)
        var boundary: TimeInterval = 0
        for frame in frames {
            boundary += frame.duration
            if elapsed < boundary { return frame.image }
        }
        return frames.last?.image
    }

    private static func loadFrames() -> [Frame] {
        let url = Bundle.main.url(forResource: "SwitchBrand", withExtension: "gif", subdirectory: "Brand")
            ?? Bundle.main.url(forResource: "SwitchBrand", withExtension: "gif")
        guard let url,
              let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { return [] }

        return (0..<CGImageSourceGetCount(source)).compactMap { index in
            guard let image = CGImageSourceCreateImageAtIndex(source, index, nil) else { return nil }
            let properties = CGImageSourceCopyPropertiesAtIndex(source, index, nil) as? [CFString: Any]
            let gif = properties?[kCGImagePropertyGIFDictionary] as? [CFString: Any]
            let unclamped = (gif?[kCGImagePropertyGIFUnclampedDelayTime] as? NSNumber)?.doubleValue ?? 0
            let clamped = (gif?[kCGImagePropertyGIFDelayTime] as? NSNumber)?.doubleValue ?? 0.1
            let delay = unclamped > 0 ? unclamped : clamped
            return Frame(image: image, duration: max(delay, 0.05))
        }
    }
}

/// Compact filled Tampermonkey-style mark. Blue means protected; gray means
/// revealed. The eye holes are genuinely transparent rather than a toolbar plate.
struct PrivacyMaskControlMark: View {
    let isMasked: Bool

    private var tint: Color {
        isMasked ? WorkspaceColors.accent : Color(nsColor: .tertiaryLabelColor)
    }

    var body: some View {
        Canvas { context, size in
            // Match the supplied Tampermonkey mark: the face fills nearly the
            // whole square, with the eye cutouts low on the face.
            let side = min(size.width, size.height) * 0.98
            let origin = CGPoint(x: (size.width - side) / 2, y: (size.height - side) / 2)
            let face = CGRect(x: origin.x, y: origin.y, width: side, height: side)
            var mark = Path(roundedRect: face, cornerSize: CGSize(width: side * 0.24, height: side * 0.24))
            let eye = side * 0.40
            let eyeY = face.minY + side * 0.75
            for centerX in [face.minX + side * 0.27, face.minX + side * 0.75] {
                mark.addEllipse(in: CGRect(x: centerX - eye / 2, y: eyeY - eye / 2, width: eye, height: eye))
            }
            context.fill(mark, with: .color(tint), style: FillStyle(eoFill: true, antialiased: true))
        }
        .frame(width: 23, height: 23)
        .contentTransition(.opacity)
        .animation(ProletariusMotion.quick, value: isMasked)
        .accessibilityHidden(true)
    }
}


struct ProletariusPageBackground: ViewModifier {
    @Environment(\.workspaceAccent) private var accent
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.workspaceVisualStyle) private var visualStyle

    func body(content: Content) -> some View {
        content
            .background {
                if visualStyle == .nekoko {
                    WorkspaceColors.canvas
                } else {
                    Color(nsColor: .windowBackgroundColor)
                    LinearGradient(colors: [accent.opacity(colorScheme == .dark ? 0.10 : 0.055), .clear], startPoint: .topLeading, endPoint: .bottomTrailing)
                }
            }
    }
}

// MARK: - macOS 版本兼容包装

extension View {
    /// `toolbar(removing: .sidebarToggle)` 仅 macOS 26+ 可用。
    /// 低版本无此 API，直接返回原视图（侧栏切换按钮照常显示）。
    @ViewBuilder
    func compatibleToolbarRemovingSidebarToggle() -> some View {
        if #available(macOS 26.0, *) {
            self.toolbar(removing: .sidebarToggle)
        } else {
            self
        }
    }
}

struct ProletariusSectionTitle: View {
    @Environment(\.workspaceAccent) private var accent
    let title: String
    var detail: String? = nil
    var icon: String? = nil

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: ProletariusLayout.controlSpacing) {
            if let icon {
                Image(systemName: icon)
                    .font(ProletariusIconStyle.inline)
                    .frame(width: ProletariusLayout.inlineIconFrame)
                    .foregroundStyle(accent)
                    .accessibilityHidden(true)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.headline)
                if let detail, !detail.isEmpty {
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct ProletariusStatusBadge: View {
    let title: String
    let color: Color
    var systemImage: String? = nil

    var body: some View {
        HStack(spacing: 5) {
            if let systemImage {
                Image(systemName: systemImage)
                    .font(ProletariusIconStyle.status)
                    .accessibilityHidden(true)
            } else {
                Circle().frame(width: 6, height: 6)
            }
            Text(title)
                .font(.caption.weight(.medium))
                .lineLimit(1)
        }
        .foregroundStyle(color)
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(Capsule().fill(color.opacity(0.11)))
        .overlay(Capsule().stroke(color.opacity(0.18), lineWidth: 0.5))
        .accessibilityElement(children: .combine)
    }
}

extension View {
    /// 集中封装状态变化处理，兼容 macOS 13+。
    /// `onChange(of:initial:_:)` 双参数闭包为 macOS 14+ API；
    /// 低版本回退到 `onChange(of:perform:)` 单参数闭包。
    func compatibleOnChange<Value: Equatable>(
        of value: Value,
        perform action: @escaping (Value) -> Void
    ) -> some View {
        modifier(CompatibleOnChangeModifier(value: value, action: action))
    }

    func proletariusPageBackground() -> some View {
        modifier(ProletariusPageBackground())
    }

    /// 主内容面板始终属于内容层。Liquid Glass 由系统侧边栏、工具栏和控件提供，
    /// 避免在内容区叠加玻璃导致层级混乱。
    @ViewBuilder
    func glassCard(cornerRadius: CGFloat = ProletariusLayout.cardRadius) -> some View {
        modifier(WorkspaceCardModifier(tint: nil, cornerRadius: cornerRadius))
    }

    /// 带轻量状态色的主面板，强调状态但不牺牲深浅色可读性。
    @ViewBuilder
    func glassCardTinted(_ tint: Color, cornerRadius: CGFloat = ProletariusLayout.cardRadius) -> some View {
        modifier(WorkspaceCardModifier(tint: tint, cornerRadius: cornerRadius))
    }

    /// 紧凑的次级表面，用于行内工具和嵌套信息。
    @ViewBuilder
    func glassPill(cornerRadius: CGFloat = ProletariusLayout.compactRadius) -> some View {
        background(
            RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                .fill(Color(nsColor: .underPageBackgroundColor))
        )
        .overlay(
            RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                .stroke(Color(nsColor: .separatorColor).opacity(0.6), lineWidth: 0.5)
        )
    }
}

struct ProletariusEmptyState: View {
    @Environment(\.workspaceAccent) private var accent
    let title: String
    let detail: String
    let systemImage: String
    var actionTitle: String? = nil
    var action: (() -> Void)? = nil

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: systemImage)
                .font(ProletariusIconStyle.emptyState)
                .foregroundStyle(accent)
                .frame(width: 66, height: 66)
                .background(accent.opacity(0.10), in: RoundedRectangle(cornerRadius: 18))
                .accessibilityHidden(true)
            Text(title)
                .font(.headline)
            Text(detail)
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 420)
            if let actionTitle, let action {
                Button(actionTitle, action: action)
                    .buttonStyle(.borderedProminent)
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .contain)
    }
}

// MARK: - compatibleOnChange 实现

private struct CompatibleOnChangeModifier<Value: Equatable>: ViewModifier {
    let value: Value
    let action: (Value) -> Void

    func body(content: Content) -> some View {
        if #available(macOS 14.0, *) {
            content.onChange(of: value) { _, newValue in
                action(newValue)
            }
        } else {
            content.onChange(of: value, perform: action)
        }
    }
}
