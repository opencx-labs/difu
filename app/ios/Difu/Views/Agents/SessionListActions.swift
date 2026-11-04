import DifuKit
import Observation
import SwiftUI

/// What the terminal says when deleting a chat, adapted for the phone.
func deleteSessionMessage(serverName: String) -> String {
    "Stops the agent and permanently deletes its difu history, questions, queue and attachment copies on \(serverName), and removes its difu-owned worktree, including uncommitted, untracked and ignored files. Branches, commits and pushed work are kept. This can't be undone."
}

/// Row actions of the agents list (pin, rename, file into a folder,
/// archive, delete) and folder actions, with the dialogs they need. One per
/// list screen; `SessionActionsHost` presents its dialogs.
@MainActor
@Observable
final class SessionListActions {
    var renaming: ServerSession?
    var renameText = ""
    var deleting: ServerSession?
    var filing: ServerSession?
    var renamingFolder: ServerFolder?
    var folderName = ""
    var deletingFolder: ServerFolder?
    var error: String?

    func pin(_ item: ServerSession) {
        run(item.server) { try await $0.pin(item.summary.id, pinned: !item.summary.pinned) }
    }

    func archive(_ item: ServerSession) {
        run(item.server) { try await $0.archive(item.summary.id, archived: !item.summary.archived) }
    }

    func startRename(_ item: ServerSession) {
        renameText = item.summary.displayTitle
        renaming = item
    }

    func rename() {
        guard let item = renaming else { return }
        let title = renameText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else { return }
        run(item.server) { try await $0.rename(item.summary.id, title: title) }
    }

    func delete(_ item: ServerSession) {
        run(item.server) { try await $0.delete(item.summary.id) }
    }

    func file(_ item: ServerSession, into folder: String?) {
        let server = item.server
        run(server) { api in
            let folders = try await api.moveToFolder([item.summary.id], folder: folder)
            server.sessions.receive(folders: folders)
        }
    }

    func createFolder(named name: String, with item: ServerSession) {
        let server = item.server
        run(server) { api in
            let folders = try await api.createFolder(name: name, sessions: [item.summary.id])
            server.sessions.receive(folders: folders)
        }
    }

    func startRenameFolder(_ folder: ServerFolder) {
        folderName = folder.folder.name
        renamingFolder = folder
    }

