import DifuKit
import SwiftUI

/// A session on one of the saved servers.
struct SessionScreen: View {
    let ref: SessionRef

    @Environment(AppModel.self) private var app

    var body: some View {
        if let server = app.server(ref.server) {
            SessionContent(id: ref.session, server: server)
                .environment(server)
        } else {
            ContentUnavailableView(
                "Server removed", systemImage: "server.rack",
                description: Text("This session's server is no longer saved on this phone."))
        }
    }
}

/// A live agent session: transcript, pending requests, queue and composer.
struct SessionContent: View {
    let id: String
    let server: ServerConnection

    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var composer = ComposerModel()
    @State private var commands = SlashCommandModel()
    @State private var scrollTrigger = 0
    @State private var sheet: Sheet?
    @State private var confirmation: Confirmation?
    @State private var showingRename = false
    @State private var renameText = ""
    @State private var error: String?
    /// A destructive slash command waiting for confirmation.
    @State private var pendingCommand: PendingCommand?
    /// A short note after a slash command ran ("Compacting the conversation").
    @State private var notice: String?
    /// The pull request opened from the PR strip.
    @State private var pr: PrRoute?
    /// Artifacts, shells, sub-agents and the actions beyond messages.
    @State private var extras = SessionExtrasModel()

    enum Sheet: String, Identifiable {
        case model, changes, rules, usage, status, permissions, newSession
        var id: String { rawValue }
    }

    struct PendingCommand: Identifiable {
        let command: SessionCommand
        let arguments: String
        var id: String { "\(command.trigger) \(arguments)" }
    }

    enum Confirmation: String, Identifiable {
        case clear, delete
        var id: String { rawValue }
    }

