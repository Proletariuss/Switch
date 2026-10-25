import AppKit
import SwiftUI

final class DashboardStore: ObservableObject {
    func notifyCopied() {}
}

@main
private enum WorkspacePresentationTests {
    @MainActor
    static func main() throws {
        _ = NSApplication.shared
        NSApp.setActivationPolicy(.prohibited)
        let output = CommandLine.arguments.dropFirst().first.map(URL.init(fileURLWithPath:))
        var observedViewport: WorkspaceViewport?
        let probe = ViewportProbe(onRead: { observedViewport = $0 })
            .workspaceAdaptiveLayout()
            .frame(width: 688, height: 550)
        _ = try nativeRender(probe, size: CGSize(width: 688, height: 550))
        precondition(observedViewport == WorkspaceViewport(width: 688, height: 550),
                     "all modern details must receive their actual available size")
        observedViewport = nil
        _ = try nativeRender(ViewportProbe(onRead: { observedViewport = $0 })
            .workspaceAdaptiveLayout(enabled: false), size: CGSize(width: 688, height: 550))
        precondition(observedViewport == WorkspaceViewport(), "an explicitly disabled container must not inject viewport changes")
        let hero = WorkspaceHero(title: "读卡器", subtitle: "卡片与连接", symbol: "simcard", eyebrow: "READER").frame(width: 640)
        let regularHero = try render(hero.environment(\.workspaceViewport, WorkspaceViewport(height: 720)))
        let compactHero = try render(hero.environment(\.workspaceViewport, WorkspaceViewport(height: 550)))
        precondition(compactHero.height < regularHero.height, "short-window headings must reclaim space without dropping content")
        for scheme in [ColorScheme.light, .dark] {
            NSApp.appearance = NSAppearance(named: scheme == .dark ? .darkAqua : .aqua)
            let legacy = try pixels(card.environment(\.workspaceVisualStyle, .legacy).environment(\.colorScheme, scheme))
            let restored = try pixels(card.environment(\.workspaceVisualStyle, .legacy)
                .environment(\.workspaceVisualStyle, .nekoko).environment(\.colorScheme, scheme))
            precondition(legacy == restored, "explicit legacy surfaces must override the outer workspace")
            let modern = try pixels(card.environment(\.workspaceVisualStyle, .nekoko).environment(\.colorScheme, scheme))
            precondition(legacy != modern, "modern and legacy rendering must remain independent")
            for masked in [true, false] {
                let toolbar = toolbarFixture(masked: masked)
                let before = try pixels(toolbar.environment(\.workspaceVisualStyle, .legacy).environment(\.colorScheme, scheme))
                let after = try pixels(toolbar.environment(\.workspaceVisualStyle, .nekoko).environment(\.colorScheme, scheme))
                precondition(before == after, "privacy and appearance marks must not inherit page styling")
            }
            if let output {
                let image = try nativeRender(preview.environment(\.workspaceVisualStyle, .nekoko).environment(\.colorScheme, scheme))
                let name = scheme == .dark ? "workspace-dark.png" : "workspace-light.png"
                try NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])!
                    .write(to: output.appendingPathComponent(name))
            }
        }
        print("Presentation boundaries: actual viewport injection, compact headers, legacy isolation and unchanged protected toolbar marks passed in light/dark")
    }

    private static var card: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("任务提交").font(.headline)
            Text("操作状态与任务记录").font(.caption).foregroundStyle(.secondary)
            HStack {
                Button("取消") {}.buttonStyle(.proletarius)
                Button("发送") {}.buttonStyle(.proletariusPrimary)
            }
        }
        .padding(16)
        .frame(width: 300, height: 170)
        .glassCardTinted(.blue)
        .padding(20)
        .frame(width: 350, height: 220)
    }

    private static func toolbarFixture(masked: Bool) -> some View {
        HStack(spacing: 20) {
            Button {} label: { PrivacyMaskControlMark(isMasked: masked) }
                .buttonStyle(.proletariusToolbar)
            Button {} label: {
                Image(systemName: "sun.max.fill")
                    .symbolRenderingMode(.hierarchical).foregroundStyle(.primary)
                    .frame(width: 28, height: 28)
            }
            .buttonStyle(.proletariusToolbar)
        }
        .frame(width: 140, height: 60)
    }

    private static var preview: some View {
        VStack(alignment: .leading, spacing: 20) {
            WorkspaceHero(title: "读卡器与 eSIM", subtitle: "卡片、连接与通信能力，在同一工作区清晰呈现。", symbol: "simcard.2", eyebrow: "DEVICE / READER")
            HStack(alignment: .top, spacing: 18) {
                VStack(alignment: .leading, spacing: 16) {
                    HStack(spacing: 12) {
                        WorkspaceSymbol(systemName: "simcard", tint: WorkspaceColors.accent)
                        WorkspaceSectionHeader(title: "当前号卡", detail: "USB 读卡器 · eUICC")
                    }
                    Text("🇬🇧 CTExcel").font(.title2.weight(.semibold))
                    Text("+44••••••••31").font(.title3.monospacedDigit())
                    Divider()
                    ProletariusStatusBadge(title: "已连接", color: .green)
                    HStack {
                        Button("eSIM 管理") {}.buttonStyle(.proletarius)
                        Button("下载") {}.buttonStyle(.proletariusPrimary)
                    }
                }
                .padding(20).frame(maxWidth: .infinity, alignment: .leading).workspaceCard()
                VStack(alignment: .leading, spacing: 16) {
                    HStack(spacing: 12) {
                        WorkspaceSymbol(systemName: "wifi", tint: WorkspaceColors.accent)
                        WorkspaceSectionHeader(title: "Wi-Fi Calling", detail: "当前号卡的通信连接")
                    }
                    ProletariusStatusBadge(title: "IMS 已注册", color: .green)
                    Text("代理（可选）").font(.caption).foregroundStyle(.secondary)
                    TextField("自动使用系统网络", text: .constant("")).textFieldStyle(.workspace)
                    Text("SIM → 接入 → 隧道 → IMS → SMS").font(.caption).foregroundStyle(.secondary)
                }
                .padding(20).frame(maxWidth: .infinity, alignment: .leading).workspaceCard()
            }
            Spacer(minLength: 0)
        }
        .padding(24).frame(width: 880, height: 450).proletariusPageBackground()
    }

    @MainActor
    private static func nativeRender<V: View>(_ view: V, size: CGSize = CGSize(width: 880, height: 450)) throws -> CGImage {
        let bounds = NSRect(origin: .zero, size: size)
        let host = NSHostingView(rootView: view)
        let window = NSWindow(contentRect: bounds, styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.appearance = NSApp.appearance
        window.contentView = host
        host.frame = bounds
        host.layoutSubtreeIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.15))
        guard let bitmap = host.bitmapImageRepForCachingDisplay(in: bounds) else {
            throw NSError(domain: "PresentationTests", code: 2)
        }
        host.cacheDisplay(in: bounds, to: bitmap)
        guard let image = bitmap.cgImage else { throw NSError(domain: "PresentationTests", code: 3) }
        return image
    }

    @MainActor
    private static func render<V: View>(_ view: V) throws -> CGImage {
        let renderer = ImageRenderer(content: view)
        renderer.scale = 2
        guard let image = renderer.cgImage else {
            throw NSError(domain: "PresentationTests", code: 1, userInfo: [NSLocalizedDescriptionKey: "SwiftUI image renderer did not produce an image"])
        }
        return image
    }

    @MainActor
    private static func pixels<V: View>(_ view: V) throws -> Data {
        let image = try render(view)
        // Compare visible pixels, not CoreGraphics' uninitialised row padding.
        // Normalise both row stride and colour space for deterministic checks.
        var result = Data(count: image.width * image.height * 4)
        try result.withUnsafeMutableBytes { bytes in
            guard let context = CGContext(data: bytes.baseAddress,
                                          width: image.width, height: image.height,
                                          bitsPerComponent: 8, bytesPerRow: image.width * 4,
                                          space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                          bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else {
                throw NSError(domain: "PresentationTests", code: 4)
            }
            context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
        }
        return result
    }
}

private struct ViewportProbe: View {
    @Environment(\.workspaceViewport) private var viewport
    let onRead: (WorkspaceViewport) -> Void

    var body: some View {
        Color.clear
            .onAppear { onRead(viewport) }
            .onChange(of: viewport) { _, value in onRead(value) }
    }
}
