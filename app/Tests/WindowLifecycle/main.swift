import AppKit

@main
private enum WindowLifecycleTests {
    @MainActor
    static func main() {
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        let coordinator = MainWindowCoordinator()
        precondition(!coordinator.show(), "no registered window must request the scene instead")

        let window = makeWindow()
        coordinator.register(window)
        precondition(window.identifier?.rawValue == AppSceneID.mainWindow)
        precondition(!window.isReleasedWhenClosed)
        precondition(window.delegate === coordinator)
        precondition(window.titlebarAppearsTransparent, "the titlebar should blend into the workspace")
        precondition(window.titleVisibility == .hidden, "the redundant app title should be hidden")
        precondition(window.styleMask.contains(.fullSizeContentView), "workspace content should extend under the titlebar")
        precondition(coordinator.show() && window.isVisible)

        window.performClose(nil)
        precondition(!window.isVisible, "closing should hide the window")
        precondition(coordinator.window === window, "closing must retain the same window")
        precondition(coordinator.show() && window.isVisible)
        precondition(coordinator.window === window, "reopening must reuse the window")

        let settings = makeWindow()
        settings.title = "Settings"
        precondition(coordinator.window !== settings, "settings cannot replace the main window")
        coordinator.register(window)
        precondition(coordinator.window === window, "repeat registration should be idempotent")

        window.miniaturize(nil)
        RunLoop.main.run(until: Date().addingTimeInterval(0.2))
        precondition(coordinator.show())
        RunLoop.main.run(until: Date().addingTimeInterval(0.2))
        precondition(!window.isMiniaturized && window.isVisible)

        let restored = makeWindow()
        coordinator.register(restored)
        precondition(!window.isVisible, "an obsolete main window must stay hidden")
        precondition(coordinator.show() && restored.isVisible)
        precondition(coordinator.window === restored)
        restored.orderOut(nil)
        print("Window lifecycle: registration, close/reopen, settings isolation, minimization and replacement passed")
    }

    @MainActor
    private static func makeWindow() -> NSWindow {
        NSWindow(
            contentRect: NSRect(x: 80, y: 80, width: 420, height: 240),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false)
    }
}
