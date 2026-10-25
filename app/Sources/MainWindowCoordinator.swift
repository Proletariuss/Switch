import AppKit

enum AppSceneID {
    static let mainWindow = "main-window"
}

/// Owns the single main window independently of settings and floating panels.
/// Closing hides this retained window so active communication stays alive.
@MainActor
final class MainWindowCoordinator: NSObject, NSWindowDelegate {
    private(set) var window: NSWindow?

    func register(_ newWindow: NSWindow) {
        newWindow.identifier = NSUserInterfaceItemIdentifier(AppSceneID.mainWindow)
        newWindow.isReleasedWhenClosed = false
        newWindow.isRestorable = false
        newWindow.delegate = self
        // Let the workspace canvas continue behind the titlebar/toolbar so the
        // top controls read as part of each page rather than a separate strip.
        newWindow.titlebarAppearsTransparent = true
        newWindow.titleVisibility = .hidden
        newWindow.styleMask.insert(.fullSizeContentView)
        guard window !== newWindow else { return }
        let previous = window
        window = newWindow
        previous?.orderOut(nil)
    }

    @discardableResult
    func show() -> Bool {
        guard let window else { return false }
        NSApp.activate(ignoringOtherApps: true)
        if window.isMiniaturized { window.deminiaturize(nil) }
        window.makeKeyAndOrderFront(nil)
        return true
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        sender.orderOut(nil)
        return false
    }
}
