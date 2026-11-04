import DifuKit
import SwiftUI

/// A computer's or server's defaults for new sessions (`agents.defaults`):
/// provider and model, effort, starting mode and repository, shared with its
/// terminal and agents. Saved with `agents.defaults.set`.
struct NewSessionDefaultsView: View {
    let server: ServerConnection

    @State private var saved: AgentDefaults?
    @State private var draft = AgentDefaults()
    @State private var loadError: String?
    @State private var models: [AgentModel]?
    @State private var modelsError: String?
    @State private var repositories: [AgentRepository]?
    @State private var repositoriesError: String?
    @State private var loading: Set<Load> = []
    @State private var saving = false
    @State private var error: String?

    private enum Load: Hashable {
        case defaults, models, repositories
    }

    var body: some View {
        Loadable(value: saved, error: loadError, loadingText: "Loading defaults…", retry: { load() }) { _ in
            form
        }
        .navigationTitle("New Session Defaults")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                if saving {
                    ProgressView()
                } else {
                    Button("Save") { save() }
                        .disabled(saved == nil || draft == saved)
                }
            }
        }
        // Not `.task`, which reruns when a picker's list closes and would
        // replace unsaved edits.
        .onAppear { load() }
        .onChange(of: server.generation) { load() }
    }

    private var form: some View {
        Form {
            opusSection

            Section {
                Picker("Provider", selection: providerChoice) {
                    ForEach(providers) { Text($0.label).tag($0.id) }
                }
                Picker("Model", selection: modelChoice) {
                    Text("Default (\(ModelPicker.providerLabel(draft.provider)))").tag(String?.none)
                    if let model = draft.model, !providerModels.contains(where: { $0.id == model }) {
                        Text(model).tag(Optional(model))
                    }
                    ForEach(providerModels) { Text($0.name).tag(Optional($0.id)) }
                }
                .pickerStyle(.navigationLink)
                Picker("Effort", selection: $draft.effort) {
                    Text("Default").tag(String?.none)
                    ForEach(efforts, id: \.self) { Text($0.capitalized).tag(Optional($0)) }
                }
                Picker("Starting mode", selection: $draft.startingMode) {
                    Text("Default").tag(String?.none)
                    ForEach(draft.startingModes(for: draft.provider)) { mode in
                        Text(mode.label).tag(Optional(mode.id))
                    }
                }
                if models == nil {
                    if let modelsError {
                        RetryRow(message: modelsError, retry: { load() })
                    } else {
                        HStack {
                            ProgressView()
                            Text("Loading models…").foregroundStyle(.secondary)
                        }
                    }
                }
            } header: {
                Text("Agent")
            } footer: {
                Text(agentFooter)
            }

            Section {
                Picker("Repository", selection: $draft.repository) {
                    Text("None").tag(String?.none)
                    ForEach(repositoryChoices) { repository in
                        Text(repository.name).tag(Optional(repository.path))
                    }
                }
                .pickerStyle(.navigationLink)
                if let path = draft.repository {
                    Text(path)
                        .font(.monoCaption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
                if repositories == nil {
                    if let repositoriesError {
                        RetryRow(message: repositoriesError, retry: { load() })
                    } else {
                        HStack {
                            ProgressView()
                            Text("Loading repositories…").foregroundStyle(.secondary)
                        }
                    }
                }
            } header: {
                Text("Repository")
            } footer: {
                Text("New sessions start here unless you pick another repository.")
            }

            Section {
                LabeledContent("Worktree", value: "Own worktree")
            } footer: {
                Text("Every new session gets its own Git worktree from the latest origin/main, so agents never change your checkout.")
            }

            if let error {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red)
                }
            }
        }
    }

    @ViewBuilder private var opusSection: some View {
        let opus = models.flatMap(NewSessionDefaults.newestOpus)
        let effort = opus.flatMap { NewSessionDefaults.effort(for: $0) }
        Section {
            Button {
                if let opus { useOpus(opus, effort: effort) }
            } label: {
                Label(
                    "Use Claude Opus · \(effort ?? NewSessionDefaults.preferredEffort)",
                    systemImage: "sparkles")
            }
            .disabled(opus == nil || saving || (saved?.model == opus?.id && saved?.effort == effort))
        } footer: {
            if let opus {
                if saved?.model == opus.id && saved?.effort == effort {
                    Text("New sessions on \(server.name) use \(opus.name) with \(effort ?? "default") reasoning.")
                } else {
                    Text("Saves \(opus.name) with \(effort ?? "default") reasoning as the default right away.")
                }
            } else if models != nil {
                Text("Claude Code has no Opus model on \(server.name). Sign in to Claude Code there to use it.")
            } else if modelsError == nil {
                Text("Looking for Claude Opus on \(server.name)…")
            }
        }
    }

    private var providers: [DefaultsProvider] {
        let listed = saved?.providers ?? []
        return listed.isEmpty ? [DefaultsProvider(id: "codex"), DefaultsProvider(id: "claude")] : listed
    }

    private var providerModels: [AgentModel] {
        (models ?? []).filter { ($0.provider ?? ModelPicker.provider(of: $0.id, in: [])) == draft.provider }
    }

    private var efforts: [String] {
        var list = ModelPicker.efforts(models ?? [], model: draft.model, provider: draft.provider)
        if let effort = draft.effort, !list.contains(effort) { list.append(effort) }
        return list
    }

    private var repositoryChoices: [AgentRepository] {
        NewSessionDefaults.repositories(repositories ?? [], saved: draft.repository)
    }

    private var agentFooter: String {
        if let mode = draft.startingModes(for: draft.provider).first(where: { $0.id == draft.startingMode }),
            let description = mode.description
        {
            return "\(mode.label): \(description)."
        }
        return "Default uses \(ModelPicker.providerLabel(draft.provider))'s configured model and mode on \(server.name). Sessions can still pick another model."
    }

    /// A provider change keeps only what the new provider offers.
    private var providerChoice: Binding<String> {
        Binding(
            get: { draft.provider },
            set: { provider in
                guard provider != draft.provider else { return }
                draft.provider = provider
                if let model = draft.model, ModelPicker.provider(of: model, in: models ?? []) != provider {
                    draft.model = nil
                }
                draft.effort = ModelPicker.syncedEffort(
                    draft.effort, models: models ?? [], model: draft.model, provider: provider)
                if !draft.startingModes(for: provider).contains(where: { $0.id == draft.startingMode }) {
                    draft.startingMode = nil
                }
            })
    }

    /// Efforts follow the chosen model.
    private var modelChoice: Binding<String?> {
        Binding(
            get: { draft.model },
            set: { model in
                draft.model = model
                draft.effort = ModelPicker.syncedEffort(
                    draft.effort, models: models ?? [], model: model, provider: draft.provider)
            })
    }

    private func load() {
        if saved == nil { start(.defaults) }
        if models == nil { start(.models) }
        if repositories == nil { start(.repositories) }
    }

    private func start(_ load: Load) {
        guard !loading.contains(load) else { return }
        loading.insert(load)
        Task {
            defer { loading.remove(load) }
            switch load {
            case .defaults:
                do {
                    let defaults = try await server.api.agentDefaults()
                    saved = defaults
                    draft = defaults
                    loadError = nil
                } catch let remote as RemoteError where remote.isUnknownMethod {
                    loadError = "\(server.name) runs an older difu without shared new-session defaults. Update difu there."
                } catch {
                    loadError = error.localizedDescription
                }
            case .models:
                do {
                    models = try await server.api.models()
                    modelsError = nil
                } catch {
                    modelsError = error.localizedDescription
                }
            case .repositories:
                do {
                    repositories = try await server.api.repositories()
                    repositoriesError = nil
                } catch {
                    repositoriesError = error.localizedDescription
                }
            }
        }
    }

    private func useOpus(_ opus: AgentModel, effort: String?) {
        draft.provider = "claude"
        draft.model = opus.id
        draft.effort = effort
        if !draft.startingModes(for: "claude").contains(where: { $0.id == draft.startingMode }) {
            draft.startingMode = nil
        }
        save()
    }

    private func save() {
        guard let saved else { return }
        let change = AgentDefaultsChange(from: saved, to: draft)
        guard !change.isEmpty else { return }
        saving = true
        error = nil
        Task {
            defer { saving = false }
            do {
                let result = try await server.api.setAgentDefaults(change)
                self.saved = result
                draft = result
                Haptics.success()
            } catch {
                self.error = error.localizedDescription
                Haptics.warning()
            }
        }
    }
}
