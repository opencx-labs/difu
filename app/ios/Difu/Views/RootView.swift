import DifuKit
import SwiftUI

struct RootView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Group {
            if app.isPaired {
                TabView {
                    SessionListView()
                        .tabItem { Label("Agents", systemImage: "sparkles.rectangle.stack") }
                    ReviewsView()
                        .tabItem { Label("Reviews", systemImage: "arrow.triangle.pull") }
                    SettingsView()
                        .tabItem { Label("Settings", systemImage: "gearshape") }
                }
                .safeAreaInset(edge: .top, spacing: 0) {
                    VStack(spacing: 0) {
                        DevicePairedBanner()
                        ConnectionBanner()
                    }
                }
            } else {
                WelcomeView()
            }
        }
        // Covers the first frame at launch until the lock window is up.
        .overlay {
            if app.lock.isShielded { LockScreen() }
        }
        .alert(
            pendingLinkTitle,
            isPresented: Binding(get: { app.pendingLink != nil }, set: { if !$0 { app.pendingLink = nil } }),
            presenting: app.pendingLink
        ) { link in
            Button("Cancel", role: .cancel) {}
            Button(app.savedServer(for: link) == nil ? "Add" : "Pair Again") {
                Task { await app.pair(link) }
            }
        } message: { link in
            if let saved = app.savedServer(for: link) {
                Text("\(saved.name) is already saved. Pairing again replaces its saved pairing.")
            } else {
                Text("difu connects to \(link.name) as well as your other saved servers.")
            }
        }
        .alert(
            "Server removed",
            isPresented: Binding(
                get: { app.isPaired && app.notice != nil }, set: { if !$0 { app.notice = nil } }),
            presenting: app.notice
        ) { _ in
            Button("OK", role: .cancel) {}
        } message: { notice in
            Text(notice)
        }
    }

    private var pendingLinkTitle: String {
        guard let link = app.pendingLink else { return "Add server?" }
        return app.savedServer(for: link) == nil ? "Add \(link.name)?" : "Pair with \(link.name) again?"
    }
}

/// "Connecting to <server>…" / "Can't reach <server>".
struct ConnectionBanner: View {
    @Environment(AppModel.self) private var app

    @State private var showConnecting = false

    var body: some View {
        let unreachable = app.servers.filter(\.isUnreachable)
        let connecting = app.servers.filter(isFirstAttempt)
        // A VStack (not Group) so `.task` runs even while nothing is shown.
        VStack(spacing: 0) {
            if !unreachable.isEmpty {
                HStack(spacing: 8) {
                    Image(systemName: "wifi.exclamationmark")
                    Text(unreachableText(unreachable))
                        .lineLimit(1)
                        .minimumScaleFactor(0.8)
                    Spacer(minLength: 4)
                    Button("Retry") { app.retryAll() }
                        .font(.caption.weight(.semibold))
                        .buttonStyle(.borderless)
                }
                .bannerStyle(color: .warning)
            } else if showConnecting, !connecting.isEmpty {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.mini)
                    Text(connecting.count == 1 ? "Connecting to \(connecting[0].name)…" : "Connecting…")
                }
                .bannerStyle(color: nil)
            }
        }
        .animation(.easeInOut(duration: 0.25), value: unreachable.map(\.id))
        .animation(.easeInOut(duration: 0.25), value: showConnecting)
        .task(id: connecting.map(\.id)) {
            // Quick reconnects (e.g. returning to the app) shouldn't flash a banner.
            showConnecting = false
            guard !connecting.isEmpty else { return }
            try? await Task.sleep(for: .seconds(1.2))
            if !Task.isCancelled { showConnecting = true }
        }
    }

    private func isFirstAttempt(_ server: ServerConnection) -> Bool {
        if case .connecting(0, _) = server.state { return true }
        return false
    }

    private func unreachableText(_ servers: [ServerConnection]) -> String {
        servers.count == 1 ? "Can't reach \(servers[0].name)" : "Can't reach \(servers.count) servers"
    }
}

/// "New device paired: <name>" with a Revoke action (`device.paired`).
struct DevicePairedBanner: View {
    @Environment(AppModel.self) private var app
    @State private var revoking = false
    @State private var error: String?

    var body: some View {
        VStack(spacing: 0) {
            if let notice = app.pairedDevices.first {
                HStack(spacing: 10) {
                    Image(systemName: "iphone.badge.exclamationmark")
                        .font(.body)
                    VStack(alignment: .leading, spacing: 1) {
                        Text("New device paired: \(notice.device.name)")
                            .font(.caption.weight(.semibold))
                            .lineLimit(1)
                        Text(subtitle(notice))
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 4)
                    if revoking {
                        ProgressView().controlSize(.mini)
                    } else {
                        Button("Revoke", role: .destructive) { revoke(notice) }
                            .font(.caption.weight(.semibold))
                            .buttonStyle(.borderless)
                    }
                    Button {
                        app.dismiss(notice)
                    } label: {
                        Image(systemName: "xmark")
                            .font(.caption.weight(.semibold))
                    }
                    .buttonStyle(.borderless)
                    .foregroundStyle(.secondary)
                    .accessibilityLabel("Dismiss")
                }
                .bannerStyle(color: .info)
            }
        }
        .animation(.easeInOut(duration: 0.25), value: app.pairedDevices)
        .errorAlert($error, title: "Couldn't revoke the device")
    }

    private func subtitle(_ notice: PairedDeviceNotice) -> String {
        var parts = ["\(notice.device.access == .full ? "Full" : "Standard") access"]
        if app.showsServerBadges { parts.insert("on \(notice.serverName)", at: 0) }
        parts.append("not you? Revoke it")
        return parts.joined(separator: " · ")
    }

    private func revoke(_ notice: PairedDeviceNotice) {
        guard let server = app.server(notice.serverID) else {
            app.dismiss(notice)
            return
        }
        revoking = true
        Task {
            defer { revoking = false }
            do {
                try await server.api.forgetDevice(notice.device.id)
                Haptics.warning()
                app.dismiss(notice)
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

extension View {
    /// The thin full-width strip used by the connection and device banners.
    fileprivate func bannerStyle(color: Color?) -> some View {
        self
            .font(.caption.weight(.medium))
            .foregroundStyle(color.map { AnyShapeStyle($0) } ?? AnyShapeStyle(.secondary))
            .padding(.horizontal, 16)
            .padding(.vertical, 7)
            .frame(maxWidth: .infinity)
            .background(color.map { AnyShapeStyle($0.opacity(0.14)) } ?? AnyShapeStyle(.bar))
            .transition(.move(edge: .top).combined(with: .opacity))
    }
}