    func renameFolder() {
        guard let folder = renamingFolder else { return }
        let name = folderName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return }
        let server = folder.server
        run(server) { api in
            server.sessions.receive(folders: try await api.renameFolder(folder.folder.id, name: name))
        }
    }

    func deleteFolder(_ folder: ServerFolder) {
        let server = folder.server
        run(server) { api in
            server.sessions.receive(folders: try await api.deleteFolder(folder.folder.id))
        }
    }

    /// Moves a folder up or down among its server's folders.
    func shift(_ folder: ServerFolder, by offset: Int) {
        let server = folder.server
        let folders = server.sessions.folders
        guard let index = folders.firstIndex(where: { $0.id == folder.folder.id }) else { return }
        let target = index + offset
        guard folders.indices.contains(target) else { return }
        run(server) { api in
            server.sessions.receive(folders: try await api.reorderFolder(folder.folder.id, position: target))
        }
    }

    private func run(_ server: ServerConnection, _ action: @escaping @MainActor (DifuAPI) async throws -> Void) {
        Task {
            do {
                try await action(server.api)
            } catch let remote as RemoteError where remote.isUnknownMethod {
                error = "Update difu on \(server.name) to do this from the phone."
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

/// A folder on one computer or server, with its listed sessions.
struct ServerFolder: Identifiable {
    let server: ServerConnection
    let folder: SessionFolder
    let items: [ServerSession]

    var id: FolderRef { FolderRef(server: server.id, folder: folder.id) }
}

struct FolderRef: Hashable {
    let server: String
    let folder: String
}

/// Presents the dialogs of `SessionListActions`.
struct SessionActionsHost: ViewModifier {
    @Bindable var actions: SessionListActions

    func body(content: Content) -> some View {
        content
            .alert("Rename Session", isPresented: present(\.renaming)) {
                TextField("Title", text: $actions.renameText)
                Button("Cancel", role: .cancel) {}
                Button("Rename") { actions.rename() }
            }
            .confirmationDialog(
                "Delete this session?", isPresented: present(\.deleting), titleVisibility: .visible,
                presenting: actions.deleting
            ) { item in
                Button("Delete Session", role: .destructive) { actions.delete(item) }
            } message: { item in
                Text(deleteSessionMessage(serverName: item.server.name))
            }
            .sheet(item: $actions.filing) { item in
                MoveToFolderSheet(item: item, actions: actions)
            }
            .alert("Rename Folder", isPresented: present(\.renamingFolder)) {
                TextField("Name", text: $actions.folderName)
                Button("Cancel", role: .cancel) {}
                Button("Rename") { actions.renameFolder() }
            }
            .confirmationDialog(
                "Delete this folder?", isPresented: present(\.deletingFolder), titleVisibility: .visible,
                presenting: actions.deletingFolder
            ) { folder in
                Button("Delete Folder", role: .destructive) { actions.deleteFolder(folder) }
            } message: { folder in
                Text("Its sessions return to the main list on \(folder.server.name). No session is deleted.")
            }
            .errorAlert($actions.error)
    }

    private func present<T>(_ keyPath: ReferenceWritableKeyPath<SessionListActions, T?>) -> Binding<Bool> {
        Binding(
            get: { actions[keyPath: keyPath] != nil },
            set: { if !$0 { actions[keyPath: keyPath] = nil } })
    }
}

/// Swipe actions and the context menu of a session row.
struct SessionRowActions: ViewModifier {
    let item: ServerSession
    let actions: SessionListActions

    func body(content: Content) -> some View {
        let summary = item.summary
        content
            .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                if !summary.orchestrator {
                    Button {
                        actions.archive(item)
                    } label: {
                        Label(
                            summary.archived ? "Unarchive" : "Archive",
                            systemImage: summary.archived ? "tray.and.arrow.up" : "archivebox")
                    }
                    .tint(.indigo)
                }
                Button {
                    actions.startRename(item)
                } label: {
                    Label("Rename", systemImage: "pencil")
                }
                .tint(.gray)
            }
            .swipeActions(edge: .leading, allowsFullSwipe: true) {
                if !summary.orchestrator {
                    Button {
                        actions.pin(item)
                    } label: {
                        Label(summary.pinned ? "Unpin" : "Pin", systemImage: summary.pinned ? "pin.slash" : "pin")
                    }
                    .tint(.accentColor)
                }
            }
            .contextMenu {
                if !summary.orchestrator {
                    Button(summary.pinned ? "Unpin" : "Pin", systemImage: summary.pinned ? "pin.slash" : "pin") {
                        actions.pin(item)
                    }
                }
                Button("Rename", systemImage: "pencil") { actions.startRename(item) }
                if !summary.orchestrator {
                    Button("Move to Folder…", systemImage: "folder") { actions.filing = item }
                    Button(
                        summary.archived ? "Unarchive" : "Archive",
                        systemImage: summary.archived ? "tray.and.arrow.up" : "archivebox"
                    ) { actions.archive(item) }
                }
                if summary.kind == .coding && !summary.orchestrator {
                    Divider()
                    Button("Delete…", systemImage: "trash", role: .destructive) { actions.deleting = item }
                }
            }
    }
}

extension View {
    func sessionRowActions(_ item: ServerSession, actions: SessionListActions) -> some View {
        modifier(SessionRowActions(item: item, actions: actions))
    }
}

/// Files a session into one of its server's folders, a new folder, or back
/// into the main list, like **f** in the terminal.
struct MoveToFolderSheet: View {
    let item: ServerSession
    let actions: SessionListActions

    @Environment(\.dismiss) private var dismiss
    @State private var newName = ""

    var body: some View {
        let folders = item.server.sessions.folders
        let current = folders.first(where: { $0.sessions.contains(item.summary.id) })?.id ?? item.summary.folder
        NavigationStack {
            Form {
                Section {
                    ForEach(folders) { folder in
                        Button {
                            actions.file(item, into: folder.id)
                            dismiss()
                        } label: {
                            HStack {
                                Label(folder.name, systemImage: "folder")
                                Spacer()
                                if folder.id == current {
                                    Image(systemName: "checkmark").foregroundStyle(Color.accentColor)
                                }
                            }
                        }
                        .foregroundStyle(.primary)
                    }
                    if current != nil {
                        Button {
                            actions.file(item, into: nil)
                            dismiss()
                        } label: {
                            Label("Main List", systemImage: "tray")
                        }
                    }
                } header: {
                    Text(item.summary.displayTitle)
                } footer: {
                    if folders.isEmpty {
                        Text("Folders group sessions in the list, here and in difu on \(item.server.name).")
                    }
                }
                Section("New Folder") {
                    TextField("Name", text: $newName)
                        .submitLabel(.done)
                        .onSubmit(create)
                    Button("Create and Move", action: create)
                        .disabled(newName.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .navigationTitle("Move to Folder")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium, .large])
    }

    private func create() {
        let name = newName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return }
        actions.createFolder(named: name, with: item)
        dismiss()
    }
}
