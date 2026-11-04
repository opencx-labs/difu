import DifuKit
import Observation
import SwiftUI

/// The session screen's resource sheets and its actions beyond messages
/// (pin, folder, attach a PR, change repository, move to a server, edit a
/// queued message). `SessionExtrasHost` presents them; `SessionExtrasMenu`
/// adds them to the session menu.
@MainActor
@Observable
final class SessionExtrasModel {
    enum Sheet: Identifiable {
        case artifacts, shells, subagents, repository, folder
        case editQueued(QueuedMessage)

        var id: String {
            switch self {
            case .artifacts: "artifacts"
            case .shells: "shells"
            case .subagents: "subagents"
            case .repository: "repository"
            case .folder: "folder"
            case .editQueued(let message): "queued-\(message.index)"
            }
        }
    }

    var sheet: Sheet?
    var attachingPr = false
    var prURL = ""
    /// Saved servers the computer can move sessions to; nil until listed
    /// (and on servers, which move nothing).
    var moveTargets: [MoveTarget]?
    var moving: MoveTarget?
    /// A long action in progress ("Moving to box…").
    var busy: String?
    var notice: String?
    var error: String?
    /// Pins and folders, shared with the list's actions.
    let list = SessionListActions()
}

/// Session menu items for the extras; placed inside the screen's `Menu`.
struct SessionExtrasMenu: View {
    let summary: SessionSummary?
    let view: SessionView?
    let server: ServerConnection
    let model: SessionExtrasModel

    var body: some View {
        let coding = summary?.kind == .coding && summary?.orchestrator != true
        if let summary, !summary.orchestrator {
            Button(summary.pinned ? "Unpin" : "Pin", systemImage: summary.pinned ? "pin.slash" : "pin") {
                model.list.pin(ServerSession(server: server, summary: summary))
            }
            Button("Move to Folder…", systemImage: "folder") { model.sheet = .folder }
        }
        if coding {
            Button("Shells", systemImage: "terminal") { model.sheet = .shells }
        }
        if let view, !view.artifacts.isEmpty {
            Button("Artifacts", systemImage: "doc.richtext") { model.sheet = .artifacts }
        }
        if let view, !view.subagents.isEmpty {
            Button("Sub-agents", systemImage: "person.2") { model.sheet = .subagents }
        }
        if coding {
            Button("Attach Pull Request…", systemImage: "link") {
                model.prURL = ""
                model.attachingPr = true
            }
            Button("Change Repository…", systemImage: "arrow.triangle.branch") { model.sheet = .repository }
            if let targets = model.moveTargets, !targets.isEmpty, summary?.movedTo == nil {
                Menu {
                    ForEach(targets) { target in
                        Button(target.connected ? target.name : "\(target.name) (offline)") {
                            model.moving = target
                        }
                        .disabled(!target.connected)
                    }
                } label: {
                    Label("Move to Server", systemImage: "icloud.and.arrow.up")
                }
            }
        }
    }
}

/// Presents the extras' sheets and dialogs over the session screen.
struct SessionExtrasHost: ViewModifier {
    let id: String
    let server: ServerConnection
    let view: SessionView?
    @Bindable var model: SessionExtrasModel

    func body(content: Content) -> some View {
        content
            .sheet(item: $model.sheet) { sheet in
                sheetContent(sheet).environment(server)
            }
            .alert("Attach Pull Request", isPresented: $model.attachingPr) {
                TextField("https://github.com/owner/repo/pull/123", text: $model.prURL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.URL)
                Button("Cancel", role: .cancel) {}
                Button("Attach") { attach() }
            } message: {
                Text("A pull request from any repository; it shows as a badge on this session, as in difu.")
            }
            .confirmationDialog(
                "Move to \(model.moving?.name ?? "server")?",
                isPresented: Binding(get: { model.moving != nil }, set: { if !$0 { model.moving = nil } }),
                titleVisibility: .visible, presenting: model.moving
            ) { target in
                Button("Move Session") { move(to: target) }
            } message: { target in
                Text(
                    "The session continues on \(target.name) with its chat and worktree changes. This copy on \(server.name) is archived and takes no more messages."
                )
            }
            .overlay(alignment: .top) {
                if let busy = model.busy {
                    HStack(spacing: 8) {
                        ProgressView()
                        Text(busy)
                    }
                    .font(.callout.weight(.medium))
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
                    .background(.regularMaterial, in: Capsule())
                    .padding(.top, 8)
                } else if let notice = model.notice {
                    Text(notice)
                        .font(.callout.weight(.medium))
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .background(.regularMaterial, in: Capsule())
                        .padding(.top, 8)
                        .transition(.move(edge: .top).combined(with: .opacity))
                        .task(id: notice) {
                            try? await Task.sleep(for: .seconds(2))
                            withAnimation { model.notice = nil }
                        }
                }
            }
            .modifier(SessionActionsHost(actions: model.list))
            .errorAlert($model.error)
            .task(id: server.generation) { await loadTargets() }
    }

    @ViewBuilder private func sheetContent(_ sheet: SessionExtrasModel.Sheet) -> some View {
        let canOpen = view?.kind == .coding && view?.archived == false && view?.canSend == true
        switch sheet {
        case .artifacts:
            ArtifactsSheet(sessionID: id, artifacts: view?.artifacts ?? [])
        case .shells:
            ShellsSheet(sessionID: id, canOpen: canOpen)
        case .subagents:
            SubagentsSheet(sessionID: id, subagents: view?.subagents ?? [])
        case .repository:
            RepositorySheet(sessionID: id, current: view?.repositoryPath, model: model)
        case .folder:
            if let summary = view?.summary ?? server.sessions.summary(for: id) {
                MoveToFolderSheet(item: ServerSession(server: server, summary: summary), actions: model.list)
            }
        case .editQueued(let message):
            QueuedMessageEditor(sessionID: id, message: message, model: model)
        }
    }

