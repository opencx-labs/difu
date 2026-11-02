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
    @State private var scrollTrigger = 0
    @State private var sheet: Sheet?
    @State private var confirmation: Confirmation?
    @State private var showingRename = false
    @State private var renameText = ""
    @State private var error: String?
    /// The pull request opened from the PR strip.
    @State private var pr: PrRoute?

    enum Sheet: String, Identifiable {
        case model, changes
        var id: String { rawValue }
    }

    enum Confirmation: String, Identifiable {
        case clear, delete
        var id: String { rawValue }
    }

    var body: some View {
        let model = server.sessions.model(for: id)
        content(model)
            .navigationTitle(title(model))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) { header(model) }
                ToolbarItem(placement: .primaryAction) { menu(model) }
            }
            .onAppear { server.sessions.open(id) }
            .onDisappear { server.sessions.close(id) }
            .sheet(item: $sheet) { sheet in
                switch sheet {
                case .model:
                    ModelSheet(sessionID: id, view: model.view).environment(server)
                case .changes:
                    ChangesSheet(sessionID: id).environment(server)
                }
            }
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
                    Text("Removes the chat and its worktree on \(server.name). This can't be undone.")
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

    @ViewBuilder private func content(_ model: SessionModel) -> some View {
        if let view = model.view {
            TranscriptView(
                view: view, model: model, scrollTrigger: scrollTrigger,
                stop: stop, loadFull: { loadFull($0, model: model) }
            )
            .safeAreaInset(edge: .top, spacing: 0) {
                let prs = view.prs.isEmpty ? (server.sessions.summary(for: id)?.prs ?? []) : view.prs
                if !prs.isEmpty {
                    SessionPrStrip(prs: prs) { pr in
                        self.pr = PrRoute(server: server.id, repo: pr.repo, number: pr.number, title: pr.title)
                    }
                    .background(.bar)
                }
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                VStack(spacing: 0) {
                    if !view.queue.isEmpty {
                        QueueSection(
                            view: view,
                            remove: { index in perform { try await server.api.unqueue(id, index: index) } },
                            sendNow: { perform { try await server.api.sendNow(id) } }
                        )
                        .padding(.horizontal, 12)
                        .padding(.top, 8)
                    }
                    ComposerView(composer: composer, view: view, send: send, stop: stop)
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
                try await server.api.send(id, text: text, queue: queue, attachments: attachments)
            } catch {
                // Put the draft back so nothing is lost.
                if composer.text.isEmpty { composer.text = text }
                if composer.attachments.isEmpty { composer.attachments = images }
                self.error = error.localizedDescription
            }
        }
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

private struct SessionPrChip: View {
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
        case "Merged": (icon, color) = ("arrow.triangle.merge", .purple)
        case "Closed": (icon, color) = ("xmark.circle", .red)
        case "Has conflicts": (icon, color) = ("exclamationmark.triangle", .red)
        case "Draft": (icon, color) = ("pencil.circle", .secondary)
        case "Open": (icon, color) = ("arrow.triangle.pull", Color.success)
        default: (icon, color) = ("arrow.triangle.pull", .secondary)
        }
    }
}
