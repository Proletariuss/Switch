import Foundation

/// The backend owns retry safety, cleanup and attempt count. UI presents this
/// receipt without converting a failed or unknown outcome into another read.
struct ESIMRefreshOutcome: Decodable, Equatable {
    let operationID: String
    let scope: String
    let phase: String
    let outcome: String
    let attempts: Int
    let retryPerformed: Bool
    let message: String

    var completed: Bool { phase == "completed" && outcome == "succeeded" }

    enum CodingKeys: String, CodingKey {
        case scope, phase, outcome, attempts, message
        case operationID = "operation_id"
        case retryPerformed = "retry_performed"
    }
}

protocol ESIMRefreshFailure: Error {
    var refreshOutcome: ESIMRefreshOutcome? { get }
}
