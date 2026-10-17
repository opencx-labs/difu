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
                    ConnectionBanner()
                }
                // Pairing with another computer starts from fresh navigation.
                .id(app.credential?.fingerprint)
            } else {
                WelcomeView()
            }
        }
        .alert(
            "Pair with \(app.pendingLink?.name ?? "this computer")?",
            isPresented: Binding(get: { app.pendingLink != nil }, set: { if !$0 { app.pendingLink = nil } }),
            presenting: app.pendingLink
        ) { link in
            Button("Cancel", role: .cancel) {}
            Button("Pair") { Task { await app.pair(link) } }
        } message: { _ in
            Text("This replaces the pairing with \(app.computerName).")
        }
    }
}

/// "Connecting to <computer>…" / "Can't reach <computer> — same Wi-Fi?"
struct ConnectionBanner: View {
    @Environment(AppModel.self) private var app

    @State private var showConnecting = false

    var body: some View {
        // A VStack (not Group) so `.task` runs even while nothing is shown.
        VStack(spacing: 0) {
            if case .connecting(let failures, _) = app.connection, failures > 0 || showConnecting {
                HStack(spacing: 8) {
                    if failures == 0 {
                        ProgressView().controlSize(.mini)
                        Text("Connecting to \(app.computerName)…")
                    } else {
                        Image(systemName: "wifi.exclamationmark")
                        Text("Can't reach \(app.computerName) — same Wi-Fi?")
                            .lineLimit(1)
                            .minimumScaleFactor(0.8)
                        Spacer(minLength: 4)
                        Button("Retry") { app.retryNow() }
                            .font(.caption.weight(.semibold))
                            .buttonStyle(.borderless)
                    }
                }
                .font(.caption.weight(.medium))
                .foregroundStyle(failures == 0 ? Color.secondary : Color.orange)
                .padding(.horizontal, 16)
                .padding(.vertical, 7)
                .frame(maxWidth: .infinity)
                .background(failures == 0 ? AnyShapeStyle(.bar) : AnyShapeStyle(Color.orange.opacity(0.14)))
                .transition(.move(edge: .top).combined(with: .opacity))
            }
        }
        .animation(.easeInOut(duration: 0.25), value: app.connection)
        .animation(.easeInOut(duration: 0.25), value: showConnecting)
        .task(id: app.connection) {
            // Quick reconnects (e.g. returning to the app) shouldn't flash a banner.
            showConnecting = false
            guard case .connecting(0, _) = app.connection else { return }
            try? await Task.sleep(for: .seconds(1.2))
            if !Task.isCancelled { showConnecting = true }
        }
    }
}
