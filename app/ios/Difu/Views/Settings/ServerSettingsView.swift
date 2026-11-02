import DifuKit
import SwiftUI

/// One saved computer or server: connection, disk, logins, repositories,
/// rules, paired devices, and Forget.
struct ServerSettingsView: View {
    let server: ServerConnection

    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var confirmingForget = false
    @State private var forgetting = false
    @State private var disk: ServerDisk?
    @State private var diskError: String?

    var body: some View {
        let credential = server.credential
        Form {
            Section("Connection") {
                LabeledContent("Name", value: server.name)
                LabeledContent("Kind", value: server.isServer ? "Cloud server" : "Computer")
                LabeledContent("Status") { ConnectionStatusLabel(server: server) }
                if case .connected(let host) = server.state {
                    LabeledContent("Connected via") {
                        Text(HostPort(host: host, port: port(for: host)).label).font(.monoCaption)
                    }
                }
                LabeledContent("difu version", value: server.version ?? "—")
                if let access = credential.access {
                    LabeledContent("This phone's access", value: access == .full ? "Full" : "Standard")
                }
                if case .connecting(let failures, let error?) = server.state, failures > 0 {
                    Text(error)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Button("Reconnect Now") { server.retryNow() }
                    .disabled(server.isConnected)
            }

            if server.isServer {
                Section("Disk") {
                    NavigationLink {
                        ServerDiskView(server: server)
                    } label: {
                        if let disk {
                            DiskUsageSummary(disk: disk)
                        } else if let note = diskError ?? (server.isConnected ? nil : "Shown once connected.") {
                            VStack(alignment: .leading, spacing: 2) {
                                Label("Disk Space", systemImage: "internaldrive")
                                Text(note)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        } else {
                            HStack {
                                ProgressView()
                                Text("Loading disk space…").foregroundStyle(.secondary)
                            }
                        }
                    }
                }

                Section("Server") {
                    NavigationLink {
                        LoginsView(server: server)
                    } label: {
                        Label("Logins", systemImage: "person.badge.key")
                    }
                    NavigationLink {
                        ServerRepositoriesView(server: server)
                    } label: {
                        Label("Repositories", systemImage: "shippingbox")
                    }
                }
            }

            Section {
                NavigationLink {
                    RulesEditorView(server: server)
                } label: {
                    Label("Repository Rules", systemImage: "list.bullet.rectangle")
                }
                NavigationLink {
                    DevicesView(server: server)
                } label: {
                    Label("Paired Devices", systemImage: "iphone.gen3")
                }
            }

            Section {
                ForEach(credential.candidateEndpoints, id: \.self) { endpoint in
                    HStack {
                        Text(endpoint.label).font(.monoCaption)
                        Spacer()
                        if endpoint.host == credential.preferredHost {
                            Text("last used").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            } header: {
                Text("Addresses")
            } footer: {
                Text("Tried in order; refreshed every time the phone connects.")
            }

            Section {
                Button(role: .destructive) {
                    confirmingForget = true
                } label: {
                    if forgetting {
                        HStack {
                            ProgressView()
                            Text("Revoking…")
                        }
                    } else {
                        Text(server.isServer ? "Forget This Server" : "Forget This Computer")
                    }
                }
                .disabled(forgetting)
            } footer: {
                Text("Paired \(credential.pairedAt.formatted(date: .abbreviated, time: .shortened)). Forgetting revokes this phone on \(server.name), then removes it from this phone.")
            }
        }
        .navigationTitle(server.name)
        .navigationBarTitleDisplayMode(.inline)
        .task(id: server.isConnected) { await loadDisk() }
        .confirmationDialog(
            "Forget \(server.name)?", isPresented: $confirmingForget, titleVisibility: .visible
        ) {
            Button("Forget", role: .destructive) { forget() }
        } message: {
            Text("This phone loses access until you pair again.")
        }
    }

    /// Reloads on appear (so it's fresh after cleaning up) and on reconnect.
    private func loadDisk() async {
        guard server.isServer, server.isConnected else { return }
        do {
            disk = try await server.api.disk()
            diskError = nil
        } catch is CancellationError {
            return
        } catch {
            diskError = ServerDiskView.message(for: error, on: server)
        }
    }

    private func port(for host: String) -> Int {
        server.credential.candidateEndpoints.first { $0.host == host }?.port ?? server.credential.port
    }

    private func forget() {
        forgetting = true
        Task {
            let warning = await app.forget(server)
            forgetting = false
            if let warning { app.notice = warning }
            dismiss()
        }
    }
}

extension HostPort {
    /// `host:port`, with IPv6 hosts in brackets.
    var label: String {
        host.contains(":") ? "[\(host)]:\(port)" : "\(host):\(port)"
    }
}
