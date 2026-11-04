import DifuKit
import SwiftUI

/// A folder in the agents list: its name, what its sessions do, and the
/// server it's on when several are saved. Opens into `FolderSessionsView`.
struct FolderRow: View {
    let folder: ServerFolder
    var showsServer = false

    var body: some View {
        let summaries = folder.items.map(\.summary)
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: summaries.contains(where: { $0.pending > 0 }) ? "folder.badge.questionmark" : "folder")
                .foregroundStyle(Color.accent)
                .frame(width: 22, height: 22)
            VStack(alignment: .leading, spacing: 4) {
                Text(folder.folder.name)
                    .font(.body.weight(.semibold))
                    .lineLimit(1)
                FlowLayout {
                    Text(SessionGrouping.folderDetail(summaries))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if showsServer {
                        ServerBadge(server: folder.server)
                    }
                }
            }
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .combine)
    }
}

/// Folder actions for a folder row or screen.
struct FolderMenuItems: View {
    let folder: ServerFolder
    let actions: SessionListActions

    var body: some View {
        let folders = folder.server.sessions.folders
        let index = folders.firstIndex { $0.id == folder.folder.id }
        Button("Rename Folder", systemImage: "pencil") { actions.startRenameFolder(folder) }
        if let index, index > 0 {
            Button("Move Up", systemImage: "arrow.up") { actions.shift(folder, by: -1) }
        }
        if let index, index < folders.count - 1 {
            Button("Move Down", systemImage: "arrow.down") { actions.shift(folder, by: 1) }
        }
        Button("Delete Folder…", systemImage: "trash", role: .destructive) { actions.deletingFolder = folder }
    }
}

/// One folder's sessions, as the terminal's list shows them inside a folder.
/// New sessions started here are filed into it.
struct FolderSessionsView: View {
    let ref: FolderRef

    @Environment(AppModel.self) private var app
    @State private var archived = false
    @State private var actions = SessionListActions()
    @State private var showingNewSession = false

    var body: some View {
        if let server = app.server(ref.server), let folder = server.sessions.folder(ref.folder) {
            let rows = app.sessions.filter {
                $0.server === server && folder.sessions.contains($0.summary.id) && $0.summary.archived == archived
            }
            let current = ServerFolder(server: server, folder: folder, items: rows)
            ThemedList {
                Section {
                    Picker("Show", selection: $archived) {
                        Text("Active").tag(false)
                        Text("Archived").tag(true)
                    }
                    .pickerStyle(.segmented)
                    .listRowInsets(EdgeInsets(top: 4, leading: 0, bottom: 4, trailing: 0))
                    .listRowBackground(Color.clear)
                }
                Section {
                    ForEach(rows) { item in
                        NavigationLink(value: item.id) {
                            SessionRow(
                                summary: item.summary, server: app.showsServerBadges ? server : nil,
                                onServer: server.isServer)
                        }
                        .sessionRowActions(item, actions: actions)
                    }
                }
            }
            .listStyle(.insetGrouped)
            .overlay {
                if rows.isEmpty {
                    ContentUnavailableView(
                        archived ? "No archived sessions" : "This folder is empty", systemImage: "folder",
                        description: Text(
                            archived
                                ? "Archived sessions in this folder appear here."
                                : "Start a session here, or use Move to Folder on a session."))
                }
            }
            .navigationTitle(folder.name)
            .toolbar {
                ToolbarItemGroup(placement: .primaryAction) {
                    Button {
                        showingNewSession = true
                    } label: {
                        Label("New Session Here", systemImage: "plus")
                    }
                    .disabled(!server.isConnected)
                    Menu {
                        FolderMenuItems(folder: current, actions: actions)
                    } label: {
                        Label("Folder", systemImage: "ellipsis.circle")
                    }
                }
            }
            .refreshable { await server.sessions.refresh() }
            .sheet(isPresented: $showingNewSession) {
                NewSessionSheet(initialServerID: server.id) { ref in
                    file(ref, into: folder.id, on: server)
                }
                .environment(app)
            }
            .modifier(SessionActionsHost(actions: actions))
        } else {
            ContentUnavailableView(
                "Folder removed", systemImage: "folder",
                description: Text("This folder no longer exists. Its sessions are in the main list."))
        }
    }

    /// Files a session started here into the folder, then opens it.
    private func file(_ ref: SessionRef, into folder: String, on server: ServerConnection) {
        Task {
            do {
                server.sessions.receive(folders: try await server.api.moveToFolder([ref.session], folder: folder))
            } catch {
                actions.error = error.localizedDescription
            }
            app.requestedSession = ref
        }
    }
}
