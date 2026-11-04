import DifuKit
import SwiftUI

/// A computer's or server's defaults for new sessions (`agents.defaults`):
/// provider and model, effort, starting mode, Codex approvals and repository,
/// shared with its terminal and agents. Saved with `agents.defaults.set`.
/// Computers that report effective defaults show what "Automatic" picks.
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
    @State private var confirmingChecks = false
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
                    Button("Save") { requestSave() }
                        .disabled(saved == nil || draft == saved)
                }
            }
        }
        // Not `.task`, which reruns when a picker's list closes and would
        // replace unsaved edits.
        .onAppear { load() }
        .onChange(of: server.generation) { load() }
        .confirmationDialog(
            "Skip permission checks by default?", isPresented: $confirmingChecks, titleVisibility: .visible
        ) {
            Button("Save", role: .destructive) { save() }
        } message: {
            Text("New sessions on \(server.name) would run every command and edit without asking.")
        }
    }

    private var form: some View {
        ThemedForm {
            opusSection

            Section {
                Picker("Provider", selection: providerChoice) {
                    if draft.reportsEffective {
                        Text("Automatic").tag("")
                    }
                    ForEach(providers) { Text($0.label).tag($0.id) }
                }
                if draft.automaticModel {
                    LabeledContent("Model") {
                        Text(automaticModelText)
                    }
                    LabeledContent("Effort") {
                        Text(automaticEffortText)
                    }
                } else {
                    Picker("Model", selection: modelChoice) {
                        Text(defaultModelLabel).tag(String?.none)
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
                Picker("Starting mode", selection: modeChoice) {
                    Text(draft.reportsEffective
                        ? "Automatic (\(draft.automaticModeLabel(for: sessionProvider)))" : "Default")
                        .tag(String?.none)
                    ForEach(draft.startingModes(for: sessionProvider)) { mode in
                        Text(mode.label).tag(Optional(mode.id))
                    }
                }
                if !draft.codexPresets.isEmpty {
                    Picker("Codex approvals", selection: $draft.codexPreset) {
                        Text("Automatic (\(draft.automaticPresetLabel))").tag(String?.none)
                        ForEach(draft.codexPresets) { preset in
                            Text(preset.label).tag(Optional(preset.id))
                        }
                    }
                }
            } header: {
                Text("Mode")
            } footer: {
                Text(modeFooter)
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
                        .foregroundStyle(.failure)
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
                if saved?.automaticModel == true && saved?.effective?.model == opus.id {
                    Text("Automatic already picks \(opus.name) on \(server.name). Saving it keeps it when a newer Opus arrives.")
                } else if saved?.model == opus.id && saved?.effort == effort {
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
        if draft.automaticModel {
            return "Automatic uses the newest Claude Opus on \(server.name) with extra-high reasoning (or its highest), or the provider's configured model without Claude Code, like its terminal. Sessions can still pick another model."
        }
        if !draft.reportsEffective,
            let mode = draft.startingModes(for: draft.provider).first(where: { $0.id == draft.startingMode }),
            let description = mode.description
        {
            return "\(mode.label): \(description)."
        }
        return "Default uses \(ModelPicker.providerLabel(draft.provider))'s configured model on \(server.name). Sessions can still pick another model."
    }

    private var modeFooter: String {
        var parts: [String] = []
        if let mode = draft.startingModes(for: sessionProvider).first(where: { $0.id == draft.startingMode }),
            let description = mode.description
        {
            parts.append("\(mode.label): \(description).")
        }
        if draft.server == true {
            parts.append("Automatic starts Claude Code in Auto and gives Codex Approve for me on servers.")
        }
        if draft.reportsEffective {
            parts.append("Bypass permissions and Full Access are never automatic.")
        }
        return parts.joined(separator: " ")
    }

    /// The "Default" model row: on newer computers Claude Code's means the
    /// newest Opus.
    private var defaultModelLabel: String {
        if draft.reportsEffective && draft.provider == "claude" { return "Newest Opus (automatic)" }
        return "Default (\(ModelPicker.providerLabel(draft.provider)))"
    }

    /// The provider new sessions run on with this draft.
    private var sessionProvider: String {
        guard draft.automaticModel else { return draft.provider }
        if let saved, saved.automaticModel, let effective = saved.effective { return effective.provider }
        return NewSessionDefaults.automaticChoice(models: models).model == nil ? draft.provider : "claude"
    }

    /// What Automatic picks: as the computer reported it when saved that
    /// way, otherwise previewed from its models.
    private var automaticChoice: NewSessionDefaults.Choice {
        if let saved, saved.automaticModel, let effective = saved.effective {
            return .init(model: effective.model, effort: effective.effort)
        }
        return NewSessionDefaults.automaticChoice(models: models)
    }

    private var automaticModelText: String {
        guard let id = automaticChoice.model else {
            return "\(ModelPicker.providerLabel(sessionProvider))'s configured model"
        }
        return models?.first { $0.id == id }?.name ?? id
    }

    private var automaticEffortText: String {
        automaticChoice.effort?.capitalized ?? "Default"
    }

    /// A provider change keeps only what the new provider offers. Automatic
    /// ("") clears the saved provider, model and effort.
    private var providerChoice: Binding<String> {
        Binding(
            get: { draft.automaticModel ? "" : draft.provider },
            set: { provider in
                if provider.isEmpty {
                    guard !draft.automaticModel else { return }
                    draft.savedProvider = nil
                    draft.provider = "codex"
                    draft.model = nil
                    draft.effort = nil
                    if !draft.startingModes(for: "codex").contains(where: { $0.id == draft.startingMode }) {
                        draft.startingMode = nil
                    }
                    return
                }
                guard provider != draft.provider || draft.automaticModel else { return }
                if draft.reportsEffective { draft.savedProvider = provider }
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

    /// A mode the saved provider doesn't offer saves the provider sessions
    /// run on with it (the model stays automatic).
    private var modeChoice: Binding<String?> {
        Binding(
            get: { draft.startingMode },
            set: { mode in
                draft.startingMode = mode
                guard let mode, draft.reportsEffective,
                    !draft.startingModes(for: draft.modeProvider).contains(where: { $0.id == mode })
                else { return }
                let provider = sessionProvider
                draft.savedProvider = provider
                draft.provider = provider
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
        if draft.reportsEffective { draft.savedProvider = "claude" }
        draft.provider = "claude"
        draft.model = opus.id
        draft.effort = effort
        if !draft.startingModes(for: "claude").contains(where: { $0.id == draft.startingMode }) {
            draft.startingMode = nil
        }
        save()
    }

    /// Asks first when the draft newly skips permission checks.
    private func requestSave() {
        guard let saved else { return }
        let bypass = draft.startingMode == "bypassPermissions" && saved.startingMode != draft.startingMode
        let full = draft.codexPreset == "Full" && saved.codexPreset != draft.codexPreset
        if bypass || full {
            confirmingChecks = true
        } else {
            save()
        }
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
