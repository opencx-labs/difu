import DifuKit
import SwiftUI

/// Starts a session: server (when several are saved), repository, model and
/// effort, mode, first message. Choices start from the server's effective
/// new-session defaults (Claude Opus · xhigh when it saved no model, Auto on
/// servers) and every control works while the lists load.
struct NewSessionSheet: View {
    /// The server picked first (a session's `/new` starts on its own server).
    var initialServerID: String?
    var onCreated: (SessionRef) -> Void

    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var serverID: String?
    /// The server the lists and choices below belong to.
    @State private var loadedServerID: String?
    @State private var loading: Set<Load> = []
    @State private var repositories: [AgentRepository]?
    @State private var repositoriesError: String?
    @State private var repository: String?
    @State private var otherRepository = ""
    @State private var models: [AgentModel]?
    @State private var modelsError: String?
    @State private var model: String?
    @State private var effort: String?
    /// Chosen starting mode and Codex approvals; nil leaves the location's.
    @State private var mode: String?
    @State private var preset: String?
    @State private var saved = Saved.loading
    /// Chosen by the person: lists and defaults arriving later keep them.
    @State private var choseRepository = false
    @State private var choseModel = false
    @State private var text = ""
    @State private var creating = false
    @State private var confirmingChecks = false
    @State private var error: String?
    @FocusState private var focused: Bool

    private enum Load: Hashable {
        case repositories, models, defaults
    }

    /// The server's saved new-session defaults.
    private enum Saved: Equatable {
        case loading
        case loaded(AgentDefaults)
        /// An older computer without `agents.defaults`, or reading them failed.
        case unavailable

        var defaults: AgentDefaults? {
            if case .loaded(let defaults) = self { return defaults }
            return nil
        }
    }

    private var server: ServerConnection? {
        app.server(serverID) ?? app.servers.first(where: \.isConnected) ?? app.servers.first
    }

