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
    @State private var path: [SessionRef] = []
    @State private var showingNewSession = false
    @State private var renaming: ServerSession?
    @State private var renameText = ""
    @State private var error: String?

    var body: some View {
        NavigationStack(path: $path) {
            content
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
                .sheet(isPresented: $showingNewSession) {
                    NewSessionSheet { ref in path.append(ref) }
                }
                .onChange(of: app.requestedSession) { _, ref in
                    guard let ref else { return }
                    app.requestedSession = nil
                    path.append(ref)
                }
                .alert("Rename Session", isPresented: renameBinding) {
                    TextField("Title", text: $renameText)
                    Button("Cancel", role: .cancel) {}
                    Button("Rename") { rename() }
                }
                .errorAlert($error)
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
            List {
                Section {
                    Picker("Show", selection: $filter) {
                        ForEach(Filter.allCases, id: \.self) { Text($0.rawValue).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .listRowInsets(EdgeInsets(top: 4, leading: 0, bottom: 4, trailing: 0))
                    .listRowBackground(Color.clear)
                }
                Section {
                    ForEach(filtered) { item in
                        let summary = item.summary
                        NavigationLink(value: item.id) {
                            SessionRow(summary: summary, server: app.showsServerBadges ? item.server : nil)
                        }
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            if !summary.orchestrator {
                                Button {
                                    archive(item)
                                } label: {
                                    Label(
                                        summary.archived ? "Unarchive" : "Archive",
                                        systemImage: summary.archived ? "tray.and.arrow.up" : "archivebox")
                                }
                                .tint(.indigo)
                            }
                            Button {
                                startRename(item)
                            } label: {
                                Label("Rename", systemImage: "pencil")
                            }
                            .tint(.gray)
                        }
                        .contextMenu {
                            Button("Rename", systemImage: "pencil") { startRename(item) }
                            if !summary.orchestrator {
                                Button(
                                    summary.archived ? "Unarchive" : "Archive",
                                    systemImage: summary.archived ? "tray.and.arrow.up" : "archivebox"
                                ) { archive(item) }
                            }
                        }
                    }
                }
            }
            .listStyle(.insetGrouped)
            .overlay {
                if filtered.isEmpty {
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

    private var filtered: [ServerSession] {
        let query = search.trimmingCharacters(in: .whitespaces)
        return app.sessions.filter { item in
            let summary = item.summary
            guard summary.archived == (filter == .archived) else { return false }
            guard !query.isEmpty else { return true }
            return [summary.displayTitle, summary.repository, summary.branch, summary.id, item.server.name]
                .compactMap { $0 }
                .contains { $0.localizedCaseInsensitiveContains(query) }
        }
    }

    private var renameBinding: Binding<Bool> {
        Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })
    }

    private func startRename(_ item: ServerSession) {
        renameText = item.summary.displayTitle
        renaming = item
    }

    private func rename() {
        guard let item = renaming else { return }
        let title = renameText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else { return }
        Task {
            do {
                try await item.server.api.rename(item.summary.id, title: title)
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    private func archive(_ item: ServerSession) {
        Task {
            do {
                try await item.server.api.archive(item.summary.id, archived: !item.summary.archived)
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}
