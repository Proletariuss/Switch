import AppKit
import SwiftUI

// The production sendAT body is extracted unchanged; transport is deliberately
// an in-memory double, not the app's APIClient or modem service.
struct ATEntry { let command: String; let response: String; var isError = false }
struct ATRequest { let command: String }
struct ATResult { let response: String? }
func isModuleDisconnectedError(_ error: String) -> Bool { false }
let moduleDisconnectedFriendlyMessage = "Offline fixture"
@MainActor struct APIClient {
    static var commands: [String] = []
    func send(_ path: String, body: ATRequest) async throws -> ATResult {
        precondition(path == "api/at")
        Self.commands.append(body.command)
        try await Task.sleep(for: .milliseconds(20))
        return ATResult(response: "OK")
    }
}

@MainActor
private final class LayoutFixtureModel: ObservableObject {
    @Published var viewport = WorkspaceViewport(width: 688, height: 550)
    var identities: [UUID] = []
}

private struct StateProbe: View {
    @ObservedObject var model: LayoutFixtureModel
    @State private var identity = UUID()

    var body: some View {
        Text("草稿与焦点身份")
            .onAppear { model.identities.append(identity) }
            .onChange(of: model.viewport) { _, _ in model.identities.append(identity) }
    }
}

private struct LayoutFixture: View {
    @ObservedObject var model: LayoutFixtureModel
    let isSMS: Bool

    var body: some View {
        let policy = PageLayoutPolicy(viewport: model.viewport)
        let layout = isSMS ? policy.conversationLayout : policy.aboutCardsLayout
        layout {
            Text("会话列表 / 软件更新")
            StateProbe(model: model)
        }
    }
}

@main
private enum WorkspacePageLayoutTests {
    @MainActor
    private static func waitUntil(_ condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(1)
        while !condition(), Date() < deadline {
            RunLoop.main.run(until: Date().addingTimeInterval(0.005))
        }
        precondition(condition(), "offline fixture did not settle within one second")
    }

    @MainActor
    static func main() {
        let compact = WorkspaceViewport(width: 688, height: 550)
        let policy = PageLayoutPolicy(viewport: compact)
        precondition(policy.usesConversationColumns && policy.conversationListWidth == 230)
        let detailWidth = compact.contentWidth - compact.sectionSpacing - policy.conversationListWidth!
        precondition(detailWidth == 424, "compact SMS detail must retain space for sender picker and composer")
        for size in [CGSize(width: 560, height: 550), CGSize(width: 600, height: 720)] {
            let narrow = PageLayoutPolicy(viewport: WorkspaceViewport(width: size.width, height: size.height))
            precondition(!narrow.usesConversationColumns && narrow.conversationListWidth == nil)
        }
        let wide = PageLayoutPolicy(viewport: WorkspaceViewport(width: 1400, height: 900))
        precondition(wide.usesConversationColumns && wide.conversationListWidth == 300)
        for height: CGFloat in [550, 719, 720, 900] {
            precondition(PageLayoutPolicy(viewport: WorkspaceViewport(width: 900, height: height)).usesAboutColumns)
            precondition(!PageLayoutPolicy(viewport: WorkspaceViewport(width: 700, height: height)).usesAboutColumns)
        }

        _ = NSApplication.shared
        NSApp.setActivationPolicy(.prohibited)
        let at = ATConsoleFixture()
        at.command = "  AT+COMMAND=\"two words\"  "
        at.sendAT()
        precondition(at.atBusy && at.command.isEmpty)
        at.command = "  AT+CSQ  "
        at.sendAT()
        precondition(at.command == "  AT+CSQ  ", "busy Return must preserve next command")
        waitUntil { !at.atBusy }
        precondition(APIClient.commands == ["AT+COMMAND=\"two words\""] && !at.atBusy,
                     "first command retains internal spaces and has only one request")
        at.sendAT()
        waitUntil { !at.atBusy }
        precondition(APIClient.commands == ["AT+COMMAND=\"two words\"", "AT+CSQ"] && at.history.count == 2)
        at.command = "   "
        at.sendAT()
        precondition(!at.atBusy && APIClient.commands.count == 2, "blank Return must not submit")
        for isSMS in [true, false] {
            let model = LayoutFixtureModel()
            let host = NSHostingView(rootView: LayoutFixture(model: model, isSMS: isSMS))
            let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 688, height: 550),
                                  styleMask: .borderless, backing: .buffered, defer: false)
            window.isReleasedWhenClosed = false
            window.contentView = host
            host.layoutSubtreeIfNeeded()
            RunLoop.main.run(until: Date().addingTimeInterval(0.06))
            for width: CGFloat in [560, 960, 688] {
                model.viewport = WorkspaceViewport(width: width, height: 550)
                host.layoutSubtreeIfNeeded()
                RunLoop.main.run(until: Date().addingTimeInterval(0.06))
            }
            precondition(model.identities.count >= 4, "every tested resize must reach the state probe")
            precondition(Set(model.identities).count == 1, "reflow must not recreate local state")
            window.close()
        }
        print("Page layout: compact SMS 424pt detail, narrow/wide fallback, height-independent About columns, SwiftUI state preservation and AT Return single-flight/draft/space retention passed")
    }
}
