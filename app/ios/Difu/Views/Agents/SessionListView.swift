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
    @State private var path: [String] = []
    @State private var showingNewSession = false
    @State private var renaming: SessionSummary?
    @State private var renameText = ""
    @State private var error: String?

    var body: some View {
        NavigationStack(path: $path) {
            content
                .navigationTitle("Agents")
                .searchable(text: $search, prompt: "Search sessions")
                .refreshable { await app.sessions.refresh() }
                .toolbar {
                    ToolbarItem(placement: .primaryAction) {
                        Button {
                            showingNewSession = true
                        } label: {
                            Label("New Session", systemImage: "plus")
                        }
                        .disabled(!app.isConnected)
                    }
                }
                .navigationDestination(for: String.self) { id in
                    SessionScreen(id: id)
                }
                .sheet(isPresented: $showingNewSession) {
                    NewSessionSheet { id in path.append(id) }
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
        let store = app.sessions
        if !store.loaded {
            if let error = store.error {
                ContentUnavailableView {
                    Label("Couldn't load sessions", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(error)
                } actions: {
                    Button("Try Again") { Task { await store.refresh() } }
                }
            } else {
                ProgressView(app.isConnected ? "Loading sessions…" : "Waiting for \(app.computerName)…")
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
                    ForEach(filtered) { summary in
                        NavigationLink(value: summary.id) {
                            SessionRow(summary: summary)
                        }
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            if !summary.orchestrator {
                                Button {
                                    archive(summary)
                                } label: {
                                    Label(
                                        summary.archived ? "Unarchive" : "Archive",
                                        systemImage: summary.archived ? "tray.and.arrow.up" : "archivebox")
                                }
                                .tint(.indigo)
                            }
                            Button {
                                startRename(summary)
                            } label: {
                                Label("Rename", systemImage: "pencil")
                            }
                            .tint(.gray)
                        }
                        .contextMenu {
                            Button("Rename", systemImage: "pencil") { startRename(summary) }
                            if !summary.orchestrator {
                                Button(
                                    summary.archived ? "Unarchive" : "Archive",
                                    systemImage: summary.archived ? "tray.and.arrow.up" : "archivebox"
                                ) { archive(summary) }
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
                            Text("Start a coding agent on \(app.computerName).")
                        } actions: {
                            Button("New Session") { showingNewSession = true }
                                .buttonStyle(.borderedProminent)
                                .disabled(!app.isConnected)
                        }
                    }
                }
            }
        }
    }

    private var filtered: [SessionSummary] {
        let query = search.trimmingCharacters(in: .whitespaces)
        return app.sessions.summaries.filter { summary in
            guard summary.archived == (filter == .archived) else { return false }
            guard !query.isEmpty else { return true }
            return [summary.displayTitle, summary.repository, summary.branch, summary.id]
                .compactMap { $0 }
                .contains { $0.localizedCaseInsensitiveContains(query) }
        }
    }

    private var renameBinding: Binding<Bool> {
        Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })
    }

    private func startRename(_ summary: SessionSummary) {
        renameText = summary.displayTitle
        renaming = summary
    }

    private func rename() {
        guard let summary = renaming else { return }
        let title = renameText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else { return }
        Task {
            do { try await app.api.rename(summary.id, title: title) } catch { self.error = error.localizedDescription }
        }
    }

    private func archive(_ summary: SessionSummary) {
        Task {
            do {
                try await app.api.archive(summary.id, archived: !summary.archived)
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}
