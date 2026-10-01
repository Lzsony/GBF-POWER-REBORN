import Foundation

@MainActor
final class AppModel: ObservableObject {
    @Published var gate = AuthorizationGate()
    @Published var snapshot: [String: Any] = [:]
    @Published var busy = false
    @Published var errorCode: String?
    @Published var certificateURL: URL?
    @Published var nodeLatency = NodeLatencyState()
    let vpn = VPNController()
    private var bootstrapped = false
    private var connectionCheckTask: Task<Void, Never>?

    var canCheckConnection: Bool {
        gate.isAuthorized && vpn.status == .connected &&
        (snapshot["acceleration"] as? [String: Any])?["state"] as? String == "connected" &&
        nodeLatency.connectionId != nil
    }

    func invalidateNodeLatency() {
        connectionCheckTask?.cancel()
        connectionCheckTask = nil
        nodeLatency.synchronize(connectionId: nil)
    }

    private func decodeCheck(_ value: [String: Any]) throws -> ConnectionCheckSnapshot {
        try JSONDecoder().decode(ConnectionCheckSnapshot.self, from: JSONSerialization.data(withJSONObject: value))
    }

    private func synchronizeCheck() {
        guard gate.isAuthorized, vpn.status == .connected,
              (snapshot["acceleration"] as? [String: Any])?["state"] as? String == "connected",
              let value = snapshot["connectionCheck"] as? [String: Any],
              let check = try? decodeCheck(value) else { invalidateNodeLatency(); return }
        if nodeLatency.synchronize(connectionId: check.connectionId) {
            connectionCheckTask?.cancel()
            connectionCheckTask = nil
        }
        if let id = nodeLatency.testId { nodeLatency.accept(check, requestId: id) }
    }

    func checkConnection() {
        guard canCheckConnection else { return }
        let id = UUID().uuidString
        guard nodeLatency.begin(testId: id) else { return }
        connectionCheckTask = Task { [weak self] in
            guard let self else { return }
            do {
                var check = try self.decodeCheck(await self.vpn.query("startConnectionCheck", fields: ["testId": id]))
                while !Task.isCancelled && self.nodeLatency.testId == id {
                    guard check.connectionId == self.nodeLatency.connectionId else {
                        self.invalidateNodeLatency()
                        break
                    }
                    guard check.result?.testId == id else {
                        self.nodeLatency.fail(requestId: id)
                        break
                    }
                    self.nodeLatency.accept(check, requestId: id)
                    if !self.nodeLatency.isChecking { return }
                    try await Task.sleep(nanoseconds: 500_000_000)
                    check = try self.decodeCheck(await self.vpn.query("connectionCheckStatus", fields: ["testId": id]))
                }
            } catch {
                if !Task.isCancelled { self.nodeLatency.fail(requestId: id) }
            }
            // Scoped by request ID: an old cancellation cannot stop a newer check.
            _ = try? await self.vpn.query("cancelConnectionCheck", fields: ["testId": id])
        }
    }

    func bootstrap() async {
        guard !bootstrapped else { return }
        bootstrapped = true
        await verify()
    }

    func verify(code: String? = nil) async {
        guard !busy else { return }
        busy = true
        invalidateNodeLatency()
        snapshot = [:]
        let attempt = gate.begin()
        defer { busy = false }
        do {
            try await vpn.load()
            let result: [String: Any]
            if vpn.ownsCore {
                guard code == nil else { throw CoreFailure("STOP_REQUIRED") }
                let expected = try await CoreClient.shared.call("buildInfo")
                let live = try await vpn.query("refresh")
                if live["profileTag"] as? String == expected["profileTag"] as? String &&
                    live["coreVersionTag"] as? String == expected["coreVersionTag"] as? String {
                    result = live
                } else {
                    // An old extension cannot authorize acceleration for a new deployment.
                    try await vpn.stop()
                    result = try await CoreClient.shared.local("refresh")
                }
            } else {
                result = try await CoreClient.shared.local(code == nil ? "refresh" : "activate", fields: code.map { ["code": $0] } ?? [:])
            }
            let active = (result["authorization"] as? [String: Any])?["state"] as? String == "active"
            gate.complete(attempt, active: active)
            if gate.state == .authorized { snapshot = result; synchronizeCheck() }
        } catch {
            gate.complete(attempt, active: false, error: (error as? CoreFailure)?.code ?? "AUTH_UNAVAILABLE")
        }
    }

