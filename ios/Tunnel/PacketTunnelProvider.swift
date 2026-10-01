import Foundation
import NetworkExtension
import Darwin

final class PacketTunnelProvider: NEPacketTunnelProvider {
    private var startTask: Task<Void, Never>?
    private var pollTask: Task<Void, Never>?

    private func memoryStatistics() -> [String: UInt64]? {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size)
        let result = withUnsafeMutablePointer(to: &info) { pointer in
            pointer.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
            }
        }
        guard result == KERN_SUCCESS else { return nil }
        return ["physicalBytes": info.phys_footprint,
                "peakPhysicalBytes": UInt64(max(0, info.ledger_phys_footprint_peak))]
    }

    override func startTunnel(options: [String: NSObject]?, completionHandler: @escaping (Error?) -> Void) {
        startTask = Task {
            do {
                _ = try await CoreClient.shared.call("open", fields: ["root": try SharedEnvironment.root().path, "tunnel": true])
                try Task.checkCancellation()
                // The extension performs its own online authorization even for Settings.app starts.
                let result = try await CoreClient.shared.call("start")
                try Task.checkCancellation()
                guard let pac = result["pac"] as? String else { throw CoreFailure("CONFIG_READ_FAILED") }
                let network = NEPacketTunnelNetworkSettings(tunnelRemoteAddress: "198.18.0.1")
                let ipv4 = NEIPv4Settings(addresses: ["198.18.0.2"], subnetMasks: ["255.255.255.252"])
                ipv4.includedRoutes = []
                network.ipv4Settings = ipv4
                let proxy = NEProxySettings()
                proxy.autoProxyConfigurationEnabled = true
                proxy.proxyAutoConfigurationJavaScript = pac
                proxy.matchDomains = [""]
                proxy.excludeSimpleHostnames = true
                network.proxySettings = proxy
                try await setTunnelNetworkSettings(network)
                try Task.checkCancellation()
                pollTask = Task { [weak provider = self] in
                    while !Task.isCancelled {
                        do { try await Task.sleep(nanoseconds: 15_000_000_000) } catch { break }
                        do {
                            let status = try await CoreClient.shared.call("poll")
                            if status["running"] as? Bool != true {
                                provider?.cancelTunnelWithError(CoreFailure("AUTH_LINE_DENIED"))
                                break
                            }
                        } catch let error as CoreFailure {
                            if ["AUTH_REVOKED", "AUTH_INVALID", "AUTH_REQUIRED"].contains(error.code) {
                                provider?.cancelTunnelWithError(error)
                                break
                            }
                        } catch { /* Temporary Control outages do not become revocation. */ }
                    }
                }
                completionHandler(nil)
            } catch {
                _ = try? await CoreClient.shared.call("close")
                completionHandler(error)
            }
        }
    }

    override func stopTunnel(with reason: NEProviderStopReason, completionHandler: @escaping () -> Void) {
        startTask?.cancel()
        pollTask?.cancel()
        Task {
            await startTask?.value
            await pollTask?.value
            _ = try? await CoreClient.shared.call("close")
            completionHandler()
        }
    }

    override func handleAppMessage(_ messageData: Data, completionHandler: ((Data?) -> Void)?) {
        guard messageData.count <= 1024,
              let request = try? JSONSerialization.jsonObject(with: messageData) as? [String: String],
              Set(request.keys).isSubset(of: ["command", "testId"]),
              let command = request["command"], ["status", "refresh", "startConnectionCheck", "connectionCheckStatus", "cancelConnectionCheck"].contains(command) else {
            completionHandler?(nil)
            return
        }
        Task {
            let result: [String: Any]
            do {
                let fields = request["testId"].map { ["testId": $0] } ?? [:]
                var value = try await CoreClient.shared.call(command, fields: fields)
                if command == "status" || command == "refresh", let memory = memoryStatistics() {
                    value["memory"] = memory
                }
                result = ["ok": true, "value": value]
            } catch {
                let code = (error as? CoreFailure)?.code ?? "INTERNAL_FAILURE"
                if ["AUTH_REVOKED", "AUTH_INVALID"].contains(code) { cancelTunnelWithError(error) }
                result = ["ok": false, "error": ["code": code]]
            }
            completionHandler?(try? JSONSerialization.data(withJSONObject: result))
        }
    }
}
