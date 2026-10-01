import Foundation
import NetworkExtension

@MainActor
final class VPNController: ObservableObject {
    @Published private(set) var status: NEVPNStatus = .invalid
    private var manager: NETunnelProviderManager?
    private var observer: NSObjectProtocol?
    var ownsCore: Bool { [.connecting, .connected, .reasserting, .disconnecting].contains(status) }

    init() {
        observer = NotificationCenter.default.addObserver(forName: .NEVPNStatusDidChange, object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor [weak self] in self?.status = self?.manager?.connection.status ?? .invalid }
        }
    }
    deinit { if let observer { NotificationCenter.default.removeObserver(observer) } }

    func load() async throws {
        let identifier = try SharedEnvironment.value("GPRTunnelIdentifier")
        manager = try await NETunnelProviderManager.loadAllFromPreferences().first {
            ($0.protocolConfiguration as? NETunnelProviderProtocol)?.providerBundleIdentifier == identifier
        }
        status = manager?.connection.status ?? .invalid
    }

    func start() async throws {
        if manager == nil { manager = NETunnelProviderManager() }
        guard let manager else { throw CoreFailure("NATIVE_OPERATION_FAILED") }
        let configuration = NETunnelProviderProtocol()
        configuration.providerBundleIdentifier = try SharedEnvironment.value("GPRTunnelIdentifier")
        configuration.serverAddress = "GBF POWER REBORN"
        configuration.disconnectOnSleep = false
        manager.protocolConfiguration = configuration
        manager.localizedDescription = "GBF POWER REBORN"
        manager.isEnabled = true
        try await manager.saveToPreferences()
        try await manager.loadFromPreferences()
        try manager.connection.startVPNTunnel()
        status = manager.connection.status
    }

    func stop() async throws {
        manager?.connection.stopVPNTunnel()
        for _ in 0..<200 {
            status = manager?.connection.status ?? .invalid
            if !ownsCore { return }
            try await Task.sleep(nanoseconds: 100_000_000)
        }
        throw CoreFailure("STOP_REQUIRED")
    }

    func query(_ command: String, fields: [String: String] = [:]) async throws -> [String: Any] {
        guard [.connected, .reasserting].contains(status), let session = manager?.connection as? NETunnelProviderSession else {
            throw CoreFailure("AUTH_UNAVAILABLE")
        }
        var request = fields
        request["command"] = command
        let data = try JSONSerialization.data(withJSONObject: request)
        return try await withCheckedThrowingContinuation { continuation in
            let reply = OnceReply(continuation)
            DispatchQueue.main.asyncAfter(deadline: .now() + 12) { reply.finish(.failure(CoreFailure("AUTH_UNAVAILABLE"))) }
            do {
                try session.sendProviderMessage(data) { response in
                    guard let response,
                          let envelope = try? JSONSerialization.jsonObject(with: response) as? [String: Any] else {
                        reply.finish(.failure(CoreFailure("AUTH_UNAVAILABLE"))); return
                    }
                    if envelope["ok"] as? Bool == true {
                        reply.finish(.success(envelope["value"] as? [String: Any] ?? [:]))
                    } else {
                        let error = envelope["error"] as? [String: String]
                        reply.finish(.failure(CoreFailure(error?["code"] ?? "INTERNAL_FAILURE")))
                    }
                }
            } catch { reply.finish(.failure(error)) }
        }
    }
}

private final class OnceReply: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<[String: Any], Error>?
    init(_ continuation: CheckedContinuation<[String: Any], Error>) { self.continuation = continuation }
    func finish(_ result: Result<[String: Any], Error>) {
        lock.lock()
        let pending = continuation
        continuation = nil
        lock.unlock()
        pending?.resume(with: result)
    }
}
