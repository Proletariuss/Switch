import Combine
import Foundation

/// 通话记录的单一状态源。
///
/// 记录读写与实时通话状态分离，避免历史记录请求与两秒一次的通话轮询
/// 相互覆盖。后端停止时主动取消在途请求并清空内存快照。
@MainActor
final class CallHistoryStore: ObservableObject {
    @Published private(set) var records: [CallRecord] = []
    @Published private(set) var loading = false

    var onError: ((String) -> Void)?

    private let backend: BackendProcess
    private var loadTask: Task<Void, Never>?
    private var mutationTasks: [UUID: Task<Void, Never>] = [:]
    private var snapshotRevision: UInt64 = 0
    private var lifecycleGeneration: UInt64 = 0
    private var cancellables = Set<AnyCancellable>()

    init(backend: BackendProcess) {
        self.backend = backend
        backend.$state
            .receive(on: RunLoop.main)
            .sink { [weak self] state in
                guard let self else { return }
                if case .running = state {
                    self.load()
                } else {
                    self.reset()
                }
            }
            .store(in: &cancellables)
    }

    deinit {
        loadTask?.cancel()
        mutationTasks.values.forEach { $0.cancel() }
    }

    func load() {
        guard case .running = backend.state else { return }
        snapshotRevision &+= 1
        let expectedRevision = snapshotRevision
        loadTask?.cancel()
        loading = true
        loadTask = Task { [weak self] in
            do {
                let list: [CallRecord] = try await APIClient().get("api/calls")
                guard let self,
                      !Task.isCancelled,
                      self.snapshotRevision == expectedRevision else { return }
                // 固话外呼已从通话页移除；后端旧数据继续可读，但不进入
                // 当前通话列表和未接来电统计。
                let visible = list.filter { $0.device != "landline" }
                self.records = visible
                self.loading = false
                self.loadTask = nil
                AttentionStore.shared.reconcileCallHistory(visible)
            } catch {
                guard let self,
                      !Task.isCancelled,
                      self.snapshotRevision == expectedRevision else { return }
                // 刷新失败保留最近一次成功快照。
                self.loading = false
                self.loadTask = nil
            }
        }
    }

    func clear() {
        guard case .running = backend.state else { return }
        invalidateLoad()
        let taskID = UUID()
        let expectedGeneration = lifecycleGeneration
        mutationTasks[taskID] = Task { [weak self] in
            do {
                let _: CallClearResult = try await APIClient().send("api/calls/clear")
                guard let self,
                      !Task.isCancelled,
                      self.lifecycleGeneration == expectedGeneration else { return }
                self.records = []
                self.mutationTasks[taskID] = nil
                AttentionStore.shared.markCallsViewed()
            } catch {
                guard let self,
                      !Task.isCancelled,
                      self.lifecycleGeneration == expectedGeneration else { return }
                self.mutationTasks[taskID] = nil
                self.onError?("清空失败：\(error.localizedDescription)")
            }
        }
    }

    func delete(_ id: String) {
        guard case .running = backend.state else { return }
        invalidateLoad()
        let taskID = UUID()
        let expectedGeneration = lifecycleGeneration
        mutationTasks[taskID] = Task { [weak self] in
            do {
                let _: CallClearResult = try await APIClient().send(
                    "api/calls/delete",
                    body: CallDeleteRequest(id: id))
                guard let self,
                      !Task.isCancelled,
                      self.lifecycleGeneration == expectedGeneration else { return }
                self.records.removeAll { $0.id == id }
                self.mutationTasks[taskID] = nil
            } catch {
                guard let self,
                      !Task.isCancelled,
                      self.lifecycleGeneration == expectedGeneration else { return }
                self.mutationTasks[taskID] = nil
                self.onError?("删除失败：\(error.localizedDescription)")
            }
        }
    }

    private func invalidateLoad() {
        snapshotRevision &+= 1
        loadTask?.cancel()
        loadTask = nil
        loading = false
    }

    private func reset() {
        lifecycleGeneration &+= 1
        invalidateLoad()
        mutationTasks.values.forEach { $0.cancel() }
        mutationTasks.removeAll()
        records = []
    }
}
