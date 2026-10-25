import Foundation

/// A transient in-app notification emitted by stores and rendered by the UI.
/// Keeping this model independent prevents DashboardStore from owning view-only
/// data definitions.
struct ToastItem: Identifiable, Equatable {
    let id = UUID()
    let message: String
    let isSuccess: Bool
    var title: String? = nil
    var icon: String? = nil
}
