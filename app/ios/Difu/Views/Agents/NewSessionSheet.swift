import DifuKit
import SwiftUI

/// Starts a session: server (when several are saved), repository, model and
/// effort, first message.
struct NewSessionSheet: View {
    var onCreated: (SessionRef) -> Void

    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var serverID: String?
    @State private var repositories: [AgentRepository]?
    @State private var repositoriesError: String?
    @State private var repository: String?
    @State private var otherRepository = ""
    @State private var models: [AgentModel]?
    @State private var modelsError: String?
    @State private var model: String?
    @State private var effort: String?
    @State private var text = ""
    @State private var creating = false
    @State private var error: String?
    @FocusState private var focused: Bool

    private var server: ServerConnection? {
        app.server(serverID) ?? app.servers.first(where: \.isConnected) ?? app.servers.first
    }

    var body: some View {
        NavigationStack {
            Form {
                if app.servers.count > 1 {
                    Section("Server") {
                        Picker("Server", selection: $serverID) {
                            ForEach(app.servers) { server in
                                Label {
                                    Text(server.isConnected ? server.name : "\(server.name) (offline)")
                                } icon: {
                                    Image(systemName: server.isServer ? "server.rack" : "laptopcomputer")
                                }
                                .tag(Optional(server.id))
                            }
                        }
                        .pickerStyle(.navigationLink)
                    }
                }

                repositorySection

                ModelPickerSection(models: models, error: modelsError, model: $model, effort: $effort)

                Section("First message") {
                    TextField("What should the agent do?", text: $text, axis: .vertical)
                        .lineLimit(4...12)
                        .focused($focused)
                }
            }
            .navigationTitle("New Session")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if creating {
                        ProgressView()
                    } else {
                        Button("Start") { create() }
                            .disabled(server?.isConnected != true)
                    }
                }
            }
            .onAppear {
                if serverID == nil { serverID = server?.id }
            }
            .task(id: serverID) { await load() }
            .errorAlert($error, title: "Couldn't start the session")
        }
    }

    @ViewBuilder private var repositorySection: some View {
        Section {
            if let repositories {
                if repositories.isEmpty {
                    Text(server?.isServer == true
                        ? "No server repositories yet. Type owner/name below, or add some in Settings."
                        : "No repositories yet — the default workspace is used.")
                        .foregroundStyle(.secondary)
                } else {
                    Picker("Repository", selection: $repository) {
                        ForEach(repositories) { repository in
                            Text(repository.isDefault ? "\(repository.name) (default)" : repository.name)
                                .tag(Optional(repository.path))
                        }
                    }
                    .pickerStyle(.navigationLink)
                    .disabled(!otherRepository.isEmpty)
                    if let path = repository, otherRepository.isEmpty {
                        Text(path)
                            .font(.monoCaption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.head)
                    }
                }
                if server?.isServer == true {
                    TextField("Other repository (owner/name)", text: $otherRepository)
                        .font(.monoBody)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                }
            } else if let repositoriesError {
                Label(repositoriesError, systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.secondary)
            } else {
                ProgressView()
            }
        } header: {
            Text("Repository")
        } footer: {
            if server?.isServer == true {
                Text("A repository the server doesn't have yet is added and cloned first.")
            }
        }
    }

    private func load() async {
        // `serverID` is set on appear; loading before that would load twice.
        guard serverID != nil, let server else { return }
        repositories = nil
        repositoriesError = nil
        repository = nil
        models = nil
        modelsError = nil
        model = nil
        effort = nil
        async let repositories: Void = loadRepositories(server)
        async let models: Void = loadModels(server)
        _ = await (repositories, models)
    }

    private func loadRepositories(_ server: ServerConnection) async {
        do {
            let list = try await server.api.repositories()
            guard server.id == self.server?.id else { return }
            repositories = list
            repository = (list.first(where: \.isDefault) ?? list.first)?.path
        } catch {
            repositoriesError = error.localizedDescription
        }
    }

    private func loadModels(_ server: ServerConnection) async {
        do {
            let list = try await server.api.models()
            guard server.id == self.server?.id else { return }
            models = list
        } catch {
            modelsError = error.localizedDescription
        }
    }

    private func create() {
        guard let server else { return }
        creating = true
        let message = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let other = otherRepository.trimmingCharacters(in: .whitespacesAndNewlines)
        let isServer = server.isServer
        Task {
            defer { creating = false }
            do {
                let id = try await server.api.newSession(
                    repository: other.isEmpty || !isServer ? repository : other,
                    model: model, effort: effort, text: message)
                Haptics.send()
                dismiss()
                onCreated(SessionRef(server: server.id, session: id))
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

/// Model and effort pickers fed by `agents.models` ("Default" = nil).
struct ModelPickerSection: View {
    let models: [AgentModel]?
    let error: String?
    @Binding var model: String?
    @Binding var effort: String?
    var provider: String?

    var body: some View {
        Section {
            if let models {
                Picker("Model", selection: $model) {
                    Text("Default").tag(String?.none)
                    ForEach(grouped(models), id: \.0) { provider, models in
                        Section(provider) {
                            ForEach(models) { Text($0.name).tag(Optional($0.id)) }
                        }
                    }
                }
                .pickerStyle(.navigationLink)
                Picker("Effort", selection: $effort) {
                    Text("Default").tag(String?.none)
                    ForEach(efforts(models), id: \.self) { Text($0.capitalized).tag(Optional($0)) }
                }
            } else if let error {
                Label(error, systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.secondary)
            } else {
                HStack {
                    ProgressView()
                    Text("Loading models…").foregroundStyle(.secondary)
                }
            }
        } header: {
            Text("Model")
        } footer: {
            Text("Leave on Default to use the agent's configured model.")
        }
        .onChange(of: model) {
            if let effort, let models, !efforts(models).contains(effort) { self.effort = nil }
        }
    }

    private func grouped(_ models: [AgentModel]) -> [(String, [AgentModel])] {
        let relevant = provider.map { provider in
            models.filter { $0.provider == nil || $0.provider == provider }
        } ?? models
        var order: [String] = []
        var groups: [String: [AgentModel]] = [:]
        for model in relevant {
            let key = (model.provider ?? "Other").capitalized
            if groups[key] == nil { order.append(key) }
            groups[key, default: []].append(model)
        }
        return order.map { ($0, groups[$0] ?? []) }
    }

    private func efforts(_ models: [AgentModel]) -> [String] {
        if let model, let match = models.first(where: { $0.id == model }) {
            return match.efforts
        }
        var seen = Set<String>()
        return models.flatMap(\.efforts).filter { seen.insert($0).inserted }
    }
}
