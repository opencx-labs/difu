import DifuKit
import SwiftUI

/// Changes the session's model and effort (`session.model`). Any provider's
/// model can be chosen: picking another provider's switches the session there,
/// which the computer allows only between turns.
struct ModelSheet: View {
    let sessionID: String
    let session: SessionModel

    @Environment(ServerConnection.self) private var server
    @Environment(\.dismiss) private var dismiss
    @State private var models: [AgentModel]?
    @State private var modelsError: String?
    @State private var model: String?
    @State private var effort: String?
    @State private var loaded = false
    @State private var saving = false
    @State private var error: String?

    var body: some View {
        let view = session.view
        let change = view.map {
            ModelChange.evaluate(session: $0, model: model, effort: effort, models: models ?? [])
        } ?? .unchanged
        NavigationStack {
            Form {
                if let view {
                    Section {
                        LabeledContent("Provider", value: ModelPicker.providerLabel(view.provider))
                        LabeledContent("Current model", value: view.model ?? "Default")
                        LabeledContent("Current effort", value: view.effort?.capitalized ?? "Default")
                    }
                }
                ModelPickerSection(
                    models: models, error: modelsError, model: $model, effort: $effort,
                    provider: view?.provider,
                    footer: "Models from every provider are listed. Default uses the provider's configured model.")
                changeSection(change)
            }
            .navigationTitle("Model & Effort")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if saving {
                        ProgressView()
                    } else {
                        Button(change.isSwitch ? "Switch" : "Save") { save() }
                            .disabled(!change.canSave)
                    }
                }
            }
            .task {
                guard !loaded else { return }
                loaded = true
                model = view?.model
                effort = view?.effort
                do {
                    models = try await server.api.models()
                } catch {
                    modelsError = error.localizedDescription
                }
            }
            .errorAlert($error, title: "Couldn't change the model")
        }
        .presentationDetents([.medium, .large])
    }

    @ViewBuilder private func changeSection(_ change: ModelChange) -> some View {
        switch change {
        case .switchesProvider(let from, let to):
            Section {
                Label {
                    Text(
                        "Switches this session from \(ModelPicker.providerLabel(from)) to \(ModelPicker.providerLabel(to)). The chat and workspace stay; the conversation is handed over with your next message."
                    )
                } icon: {
                    Image(systemName: "arrow.left.arrow.right")
                }
                .font(.callout)
            }
        case .blocked(let reason):
            Section {
                Label {
                    Text(reason)
                } icon: {
                    Image(systemName: "hourglass")
                        .foregroundStyle(.orange)
                }
                .font(.callout)
            } footer: {
                Text("Your choice stays here; save once the session is ready.")
            }
        case .unchanged, .allowed:
            EmptyView()
        }
    }

    private func save() {
        saving = true
        Task {
            defer { saving = false }
            do {
                try await server.api.setModel(sessionID, model: model, effort: effort)
                Haptics.success()
                dismiss()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

extension ModelChange {
    var isSwitch: Bool {
        if case .switchesProvider = self { return true }
        return false
    }
}

/// Uncommitted plus committed worktree changes (`session.changes`).
struct ChangesSheet: View {
    let sessionID: String

    @Environment(ServerConnection.self) private var server
    @Environment(\.dismiss) private var dismiss
    @State private var files: [DiffFile]?
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Loadable(value: files, error: error, loadingText: "Loading changes…", retry: load) { files in
                DiffFileList(files: files)
                    .refreshable { await load() }
            }
            .navigationTitle("Changes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .task { await load() }
        }
    }

    private func load() async {
        do {
            files = try await server.api.changes(sessionID)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }
}
