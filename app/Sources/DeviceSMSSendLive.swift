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
    func reconcileArchive(_ request: DeviceSMSIntentRequest) async throws {
        // The server resolves the durable accepted record before any route
        // admission; this same identity can only repair its local archive.
        try await client.send("api/sms/send", body: request)
    }
    func authority() async throws -> DeviceSMSAuthorityStatus {
        try await client.get("api/sms/send-intents?authority=2")
    }
    func reserve(bytes: Data) async throws -> DeviceSMSIntent {
        do { return try await client.upload("api/sms/send-intents", data: bytes) }
        catch { throw identityError(error) }
    }
    func prepareV2(bytes: Data) async throws -> DeviceSMSIntent {
        do { return try await client.upload("api/sms/send-intents", data: bytes) }
        catch { throw identityError(error) }
    }
    private struct DispatchReply: Decodable {}
    func submitV2(bytes: Data) async throws {
        do { let _: DispatchReply = try await client.upload("api/sms/send", data: bytes) }
        catch { throw identityError(error) }
    }
    func reconcileArchiveV2(bytes: Data) async throws {
        let _: DispatchReply = try await client.upload("api/sms/send", data: bytes)
    }
    func cancelReservation(bytes: Data) async throws -> DeviceSMSIntent {
        do { return try await client.upload("api/sms/send-intents?cancel=2", data: bytes) }
        catch { throw identityError(error) }
    }
    private func identityError(_ error: Error) -> Error {
        if let error = error as? APIError,
           error.statusCode == 404 || error.statusCode == 410 ||
           ["send_intent_conflict", "mutation_authority_mismatch", "mutation_identity_not_issued",
            "mutation_identity_upgrade_required", "mutation_identity_retired"].contains(error.code ?? "") {
            return DeviceSMSIdentityRefusal()
        }
        return error
    }
    private func encoded(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? ""
    }
}

@MainActor private final class DeviceSMSSendRuntime {
    static let shared = DeviceSMSSendRuntime()
    let store = DeviceSMSSendStore(api: LiveDeviceSMSSendAPI(), preferences: .standard,
        journal: DeviceSMSIssuanceJournal(directory: SwitchRuntimePaths.dataDirectory()
            .appendingPathComponent("device-sms-issuance", isDirectory: true)))
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
