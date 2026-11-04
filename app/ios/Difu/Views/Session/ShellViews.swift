import DifuKit
import SwiftUI

/// The session's shells, as the terminal's Shells control lists them: shared
/// shells, running `!` commands and Codex's background shells. Open one to
/// read and type into it, swipe to terminate it, or start a new one.
struct ShellsSheet: View {
    let sessionID: String
    /// Coding sessions with a worktree can open shells.
    let canOpen: Bool

    @Environment(ServerConnection.self) private var server
    @Environment(\.dismiss) private var dismiss
    @State private var shells: [ShellInfo]?
    @State private var error: String?
    @State private var path: [String] = []
    @State private var runningCommand = false
    @State private var command = ""
    @State private var actionError: String?

    var body: some View {
        NavigationStack(path: $path) {
            Loadable(value: shells, error: error, loadingText: "Loading shells…", retry: load) { shells in
                ThemedList {
                    ForEach(shells) { shell in
                        NavigationLink(value: shell.id) { ShellRow(shell: shell) }
                            .swipeActions(edge: .trailing) {
                                if shell.canClose {
                                    Button(role: .destructive) {
                                        close(shell)
                                    } label: {
                                        Label("Terminate", systemImage: "xmark.circle")
                                    }
                                }
                            }
                    }
                }
                .overlay {
                    if shells.isEmpty {
                        ContentUnavailableView {
                            Label("No shells", systemImage: "terminal")
                        } description: {
                            Text("Open a shell in the session's worktree, shared with difu and the agent.")
                        } actions: {
                            if canOpen {
                                Button("Open Shell") { open(command: nil) }
                                    .buttonStyle(.borderedProminent)
                            }
                        }
                    }
                }
                .refreshable { await load() }
            }
            .navigationTitle("Shells")
            .navigationBarTitleDisplayMode(.inline)
            .navigationDestination(for: String.self) { id in
                ShellTerminalView(sessionID: sessionID, shellID: id, listed: shells?.first(where: { $0.id == id }))
            }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
                if canOpen {
                    ToolbarItem(placement: .primaryAction) {
                        Menu {
                            Button("New Shell", systemImage: "terminal") { open(command: nil) }
                            Button("Run Command…", systemImage: "play") { runningCommand = true }
                        } label: {
                            Label("Open", systemImage: "plus")
                        }
                    }
                }
            }
            .alert("Run Command", isPresented: $runningCommand) {
                TextField("cargo test", text: $command)
                    .font(.mono)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Button("Cancel", role: .cancel) {}
                Button("Run") { open(command: command) }
            } message: {
                Text("Runs in the session's worktree like a ! message; its output also goes to the chat.")
            }
            .errorAlert($actionError)
        }
        .task {
            while !Task.isCancelled {
                await load()
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }

    private func load() async {
        do {
            shells = try await server.api.shells(sessionID)
            error = nil
        } catch let remote as RemoteError where remote.isUnknownMethod {
            error = "Update difu on \(server.name) to use shells from the phone."
        } catch {
            if shells == nil { self.error = error.localizedDescription }
        }
    }

    private func open(command: String?) {
        Task {
            do {
                let id = try await server.api.openShell(sessionID, command: command)
                self.command = ""
                await load()
                path.append(id)
            } catch {
                actionError = error.localizedDescription
            }
        }
    }

    private func close(_ shell: ShellInfo) {
        Task {
            do {
                try await server.api.closeShell(sessionID, shell: shell.id)
                await load()
            } catch {
                actionError = error.localizedDescription
            }
        }
    }
}

private struct ShellRow: View {
    let shell: ShellInfo

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: symbol)
                .foregroundStyle(shell.running ? Color.accent : .secondary)
                .frame(width: 22)
            VStack(alignment: .leading, spacing: 3) {
                Text(shell.title)
                    .font(.mono)
                    .lineLimit(2)
                HStack(spacing: 6) {
                    Text(shell.stateLabel)
                    if shell.kind == .background { Text("Background · read-only") }
                    if shell.humanFocused { Text("In use") }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .combine)
    }

    private var symbol: String {
        switch shell.kind {
        case .shell: "terminal"
        case .command: "play.rectangle"
        case .background: "gearshape.2"
        }
    }
}

/// Sends a shell's input one piece at a time, in order, so quick taps on
/// the key bar never arrive out of order.
@MainActor
@Observable
final class ShellInputQueue {
    private var queue: [(text: String, enter: Bool)] = []
    private var draining = false
    var error: String?
    /// When something was last typed, for the read cadence.
    private(set) var typedAt = Date.distantPast

    func send(_ text: String, enter: Bool, api: DifuAPI, session: String, shell: String) {
        queue.append((text, enter))
        typedAt = .now
        guard !draining else { return }
        draining = true
        Task {
            while !queue.isEmpty {
                let next = queue.removeFirst()
                do {
                    try await api.writeShell(session, shell: shell, text: next.text, enter: next.enter)
                } catch {
                    self.error = error.localizedDescription
                    queue.removeAll()
                }
            }
            draining = false
        }
    }
}

/// A terminal-like view of one shell: its text (without colours, at the
/// size difu gave it, scrolling sideways unless wrapped), the keys a phone
/// lacks, and a line to type. Read quickly while output moves and slowly
/// when quiet; a shell that ended stops being read.
struct ShellTerminalView: View {
    let sessionID: String
    let shellID: String
    let listed: ShellInfo?

