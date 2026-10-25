import Foundation

/// Coordinates the app-wide module/reader snapshot without owning either
/// device domain. Every asynchronous reply is tied to one backend lifecycle
/// generation and one request revision, so a stopped or replaced backend can
/// never publish late device data.
@MainActor
final class DeviceSnapshotStore: ObservableObject {
    @Published private(set) var state = DashboardModuleState.initial

    var readerGeneration: () -> Int = { 0 }
    var readerActionInFlight: () -> Bool = { false }
    var applyReaderSnapshot: (
        _ snapshot: ReaderDeviceSnapshot,
        _ issues: [DeviceSnapshotIssue],
        _ expectedGeneration: Int,
        _ instanceID: String?
    ) -> Void = { _, _, _, _ in }
    var refreshReaderFallback: () -> Void = {}
    var onTraffic: (TrafficSnapshot) -> Void = { _ in }

    private let backend: BackendProcess
    private var refreshTask: Task<Void, Never>?
    private var lifecycleGeneration: UInt64 = 0
    private var requestRevision: UInt64 = 0
    private var upstreamInstanceID: String?

    init(backend: BackendProcess) {
        self.backend = backend
    }

    deinit {
        refreshTask?.cancel()
    }

    func reset() {
        lifecycleGeneration &+= 1
        requestRevision &+= 1
        refreshTask?.cancel()
        refreshTask = nil
        upstreamInstanceID = nil
        state = DashboardModuleState(
            revision: state.revision &+ 1,
            upstreamRevision: nil,
            health: nil,
            status: nil,
            capturedAt: nil,
            isStale: false,
            readPath: "rust-native",
            rustSnapshotComplete: nil,
            rustSnapshotIssues: [],
            rustSnapshotError: nil)
    }

    func refresh() {
        guard case .running = backend.state, refreshTask == nil else { return }
        requestRevision &+= 1
        let expectedRequest = requestRevision
        let expectedLifecycle = lifecycleGeneration
        let expectedReaderGeneration = readerGeneration()

        refreshTask = Task { [weak self] in
            guard let self else { return }
            let client = APIClient(timeoutInterval: 10)

            do {
                let snapshot: DeviceSnapshotResponse = try await client.get("api/v2/device-snapshot")
                try self.requireCurrent(expectedRequest, lifecycle: expectedLifecycle)
                try self.apply(
                    snapshot,
                    expectedReaderGeneration: expectedReaderGeneration)
            } catch is CancellationError {
                return
            } catch {
                guard self.isCurrent(expectedRequest, lifecycle: expectedLifecycle) else { return }
                if !self.readerActionInFlight() {
                    // Identity and status recovery runs independently of the
                    // aggregate Rust snapshot. It first withdraws stale
                    // readiness, then verifies list and card identity before
                    // accepting a VoWiFi status.
                    self.refreshReaderFallback()
                }
                let snapshotError = error.localizedDescription
                do {
                    try await self.refreshCompatibleModule(
                        client,
                        readPath: "rust-endpoint-fallback",
                        rustSnapshotComplete: false,
                        rustSnapshotError: snapshotError,
                        request: expectedRequest,
                        lifecycle: expectedLifecycle)
                } catch is CancellationError {
                    return
                } catch {
                    guard self.isCurrent(expectedRequest, lifecycle: expectedLifecycle) else { return }
                    self.markStale(
                        readPath: "rust-endpoint-fallback",
                        rustSnapshotComplete: false,
                        rustSnapshotError: snapshotError)
                }
            }

            do {
                let traffic: TrafficSnapshot = try await client.get("api/network/traffic")
                try self.requireCurrent(expectedRequest, lifecycle: expectedLifecycle)
                self.onTraffic(traffic)
            } catch {
                // Traffic sampling is independent from the identity snapshot.
            }

            guard self.isCurrent(expectedRequest, lifecycle: expectedLifecycle) else { return }
            self.refreshTask = nil
        }
    }

