import DifuKit
import SwiftUI

struct PrRoute: Hashable {
    /// The saved server the pull request is reviewed through.
    let server: String
    let repo: String
    let number: Int
    var title: String?
}

struct RepoRoute: Hashable {
    let server: String
    let name: String
}

struct ReviewsView: View {
    enum Tab: String, CaseIterable {
        case mine = "Mine"
        case requested = "Requested"
        case repos = "Repos"
    }

    @Environment(AppModel.self) private var app
    @State private var tab = Tab.mine

    var body: some View {
        NavigationStack {
            Group {
                if let server = app.reviewsServer {
                    content(server)
                        .task(id: "\(server.id)#\(server.generation)") {
                            let reviews = server.reviews
                            if reviews.inbox == nil { await reviews.loadInbox(refresh: false) }
                            if reviews.repositories == nil { await reviews.loadRepositories() }
                        }
                }
            }
            .themedBackground()
            .navigationTitle("Reviews")
            .toolbar {
                if app.servers.count > 1, let current = app.reviewsServer {
                    ToolbarItem(placement: .topBarTrailing) {
                        Menu {
                            Picker("Server", selection: serverBinding) {
                                ForEach(app.servers) { server in
                                    Text(server.name).tag(server.id)
                                }
                            }
                        } label: {
                            Label(current.name, systemImage: current.isServer ? "server.rack" : "laptopcomputer")
                                .labelStyle(.titleAndIcon)
                                .lineLimit(1)
                        }
                    }
                }
            }
            .navigationDestination(for: PrRoute.self) { PrScreen(route: $0) }
            .navigationDestination(for: RepoRoute.self) { route in
                if let server = app.server(route.server) {
                    RepositoryPrsView(server: server, repo: route.name)
                }
            }
        }
    }

    private var serverBinding: Binding<String> {
        Binding(
            get: { app.reviewsServer?.id ?? "" },
            set: { app.reviewsServerID = $0 })
    }

    private func content(_ server: ServerConnection) -> some View {
        let reviews = server.reviews
        return List {
            Section {
                Picker("Show", selection: $tab) {
                    ForEach(Tab.allCases, id: \.self) { Text($0.rawValue).tag($0) }
                }
                .pickerStyle(.segmented)
                .listRowInsets(EdgeInsets(top: 4, leading: 0, bottom: 4, trailing: 0))
                .listRowBackground(Color.clear)
            }
            switch tab {
            case .mine:
                prSection(reviews.inbox?.mine, server: server)
            case .requested:
                prSection(reviews.inbox?.reviewing, server: server)
            case .repos:
                repoSection(reviews.repositories, server: server)
            }
        }
        .listStyle(.insetGrouped)
        .refreshable {
            if tab == .repos {
                await reviews.loadRepositories()
            } else {
                await reviews.loadInbox(refresh: true)
            }
        }
        .overlay { overlay(reviews) }
    }

    @ViewBuilder private func prSection(_ prs: [Pr]?, server: ServerConnection) -> some View {
        if let prs, !prs.isEmpty {
            Section {
                ForEach(prs) { pr in
                    NavigationLink(
                        value: PrRoute(server: server.id, repo: pr.repo, number: pr.number, title: pr.title)
                    ) {
                        PrRow(pr: pr)
                    }
                }
            } footer: {
                if let viewer = server.reviews.inbox?.viewer {
                    Text("Signed in to GitHub as \(viewer). Pull to refresh from GitHub.")
                }
            }
        }
    }