    @Environment(ServerConnection.self) private var server
    @Environment(\.dismiss) private var dismiss
    @State private var shell: ShellInfo?
    @State private var output = ""
    @State private var truncated = false
    @State private var revision: Int?
    @State private var changedAt = Date.now
    @State private var closedMessage: String?
    @State private var shellError: String?
    @State private var input = ""
    @State private var wrap = false
    @State private var confirmingClose = false
    @State private var typing = ShellInputQueue()
    @State private var error: String?
    @FocusState private var focused: Bool

    private var current: ShellInfo? { shell ?? listed }

    var body: some View {
        VStack(spacing: 0) {
            screen
            if let current, current.canWrite, closedMessage == nil {
                inputBar
            }
        }
        .navigationTitle(current?.title ?? "Shell")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 1) {
                    Text(current?.title ?? "Shell").font(.headline.monospaced()).lineLimit(1)
                    Text(closedMessage == nil ? (current?.stateLabel ?? "Loading…") : "Closed")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    Toggle("Wrap Lines", systemImage: "text.word.spacing", isOn: $wrap)
                    Button("Copy Output", systemImage: "doc.on.doc") { UIPasteboard.general.string = output }
                    if current?.canClose == true, closedMessage == nil {
                        Divider()
                        Button("Terminate Shell", systemImage: "xmark.circle", role: .destructive) {
                            confirmingClose = true
                        }
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
        }
        .confirmationDialog("Terminate this shell?", isPresented: $confirmingClose, titleVisibility: .visible) {
            Button("Terminate", role: .destructive) { close() }
        } message: {
            Text("Stops its processes on \(server.name), for difu and the agent too.")
        }
        .errorAlert($error)
        .onChange(of: typing.error) { _, message in
            if let message {
                error = message
                typing.error = nil
            }
        }
        .task(id: server.generation) { await poll() }
    }

    private var screen: some View {
        ScrollViewReader { proxy in
            ScrollView(wrap ? Axis.Set.vertical : [.vertical, .horizontal]) {
                VStack(alignment: .leading, spacing: 6) {
                    if truncated {
                        Text("Earlier output isn't shown.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Text(output.isEmpty ? " " : output)
                        .font(.system(size: 12, design: .monospaced))
                        .textSelection(.enabled)
                        .fixedSize(horizontal: !wrap, vertical: true)
                        .frame(maxWidth: wrap ? .infinity : nil, alignment: .leading)
                    if let message = closedMessage ?? shellError {
                        Label(message, systemImage: "info.circle")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Color.clear.frame(height: 1).id("end")
                }
                .padding(10)
            }
            .defaultScrollAnchor(.bottomLeading)
            .background(Color.codeBackground)
            .onChange(of: output) {
                proxy.scrollTo("end", anchor: .bottomLeading)
            }
            .onTapGesture { focused = true }
        }
    }

    private var inputBar: some View {
        VStack(spacing: 6) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    ForEach(ShellKey.allCases) { key in
                        Button(key.rawValue) { type(key.sequence, enter: false) }
                            .font(.caption.monospaced().weight(.semibold))
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                            .accessibilityLabel(accessibility(key))
                    }
                }
                .padding(.horizontal, 10)
            }
            HStack(spacing: 8) {
                TextField("Type a command", text: $input)
                    .font(.monoBody)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.send)
                    .focused($focused)
                    .onSubmit(submit)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 7)
                    .background(Color.secondary.opacity(0.12), in: RoundedRectangle(cornerRadius: 8))
                Button(action: submit) {
                    Image(systemName: "return")
                        .font(.body.weight(.semibold))
                        .frame(width: 36, height: 34)
                }
                .buttonStyle(.borderedProminent)
                .accessibilityLabel("Send with Enter")
            }
            .padding(.horizontal, 10)
        }
        .padding(.vertical, 8)
        .background(.bar)
    }

    private func accessibility(_ key: ShellKey) -> String {
        switch key {
        case .interrupt: "Control C, interrupt"
        case .endOfInput: "Control D, end of input"
        case .tab: "Tab"
        case .escape: "Escape"
        case .up: "Up arrow"
        case .down: "Down arrow"
        case .left: "Left arrow"
        case .right: "Right arrow"
        case .clear: "Control L, clear"
        }
    }

    /// Sends the line with Enter (an empty line is just Enter).
    private func submit() {
        let line = input
        input = ""
        type(line, enter: true)
        focused = true
    }

    private func type(_ text: String, enter: Bool) {
        Haptics.send()
        typing.send(text, enter: enter, api: server.api, session: sessionID, shell: shellID)
    }

    private func poll() async {
        while !Task.isCancelled, closedMessage == nil {
            await read()
            let delay = ShellPolling.next(
                running: closedMessage == nil && (current?.running ?? true),
                sinceChange: Date.now.timeIntervalSince(changedAt),
                sinceInput: Date.now.timeIntervalSince(typing.typedAt))
            guard let delay else { return }
            try? await Task.sleep(for: delay)
        }
    }

    private func read() async {
        do {
            let read = try await server.api.readShell(sessionID, shell: shellID, revision: revision)
            shell = read.shell
            revision = read.shell.revision
            shellError = read.error
            if let text = read.output {
                let plain = read.shell.kind == .background ? TerminalText.plain(text) : text
                if plain != output {
                    output = plain
                    changedAt = .now
                }
                truncated = read.truncated
            }
        } catch let remote as RemoteError where remote.isUnknownMethod {
            closedMessage = "Update difu on \(server.name) to use shells from the phone."
        } catch RemoteError.notConnected {
            // Read again after reconnecting (`server.generation` restarts this).
        } catch {
            let message = error.localizedDescription
            if message.contains("closed") || message.contains("not found") {
                closedMessage = message
            } else {
                shellError = message
            }
        }
    }

    private func close() {
        Task {
            do {
                try await server.api.closeShell(sessionID, shell: shellID)
                dismiss()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}
