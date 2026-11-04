import DifuKit
import SwiftUI

/// Devices paired with the server, with Revoke.
struct DevicesView: View {
    let server: ServerConnection

    @State private var devices: [PairedDevice]?
    @State private var error: String?
    @State private var revoking: PairedDevice?
    @State private var actionError: String?

    var body: some View {
        Loadable(value: devices, error: error, loadingText: "Loading devices…", retry: load) { devices in
            ThemedList {
                Section {
                    ForEach(devices) { device in
                        DeviceRow(device: device)
                            .swipeActions(edge: .trailing) {
                                if !device.current {
                                    Button("Revoke", role: .destructive) { revoking = device }
                                }
                            }
                            .contextMenu {
                                if !device.current {
                                    Button("Revoke", systemImage: "xmark.shield", role: .destructive) {
                                        revoking = device
                                    }
                                }
                            }
                    }
                } footer: {
                    Text("Swipe to revoke a device. To remove this phone, use Forget on the previous screen.")
                }
            }
            .listStyle(.insetGrouped)
        }
        .refreshable { await load() }
        .navigationTitle("Paired Devices")
        .navigationBarTitleDisplayMode(.inline)
        .task { await load() }
        .confirmationDialog(
            "Revoke \(revoking?.name ?? "this device")?",
            isPresented: Binding(get: { revoking != nil }, set: { if !$0 { revoking = nil } }),
            titleVisibility: .visible,
            presenting: revoking
        ) { device in
            Button("Revoke", role: .destructive) { revoke(device) }
        } message: { _ in
            Text("It's disconnected from \(server.name) and must pair again to reconnect.")
        }
        .errorAlert($actionError, title: "Couldn't revoke the device")
    }

    private func load() async {
        do {
            devices = try await server.api.devices()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func revoke(_ device: PairedDevice) {
        Task {
            do {
                try await server.api.forgetDevice(device.id)
                Haptics.warning()
                await load()
            } catch {
                actionError = error.localizedDescription
            }
        }
    }
}

private struct DeviceRow: View {
    let device: PairedDevice

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: device.access == .full ? "laptopcomputer" : "iphone.gen3")
                .foregroundStyle(.secondary)
                .frame(width: 24)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(device.name).lineLimit(1)
                    if device.current {
                        Pill(text: "This phone", color: .accent)
                    }
                }
                Text(details)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 4)
            if device.connected {
                Circle().fill(Color.success).frame(width: 8, height: 8)
                    .accessibilityLabel("Connected")
            }
        }
        .padding(.vertical, 2)
    }

    private var details: String {
        var parts = [device.access == .full ? "Full access" : "Standard access"]
        if device.connected {
            parts.append("connected")
        } else if let seen = device.seen {
            parts.append("seen \(Formatting.relative(Date(milliseconds: seen)))")
        }
        if let paired = device.paired {
            parts.append("paired \(Date(milliseconds: paired).formatted(date: .abbreviated, time: .omitted))")
        }
        return parts.joined(separator: " · ")
    }
}
