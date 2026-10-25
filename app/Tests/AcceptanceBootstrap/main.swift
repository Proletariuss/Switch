import Foundation

/// No AppKit/SwiftUI/services. The sentinel represents the first dependency
/// constructor and must never execute after any invalid acceptance parameter.
@main
enum AcceptanceBootstrapFixture {
    static func main() {
        _ = AcceptanceBootstrap.context
        print("SERVICE_INITIALIZATION_SENTINEL")
    }
}