    var body: some View {
        let model = server.sessions.model(for: id)
        content(model)
            .themedBackground(.plain)
            .navigationTitle(title(model))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) { header(model) }
                ToolbarItem(placement: .primaryAction) { menu(model) }
            }
            .onAppear { server.sessions.open(id) }
            .onDisappear { server.sessions.close(id) }
            .sheet(item: $sheet) { sheet in sheetContent(sheet, model: model) }
            .modifier(commandFeedback)
            .modifier(SessionExtrasHost(id: id, server: server, view: model.view, model: extras))
            .sheet(item: $pr) { route in
                // Its own stack keeps the transcript watched underneath and
                // the PR screen's toolbar actions available.
                NavigationStack {
                    PrScreen(route: route)
                        .toolbar {
                            ToolbarItem(placement: .cancellationAction) {
                                Button("Done") { pr = nil }
                            }
                        }
                }
                .environment(app)
            }
            .confirmationDialog(
                confirmation == .delete ? "Delete this session?" : "Clear this session?",
                isPresented: Binding(get: { confirmation != nil }, set: { if !$0 { confirmation = nil } }),
                titleVisibility: .visible,
                presenting: confirmation
            ) { confirmation in
                switch confirmation {
                case .clear:
                    Button("Clear Session", role: .destructive) {
                        perform { try await server.api.clear(id) }
                    }
                case .delete:
                    Button("Delete Session", role: .destructive) {
                        perform {
                            try await server.api.delete(id)
                            dismiss()
                        }
                    }
                }
            } message: { confirmation in
                switch confirmation {
                case .clear:
                    Text("Starts a fresh conversation and worktree. The current transcript is discarded.")
                case .delete:
                    Text(deleteSessionMessage(serverName: server.name))
                }
            }
            .alert("Rename Session", isPresented: $showingRename) {
                TextField("Title", text: $renameText)
                Button("Cancel", role: .cancel) {}
                Button("Rename") {
                    let title = renameText.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !title.isEmpty else { return }
                    perform { try await server.api.rename(id, title: title) }
                }
            }
            .errorAlert($error)
    }

    @ViewBuilder private func sheetContent(_ sheet: Sheet, model: SessionModel) -> some View {
        switch sheet {
        case .model:
            ModelSheet(sessionID: id, session: model).environment(server)
        case .changes:
            ChangesSheet(sessionID: id).environment(server)
        case .rules:
            NavigationStack {
                RulesEditorView(server: server, initialRepository: repositoryPath(model))
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) {
                            Button("Done") { self.sheet = nil }
                        }
                    }
            }
        case .usage, .status:
            SessionInfoSheet(sessionID: id, tab: sheet == .usage ? .usage : .status)
                .environment(server)
        case .permissions:
            PermissionsSheet(sessionID: id).environment(server)
        case .newSession:
            NewSessionSheet(initialServerID: server.id) { ref in app.requestedSession = ref }
                .environment(app)
        }
    }

    /// The confirmation for destructive slash commands and the note after one ran.
    private var commandFeedback: CommandFeedback {
        CommandFeedback(
            pending: $pendingCommand, notice: $notice,
            run: { run($0.command, arguments: $0.arguments, confirmed: true) })
    }

    @ViewBuilder private func content(_ model: SessionModel) -> some View {
        if let view = model.view {
            TranscriptView(
                view: view, model: model, scrollTrigger: scrollTrigger,
                stop: stop, loadFull: { loadFull($0, model: model) }
            )
            .safeAreaInset(edge: .top, spacing: 0) {
                let listed = server.sessions.summary(for: id)
                SessionResourcesBar(
                    view: view, listed: listed, prs: view.prs.isEmpty ? (listed?.prs ?? []) : view.prs,
                    open: { extras.sheet = $0 },
                    openPr: { pr in
                        self.pr = PrRoute(server: server.id, repo: pr.repo, number: pr.number, title: pr.title)
                    })
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                VStack(spacing: 0) {
                    if !view.queue.isEmpty {
                        QueueSection(
                            view: view,
                            remove: { index in perform { try await server.api.unqueue(id, index: index) } },
                            sendNow: { perform { try await server.api.sendNow(id) } },
                            edit: { extras.sheet = .editQueued($0) }
                        )
                        .padding(.horizontal, 12)
                        .padding(.top, 8)
                    }
                    if let mode = view.permissionMode, view.kind == .coding {
                        HStack {
                            PermissionModeChip(mode: mode) { sheet = .permissions }
                            Spacer()
                        }
                        .padding(.horizontal, 12)
                        .padding(.top, 6)
                    }
                    ComposerView(
                        composer: composer, view: view, commands: commands, serverName: server.name,
                        send: send, stop: stop, choose: choose,
                        loadCommands: { refresh in
                            Task { await commands.load(server.api, session: id, refresh: refresh) }
                        })
                }
                .background(.bar)
            }
        } else if let message = model.closedMessage {
            ContentUnavailableView {
                Label("Session closed", systemImage: "xmark.bin")
            } description: {
                Text(message)
            } actions: {
                Button("Back to Sessions") { dismiss() }
            }
        } else if let message = model.watchError {
            ContentUnavailableView {
                Label("Couldn't open the session", systemImage: "exclamationmark.triangle")
            } description: {
                Text(message)
            } actions: {
                Button("Try Again") { Task { await server.sessions.watch(id) } }
            }
        } else {
            ProgressView(server.isConnected ? "Loading transcript…" : "Waiting for \(server.name)…")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func title(_ model: SessionModel) -> String {
        model.view?.displayTitle ?? server.sessions.summary(for: id)?.displayTitle ?? "Session"
    }

    private func header(_ model: SessionModel) -> some View {
        VStack(spacing: 1) {
            Text(title(model))
                .font(.headline)
                .lineLimit(1)
            if let summary = model.view?.summary ?? server.sessions.summary(for: id) {
                HStack(spacing: 4) {
                    Circle().fill(StatusTone(summary).color).frame(width: 6, height: 6)
                    Text(subtitle(summary, view: model.view))
                        .lineLimit(1)
                }
                .font(.caption2)
                .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: 240)
    }

    private func subtitle(_ summary: SessionSummary, view: SessionView?) -> String {
        var parts = [summary.statusLabel]
        if let repository = summary.repository { parts.append(repository) }
        if let model = view?.model { parts.append(model) }
        return parts.joined(separator: " · ")
    }

    private func menu(_ model: SessionModel) -> some View {
        let view = model.view
        let summary = view?.summary ?? server.sessions.summary(for: id)
        return Menu {
            Button("Rename", systemImage: "pencil") {
                renameText = summary?.displayTitle ?? ""
                showingRename = true
            }
            Button("Model & Effort", systemImage: "cpu") { sheet = .model }
            Button("Status & Usage", systemImage: "gauge.with.dots.needle.33percent") { sheet = .status }
            if summary?.kind == .coding {
                Button("Permissions", systemImage: "lock.shield") { sheet = .permissions }
            }
            Button("Compact", systemImage: "arrow.down.right.and.arrow.up.left") {
                perform { try await server.api.compact(id) }
            }
            Button("Continue Session", systemImage: "play") {
                perform { try await server.api.resume(id) }
            }
            if summary?.canReadChanges ?? true {
                Button("Changes", systemImage: "plus.forwardslash.minus") { sheet = .changes }
            }
            Divider()
            SessionExtrasMenu(summary: summary, view: view, server: server, model: extras)
            Divider()
            if summary?.orchestrator != true {
                let archived = summary?.archived ?? false
                Button(
                    archived ? "Unarchive" : "Archive",
                    systemImage: archived ? "tray.and.arrow.up" : "archivebox"
                ) {
                    perform { try await server.api.archive(id, archived: !archived) }
                }
            }
            Button("Clear…", systemImage: "eraser", role: .destructive) { confirmation = .clear }
            if summary?.orchestrator != true {
                Button("Delete…", systemImage: "trash", role: .destructive) { confirmation = .delete }
            }
        } label: {
            Image(systemName: "ellipsis.circle")
        }
        .disabled(view == nil)
    }

    // MARK: Actions

    private func send(queue: Bool) {
        let text = composer.text.trimmingCharacters(in: .whitespacesAndNewlines)
        let images = composer.attachments
        guard !text.isEmpty || !images.isEmpty else { return }
        if images.isEmpty, runBang(text) { return }
        var skills: [String] = []
        switch SlashCommands.submission(text, commands: commands.commands) {
        case .command(let command, let arguments) where images.isEmpty:
            composer.text = ""
            commands.skillsOnly = false
            if command.kind == .screen && arguments.isEmpty {
                open(command.screen ?? .unknown)
            } else {
                request(command, arguments: arguments)
            }
            return
        case .message(_, let named):
            skills = named
        case .command:
            break
        }
        let attachments = images.enumerated().map { index, image in
            Attachment(name: "photo-\(index + 1).jpg", bytes: image.jpeg)
        }
        Haptics.send()
        composer.sending = true
        composer.text = ""
        composer.attachments = []
        scrollTrigger += 1
        Task {
            defer { composer.sending = false }
            do {
                try await server.api.send(
                    id, text: text, queue: queue, attachments: attachments, skills: skills)
            } catch {
                // Put the draft back so nothing is lost.
                if composer.text.isEmpty { composer.text = text }
                if composer.attachments.isEmpty { composer.attachments = images }
                self.error = error.localizedDescription
            }
        }
    }

    /// `!command` runs in the session's worktree right away, as in the
    /// terminal; its output streams into the chat. False for other text.
    private func runBang(_ text: String) -> Bool {
        guard text.hasPrefix("!"), server.sessions.model(for: id).view?.kind == .coding else { return false }
        let command = String(text.dropFirst()).trimmingCharacters(in: .whitespacesAndNewlines)
        guard !command.isEmpty else { return false }
        Haptics.send()
        composer.text = ""
        scrollTrigger += 1
        Task {
            do {
                _ = try await server.api.openShell(id, command: command)
            } catch {
                if composer.text.isEmpty { composer.text = text }
                self.error = (error as? RemoteError)?.isUnknownMethod == true
                    ? "Update difu on \(server.name) to run ! commands from the phone."
                    : error.localizedDescription
            }
        }
        return true
    }

    /// Chose a command from the `/` list.
    private func choose(_ command: SessionCommand) {
        switch SlashCommands.choose(command, text: composer.text) {
        case .insert(let text):
            composer.text = text
            commands.skillsOnly = false
        case .run(let command, let arguments):
            composer.text = ""
            request(command, arguments: arguments)
        case .open(let screen, _):
            composer.text = ""
            open(screen)
        }
    }

    /// Runs `command`, asking first when it's destructive.
    private func request(_ command: SessionCommand, arguments: String) {
        if command.confirm {
            pendingCommand = PendingCommand(command: command, arguments: arguments)
        } else {
            run(command, arguments: arguments, confirmed: false)
        }
    }

    private func run(_ command: SessionCommand, arguments: String, confirmed: Bool) {
        Task {
            do {
                let outcome: CommandOutcome?
                do {
                    outcome = try await server.api.runCommand(
                        id, name: command.trigger, arguments: arguments, confirm: confirmed)
                } catch let remote as RemoteError where remote.isUnknownMethod {
                    outcome = try await runWithoutCommands(command, arguments: arguments)
                }
                Haptics.success()
                if let screen = outcome?.screen {
                    open(screen)
                } else if let message = outcome?.message {
                    withAnimation { notice = message }
                }
            } catch {
                // Keep the draft so it can be fixed and sent again.
                if composer.text.isEmpty {
                    composer.text = arguments.isEmpty ? command.trigger : "\(command.trigger) \(arguments)"
                }
                self.error = error.localizedDescription
            }
        }
    }

    /// A computer without `session.command`: the commands existing methods run.
    private func runWithoutCommands(_ command: SessionCommand, arguments: String) async throws -> CommandOutcome? {
        switch command.name {
        case "compact":
            try await server.api.compact(id)
        case "clear":
            try await server.api.clear(id)
        case "rename" where !arguments.isEmpty:
            try await server.api.rename(id, title: arguments)
        default:
            open(command.screen ?? .unknown)
        }
        return nil
    }

    /// Opens a slash command's screen.
    private func open(_ screen: CommandScreen) {
        switch screen {
        case .model: sheet = .model
        case .changes: sheet = .changes
        case .rules: sheet = .rules
        case .usage: sheet = .usage
        case .status: sheet = .status
        case .permissions: sheet = .permissions
        case .new: sheet = .newSession
        case .rename:
            renameText = title(server.sessions.model(for: id))
            showingRename = true
        case .skills:
            commands.skillsOnly = true
            Task { await commands.load(server.api, session: id) }
        case .unknown:
            error = "This version of the difu app can't open that screen. Update the app."
        }
    }

    private func repositoryPath(_ model: SessionModel) -> String? {
        model.view?.repositoryPath ?? server.sessions.summary(for: id)?.repositoryPath
    }

    private func stop() {
        Haptics.warning()
        perform { try await server.api.interrupt(id) }
    }

    private func loadFull(_ entry: EntryView, model: SessionModel) {
        guard !entry.id.isEmpty, !model.loadingEntries.contains(entry.id) else { return }
        model.loadingEntries.insert(entry.id)
        Task {
            defer { model.loadingEntries.remove(entry.id) }
            do {
                model.fullEntries[entry.id] = try await server.api.entry(session: id, entry: entry.id)
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    private func perform(_ action: @escaping () async throws -> Void) {
        Task {
            do {
                try await action()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

/// Asks before a destructive slash command runs, and briefly shows what a
/// command did.
struct CommandFeedback: ViewModifier {
    @Binding var pending: SessionContent.PendingCommand?
    @Binding var notice: String?
    let run: (SessionContent.PendingCommand) -> Void

    func body(content: Content) -> some View {
        content
            .confirmationDialog(
                "Run \(pending?.command.trigger ?? "command")?",
                isPresented: Binding(get: { pending != nil }, set: { if !$0 { pending = nil } }),
                titleVisibility: .visible,
                presenting: pending
            ) { pending in
                Button("Run \(pending.command.trigger)", role: .destructive) { run(pending) }
            } message: { pending in
                Text(pending.command.description)
            }
            .overlay(alignment: .top) {
                if let notice {
                    Text(notice)
                        .font(.callout.weight(.medium))
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .background(.regularMaterial, in: Capsule())
                        .padding(.top, 8)
                        .transition(.move(edge: .top).combined(with: .opacity))
                        .task(id: notice) {
                            try? await Task.sleep(for: .seconds(2))
                            withAnimation { self.notice = nil }
                        }
                }
            }
    }
}

// MARK: Pull requests

extension PrRoute: Identifiable {
    var id: String { "\(server)|\(repo)#\(number)" }
}

/// The session's attached and discovered pull requests as compact chips;
/// tapping one opens it on the session's computer or server.
struct SessionPrStrip: View {
    let prs: [SessionPr]
    let open: (SessionPr) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(prs) { pr in
                    Button { open(pr) } label: { SessionPrChip(pr: pr) }
                        .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
        }
    }
}

struct SessionPrChip: View {
    let pr: SessionPr

    var body: some View {
        let style = SessionPrStyle(pr)
        HStack(spacing: 6) {
            Image(systemName: style.icon)
                .imageScale(.small)
                .foregroundStyle(style.color)
            // Attached PRs may live in another repository, as in the terminal.
            Text(pr.attached ? "\(pr.repoName)#\(pr.number)" : "#\(pr.number)")
                .font(.caption.monospacedDigit().weight(.semibold))
            if let title = pr.title {
                Text(title)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: 180, alignment: .leading)
            }
            if pr.label != "Unknown" {
                Pill(text: pr.label, color: style.color)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .background(Color.secondary.opacity(0.12), in: Capsule())
        .contentShape(Capsule())
        .accessibilityElement(children: .combine)
        .accessibilityLabel(
            ["Pull request \(pr.repoName) number \(pr.number)", pr.title, pr.label]
                .compactMap { $0 }.joined(separator: ", "))
        .accessibilityAddTraits(.isButton)
    }
}

/// Icon and colour for a session PR's display label, matching the terminal's
/// badge colours (conflicts in red).
struct SessionPrStyle {
    let icon: String
    let color: Color

    init(_ pr: SessionPr) {
        switch pr.label {
        case "Merged": (icon, color) = ("arrow.triangle.merge", .violet)
        case "Closed": (icon, color) = ("xmark.circle", .failure)
        case "Has conflicts": (icon, color) = ("exclamationmark.triangle", .failure)
        case "Draft": (icon, color) = ("pencil.circle", .secondary)
        case "Open": (icon, color) = ("arrow.triangle.pull", Color.success)
        default: (icon, color) = ("arrow.triangle.pull", .secondary)
        }
    }
}
