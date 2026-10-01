import SwiftUI

@main
struct GPRApp: App {
    @StateObject private var model = AppModel()
    @AppStorage("language") private var language = Locale.preferredLanguages.first.map { value in
        value.hasPrefix("ja") ? "ja" : (value.hasPrefix("zh") ? (value.contains("Hans") ? "zh-CN" : "zh-TW") : "en")
    } ?? "en"
    @AppStorage("theme") private var theme = "auto"
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            Dashboard(model: model, vpn: model.vpn, language: $language, theme: $theme)
            .preferredColorScheme(theme == "dark" ? .dark : theme == "light" ? .light : nil)
            .task { await model.bootstrap() }
            .task(id: scenePhase) {
                guard scenePhase == .active else { return }
                while !Task.isCancelled {
                    do { try await Task.sleep(nanoseconds: model.vpn.ownsCore ? 2_000_000_000 : 15_000_000_000) } catch { break }
                    await model.refreshStatus()
                }
            }
        }
    }
}

struct AuthorizationView: View {
    @ObservedObject var model: AppModel
    @Binding var language: String
    @State private var code = ""
    @Environment(\.dismiss) private var dismiss
    private func text(_ key: String) -> String { Strings.get(key, language) }
    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text("GBF POWER REBORN").font(.title2.bold())
                    Text(text("authorizationDescription")).foregroundStyle(.secondary)
                }
                Section {
                    switch model.gate.state {
                    case .checking:
                        ProgressView(text("verifying")).accessibilityIdentifier("authorizationChecking")
                    case .activationRequired:
                        SecureField(text("accessCode"), text: $code)
                            .textInputAutocapitalization(.never).autocorrectionDisabled()
                            .accessibilityIdentifier("accessCode")
                        PasteButton(payloadType: String.self) { values in code = values.first ?? "" }
                        Button(text("activate")) {
                            let submitted = code
                            code = ""
                            Task { await model.verify(code: submitted) }
                        }.disabled(code.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.busy)
                            .accessibilityIdentifier("activate")
                    case .verificationUnavailable:
                        Text(text("verificationUnavailable"))
                        Button(text("retry")) { Task { await model.verify() } }.disabled(model.busy)
                    case .configurationError:
                        Text(text("configurationError"))
                    case .authorized:
                        Label(text("authorized"), systemImage: "checkmark.shield")
                        Button(text("verifyAuthorization")) { Task { await model.verify() } }.disabled(model.busy)
                    }
                    if let error = model.gate.errorCode {
                        Text(Strings.error(error, language)).foregroundStyle(.red).accessibilityIdentifier("authorizationError")
                    }
                }
                Section { LanguagePicker(language: $language) }
                Section { Text(text("experimental")).font(.footnote).foregroundStyle(.secondary) }
            }
            .navigationTitle(text("authorization"))
            .toolbar { Button(text("done")) { dismiss() } }
            .onChange(of: model.gate.state) { _, state in
                if state == .authorized { dismiss() }
            }
        }.accessibilityIdentifier("authorizationSheet")
    }
}

