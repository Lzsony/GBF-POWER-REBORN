import Foundation

enum AuthorizationState: Equatable {
    case checking
    case activationRequired
    case verificationUnavailable
    case configurationError
    case authorized
}

/// Gates acceleration, not navigation. Stale replies cannot restore revoked access.
struct AuthorizationGate {
    private(set) var state: AuthorizationState = .checking
    private(set) var errorCode: String?
    private var generation: UInt64 = 0

    var isAuthorized: Bool { state == .authorized }

    func canStart(hasAvailableNodes: Bool) -> Bool { isAuthorized && hasAvailableNodes }

    func permits(_ operation: String) -> Bool { operation == "stop" || isAuthorized }

    mutating func begin() -> UInt64 {
        generation &+= 1
        state = .checking
        errorCode = nil
        return generation
    }

    mutating func complete(_ attempt: UInt64, active: Bool, error: String? = nil) {
        guard attempt == generation else { return }
        errorCode = error
        if active && error == nil { state = .authorized; return }
        switch error {
        case "AUTH_NOT_CONFIGURED": state = .configurationError
        case "AUTH_REQUIRED", "AUTH_INVALID", "AUTH_REVOKED", "AUTH_DEVICE_LIMIT", "AUTH_DEVICE_BOUND": state = .activationRequired
        default: state = .verificationUnavailable
        }
    }

    mutating func revoke(_ error: String = "AUTH_REVOKED") {
        generation &+= 1
        errorCode = error
        state = .activationRequired
    }
}

struct NodeLatencyResult: Decodable {
    let testId: String
    let lineId: String
    let medianMs: Double?
    let completedAt: UInt64
    let state: String
}

struct ConnectionCheckSnapshot: Decodable {
    let connectionId: String?
    let result: NodeLatencyResult?
}

/// Results belong to both a request and an SSH session, including same-node reconnects.
struct NodeLatencyState {
    private(set) var connectionId: String?
    private(set) var testId: String?
    private(set) var result: NodeLatencyResult?
    private(set) var isChecking = false
    private(set) var failed = false
    private(set) var completedAt: UInt64?

    @discardableResult
    mutating func synchronize(connectionId: String?) -> Bool {
        guard self.connectionId != connectionId else { return false }
        self.connectionId = connectionId
        testId = nil
        result = nil
        isChecking = false
        failed = false
        completedAt = nil
        return true
    }

    mutating func begin(testId: String) -> Bool {
        guard connectionId != nil, !isChecking else { return false }
        self.testId = testId
        result = nil
        failed = false
        completedAt = nil
        isChecking = true
        return true
    }

    mutating func accept(_ snapshot: ConnectionCheckSnapshot, requestId: String) {
        guard isChecking, testId == requestId, connectionId == snapshot.connectionId,
              let result = snapshot.result, result.testId == requestId else { return }
        if result.state == "running" { return }
        isChecking = false
        completedAt = result.completedAt
        if result.state == "success", let median = result.medianMs, median.isFinite, median >= 0 {
            self.result = result
            failed = false
        } else {
            self.result = nil
            failed = true
        }
    }

    mutating func fail(requestId: String) {
        guard isChecking, testId == requestId else { return }
        isChecking = false
        result = nil
        failed = true
        completedAt = UInt64(Date().timeIntervalSince1970 * 1000)
    }
}
