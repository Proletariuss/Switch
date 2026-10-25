import SwiftUI

/// Keep the original App/Scene tree. Authority precedes SwiftUI property
/// wrappers and delegates, rather than being a late view/settings flag.
@main
enum SwitchEntry {
    @MainActor
    static func main() {
        _ = AcceptanceBootstrap.context
        ProletariusApp.main()
    }
}