struct Dashboard: View {
    @ObservedObject var model: AppModel
    @ObservedObject var vpn: VPNController
    @Binding var language: String
    @Binding var theme: String
    private enum Sheet: String, Identifiable {
        case settings, authorization
        var id: String { rawValue }
    }
    @State private var sheet: Sheet?
    private func text(_ key: String) -> String { Strings.get(key, language) }
    private var lines: [[String: Any]] { model.snapshot["accelerationLines"] as? [[String: Any]] ?? [] }
    private var metrics: [String: Any] { model.snapshot["metrics"] as? [String: Any] ?? [:] }
    var body: some View {
        NavigationStack {
            List {
                Section(text("authorization")) {
                    if model.gate.state == .checking {
                        ProgressView(text("verifying")).accessibilityIdentifier("authorizationChecking")
                    } else {
                        Label(text(model.gate.isAuthorized ? "authorized" : "authorizationRequired"), systemImage: model.gate.isAuthorized ? "checkmark.shield" : "lock.shield")
                    }
                    if let error = model.gate.errorCode {
                        Text(Strings.error(error, language)).foregroundStyle(.secondary)
                    }
                    Button(text("verifyAuthorization")) { sheet = .authorization }
                        .accessibilityIdentifier("openAuthorization")
                }
                Section {
                    Label(text(vpn.status == .connected ? "connected" : vpn.ownsCore ? "connecting" : "stopped"), systemImage: vpn.status == .connected ? "network.badge.shield.half.filled" : "network")
                    Button {
                        if !vpn.ownsCore && !model.gate.isAuthorized { sheet = .authorization }
                        else { Task { await model.perform(vpn.ownsCore ? "stop" : "start") } }
                    } label: {
                        Text(text(vpn.ownsCore ? "stop" : model.gate.isAuthorized ? "start" : "authorizeToStart")).frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent).tint(vpn.ownsCore ? .red : .blue)
                    .disabled(!vpn.ownsCore && (model.busy || (model.gate.isAuthorized && lines.isEmpty)))
                    .accessibilityIdentifier("toggleAcceleration")
                    if vpn.status == .connected {
                        Button(text("checkConnection")) { model.checkConnection() }
                            .disabled(!model.canCheckConnection || model.nodeLatency.isChecking)
                            .accessibilityIdentifier("checkConnection")
                        if model.nodeLatency.isChecking {
                            ProgressView(text("checkingConnection"))
                        } else if let result = model.nodeLatency.result, let milliseconds = result.medianMs {
                            LabeledContent(text("nodeLatency"), value: String(format: "%.1f ms", milliseconds))
                                .accessibilityIdentifier("nodeLatency")
                        } else if model.nodeLatency.failed {
                            Text(text("connectionCheckFailed")).foregroundStyle(.secondary)
                        }
                        if let time = model.nodeLatency.completedAt {
                            HStack {
                                Text(text("checkedAt"))
                                Text(Date(timeIntervalSince1970: Double(time) / 1000), style: .time)
                            }.font(.footnote).foregroundStyle(.secondary)
                        }
                    }
                    if model.gate.isAuthorized && lines.isEmpty { Text(text("noNodes")).foregroundStyle(.secondary) }
                }
                Section(text("statistics")) {
                    LabeledContent(text("requests"), value: String(describing: metrics["requests"] ?? 0))
                    LabeledContent(text("received"), value: ByteCountFormatter.string(fromByteCount: (metrics["received"] as? NSNumber)?.int64Value ?? 0, countStyle: .binary))
                    LabeledContent(text("sent"), value: ByteCountFormatter.string(fromByteCount: (metrics["sent"] as? NSNumber)?.int64Value ?? 0, countStyle: .binary))
                    LabeledContent(text("hitRate"), value: (metrics["hitRate"] as? Double).map { String(format: "%.1f%%", $0) } ?? "—")
                    if vpn.status == .connected, let memory = model.snapshot["memory"] as? [String: Any] {
                        LabeledContent(text("vpnMemory"), value: ByteCountFormatter.string(fromByteCount: (memory["physicalBytes"] as? NSNumber)?.int64Value ?? 0, countStyle: .binary))
                        LabeledContent(text("vpnMemoryPeak"), value: ByteCountFormatter.string(fromByteCount: (memory["peakPhysicalBytes"] as? NSNumber)?.int64Value ?? 0, countStyle: .binary))
                    }
                }
                if let error = model.errorCode { Section { Text(Strings.error(error, language)).foregroundStyle(.red) } }
                Section { Text(text("experimental")).font(.footnote).foregroundStyle(.secondary) }
            }
            .navigationTitle("GBF POWER REBORN")
            .onChange(of: vpn.status) { _, status in
                if status != .connected { model.invalidateNodeLatency() }
            }
            .toolbar { Button { sheet = .settings } label: { Image(systemName: "gearshape") }.accessibilityLabel(text("settings")) }
            .sheet(item: $sheet) { selected in
                switch selected {
                case .settings: SettingsView(model: model, vpn: vpn, language: $language, theme: $theme)
                case .authorization: AuthorizationView(model: model, language: $language)
                }
            }
        }.accessibilityIdentifier("dashboard")
    }
}

