import DifuKit
import SwiftUI

/// The server's repositories: clone and fetch status, add, remove, refresh.
struct ServerRepositoriesView: View {
    let server: ServerConnection

    @State private var result: ServerRepositories?
    @State private var error: String?
    @State private var adding = false
    @State private var removing: ServerRepository?
    @State private var refreshing = false
    @State private var actionError: String?
    /// Bumped after changes so polling restarts.
    @State private var changes = 0

    var body: some View {
        Loadable(value: result, error: error, loadingText: "Loading repositories…", retry: load) { result in
            ThemedList {
                Section {
                    ForEach(result.repositories) { repository in
                        NavigationLink {
                            ServerRepositoryView(server: server, repository: repository)
                        } label: {
                            RepositoryRow(repository: repository)
                        }
                        .swipeActions(edge: .trailing) {
                            Button("Remove", role: .destructive) { removing = repository }
                            Button("Refresh") { refresh(repository.name) }
                                .tint(.indigo)
                        }
                        .contextMenu {
                            Button("Refresh", systemImage: "arrow.clockwise") { refresh(repository.name) }
                            Button("Remove", systemImage: "trash", role: .destructive) { removing = repository }
                        }
                    }
                } footer: {
                    if let minutes = result.refreshMinutes {
                        Text("The server fetches every repository in the background every \(minutes) minutes. Sessions start from these clones.")
                    }
                }
                Section {
                    Button {
                        refresh(nil)
                    } label: {
                        HStack {
                            Label("Refresh Mains", systemImage: "arrow.triangle.2.circlepath")
                            if refreshing {
                                Spacer()
                                ProgressView()
                            }
                        }
                    }
                    .disabled(refreshing || result.repositories.isEmpty)
                } footer: {
                    Text("Fetches every repository now, so new sessions start from the latest main branch.")
                }
            }
            .listStyle(.insetGrouped)
            .overlay {
                if result.repositories.isEmpty {
                    ContentUnavailableView {
                        Label("No repositories", systemImage: "shippingbox")
                    } description: {
                        Text("Add the repositories sessions on \(server.name) should work in.")
                    } actions: {
                        Button("Add Repository") { adding = true }
                            .buttonStyle(.borderedProminent)
                            .foregroundStyle(Color.onAccent)
                    }
                }
            }
        }
        .refreshable { await load() }
        .navigationTitle("Repositories")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    adding = true
                } label: {
                    Label("Add Repository", systemImage: "plus")
                }
            }
        }
        .task(id: changes) { await pollWhileCloning() }
        .sheet(isPresented: $adding) {
            AddRepositorySheet(
                server: server, existing: Set(result?.repositories.map(\.name) ?? [])
            ) {
                changes += 1
            }
        }
        .confirmationDialog(
            "Remove \(removing?.name ?? "")?",
            isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
            titleVisibility: .visible,
            presenting: removing
        ) { repository in
            Button("Remove", role: .destructive) { remove(repository) }
        } message: { _ in
            Text("The server stops listing it. Its clone stays while sessions use it.")
        }
        .errorAlert($actionError)
    }

    private func load() async {
        do {
            result = try await server.api.serverRepositories()
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// Loads, then keeps polling while a repository is still cloning.
    private func pollWhileCloning() async {
        await load()
        while !Task.isCancelled,
            result?.repositories.contains(where: { !$0.cloned && $0.error == nil }) == true
        {
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled else { return }
            await load()
        }
    }

    private func refresh(_ name: String?) {
        refreshing = true
        Task {
            defer { refreshing = false }
            do {
                try await server.api.refreshRepositories(name)
                Haptics.success()
                await load()
            } catch {
                actionError = error.localizedDescription
            }
        }
    }

    private func remove(_ repository: ServerRepository) {
        Task {
            do {
                try await server.api.removeRepository(repository.name)
                await load()
            } catch {
                actionError = error.localizedDescription
            }
        }
    }
}

/// One server repository: what new sessions in it get.
private struct ServerRepositoryView: View {
    let server: ServerConnection
    let repository: ServerRepository

    var body: some View {
        ThemedList {
            Section {
                RepositoryRow(repository: repository)
            }
            Section {
                NavigationLink {
                    EnvironmentEditorView(server: server, repository: repository.name)
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Label("Environment", systemImage: "wrench.and.screwdriver")
                        Text("Tools, setup commands, variables and notes")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                NavigationLink {
                    RulesEditorView(server: server, initialRepository: repository.path ?? repository.name)
                } label: {
                    Label("Rules", systemImage: "list.bullet.rectangle")
                }
            } footer: {
                Text("Every new session in this repository on \(server.name) starts with these. Agents add to the environment as they learn what the repository needs.")
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(repository.name)
        .navigationBarTitleDisplayMode(.inline)
    }
}

private struct RepositoryRow: View {
    let repository: ServerRepository

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(repository.name)
                .font(.callout.monospaced())
                .lineLimit(1)
            if let error = repository.error {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(.failure)
                    .lineLimit(3)
            } else if !repository.cloned {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.mini)
                    Text("Cloning…")
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            } else if let fetched = repository.fetchedAt {
                Text("Fetched \(Formatting.relative(Date(milliseconds: fetched)))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                Text("Cloned")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
    }
}

/// Adds repositories from the server's GitHub account, or by name.
private struct AddRepositorySheet: View {
    let server: ServerConnection
    let existing: Set<String>
    let added: () -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var available: [AvailableRepository]?
    @State private var error: String?
    @State private var search = ""
    @State private var name = ""
    @State private var addedNames: Set<String> = []
    @State private var addingName: String?
    @State private var actionError: String?

    var body: some View {
        NavigationStack {
            ThemedList {
                Section {
                    HStack {
                        TextField("owner/name", text: $name)
                            .font(.monoBody)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .onSubmit { add(name) }
                        Button("Add") { add(name) }
                            .disabled(!isValid(name))
                    }
                } footer: {
                    Text("Or pick one the server's GitHub login can access.")
                }
                Section {
                    if let available {
                        ForEach(filtered(available)) { repository in
                            Button {
                                add(repository.name)
                            } label: {
                                row(repository)
                            }
                            .disabled(isAdded(repository.name) || addingName != nil)
                        }
                    } else if let error {
                        Label(error, systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.secondary)
                    } else {
                        HStack {
                            ProgressView()
                            Text("Loading repositories…").foregroundStyle(.secondary)
                        }
                    }
                }
            }
            .searchable(text: $search, prompt: "Search repositories")
            .navigationTitle("Add Repository")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .task { await load() }
            .errorAlert($actionError, title: "Couldn't add the repository")
        }
    }

    private func row(_ repository: AvailableRepository) -> some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(repository.name)
                        .font(.callout.monospaced())
                        .foregroundStyle(.primary)
                    if repository.isPrivate == true {
                        Image(systemName: "lock.fill")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
                if let description = repository.description {
                    Text(description)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }
            Spacer()
            if addingName == repository.name {
                ProgressView()
            } else if isAdded(repository.name) {
                Image(systemName: "checkmark")
                    .foregroundStyle(Color.accent)
            }
        }
    }

    private func filtered(_ list: [AvailableRepository]) -> [AvailableRepository] {
        let query = search.trimmingCharacters(in: .whitespaces)
        guard !query.isEmpty else { return list }
        return list.filter {
            $0.name.localizedCaseInsensitiveContains(query)
                || ($0.description?.localizedCaseInsensitiveContains(query) ?? false)
        }
    }

    private func isAdded(_ name: String) -> Bool {
        existing.contains(name) || addedNames.contains(name)
    }

    private func isValid(_ text: String) -> Bool {
        let parts = text.trimmingCharacters(in: .whitespaces).split(separator: "/", omittingEmptySubsequences: false)
        return parts.count == 2 && parts.allSatisfy { !$0.isEmpty && !$0.contains(" ") }
    }

    private func load() async {
        do {
            available = try await server.api.availableRepositories()
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func add(_ text: String) {
        let name = text.trimmingCharacters(in: .whitespaces)
        guard isValid(name), !isAdded(name), addingName == nil else { return }
        addingName = name
        Task {
            defer { addingName = nil }
            do {
                try await server.api.addRepository(name)
                addedNames.insert(name)
                if self.name == text { self.name = "" }
                Haptics.success()
                added()
            } catch {
                actionError = error.localizedDescription
            }
        }
    }
}