    /// Computers list their saved servers; servers move nothing.
    private func loadTargets() async {
        guard !server.isServer, server.isConnected else { return }
        model.moveTargets = (try? await server.api.moveTargets()) ?? []
    }

    private func attach() {
        let url = model.prURL.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !url.isEmpty else { return }
        model.busy = "Attaching the pull request…"
        Task {
            defer { model.busy = nil }
            do {
                try await server.api.attachPr(id, url: url)
                Haptics.success()
                withAnimation { model.notice = "Attached the pull request" }
            } catch let remote as RemoteError where remote.isUnknownMethod {
                model.error = "Update difu on \(server.name) to attach pull requests from the phone."
            } catch {
                model.error = error.localizedDescription
            }
        }
    }

    private func move(to target: MoveTarget) {
        model.busy = "Moving to \(target.name)…"
        Task {
            defer { model.busy = nil }
            do {
                let moved = try await server.api.move(id, to: target.name)
                Haptics.success()
                withAnimation { model.notice = "Moved to \(target.name) as \(moved)" }
            } catch {
                model.error = error.localizedDescription
            }
        }
    }
}

/// Changes the session's repository with `/repo`, after saying what it
/// discards, as the terminal does.
struct RepositorySheet: View {
    let sessionID: String
    let current: String?
    let model: SessionExtrasModel

    @Environment(ServerConnection.self) private var server
    @Environment(\.dismiss) private var dismiss
    @State private var repositories: [AgentRepository]?
    @State private var custom = ""
    @State private var chosen: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    if let repositories {
                        ForEach(repositories) { repository in
                            Button {
                                chosen = repository.path
                            } label: {
                                HStack {
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(repository.name)
                                        Text(repository.path)
                                            .font(.monoCaption)
                                            .foregroundStyle(.secondary)
                                            .lineLimit(1)
                                            .truncationMode(.middle)
                                    }
                                    Spacer()
                                    if repository.path == current {
                                        Text("Current").font(.caption).foregroundStyle(.secondary)
                                    }
                                }
                            }
                            .foregroundStyle(.primary)
                            .disabled(repository.path == current)
                        }
                    } else {
                        ProgressView()
                    }
                } header: {
                    Text("Repositories")
                }
                Section {
                    TextField(server.isServer ? "owner/name" : "/path/to/checkout", text: $custom)
                        .font(.mono)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Button("Use This Repository") {
                        chosen = custom.trimmingCharacters(in: .whitespacesAndNewlines)
                    }
                    .disabled(custom.trimmingCharacters(in: .whitespaces).isEmpty)
                } header: {
                    Text("Other")
                } footer: {
                    Text(
                        "Changing the repository stops the session's shells and discards its worktree and local branches, committed and uncommitted work included, along with its artifacts and PRs. The conversation stays; the next message starts a fresh worktree."
                    )
                }
            }
            .navigationTitle("Change Repository")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
            .confirmationDialog(
                "Change the repository?",
                isPresented: Binding(get: { chosen != nil }, set: { if !$0 { chosen = nil } }),
                titleVisibility: .visible, presenting: chosen
            ) { path in
                Button("Discard Work and Change", role: .destructive) { change(to: path) }
            } message: { path in
                Text("Moves the session to \(path). Its current worktree's work is discarded.")
            }
            .task {
                repositories = (try? await server.api.repositories()) ?? []
            }
        }
    }

    private func change(to path: String) {
        dismiss()
        model.busy = "Changing the repository…"
        Task {
            defer { model.busy = nil }
            do {
                let outcome = try await server.api.runCommand(
                    sessionID, name: "/repo", arguments: path, confirm: true)
                Haptics.success()
                withAnimation { model.notice = outcome.message ?? "Changed the repository" }
            } catch let remote as RemoteError where remote.isUnknownMethod {
                model.error = "Update difu on \(server.name) to change repositories from the phone."
            } catch {
                model.error = error.localizedDescription
            }
        }
    }
}

/// Edits or removes a queued message, as the terminal's queue does.
struct QueuedMessageEditor: View {
    let sessionID: String
    let message: QueuedMessage
    let model: SessionExtrasModel

    @Environment(ServerConnection.self) private var server
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var saving = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            TextEditor(text: $text)
                .font(.body)
                .padding(.horizontal, 12)
                .navigationTitle("Queued Message")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { dismiss() }
                    }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Save", action: save)
                            .disabled(saving || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                    ToolbarItem(placement: .bottomBar) {
                        Button("Remove from Queue", role: .destructive, action: remove)
                            .disabled(saving)
                    }
                }
                .errorAlert($error)
        }
        .onAppear { text = message.text }
    }

    private func save() {
        saving = true
        Task {
            defer { saving = false }
            do {
                try await server.api.editQueued(sessionID, message: message, text: text)
                dismiss()
            } catch let remote as RemoteError where remote.isUnknownMethod {
                error = "Update difu on \(server.name) to edit queued messages from the phone."
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    private func remove() {
        saving = true
        Task {
            defer { saving = false }
            do {
                try await server.api.unqueue(sessionID, index: message.index)
                dismiss()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}
