import DifuKit
import SwiftUI
import UIKit

/// The server's GitHub, Codex and Claude Code logins.
struct LoginsView: View {
    let server: ServerConnection

    @State private var logins: [ServerLogin]?
    @State private var error: String?
    @State private var active: ActiveLogin?
    @State private var starting: LoginTool?
    @State private var tokenSheet = false
    @State private var loggingOut: ServerLogin?
    @State private var actionError: String?

    /// A device-code login waiting for the person to finish it in a browser.
    struct ActiveLogin: Identifiable, Equatable {
        let id = UUID()
        let tool: LoginTool
        let login: DeviceLogin
    }

    var body: some View {
        Loadable(value: logins, error: error, loadingText: "Loading logins…", retry: load) { logins in
            List {
                if let active {
                    Section {
                        DeviceLoginCard(active: active) { self.active = nil }
                    } header: {
                        Text("Log in to \(active.tool.displayName)")
                    }
                }
                Section {
                    ForEach(logins) { login in
                        row(login)
                    }
                } footer: {
                    Text("These are the server's own logins. Every session on \(server.name) uses them.")
                }
            }
            .listStyle(.insetGrouped)
        }
        .refreshable { await load() }
        .navigationTitle("Logins")
        .navigationBarTitleDisplayMode(.inline)
        .task { await load() }
        .task(id: active?.id) { await pollWhileLoggingIn() }
        .sheet(isPresented: $tokenSheet) {
            ClaudeTokenSheet(server: server) { Task { await load() } }
        }
        .confirmationDialog(
            "Log out of \(loggingOut?.displayName ?? "")?",
            isPresented: Binding(get: { loggingOut != nil }, set: { if !$0 { loggingOut = nil } }),
            titleVisibility: .visible,
            presenting: loggingOut
        ) { login in
            Button("Log Out", role: .destructive) { logout(login) }
        } message: { _ in
            Text("Sessions on \(server.name) can't use it until you log in again.")
        }
        .errorAlert($actionError)
    }

    private func row(_ login: ServerLogin) -> some View {
        HStack(spacing: 12) {
            Image(systemName: login.loggedIn ? "checkmark.circle.fill" : "circle.dashed")
                .foregroundStyle(login.loggedIn ? Color.success : .secondary)
                .font(.title3)
            VStack(alignment: .leading, spacing: 2) {
                Text(login.displayName)
                Text(status(login))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 8)
            if starting == login.tool {
                ProgressView()
            } else if login.loggedIn {
                Menu {
                    if login.tool == .claude {
                        Button("Replace Token", systemImage: "key") { tokenSheet = true }
                    }
                    Button("Log Out", systemImage: "rectangle.portrait.and.arrow.right", role: .destructive) {
                        loggingOut = login
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            } else if login.tool.usesDeviceCode {
                Button("Log In") { start(login.tool) }
                    .buttonStyle(.bordered)
                    .disabled(active != nil)
            } else if login.tool == .claude {
                Button("Add Token") { tokenSheet = true }
                    .buttonStyle(.bordered)
            }
        }
    }

    private func status(_ login: ServerLogin) -> String {
        if login.loggedIn {
            return [login.account.map { "Logged in as \($0)" } ?? "Logged in", login.detail]
                .compactMap { $0 }.joined(separator: " · ")
        }
        return login.detail ?? "Not logged in"
    }

    private func load() async {
        do {
            logins = try await server.api.logins()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func start(_ tool: LoginTool) {
        starting = tool
        Task {
            defer { starting = nil }
            do {
                let login = try await server.api.startLogin(tool)
                active = ActiveLogin(tool: tool, login: login)
            } catch {
                actionError = error.localizedDescription
            }
        }
    }

    /// The login finishes in the background on the server; poll until it does.
    private func pollWhileLoggingIn() async {
        guard let tool = active?.tool else { return }
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled else { return }
            await load()
            if logins?.first(where: { $0.tool == tool })?.loggedIn == true {
                Haptics.success()
                active = nil
                return
            }
        }
    }

    private func logout(_ login: ServerLogin) {
        Task {
            do {
                try await server.api.logout(login.toolName)
                await load()
            } catch {
                actionError = error.localizedDescription
            }
        }
    }
}

/// The URL and code for a device-code login, with copy and open buttons.
private struct DeviceLoginCard: View {
    let active: LoginsView.ActiveLogin
    let cancel: () -> Void

    @Environment(\.openURL) private var openURL
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            if let message = active.login.message {
                Text(message)
                    .font(.callout)
                    .textSelection(.enabled)
            }
            if let code = active.login.code {
                HStack {
                    Text(code)
                        .font(.system(.title2, design: .monospaced).weight(.semibold))
                        .textSelection(.enabled)
                    Spacer()
                    Button {
                        UIPasteboard.general.string = code
                        copied = true
                        Haptics.success()
                    } label: {
                        Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
                    }
                    .buttonStyle(.bordered)
                }
            }
            if let url = active.login.url {
                Button {
                    if let code = active.login.code { UIPasteboard.general.string = code }
                    openURL(url)
                } label: {
                    Label(
                        active.login.code == nil ? "Open \(url.host() ?? "Login Page")" : "Copy Code and Open \(url.host() ?? "Login Page")",
                        systemImage: "safari")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .foregroundStyle(Color.onAccent)
                Text(url.absoluteString)
                    .font(.monoCaption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Waiting for you to finish logging in…")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Button("Hide", action: cancel)
                    .font(.caption)
                    .buttonStyle(.borderless)
            }
        }
        .padding(.vertical, 4)
    }
}

/// Pastes a `claude setup-token` token for the server.
private struct ClaudeTokenSheet: View {
    let server: ServerConnection
    let saved: () -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var token = ""
    @State private var saving = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    SecureField("sk-ant-…", text: $token)
                        .font(.monoBody)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    PasteButton(payloadType: String.self) { strings in
                        if let first = strings.first {
                            token = first.trimmingCharacters(in: .whitespacesAndNewlines)
                        }
                    }
                } footer: {
                    Text("Run `claude setup-token` on a computer where you use Claude Code, then paste the token here. It's stored on \(server.name) for its sessions.")
                }
            }
            .navigationTitle("Claude Code Token")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if saving {
                        ProgressView()
                    } else {
                        Button("Save") { save() }
                            .disabled(token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }
            }
            .errorAlert($error, title: "Couldn't save the token")
        }
        .presentationDetents([.medium])
    }

    private func save() {
        saving = true
        let value = token.trimmingCharacters(in: .whitespacesAndNewlines)
        Task {
            defer { saving = false }
            do {
                try await server.api.setLoginToken(value)
                Haptics.success()
                saved()
                dismiss()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}
