import AppKit
import Foundation

struct CallRecord { let id: String; let isMissed: Bool }

@main @MainActor enum SMSAttentionTests {
    static func main() {
        _ = NSApplication.shared
        let domain = "Switch-SMSAttention-" + UUID().uuidString
        let defaults = UserDefaults(suiteName: domain)!
        defer { defaults.removePersistentDomain(forName: domain) }
        defaults.set(3, forKey: "attentionUnreadSMSCount")
        let store = AttentionStore(defaults: defaults)
        precondition(store.unreadSMSCount == 3, "upgrade discarded legacy unread count")
        store.recordIncomingSMS(messageID: "opaque-a", threadID: "line-a")
        store.recordIncomingSMS(messageID: "opaque-a", threadID: "line-a")
        store.recordIncomingSMS(messageID: "opaque-b", threadID: "line-b")
        precondition(store.unreadSMSCount == 5, "incoming IDs were not deduplicated")
        store.markSMSThreadViewed("line-a")
        precondition(store.unreadSMSCount == 4 && store.unreadCount(threadID: "line-b") == 1,
                     "reading one line cleared another line or legacy unread count")
        let restored = AttentionStore(defaults: defaults)
        precondition(restored.unreadSMSCount == 4 && restored.unreadCount(threadID: "line-b") == 1,
                     "restart lost per-thread unread progress")
        restored.forgetSMS(messageID: "opaque-b")
        precondition(restored.unreadSMSCount == 3, "deletion retained unread")
        restored.setViewingSMS(threadID: "line-a")
        if !NSApp.isActive {
            precondition(!restored.isViewingSMS(threadID: "line-a"), "background window suppressed notification")
        }
        restored.markSMSViewed()
        precondition(AttentionStore(defaults: defaults).unreadSMSCount == 0, "explicit all-read did not persist")
        print("SMS attention: deduplication, line isolation, migration, restart and deletion passed (isolated defaults)")
    }
}