    @ViewBuilder private func repoSection(
        _ repositories: [ReviewRepository]?, server: ServerConnection
    ) -> some View {
        if let repositories, !repositories.isEmpty {
            let sorted = repositories.filter(\.pinned) + repositories.filter { !$0.pinned }
            Section {
                ForEach(sorted) { repository in
                    NavigationLink(value: RepoRoute(server: server.id, name: repository.name)) {
                        HStack(spacing: 8) {
                            Image(systemName: repository.pinned ? "pin.fill" : "folder")
                                .foregroundStyle(repository.pinned ? Color.accent : .secondary)
                                .frame(width: 20)
                            Text(repository.name)
                                .font(.callout.monospaced())
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder private func overlay(_ reviews: ReviewsStore) -> some View {
        switch tab {
        case .mine, .requested:
            if let inbox = reviews.inbox {
                let prs = tab == .mine ? inbox.mine : inbox.reviewing
                if prs.isEmpty {
                    ContentUnavailableView(
                        tab == .mine ? "No open pull requests" : "Nothing to review",
                        systemImage: tab == .mine ? "arrow.triangle.pull" : "checkmark.seal",
                        description: Text(
                            tab == .mine
                                ? "Open pull requests you authored appear here."
                                : "Pull requests where your review is requested appear here."))
                }
            } else if let error = reviews.inboxError {
                failure(error) { await reviews.loadInbox(refresh: false) }
            } else {
                ProgressView("Loading pull requests…")
            }
        case .repos:
            if let repositories = reviews.repositories {
                if repositories.isEmpty {
                    ContentUnavailableView(
                        "No repositories", systemImage: "folder",
                        description: Text("Repositories you review in difu appear here."))
                }
            } else if let error = reviews.repositoriesError {
                failure(error) { await reviews.loadRepositories() }
            } else {
                ProgressView("Loading repositories…")
            }
        }
    }

    private func failure(_ message: String, retry: @escaping () async -> Void) -> some View {
        ContentUnavailableView {
            Label("Couldn't load", systemImage: "exclamationmark.triangle")
        } description: {
            Text(message)
        } actions: {
            Button("Try Again") { Task { await retry() } }
        }
    }
}

/// Open pull requests in one repository.
struct RepositoryPrsView: View {
    let server: ServerConnection
    let repo: String
    @State private var prs: [Pr]?
    @State private var error: String?

    var body: some View {
        Loadable(value: prs, error: error, loadingText: "Loading pull requests…", retry: load) { prs in
            if prs.isEmpty {
                ContentUnavailableView(
                    "No open pull requests", systemImage: "arrow.triangle.pull",
                    description: Text(repo))
            } else {
                ThemedList {
                    ForEach(prs) { pr in
                        NavigationLink(
                            value: PrRoute(server: server.id, repo: pr.repo, number: pr.number, title: pr.title)
                        ) {
                            PrRow(pr: pr, showRepo: false)
                        }
                    }
                }
                .listStyle(.insetGrouped)
            }
        }
        .themedBackground()
        .refreshable { await load() }
        .navigationTitle(repo.split(separator: "/").last.map(String.init) ?? repo)
        .navigationBarTitleDisplayMode(.inline)
        .task { await load() }
    }

    private func load() async {
        do {
            prs = try await server.api.pullRequests(in: repo)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }
}

struct PrRow: View {
    let pr: Pr
    var showRepo = true

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(pr.title)
                .font(.body.weight(.semibold))
                .lineLimit(2)
            HStack(spacing: 4) {
                Text(showRepo ? "\(pr.repo)#\(pr.number)" : "#\(pr.number)")
                    .font(.monoCaption)
                if let author = pr.author {
                    Text("· \(author)")
                }
                Spacer(minLength: 4)
                if let updated = pr.updated {
                    Text(Formatting.relative(updated))
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            .lineLimit(1)
            HStack(spacing: 6) {
                if let additions = pr.additions, let deletions = pr.deletions {
                    DiffStat(additions: additions, deletions: deletions)
                }
                if let files = pr.changedFiles {
                    Text("\(files) file\(files == 1 ? "" : "s")")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                if pr.draft { Pill(text: "Draft", systemImage: "pencil.circle") }
                if pr.conflicts { Pill(text: "Conflicts", systemImage: "exclamationmark.triangle.fill", color: .failure) }
            }
        }
        .padding(.vertical, 2)
    }
}