    func refreshStatus() async {
        guard !busy, gate.state == .authorized else { return }
        busy = true
        defer { busy = false }
        do {
            if vpn.ownsCore {
                snapshot = try await vpn.query("status")
            } else {
                snapshot = try await CoreClient.shared.local("refresh")
            }
            synchronizeCheck()
        } catch let error as CoreFailure {
            if ["AUTH_REVOKED", "AUTH_INVALID", "AUTH_REQUIRED"].contains(error.code) {
                snapshot = [:]
                gate.revoke(error.code)
                invalidateNodeLatency()
            } else { errorCode = error.code }
        } catch { errorCode = "AUTH_UNAVAILABLE" }
    }

    func perform(_ operation: String, fields: [String: Any] = [:]) async {
        // Stopping remains available during verification, outages, and revocation.
        if operation == "stop" {
            invalidateNodeLatency()
            do { try await vpn.stop() }
            catch { errorCode = (error as? CoreFailure)?.code ?? "NATIVE_OPERATION_FAILED" }
            return
        }
        guard !busy, gate.permits(operation) else { return }
        if operation == "start" {
            let lines = snapshot["accelerationLines"] as? [[String: Any]] ?? []
            guard gate.canStart(hasAvailableNodes: !lines.isEmpty) else {
                errorCode = "AUTH_LINE_DENIED"
                return
            }
        }
        busy = true
        errorCode = nil
        defer { busy = false }
        do {
            switch operation {
            case "start": try await vpn.start()
            case "unbind":
                invalidateNodeLatency()
                try await vpn.stop()
                _ = try await CoreClient.shared.local("unbind")
                snapshot = [:]
                gate.revoke("AUTH_REQUIRED")
            default:
                guard !vpn.ownsCore else { throw CoreFailure("STOP_REQUIRED") }
                let result = try await CoreClient.shared.local(operation, fields: fields)
                if operation == "createCertificate", let pem = result["pem"] as? String {
                    certificateURL = try CertificateProfile.export(pem)
                } else { snapshot = result }
            }
        } catch let error as CoreFailure {
            if ["AUTH_REVOKED", "AUTH_INVALID", "AUTH_REQUIRED"].contains(error.code) {
                snapshot = [:]
                gate.revoke(error.code)
                invalidateNodeLatency()
            } else { errorCode = error.code }
        } catch { errorCode = "NATIVE_OPERATION_FAILED" }
    }
}

enum CertificateProfile {
    static func export(_ pem: String) throws -> URL {
        let base64 = pem.components(separatedBy: .newlines).filter { !$0.hasPrefix("-----") }.joined()
        guard let certificate = Data(base64Encoded: base64) else { throw CoreFailure("CERTIFICATE_MISSING") }
        let profile: [String: Any] = [
            "PayloadType": "Configuration", "PayloadVersion": 1,
            "PayloadIdentifier": "cc.lzsony.gpr.ios.cache-ca", "PayloadUUID": UUID().uuidString,
            "PayloadDisplayName": "GBF POWER REBORN Cache CA",
            "PayloadContent": [["PayloadType": "com.apple.security.root", "PayloadVersion": 1,
                                "PayloadIdentifier": "cc.lzsony.gpr.ios.cache-ca.root", "PayloadUUID": UUID().uuidString,
                                "PayloadDisplayName": "GBF POWER REBORN Cache CA", "PayloadContent": certificate]]
        ]
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("GPR-Cache-CA.mobileconfig")
        try PropertyListSerialization.data(fromPropertyList: profile, format: .xml, options: 0).write(to: url, options: .atomic)
        return url
    }
}
