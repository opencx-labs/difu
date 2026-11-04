import DifuKit
import SwiftUI

/// Identifies a saved server in the Settings navigation stack.
struct ServerRoute: Hashable {
    let id: String
}

/// Appearance screens in the Settings navigation stack.
enum AppearanceRoute: Hashable {
    case theme
}

struct SettingsView: View {
    @Environment(AppModel.self) private var app
    @State private var addingServer = false

    var body: some View {
        NavigationStack {
            ThemedForm {
                Section {
                    ForEach(app.servers) { server in
                        NavigationLink(value: ServerRoute(id: server.id)) {
                            ServerRow(server: server)
                        }
                    }
                    Button {
                        addingServer = true
                    } label: {
                        Label("Add Server", systemImage: "plus")
                    }
                } header: {
                    Text("Servers")
                } footer: {
                    Text("difu connects to every saved computer and server while the app is open.")
                }

                Section("Appearance") {
                    NavigationLink(value: AppearanceRoute.theme) {
                        LabeledContent("Theme", value: ThemeStore.shared.theme?.name ?? "Default")
                    }
                }

                LockSection()

                Section("About") {
                    LabeledContent("This phone", value: UIDevice.current.name)
                    LabeledContent("App version", value: appVersion)
                    LabeledContent("Protocol", value: "v\(remoteProtocolVersion)")
                }
            }
            .navigationTitle("Settings")
            .navigationDestination(for: ServerRoute.self) { route in
                if let server = app.server(route.id) {
                    ServerSettingsView(server: server)
                } else {
                    ContentUnavailableView(
                        "Server removed", systemImage: "server.rack",
                        description: Text("This server is no longer saved on this phone."))
                        .themedBackground()
                }
            }
            .navigationDestination(for: AppearanceRoute.self) { _ in ThemeSettingsView() }
            .sheet(isPresented: $addingServer) { AddServerSheet() }
        }
    }

    private var appVersion: String {
        let info = Bundle.main.infoDictionary
        let version = info?["CFBundleShortVersionString"] as? String ?? "?"
        let build = info?["CFBundleVersion"] as? String ?? "?"
        return "\(version) (\(build))"
    }
}

/// The app lock toggle.
private struct LockSection: View {
    @Environment(AppLock.self) private var lock

    var body: some View {
        Section {
            Toggle(
                "Require \(lock.methodName)",
                isOn: Binding(
                    get: { lock.enabled },
                    set: { on in Task { await lock.setEnabled(on) } }))
                // Also re-reads `enabled` when a cancelled check leaves it on.
                .disabled(lock.authenticating)
        } header: {
            Text("Security")
        } footer: {
            Text("Asks when difu opens and after 5 minutes in the background.")
        }
    }
}

/// A saved server in the Settings list.
struct ServerRow: View {
    let server: ServerConnection

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: server.isServer ? "server.rack" : "laptopcomputer")
                .foregroundStyle(Color.accent)
                .frame(width: 26)
            VStack(alignment: .leading, spacing: 2) {
                Text(server.name)
                    .lineLimit(1)
                ConnectionStatusLabel(server: server)
                    .font(.caption)
            }
        }
    }
}

struct ConnectionStatusLabel: View {
    let server: ServerConnection

    var body: some View {
        switch server.state {
        case .connected:
            Label("Connected", systemImage: "circle.fill")
                .foregroundStyle(Color.success)
                .labelStyle(StatusLabelStyle())
        case .connecting(let failures, _):
            Label(failures == 0 ? "Connecting…" : "Unreachable", systemImage: "circle.fill")
                .foregroundStyle(failures == 0 ? Color.secondary : Color.warning)
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
