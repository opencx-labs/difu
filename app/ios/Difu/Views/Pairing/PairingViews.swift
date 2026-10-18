import DifuKit
import SwiftUI

/// First launch: explains how to pair and offers scan or paste.
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

                    VStack(alignment: .leading, spacing: 14) {
                        step(1, "Open difu on your Mac.")
                        step(2, "Press ⌘K and run /remote-control.")
                        step(3, "Scan the QR code it shows.")
                    }
                    .padding(16)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Color.codeBackground, in: RoundedRectangle(cornerRadius: 14))

                    PairingButtons()

                    Text("Your phone and computer must be on the same Wi-Fi network.")
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

    private func step(_ number: Int, _ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text("\(number)")
                .font(.callout.monospaced().weight(.bold))
                .foregroundStyle(Color.accentColor)
                .frame(width: 22)
            Text(text)
                .font(.body)
        }
    }
}

/// Scan / paste buttons plus pairing progress and errors.
struct PairingButtons: View {
    @Environment(AppModel.self) private var app
    @State private var scanning = false
    @State private var pasting = false

    var body: some View {
        VStack(spacing: 12) {
            switch app.pairing {
            case .pairing(let computer):
                HStack(spacing: 10) {
                    ProgressView()
                    Text("Pairing with \(computer)…")
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
        }
        .disabled(isPairing)
        .sheet(isPresented: $scanning) {
            NavigationStack {
                ScannerScreen { code in
                    scanning = false
                    Task { await app.pair(text: code) }
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
                Task { await app.pair(text: text) }
            }
        }
    }

    private var isPairing: Bool {
        if case .pairing = app.pairing { return true }
        return false
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

/// Re-pair from Settings: same buttons in a sheet that closes on success.
struct RepairSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var startedFor: String?

    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                Text("Open difu on the other computer, press ⌘K and run /remote-control, then scan the QR code.")
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                    .padding(.top, 24)
                PairingButtons()
                Spacer()
            }
            .padding(.horizontal, 24)
            .navigationTitle("Pair a Computer")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .onAppear {
                app.pairing = .idle
                startedFor = app.credential?.token
            }
            .onChange(of: app.credential?.token) { _, token in
                if token != startedFor { dismiss() }
            }
        }
    }
}
