import XCTest
@testable import GPRAdmission

final class AuthorizationGateTests: XCTestCase {
    func testAccelerationRequiresOnlineVerificationAndAvailableNode() {
        var gate = AuthorizationGate()
        XCTAssertEqual(gate.state, .checking)
        XCTAssertFalse(gate.canStart(hasAvailableNodes: true))
        let attempt = gate.begin()
        gate.complete(attempt, active: false, error: "AUTH_UNAVAILABLE")
        XCTAssertEqual(gate.state, .verificationUnavailable)
        XCTAssertFalse(gate.canStart(hasAvailableNodes: true))
        gate.complete(gate.begin(), active: true)
        XCTAssertEqual(gate.state, .authorized)
        XCTAssertFalse(gate.canStart(hasAvailableNodes: false))
        XCTAssertTrue(gate.canStart(hasAvailableNodes: true))
    }
    func testRevocationInvalidatesInflightSuccess() {
        var gate = AuthorizationGate()
        let old = gate.begin()
        gate.revoke()
        gate.complete(old, active: true)
        XCTAssertEqual(gate.state, .activationRequired)
        XCTAssertFalse(gate.permits("start"))
    }
    func testNewAttemptIgnoresOldFailureAndSuccess() {
        var gate = AuthorizationGate()
        let old = gate.begin()
        let current = gate.begin()
        gate.complete(old, active: true)
        XCTAssertEqual(gate.state, .checking)
        gate.complete(current, active: false, error: "AUTH_DEVICE_LIMIT")
        gate.complete(old, active: true)
        XCTAssertEqual(gate.state, .activationRequired)
    }
    func testValidAccountDoesNotDependOnNodeCount() {
        var gate = AuthorizationGate()
        gate.complete(gate.begin(), active: true)
        XCTAssertEqual(gate.state, .authorized)
    }
    func testMissingBuildProfileIsNotAnActivationFailure() {
        var gate = AuthorizationGate()
        gate.complete(gate.begin(), active: false, error: "AUTH_NOT_CONFIGURED")
        XCTAssertEqual(gate.state, .configurationError)
    }
    func testStopRemainsAvailableInEveryAuthorizationFailure() {
        var gate = AuthorizationGate()
        XCTAssertTrue(gate.permits("stop"))
        for error in ["AUTH_REQUIRED", "AUTH_INVALID", "AUTH_REVOKED", "AUTH_UNAVAILABLE", "AUTH_NOT_CONFIGURED"] {
            gate.complete(gate.begin(), active: false, error: error)
            XCTAssertTrue(gate.permits("stop"), error)
            for operation in ["start", "save", "clearCache", "createCertificate", "unbind"] {
                XCTAssertFalse(gate.permits(operation), "\(error): \(operation)")
            }
        }
    }
    func testRevocationRemovesPermissionsWithoutPreventingStop() {
        var gate = AuthorizationGate()
        gate.complete(gate.begin(), active: true)
        XCTAssertTrue(gate.permits("save"))
        gate.revoke()
        XCTAssertFalse(gate.canStart(hasAvailableNodes: true))
        XCTAssertFalse(gate.permits("save"))
        XCTAssertTrue(gate.permits("stop"))
    }
}

final class NodeLatencyStateTests: XCTestCase {
    private func reply(session: String = "session-a", id: String = "request-a", state: String = "success", ms: Double? = 35) -> ConnectionCheckSnapshot {
        ConnectionCheckSnapshot(connectionId: session, result: NodeLatencyResult(testId: id, lineId: "same-node", medianMs: ms, completedAt: 1000, state: state))
    }

    func testRequiresConnectionAndRejectsDuplicateClick() {
        var state = NodeLatencyState()
        XCTAssertFalse(state.begin(testId: "request-a"))
        state.synchronize(connectionId: "session-a")
        XCTAssertTrue(state.begin(testId: "request-a"))
        XCTAssertFalse(state.begin(testId: "request-b"))
        state.accept(reply(), requestId: "request-a")
        XCTAssertEqual(state.result?.medianMs, 35)
        XCTAssertFalse(state.isChecking)
        state.fail(requestId: "request-a")
        XCTAssertEqual(state.result?.medianMs, 35)
    }

    func testSameNodeReconnectionRejectsOldResponse() {
        var state = NodeLatencyState()
        state.synchronize(connectionId: "session-a")
        XCTAssertTrue(state.begin(testId: "request-a"))
        state.synchronize(connectionId: "session-b")
        XCTAssertTrue(state.begin(testId: "request-b"))
        state.accept(reply(), requestId: "request-a")
        XCTAssertNil(state.result)
        XCTAssertTrue(state.isChecking)
        state.accept(reply(session: "session-b", id: "request-b", ms: 19), requestId: "request-b")
        XCTAssertEqual(state.result?.medianMs, 19)
    }

    func testStoppingClearsResultAndLateReplyCannotRestoreIt() {
        var state = NodeLatencyState()
        state.synchronize(connectionId: "session-a")
        XCTAssertTrue(state.begin(testId: "request-a"))
        state.accept(reply(), requestId: "request-a")
        state.synchronize(connectionId: nil)
        state.accept(reply(), requestId: "request-a")
        XCTAssertNil(state.result)
        XCTAssertFalse(state.isChecking)
        XCTAssertFalse(state.failed)
    }

    func testFailureOrInvalidNumberCannotAppearAsSuccessfulZeroLatency() {
        for (status, milliseconds) in [("failed", 0.0), ("success", Double.nan), ("success", -1.0)] {
            var state = NodeLatencyState()
            state.synchronize(connectionId: "session-a")
            XCTAssertTrue(state.begin(testId: "request-a"))
            state.accept(reply(state: status, ms: milliseconds), requestId: "request-a")
            XCTAssertTrue(state.failed)
            XCTAssertNil(state.result)
        }
    }
}
