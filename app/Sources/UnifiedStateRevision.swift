import Foundation

/// Revision vector for the independently updated device, reader and VoWiFi
/// stores. Comparing only their maximum loses updates whenever another store
/// already has a larger revision; compare the whole vector instead.
struct UnifiedStateRevision: Hashable, Sendable {
    let device: UInt64
    let reader: UInt64
    let voWiFi: UInt64
}
