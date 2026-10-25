import Combine
import Foundation

/// macOS 网络服务排序的独立状态域。
///
/// 与设备轮询和 VoWiFi 生命周期没有共享写入，单独维护可避免网络服务
/// 请求扩散到 DashboardStore，并在后端停止时明确清除旧快照。
@MainActor
final class NetworkServicesStore: ObservableObject {
    @Published private(set) var services: [NetworkService] = []
    @Published private(set) var isLoaded = false
    @Published private(set) var message: String?
    @Published private(set) var error: String?
    @Published private(set) var isSaving = false

    private struct ServicesResponse: Decodable {
        let services: [NetworkService]
    }

    private let backend: BackendProcess
    private var loadTask: Task<Void, Never>?
    private var saveTask: Task<Void, Never>?
    private var revision: UInt64 = 0

    init(backend: BackendProcess) {
        self.backend = backend
    }

    deinit {
        loadTask?.cancel()
        saveTask?.cancel()
    }

    func reset() {
        loadTask?.cancel()
        saveTask?.cancel()
        loadTask = nil
        saveTask = nil
        revision &+= 1
        services = []
        isLoaded = false
        message = nil
        error = nil
        isSaving = false
    }

    func load() {
        guard case .running = backend.state else { return }
        loadTask?.cancel()
        revision &+= 1
        let expectedRevision = revision
        loadTask = Task { [weak self] in
            guard let self else { return }
            defer {
                if expectedRevision == revision {
                    loadTask = nil
                }
            }
            do {
                let result: ServicesResponse = try await APIClient().get("api/network/services")
                guard !Task.isCancelled, expectedRevision == revision else { return }
                services = result.services
                isLoaded = true
                error = nil
            } catch {
                guard !Task.isCancelled, expectedRevision == revision else { return }
                isLoaded = true
                self.error = error.localizedDescription
            }
        }
    }

    func moveService(from source: Int, to destination: Int) {
        guard source != destination,
              services.indices.contains(source),
              services.indices.contains(destination) else { return }
        let item = services.remove(at: source)
        services.insert(item, at: destination)
        message = nil
        error = nil
    }

    func saveOrder() {
        guard case .running = backend.state,
              !services.isEmpty,
              !isSaving else { return }
        saveTask?.cancel()
        isSaving = true
        message = nil
        error = nil
        let names = services.map(\.name)
        saveTask = Task { [weak self] in
            guard let self else { return }
            defer {
                isSaving = false
                saveTask = nil
            }
            do {
                let result: MessageResponse = try await APIClient().send(
                    "api/network/services-order",
                    method: "PUT",
                    body: ServicesOrderRequest(services: names))
                message = result.message
                load()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}
