import AppKit
import SwiftUI

private struct ReaderContentHeight: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = max(value, nextValue()) }
}

@main private enum ReaderPresentationTests {
    @MainActor static func main() throws {
        _ = NSApplication.shared
        NSApp.setActivationPolicy(.prohibited)
        let output = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        precondition(NSImage(named: "BluetoothLogo") != nil, "production Bluetooth asset must be available")
        var reports: [[String: Any]] = []
        for (layout, size, scheme) in [
            ("compact-light", CGSize(width: 688, height: 550), ColorScheme.light),
            ("wide-dark", CGSize(width: 1040, height: 800), ColorScheme.dark)
        ] {
            NSApp.appearance = NSAppearance(named: scheme == .dark ? .darkAqua : .aqua)
            for count in [2, 9] {
                for masked in [false, true] {
                    ReaderPresentationFixture.profileCount = count
                    ReaderPresentationFixture.identityPending = false
                    let name = "reader-\(count)-profiles-\(masked ? "masked" : "visible")-\(layout)"
                    reports.append(try capture(name, output: output, size: size, scheme: scheme, masked: masked))
                }
            }
            ReaderPresentationFixture.profileCount = 2
            ReaderPresentationFixture.identityPending = true
            reports.append(try capture("reader-identity-pending-\(layout)", output: output, size: size, scheme: scheme, masked: true))
        }
        let data = try JSONSerialization.data(withJSONObject: reports, options: [.prettyPrinted, .sortedKeys])
        try data.write(to: output.appendingPathComponent("measurements.json"))
        print("Reader presentation: 10 native production-page renders; 2/9 profiles, long names, mask on/off, light/dark, pending USIM; only fake synchronization observed")
    }

    @MainActor private static func capture(_ name: String, output: URL, size: CGSize, scheme: ColorScheme, masked: Bool) throws -> [String: Any] {
        ReaderPresentationFixture.calls = []
        let store = DashboardStore()
        let viewport = WorkspaceViewport(width: size.width, height: size.height)
        var measuredHeight: CGFloat = 0
        let view = ScrollView {
            readerFixtureView(store: store, masked: masked)
                // A fixed screenshot viewport avoids evaluating either version
                // with NSHostingView's unbounded intrinsic-size probes.
                .frame(width: viewport.contentWidth)
                .padding(viewport.pageInset)
                .background(GeometryReader { geometry in
                    Color.clear.preference(key: ReaderContentHeight.self, value: geometry.size.height)
                })
        }
        .onPreferenceChange(ReaderContentHeight.self) { measuredHeight = $0 }
        .environmentObject(store)
        .environment(\.workspaceViewport, viewport)
        .environment(\.workspaceVisualStyle, .nekoko)
        .environment(\.colorScheme, scheme)
        .buttonStyle(.proletarius)
        .transaction { $0.disablesAnimations = true }
        .proletariusPageBackground()
        let bounds = NSRect(origin: .zero, size: size)
        let host = NSHostingView(rootView: view)
        let window = NSWindow(contentRect: bounds, styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.appearance = NSApp.appearance
        window.contentView = host
        host.frame = bounds
        host.layoutSubtreeIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.16))
        host.layoutSubtreeIfNeeded()
        defer { window.contentView = nil; window.close() }
        precondition(ReaderPresentationFixture.calls.contains("synchronize"), "real onAppear must run the fake reader binding")
        precondition(ReaderPresentationFixture.calls.allSatisfy { $0 == "synchronize" }, "rendering must never trigger device/card actions")
        precondition(measuredHeight.isFinite && measuredHeight > 200, "page must expose a measurable content height")
        guard let bitmap = host.bitmapImageRepForCachingDisplay(in: bounds) else { throw CocoaError(.coderInvalidValue) }
        host.cacheDisplay(in: bounds, to: bitmap)
        guard let png = bitmap.representation(using: .png, properties: [:]) else { throw CocoaError(.fileWriteUnknown) }
        try png.write(to: output.appendingPathComponent(name + ".png"))
        return ["name": name, "width": size.width, "height": size.height,
                "contentHeight": measuredHeight, "belowFold": max(0, measuredHeight - size.height),
                "actions": ReaderPresentationFixture.calls]
    }
}