struct SettingsView: View {
    @ObservedObject var model: AppModel
    @ObservedObject var vpn: VPNController
    @Binding var language: String
    @Binding var theme: String
    @Environment(\.dismiss) private var dismiss
    @State private var selection = "auto"
    @State private var lineID = ""
    @State private var cache = false
    @State private var confirmUnbind = false
    private func text(_ key: String) -> String { Strings.get(key, language) }
    var body: some View {
        NavigationStack {
            Form {
                Section(text("node")) {
                    Picker(text("selection"), selection: $selection) { Text(text("auto")).tag("auto"); Text(text("manual")).tag("manual") }
                    if selection == "manual" {
                        Picker(text("node"), selection: $lineID) {
                            Text(text("chooseNode")).tag("")
                            ForEach(model.snapshot["accelerationLines"] as? [[String: String]] ?? [], id: \.self) { line in
                                Text(line["name"] ?? "").tag(line["id"] ?? "")
                            }
                        }
                    }
                    Toggle(text("cache"), isOn: $cache)
                    Button(text("save")) {
                        Task { await model.perform("save", fields: ["lineSelection": selection, "selectedLineId": lineID, "httpsCache": cache]) }
                    }.disabled(model.busy || vpn.ownsCore || (selection == "manual" && lineID.isEmpty))
                    if vpn.ownsCore { Text(text("stopBeforeSettings")).font(.footnote) }
                }.disabled(model.busy || vpn.ownsCore || !model.gate.isAuthorized)
                Section(text("cacheManagement")) {
                    Button(text("exportCertificate")) { Task { await model.perform("createCertificate") } }
                    if let url = model.certificateURL { ShareLink(text("shareCertificate"), item: url) }
                    Text(text("certificateInstructions")).font(.footnote).foregroundStyle(.secondary)
                    if let certificate = model.snapshot["certificate"] as? [String: Any] {
                        Text(text(certificate["trusted"] as? Bool == true ? "trusted" : "untrusted"))
                    }
                    Button(text("clearCache"), role: .destructive) { Task { await model.perform("clearCache") } }
                }.disabled(model.busy || vpn.ownsCore || !model.gate.isAuthorized)
                Section {
                    LanguagePicker(language: $language)
                    Picker(text("theme"), selection: $theme) {
                        Text(text("auto")).tag("auto"); Text(text("light")).tag("light"); Text(text("dark")).tag("dark")
                    }
                    LabeledContent(text("version"), value: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "")
                    Button(text("unbind"), role: .destructive) { confirmUnbind = true }.disabled(model.busy || !model.gate.isAuthorized)
                }
                if let error = model.errorCode { Text(Strings.error(error, language)).foregroundStyle(.red) }
            }
            .navigationTitle(text("settings"))
            .toolbar { Button(text("done")) { dismiss() } }
            .confirmationDialog(text("unbindConfirmation"), isPresented: $confirmUnbind, titleVisibility: .visible) {
                Button(text("unbind"), role: .destructive) { Task { await model.perform("unbind") } }
            }
            .onAppear {
                let settings = model.snapshot["settings"] as? [String: Any] ?? [:]
                selection = settings["lineSelection"] as? String ?? "auto"
                lineID = settings["selectedLineId"] as? String ?? ""
                cache = settings["httpsCache"] as? Bool ?? false
            }
        }
    }
}

struct LanguagePicker: View {
    @Binding var language: String
    var body: some View {
        Picker(Strings.get("language", language), selection: $language) {
            Text("简体").tag("zh-CN"); Text("繁體").tag("zh-TW"); Text("日本語").tag("ja"); Text("English").tag("en")
        }
    }
}
