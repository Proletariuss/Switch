import Combine
import SwiftUI

@MainActor final class BackendProcess: ObservableObject {
    enum State: Equatable { case running, stopped }
    @Published var state: State = .running
}

extension View {
    func workspaceCard() -> some View { self }

    func compatibleOnChange<V: Equatable>(
        of value: V,
        perform action: @escaping (V) -> Void
    ) -> some View {
        onChange(of: value) { _, newValue in action(newValue) }
    }
}
