import Foundation

struct CoreFailure: Error, LocalizedError {
    let code: String
    init(_ code: String) { self.code = code }
    var errorDescription: String? { code }
}

/// This queue owns all FFI calls in a process. Rust also owns an interprocess lease.
final class CoreClient: @unchecked Sendable {
    static let shared = CoreClient()
    private let queue = DispatchQueue(label: "cc.lzsony.gpr.core", qos: .userInitiated)

    private func invoke(_ request: [String: Any]) throws -> [String: Any] {
        let data = try JSONSerialization.data(withJSONObject: request)
        guard let text = String(data: data, encoding: .utf8) else { throw CoreFailure("INVALID_COMMAND") }
        let pointer = text.withCString { gpr_core_call($0) }
        guard let pointer else { throw CoreFailure("INTERNAL_FAILURE") }
        defer { gpr_core_free(pointer) }
        let output = Data(String(cString: pointer).utf8)
        guard let response = try JSONSerialization.jsonObject(with: output) as? [String: Any] else {
            throw CoreFailure("INTERNAL_FAILURE")
        }
        guard response["ok"] as? Bool == true else {
            let error = response["error"] as? [String: Any]
            throw CoreFailure(error?["code"] as? String ?? "INTERNAL_FAILURE")
        }
        return response["value"] as? [String: Any] ?? [:]
    }

    func call(_ command: String, fields: [String: Any] = [:]) async throws -> [String: Any] {
        try await withCheckedThrowingContinuation { continuation in
            queue.async {
                do {
                    var request = fields
                    request["command"] = command
                    continuation.resume(returning: try self.invoke(request))
                } catch { continuation.resume(throwing: error) }
            }
        }
    }

    func local(_ command: String, fields: [String: Any] = [:]) async throws -> [String: Any] {
        try await withCheckedThrowingContinuation { continuation in
            queue.async {
                do {
                    _ = try self.invoke(["command": "open", "root": try SharedEnvironment.root().path, "tunnel": false])
                    defer { _ = try? self.invoke(["command": "close"]) }
                    var request = fields
                    request["command"] = command
                    continuation.resume(returning: try self.invoke(request))
                } catch { continuation.resume(throwing: error) }
            }
        }
    }
}
