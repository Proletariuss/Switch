import Foundation

@MainActor
final class SMSStore {
    static let shared = SMSStore()

    func forwardingAdmissionSnapshot() -> DeviceSMSForwardingHandoff {
        DeviceSMSForwardingHandoff(acceptedNotificationIDs: [], wasEnabled: nil)
    }
}
