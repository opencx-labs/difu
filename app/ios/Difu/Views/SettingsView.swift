import DifuKit
import SwiftUI

struct SettingsView: View {
    @Environment(AppModel.self) private var app
    @State private var showingRepair = false
    @State private var confirmingForget = false

    var body: some View {
        NavigationStack {
            Form {
                if let credential = app.credential {
                    Section("Computer") {
                        LabeledContent("Name", value: credential.computerName)
                        LabeledContent("Status") { ConnectionStatusLabel() }
                        if case .connected(let host) = app.connection {
                            LabeledContent("Connected via") {
                                Text("\(host):\(String(credential.port))").font(.monoCaption)
                            }
                        }
                        LabeledContent("difu version", value: app.server?.version ?? credential.serverVersion ?? "—")
                        if case .connecting(let failures, let error?) = app.connection, failures > 0 {
                            Text(error)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Button("Reconnect Now") { app.retryNow() }
                            .disabled(app.isConnected)
                    }

                    Section {
                        ForEach(credential.candidateHosts, id: \.self) { host in
                            HStack {
                                Text(host).font(.monoCaption)
                                Spacer()
                                if host == credential.preferredHost {
                                    Text("last used").font(.caption).foregroundStyle(.secondary)
                                }
                            }
                        }
                        LabeledContent("Port") { Text(String(credential.port)).font(.monoCaption) }
                    } header: {
                        Text("Addresses")
                    } footer: {
                        Text("Tried in order; refreshed every time the phone connects.")
                    }

                    Section {
                        Button("Pair with a Different Computer") { showingRepair = true }
                        Button("Forget This Computer", role: .destructive) { confirmingForget = true }
                    } footer: {
                        Text("Forgetting only removes the pairing from this phone. To revoke access, remove this phone in /remote-control on your computer.")
                    }
                }

                Section("About") {
                    LabeledContent("This phone", value: UIDevice.current.name)
                    LabeledContent("App version", value: appVersion)
                    LabeledContent("Protocol", value: "v\(remoteProtocolVersion)")
                    if let paired = app.credential?.pairedAt {
                        LabeledContent("Paired", value: paired.formatted(date: .abbreviated, time: .shortened))
                    }
                }
            }
            .navigationTitle("Settings")
            .sheet(isPresented: $showingRepair) { RepairSheet() }
            .confirmationDialog(
                "Forget \(app.computerName)?", isPresented: $confirmingForget, titleVisibility: .visible
            ) {
                Button("Forget", role: .destructive) { app.forget() }
            } message: {
                Text("You'll need to scan the QR code again to reconnect.")
            }
        }
    }

    private var appVersion: String {
        let info = Bundle.main.infoDictionary
        let version = info?["CFBundleShortVersionString"] as? String ?? "?"
        let build = info?["CFBundleVersion"] as? String ?? "?"
        return "\(version) (\(build))"
    }
}

struct ConnectionStatusLabel: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        switch app.connection {
        case .connected:
            Label("Connected", systemImage: "circle.fill")
                .foregroundStyle(Color.success)
                .labelStyle(StatusLabelStyle())
        case .connecting(let failures, _):
            Label(failures == 0 ? "Connecting…" : "Unreachable", systemImage: "circle.fill")
                .foregroundStyle(failures == 0 ? Color.secondary : Color.orange)
                .labelStyle(StatusLabelStyle())
        case .idle:
            Label("Disconnected", systemImage: "circle")
                .foregroundStyle(.secondary)
                .labelStyle(StatusLabelStyle())
        }
    }
}

private struct StatusLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 6) {
            configuration.icon.imageScale(.small).font(.caption2)
            configuration.title
        }
    }
}
