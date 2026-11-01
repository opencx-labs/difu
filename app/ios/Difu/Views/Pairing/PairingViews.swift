import DifuKit
import SwiftUI

/// First launch: explains how to pair and offers scan, paste or password.
struct WelcomeView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 28) {
                    VStack(spacing: 14) {
                        Image(systemName: "chevron.right.square.fill")
                            .font(.system(size: 64))
                            .foregroundStyle(Color.accentColor)
                            .symbolRenderingMode(.hierarchical)
                        Text("difu")
                            .font(.system(size: 40, weight: .bold, design: .monospaced))
                        Text("Control your coding agents and pull request reviews from your iPhone.")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                    }
                    .padding(.top, 40)

                    if let notice = app.notice {
                        Label(notice, systemImage: "exclamationmark.triangle.fill")
                            .font(.callout)
                            .foregroundStyle(.orange)
                            .padding(12)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(Color.orange.opacity(0.1), in: RoundedRectangle(cornerRadius: 12))
                    }

                    PairingInstructions()
                        .padding(16)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color.codeBackground, in: RoundedRectangle(cornerRadius: 14))

                    PairingButtons()

                    Text("A Mac on your local network needs this phone on the same Wi-Fi. Cloud servers work from anywhere.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
                .padding(.horizontal, 24)
                .padding(.bottom, 32)
            }
            .scrollBounceBehavior(.basedOnSize)
        }
    }
}

/// How to pair with a Mac or a cloud server.
struct PairingInstructions: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            row("laptopcomputer", "On your Mac", "Open difu, press ⌘K, run /remote-control and scan the QR code.")
            row(
                "server.rack", "On a difu server",
                "Connect with its address and server password, or scan the QR code from `difu server pair <name>` on your Mac.")
        }
    }

    private func row(_ icon: String, _ title: String, _ text: LocalizedStringKey) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Image(systemName: icon)
                .foregroundStyle(Color.accentColor)
                .frame(width: 24)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.body.weight(.semibold))
                Text(text).font(.callout).foregroundStyle(.secondary)
            }
        }
    }
}

/// Scan / paste / password buttons plus pairing progress and errors.
struct PairingButtons: View {
    /// Called after a server was added.
    var onPaired: () -> Void = {}

    @Environment(AppModel.self) private var app
    @State private var scanning = false
    @State private var pasting = false
    @State private var joining = false

    var body: some View {
        VStack(spacing: 12) {
            PairingStatus()

            Button {
                scanning = true
            } label: {
                Label("Scan QR Code", systemImage: "qrcode.viewfinder")
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 6)
            }
            .buttonStyle(.borderedProminent)
            .foregroundStyle(Color.onAccent)
            .controlSize(.large)

            Button {
                pasting = true
            } label: {
                Label("Paste Pairing Link", systemImage: "link")
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 4)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)

            Button {
                joining = true
            } label: {
                Label("Use Address and Password", systemImage: "key")
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 4)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
        }
        .disabled(isPairing)
        .sheet(isPresented: $scanning) {
            NavigationStack {
                ScannerScreen { code in
                    scanning = false
                    pair { await app.pair(text: code) }
                }
                .navigationTitle("Scan Pairing Code")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { scanning = false }
                    }
                }
            }
        }
        .sheet(isPresented: $pasting) {
            PasteLinkSheet { text in
                pair { await app.pair(text: text) }
            }
        }
        .sheet(isPresented: $joining) {
            PasswordJoinSheet(onPaired: onPaired)
        }
    }

    private var isPairing: Bool {
        if case .pairing = app.pairing { return true }
        return false
    }

    private func pair(_ action: @escaping () async -> Bool) {
        Task {
            if await action() {
                Haptics.success()
                onPaired()
            }
        }
    }
}

/// "Pairing with …" progress or the last pairing error.
struct PairingStatus: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        switch app.pairing {
        case .pairing(let name):
            HStack(spacing: 10) {
                ProgressView()
                Text("Pairing with \(name)…")
            }
            .padding(.vertical, 8)
        case .failed(let message):
            Label {
                Text(message).textSelection(.enabled)
            } icon: {
                Image(systemName: "xmark.octagon.fill")
            }
            .font(.callout)
            .foregroundStyle(.red)
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.red.opacity(0.08), in: RoundedRectangle(cornerRadius: 12))
        case .idle:
            EmptyView()
        }
    }
}

/// Pairs with a cloud server using its address and server password.
struct PasswordJoinSheet: View {
    var onPaired: () -> Void = {}

    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var address = ""
    @State private var password = ""
    @FocusState private var focus: Field?

    enum Field { case address, password }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("203.0.113.5 or my-server.example.com", text: $address)
                        .font(.monoBody)
                        .keyboardType(.URL)
                        .textContentType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .focused($focus, equals: .address)
                        .submitLabel(.next)
                        .onSubmit { focus = .password }
                } header: {
                    Text("Address")
                } footer: {
                    Text("The server's IP address or DNS name. difu uses port 3438 unless you add one, like host:8443.")
                }
                Section {
                    SecureField("Server password", text: $password)
                        .textContentType(.password)
                        .focused($focus, equals: .password)
                        .submitLabel(.join)
                        .onSubmit { join() }
                } footer: {
                    Text("Set when the server was added, or with `difu remote password` on the server. The password proves the server is genuine before this phone trusts it.")
                }
                if app.pairing != .idle {
                    Section {
                        PairingStatus()
                            .listRowInsets(EdgeInsets())
                            .listRowBackground(Color.clear)
                    }
                }
            }
            .navigationTitle("Add Server")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if isPairing {
                        ProgressView()
                    } else {
                        Button("Connect") { join() }
                            .disabled(!canJoin)
                    }
                }
            }
            .disabled(isPairing)
            .onAppear {
                app.pairing = .idle
                focus = .address
            }
        }
        .presentationDetents([.large])
        .interactiveDismissDisabled(isPairing)
    }

    private var isPairing: Bool {
        if case .pairing = app.pairing { return true }
        return false
    }

    private var canJoin: Bool {
        !address.trimmingCharacters(in: .whitespaces).isEmpty && !password.isEmpty
    }

    private func join() {
        guard canJoin, !isPairing else { return }
        Task {
            if await app.join(address: address, password: password) {
                Haptics.success()
                dismiss()
                onPaired()
            }
        }
    }
}

/// Fallback when the camera isn't an option.
struct PasteLinkSheet: View {
    let submit: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("difu://pair?…", text: $text, axis: .vertical)
                        .font(.monoCaption)
                        .lineLimit(3...8)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    PasteButton(payloadType: String.self) { strings in
                        if let first = strings.first { text = first }
                    }
                } footer: {
                    if let error {
                        Text(error).foregroundStyle(.red)
                    } else {
                        Text("Copy the link shown under the QR code in /remote-control.")
                    }
                }
            }
            .navigationTitle("Pairing Link")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Pair") { pair() }
                        .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
        .presentationDetents([.medium])
    }

    private func pair() {
        do {
            _ = try PairingLink(text)
            submit(text)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }
    }
}

/// Add Server from Settings: scan, paste or password, closing on success.
struct AddServerSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 24) {
                    PairingInstructions()
                        .padding(16)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color.codeBackground, in: RoundedRectangle(cornerRadius: 14))
                    PairingButtons { dismiss() }
                }
                .padding(.horizontal, 24)
                .padding(.top, 16)
            }
            .navigationTitle("Add Server")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .onAppear { app.pairing = .idle }
        }
    }
}