    func commitModuleStatus(_ status: DeviceStatus) {
        state = DashboardModuleState(
            revision: state.revision &+ 1,
            upstreamRevision: state.upstreamRevision,
            health: state.health,
            status: status,
            capturedAt: state.capturedAt,
            isStale: state.isStale,
            readPath: state.readPath,
            rustSnapshotComplete: state.rustSnapshotComplete,
            rustSnapshotIssues: state.rustSnapshotIssues,
            rustSnapshotError: state.rustSnapshotError)
    }

    private func refreshCompatibleModule(
        _ client: APIClient,
        readPath: String,
        rustSnapshotComplete: Bool?,
        rustSnapshotError: String?,
        request: UInt64,
        lifecycle: UInt64
    ) async throws {
        let health: HealthStatus = try await client.get("api/health")
        try requireCurrent(request, lifecycle: lifecycle)
        let status: DeviceStatus = try await client.get("api/status")
        try requireCurrent(request, lifecycle: lifecycle)
        state = DashboardModuleState(
            revision: state.revision &+ 1,
            upstreamRevision: state.upstreamRevision,
            health: health,
            status: status,
            capturedAt: Date(),
            isStale: false,
            readPath: readPath,
            rustSnapshotComplete: rustSnapshotComplete,
            rustSnapshotIssues: [],
            rustSnapshotError: rustSnapshotError)
    }

    private func markStale(
        readPath: String,
        rustSnapshotComplete: Bool?,
        rustSnapshotError: String?
    ) {
        state = DashboardModuleState(
            revision: state.revision &+ 1,
            upstreamRevision: state.upstreamRevision,
            health: state.health,
            status: state.status,
            capturedAt: state.capturedAt,
            isStale: true,
            readPath: readPath,
            rustSnapshotComplete: rustSnapshotComplete,
            rustSnapshotIssues: [],
            rustSnapshotError: rustSnapshotError)
    }

    private func apply(
        _ snapshot: DeviceSnapshotResponse,
        expectedReaderGeneration: Int
    ) throws {
        guard snapshot.schemaVersion == 2 else {
            throw NSError(
                domain: "Proletarius.DeviceSnapshot",
                code: snapshot.schemaVersion,
                userInfo: [NSLocalizedDescriptionKey: "不支持的设备快照版本：\(snapshot.schemaVersion)"])
        }
        guard snapshot.ok else {
            throw NSError(
                domain: "Proletarius.DeviceSnapshot",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey: "Rust 设备快照没有可用数据"])
        }
        let incomingInstanceID = snapshot.instanceID
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .flatMap { $0.isEmpty ? nil : $0 }
        if incomingInstanceID == upstreamInstanceID,
           let upstreamRevision = state.upstreamRevision,
           snapshot.revision <= upstreamRevision {
            return
        }
        upstreamInstanceID = incomingInstanceID

        let hasModulePayload = snapshot.module.health != nil || snapshot.module.status != nil
        state = DashboardModuleState(
            revision: state.revision &+ 1,
            upstreamRevision: snapshot.revision,
            health: snapshot.module.health ?? state.health,
            status: snapshot.module.status ?? state.status,
            capturedAt: hasModulePayload
                ? Date(timeIntervalSince1970: Double(snapshot.capturedAtMS) / 1_000)
                : state.capturedAt,
            isStale: snapshot.module.health == nil || snapshot.module.status == nil,
            readPath: snapshot.source,
            rustSnapshotComplete: snapshot.complete,
            rustSnapshotIssues: snapshot.issues,
            rustSnapshotError: nil)

        guard !readerActionInFlight(),
              expectedReaderGeneration == readerGeneration() else { return }
        applyReaderSnapshot(
            snapshot.reader, snapshot.issues, expectedReaderGeneration, incomingInstanceID)
    }

    private func isCurrent(_ request: UInt64, lifecycle: UInt64) -> Bool {
        !Task.isCancelled &&
            requestRevision == request &&
            lifecycleGeneration == lifecycle
    }

    private func requireCurrent(_ request: UInt64, lifecycle: UInt64) throws {
        guard isCurrent(request, lifecycle: lifecycle) else {
            throw CancellationError()
        }
    }
}
