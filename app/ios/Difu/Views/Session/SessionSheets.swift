import DifuKit
import SwiftUI

/// Changes the session's model and effort (`session.model`).
struct ModelSheet: View {
    let sessionID: String
    let view: SessionView?

    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var models: [AgentModel]?
    @State private var modelsError: String?
    @State private var model: String?
    @State private var effort: String?
    @State private var saving = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                if let view {
                    Section {
                        LabeledContent("Provider", value: view.provider?.capitalized ?? "—")
                        LabeledContent("Current model", value: view.model ?? "Default")
                        LabeledContent("Current effort", value: view.effort?.capitalized ?? "Default")
                    }
                }
                ModelPickerSection(
                    models: models, error: modelsError, model: $model, effort: $effort,
                    provider: view?.provider)
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
                        Button("Save") { save() }
                    }
                }
            }
            .task {
                model = view?.model
                effort = view?.effort
                do {
                    models = try await app.api.models()
                } catch {
                    modelsError = error.localizedDescription
                }
            }
            .errorAlert($error)
        }
        .presentationDetents([.medium, .large])
    }

    private func save() {
        saving = true
        Task {
            defer { saving = false }
            do {
                try await app.api.setModel(sessionID, model: model, effort: effort)
                dismiss()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

/// Uncommitted plus committed worktree changes (`session.changes`).
struct ChangesSheet: View {
    let sessionID: String

    @Environment(AppModel.self) private var app
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
            files = try await app.api.changes(sessionID)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }
}
