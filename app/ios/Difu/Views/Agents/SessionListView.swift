import DifuKit
import SwiftUI

struct SessionListView: View {
    enum Filter: String, CaseIterable {
        case active = "Active"
        case archived = "Archived"
    }

    @Environment(AppModel.self) private var app
    @State private var filter = Filter.active
    @State private var search = ""
    @State private var path = NavigationPath()
    @State private var showingNewSession = false
    @State private var actions = SessionListActions()

    var body: some View {
        NavigationStack(path: $path) {
            content
                .themedBackground()
                .navigationTitle("Agents")
                .searchable(text: $search, prompt: "Search sessions")
                .refreshable { await app.refreshSessions() }
                .toolbar {
                    ToolbarItem(placement: .primaryAction) {
                        Button {
                            showingNewSession = true
                        } label: {
                            Label("New Session", systemImage: "plus")
                        }
                        .disabled(!app.isAnyConnected)
                    }
                }
                .navigationDestination(for: SessionRef.self) { ref in
                    SessionScreen(ref: ref)
                }
                .navigationDestination(for: FolderRef.self) { ref in
                    FolderSessionsView(ref: ref)
                }
                .sheet(isPresented: $showingNewSession) {
                    NewSessionSheet { ref in path.append(ref) }
                }
                .onChange(of: app.requestedSession) { _, ref in
                    guard let ref else { return }
                    app.requestedSession = nil
                    path.append(ref)
                }
                .modifier(SessionActionsHost(actions: actions))
        }
    }

    @ViewBuilder private var content: some View {
        if !app.sessionsLoaded {
            if let error = app.sessionsError {
                ContentUnavailableView {
                    Label("Couldn't load sessions", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(error)
                } actions: {
                    Button("Try Again") { Task { await app.refreshSessions() } }
                }
            } else {
                ProgressView(app.isAnyConnected ? "Loading sessions…" : "Waiting for \(app.serversLabel)…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        } else {
            ThemedList {
                Section {
                    Picker("Show", selection: $filter) {
                        ForEach(Filter.allCases, id: \.self) { Text($0.rawValue).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .listRowInsets(EdgeInsets(top: 4, leading: 0, bottom: 4, trailing: 0))
                    .listRowBackground(Color.clear)
                }
                if isSearching {
                    Section {
                        ForEach(filtered) { item in
                            row(item, folder: item.server.sessions.folder(folderID(item))?.name)
                        }
                    }
                } else {
                    let arranged = arrangement
                    let top = arranged.loose.filter { $0.summary.orchestrator }
                    let rest = arranged.loose.filter { !$0.summary.orchestrator }
                    if arranged.folders.isEmpty {
                        Section {
                            ForEach(top + rest) { item in row(item) }
                        }
                    } else {
                        if !top.isEmpty {
                            Section {
                                ForEach(top) { item in row(item) }
                            }
                        }
                        Section("Folders") {
                            ForEach(arranged.folders) { folder in
                                NavigationLink(value: folder.id) {
                                    FolderRow(folder: folder, showsServer: app.showsServerBadges)
                                }
                                .contextMenu { FolderMenuItems(folder: folder, actions: actions) }
                            }
                        }
                        if !rest.isEmpty {
                            Section {
                                ForEach(rest) { item in row(item) }
                            }
                        }
                    }
                }
            }
            .listStyle(.insetGrouped)
            .overlay {
                if isSearching ? filtered.isEmpty : (filtered.isEmpty && arrangement.folders.isEmpty) {
                    if !search.isEmpty {
                        ContentUnavailableView.search(text: search)
                    } else if filter == .archived {
                        ContentUnavailableView(
                            "No archived sessions", systemImage: "archivebox",
                            description: Text("Sessions you archive appear here."))
                    } else {
                        ContentUnavailableView {
                            Label("No sessions", systemImage: "sparkles")
                        } description: {
                            Text("Start a coding agent on \(app.serversLabel).")
                        } actions: {
                            Button("New Session") { showingNewSession = true }
                                .buttonStyle(.borderedProminent)
                                .foregroundStyle(Color.onAccent)
                                .disabled(!app.isAnyConnected)
                        }
                    }
                }
            }
        }
    }

    private var isSearching: Bool {
        !search.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// The rows of the chosen view (active or archived) that match the search;
    /// a search also matches server and folder names.
    private var filtered: [ServerSession] {
        app.sessions.filter { item in
            let summary = item.summary
            guard summary.archived == (filter == .archived) else { return false }
            let folder = item.server.sessions.folder(folderID(item))?.name
            return SessionGrouping.matches(summary, query: search, extra: [item.server.name, folder])
        }
    }

    private func folderID(_ item: ServerSession) -> String? {
        item.server.sessions.folders.first(where: { $0.sessions.contains(item.summary.id) })?.id ?? item.summary.folder
    }

    /// Unfiled rows in list order, and every server's folders in order.
    private var arrangement: (loose: [ServerSession], folders: [ServerFolder]) {
        let rows = filtered
        var filed = Set<SessionRef>()
        var folders: [ServerFolder] = []
        for server in app.servers {
            let arranged = SessionGrouping.arrange(
                rows.filter { $0.server === server }, folders: server.sessions.folders,
                archived: filter == .archived
            ) { $0.summary }
            for group in arranged.groups {
                folders.append(ServerFolder(server: server, folder: group.folder, items: group.items))
                filed.formUnion(group.items.map(\.id))
            }
        }
        return (rows.filter { !filed.contains($0.id) }, folders)
    }

    private func row(_ item: ServerSession, folder: String? = nil) -> some View {
        NavigationLink(value: item.id) {
            SessionRow(
                summary: item.summary, server: app.showsServerBadges ? item.server : nil,
                onServer: item.server.isServer, folder: folder)
        }
        .sessionRowActions(item, actions: actions)
    }
}
