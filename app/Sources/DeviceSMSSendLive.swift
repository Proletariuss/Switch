import Foundation
import Combine

@MainActor private struct LiveDeviceSMSSendAPI: DeviceSMSSendAPI {
    private let client = APIClient()
    func list(cursor: String?) async throws -> DeviceSMSIntentPage {
        let suffix = cursor.map { "?cursor=" + encoded($0) } ?? ""
        return try await client.get("api/sms/send-intents" + suffix)
    }
    func get(id: String) async throws -> DeviceSMSIntent {
        try await client.get("api/sms/send-intents?client_request_id=" + encoded(id))
    }
    func prepare(_ request: DeviceSMSIntentRequest) async throws -> DeviceSMSIntent {
        try await client.send("api/sms/send-intents", body: request)
    }
    func submit(_ request: DeviceSMSIntentRequest) async throws {
        try await client.send("api/sms/send", body: request)
    }
    private func encoded(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? ""
    }
}

@MainActor private final class DeviceSMSSendRuntime {
    static let shared = DeviceSMSSendRuntime()
    let store = DeviceSMSSendStore(api: LiveDeviceSMSSendAPI(), preferences: .standard)
    private var backendObserver: AnyCancellable?
    private init() {
        backendObserver = BackendProcess.shared.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] state in
                guard let self else { return }
                if case .running = state { store.setAvailable(true) }
                else { store.setAvailable(false) }
            }
    }
}

extension DeviceSMSSendStore {
    static var shared: DeviceSMSSendStore { DeviceSMSSendRuntime.shared.store }
}
