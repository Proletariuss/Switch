// Test-only policy seam. Never include this file in the App target. Production
// obtains this decision only from the immutable, descriptor-validated capsule.
enum AcceptanceBootstrap {
    static var isAcceptance = false
}