    var body: some View {
        NavigationStack {
            ThemedForm {
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

                ModelPickerSection(
                    models: models, error: modelsError, model: modelChoice, effort: effortChoice,
                    defaultLabel: defaultModelLabel, footer: modelFooter, retry: load)

                if saved.defaults?.reportsEffective == true {
                    modeSection
                }

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
                        Button("Start") { start() }
                            .disabled(server?.isConnected != true)
                    }
                }
            }
            // Not `.task`: SwiftUI cancels and restarts it whenever a picker's
            // list is pushed and popped, which used to reload everything and
            // undo each choice.
            .onAppear {
                if serverID == nil { serverID = app.server(initialServerID)?.id ?? server?.id }
                load()
            }
            .onChange(of: serverID) { load() }
            .onChange(of: server?.generation) { load() }
            .onChange(of: modeProvider) {
                mode = NewSessionDefaults.keptMode(mode, provider: modeProvider, defaults: saved.defaults)
            }
            .confirmationDialog(
                "Skip permission checks?", isPresented: $confirmingChecks, titleVisibility: .visible
            ) {
                Button("Start Without Checks", role: .destructive) { create() }
            } message: {
                Text("The agent runs every command and edit without asking. Only use this for work you'd let it do unsupervised.")
            }
            .errorAlert($error, title: "Couldn't start the session")
        }
    }

    /// The listed repositories plus the saved default when they lack it.
    private var repositoryChoices: [AgentRepository] {
        NewSessionDefaults.repositories(repositories ?? [], saved: saved.defaults?.repository)
    }

    @ViewBuilder private var repositorySection: some View {
        let choices = repositoryChoices
        let isServer = server?.isServer == true
        Section {
            if !choices.isEmpty {
                Picker("Repository", selection: repositoryChoice) {
                    ForEach(choices) { repository in
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
            } else if repositories != nil {
                Text(isServer
                    ? "No server repositories yet. Type owner/name below, or add some in Settings."
                    : "No repositories yet. Type the path of a Git repository below.")
                    .foregroundStyle(.secondary)
            }
            if repositories == nil {
                if let repositoriesError {
                    RetryRow(message: repositoriesError, retry: load)
                } else {
                    HStack {
                        ProgressView()
                        Text("Loading repositories…").foregroundStyle(.secondary)
                    }
                }
            }
            TextField(
                isServer ? "Other repository (owner/name)" : "Other repository (full path)",
                text: $otherRepository
            )
            .font(.monoBody)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
        } header: {
            Text("Repository")
        } footer: {
            if isServer {
                Text("A repository the server doesn't have yet is added and cloned first.")
            }
        }
    }

    /// The provider the chosen (or default) model runs on.
    private var modeProvider: String {
        NewSessionDefaults.provider(model: model, models: models, defaults: saved.defaults)
    }

    /// The mode and, for Codex, approvals the session starts with; "· automatic"
    /// marks the location's own choice (Auto on servers).
    @ViewBuilder private var modeSection: some View {
        let defaults = saved.defaults
        let provider = modeProvider
        let modes = defaults?.startingModes(for: provider) ?? []
        Section {
            Picker("Mode", selection: $mode) {
                Text(NewSessionDefaults.startLabel(for: provider, defaults: defaults)).tag(String?.none)
                ForEach(modes) { Text($0.label).tag(Optional($0.id)) }
            }
            if provider == "codex", let presets = defaults?.codexPresets, !presets.isEmpty {
                Picker("Approvals", selection: $preset) {
                    Text(NewSessionDefaults.presetLabel(defaults: defaults)).tag(String?.none)
                    ForEach(presets) { Text($0.label).tag(Optional($0.id)) }
                }
            }
        } header: {
            Text("Mode")
        } footer: {
            Text(modeFooter(provider: provider, modes: modes))
        }
    }

    private func modeFooter(provider: String, modes: [StartingMode]) -> String {
        var parts: [String] = []
        if let mode, let description = modes.first(where: { $0.id == mode })?.description {
            parts.append("\(modes.first { $0.id == mode }?.label ?? mode): \(description).")
        }
        if saved.defaults?.server == true {
            parts.append("Servers start Claude Code in Auto and Codex with Approve for me; bypass and Full Access are never automatic.")
        }
        parts.append("Switch modes any time from the session.")
        return parts.joined(separator: " ")
    }

    /// What "Default" means here: the location's effective model.
    private var defaultModelLabel: String? {
        guard let effective = saved.defaults?.effective else { return nil }
        let name = effective.model.map { id in models?.first { $0.id == id }?.name ?? id }
        return "Default (\(name ?? ModelPicker.providerLabel(effective.provider)))"
    }

    private var modelFooter: String {
        let name = server?.name ?? "the computer"
        if let effective = saved.defaults?.effective {
            if effective.modelAutomatic {
                return "Starts with the newest Claude Opus on \(name) with \(effective.effort ?? "default") reasoning, as \(name) and its terminal do when no default model is saved. Change it in Settings › \(name) › New Session Defaults."
            }
            return "Starts with \(name)'s default model. Change it in Settings › \(name) › New Session Defaults."
        }
        if saved.defaults?.model != nil {
            return "Starts with \(name)'s default model. Change it in Settings › \(name) › New Session Defaults."
        }
        return "Starts with Claude Opus and extra-high reasoning when \(name) has no default model; Default uses the agent's configured model."
    }

    private var repositoryChoice: Binding<String?> {
        Binding(get: { repository }, set: { repository = $0; choseRepository = true })
    }

    /// Only the picker sets the model, so any set (even "Default" again) is a choice.
    private var modelChoice: Binding<String?> {
        Binding(get: { model }, set: { model = $0; choseModel = true })
    }

    /// The model picker also keeps the effort in sync, so only changes count.
    private var effortChoice: Binding<String?> {
        Binding(
            get: { effort },
            set: { value in
                if value != effort { choseModel = true }
                effort = value
            })
    }

    /// Loads what the chosen server's lists still miss. Lists and choices
    /// survive pickers opening and closing and reconnects; another server
    /// starts over.
    private func load() {
        guard serverID != nil, let server else { return }
        if loadedServerID != server.id {
            loadedServerID = server.id
            loading = []
            repositories = nil
            repositoriesError = nil
            repository = nil
            models = nil
            modelsError = nil
            model = nil
            effort = nil
            mode = nil
            preset = nil
            saved = .loading
            choseRepository = false
            choseModel = false
        }
        if repositories == nil { start(.repositories, on: server) }
        if models == nil { start(.models, on: server) }
        if saved == .loading { start(.defaults, on: server) }
    }

    private func start(_ load: Load, on server: ServerConnection) {
        guard !loading.contains(load) else { return }
        loading.insert(load)
        Task {
            switch load {
            case .repositories: await loadRepositories(server)
            case .models: await loadModels(server)
            case .defaults: await loadDefaults(server)
            }
            if loadedServerID == server.id { loading.remove(load) }
        }
    }

    private func loadRepositories(_ server: ServerConnection) async {
        do {
            let list = try await server.api.repositories()
            guard loadedServerID == server.id else { return }
            repositories = list
            repositoriesError = nil
            preselect()
        } catch {
            guard loadedServerID == server.id, !(error is CancellationError) else { return }
            repositoriesError = error.localizedDescription
        }
    }

    private func loadModels(_ server: ServerConnection) async {
        do {
            let list = try await server.api.models()
            guard loadedServerID == server.id else { return }
            models = list
            modelsError = nil
        } catch {
            guard loadedServerID == server.id, !(error is CancellationError) else { return }
            modelsError = error.localizedDescription
        }
        preselect()
    }

    private func loadDefaults(_ server: ServerConnection) async {
        let result: Saved
        do {
            result = .loaded(try await server.api.agentDefaults())
        } catch {
            // Older computers don't have them; without them the phone's own
            // preselection applies.
            result = .unavailable
        }
        guard loadedServerID == server.id else { return }
        saved = result
        preselect()
    }

    /// Applies the server's defaults to what the person hasn't chosen yet.
    private func preselect() {
        if !choseRepository, repositories != nil {
            repository = NewSessionDefaults.repository(saved: saved.defaults?.repository, in: repositoryChoices)
        }
        // The saved model wins over the Opus preselection, so wait for it.
        guard !choseModel, saved != .loading else { return }
        let choice = NewSessionDefaults.choice(saved: saved.defaults, models: models)
        model = choice.model
        effort = choice.effort
    }

    /// The mode choices `agents.new` sends: only what the person picked.
    private var startRequest: (startingMode: String?, codexPreset: String?) {
        NewSessionDefaults.startRequest(provider: modeProvider, mode: mode, preset: preset, defaults: saved.defaults)
    }

    /// Asks first when the chosen mode skips permission checks.
    private func start() {
        let request = startRequest
        if NewSessionDefaults.skipsChecks(mode: request.startingMode, preset: request.codexPreset) {
            confirmingChecks = true
        } else {
            create()
        }
    }

    private func create() {
        guard let server else { return }
        creating = true
        let message = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let other = otherRepository.trimmingCharacters(in: .whitespacesAndNewlines)
        let request = startRequest
        // Untouched choices on a computer that reports its effective defaults
        // stay blank, so it applies them itself.
        let automatic = !choseModel && saved.defaults?.reportsEffective == true
        Task {
            defer { creating = false }
            do {
                let id = try await server.api.newSession(
                    repository: other.isEmpty ? repository : other,
                    model: automatic ? nil : model, effort: automatic ? nil : effort,
                    startingMode: request.startingMode, codexPreset: request.codexPreset, text: message)
                Haptics.send()
                dismiss()
                onCreated(SessionRef(server: server.id, session: id))
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

/// A load that failed, with a retry.
struct RetryRow: View {
    let message: String
    var retry: (() -> Void)?

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Label(message, systemImage: "exclamationmark.triangle")
                .foregroundStyle(.secondary)
            if let retry {
                Spacer(minLength: 8)
                Button("Retry", action: retry)
                    .buttonStyle(.borderless)
            }
        }
    }
}

/// Model and effort pickers fed by `agents.models` ("Default" = nil). Every
/// provider's models are listed, grouped like the terminal's Alt+M picker,
/// with `provider`'s (the session's) first; efforts follow the chosen model.
/// The pickers work while the models load, offering Default and the current
/// choice.
struct ModelPickerSection: View {
    let models: [AgentModel]?
    let error: String?
    @Binding var model: String?
    @Binding var effort: String?
    var provider: String?
    /// The "Default" row's label; by default it names `provider`.
    var defaultLabel: String?
    var footer = "Leave on Default to use the agent's configured model."
    var retry: (() -> Void)?

    var body: some View {
        let list = models ?? []
        Section {
            Picker("Model", selection: $model) {
                Text(defaultLabel ?? provider.map { "Default (\(ModelPicker.providerLabel($0)))" } ?? "Default")
                    .tag(String?.none)
                // A choice the list doesn't have (yet), so the picker shows it.
                if let model, !list.contains(where: { $0.id == model }) {
                    Text(model).tag(Optional(model))
                }
                ForEach(ModelPicker.groups(list, first: provider)) { group in
                    Section(group.title) {
                        ForEach(group.models) { Text($0.name).tag(Optional($0.id)) }
                    }
                }
            }
            .pickerStyle(.navigationLink)
            Picker("Effort", selection: $effort) {
                Text("Default").tag(String?.none)
                ForEach(efforts(list), id: \.self) { Text($0.capitalized).tag(Optional($0)) }
            }
            if models == nil {
                if let error {
                    RetryRow(message: error, retry: retry)
                } else {
                    HStack {
                        ProgressView()
                        Text("Loading models…").foregroundStyle(.secondary)
                    }
                }
            }
        } header: {
            Text("Model")
        } footer: {
            Text(footer)
        }
        .onChange(of: model) {
            guard let models else { return }
            effort = ModelPicker.syncedEffort(effort, models: models, model: model, provider: target(models))
        }
    }

    /// The provider the chosen model runs on.
    private func target(_ models: [AgentModel]) -> String? {
        model.map { ModelPicker.provider(of: $0, in: models) } ?? provider
    }

    private func efforts(_ models: [AgentModel]) -> [String] {
        var list = ModelPicker.efforts(models, model: model, provider: target(models))
        // Keep a current effort the list doesn't know, so the picker shows it.
        if let effort, !list.contains(effort) { list.append(effort) }
        return list
    }
}
