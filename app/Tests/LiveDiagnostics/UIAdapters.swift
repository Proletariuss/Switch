import Combine
import SwiftUI
import Foundation

enum SwitchRuntimePaths {
    static func dataDirectory() -> URL {
        fatalError("The live-only fixture must not request a filesystem directory")
    }
}

@MainActor final class BackendProcess: ObservableObject {
    enum State: Equatable { case running, stopped, starting, stopping, failed(String) }
    @Published var state: State = .running
    @Published var childPID: Int32 = 1
}
struct TestViewport {
    let sectionSpacing: CGFloat = 10
    let cardPadding: CGFloat = 12
}
private struct TestViewportKey: EnvironmentKey { static let defaultValue = TestViewport() }
extension EnvironmentValues {
    var workspaceViewport: TestViewport {
        get { self[TestViewportKey.self] }
        set { self[TestViewportKey.self] = newValue }
    }
}
enum WorkspaceColors {
    static let accent = Color.blue
    static let inset = Color.gray.opacity(0.1)
}
struct WorkspaceSymbol: View {
    let systemName: String
    let tint: Color
    let size: CGFloat
    var body: some View { Image(systemName: systemName).foregroundStyle(tint).frame(width: size, height: size) }
}
struct WorkspaceSectionHeader: View {
    let title: String
    let detail: String
    var body: some View { VStack { Text(title); Text(detail) } }
}
extension View {
    func workspaceCard() -> some View { self }
    func compatibleOnChange<V: Equatable>(of value: V, perform action: @escaping (V) -> Void) -> some View {
        onChange(of: value) { _, newValue in action(newValue) }
    }
}
