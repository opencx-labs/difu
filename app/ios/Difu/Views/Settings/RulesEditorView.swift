import DifuKit
import SwiftUI

/// Edits a repository's rules on the server (shared by every client).
struct RulesEditorView: View {
    let server: ServerConnection
    /// Opens this repository's rules (a session's `/rules`) instead of the default's.
    var initialRepository: String?

    @State private var repositories: [AgentRepository]?
    @State private var repositoriesError: String?
    @State private var repository: String?
    @State private var rules = ""
    @State private var original: String?
    @State private var rulesError: String?
    @State private var saving = false
    @State private var error: String?
    @FocusState private var editing: Bool

    var body: some View {
        ThemedForm {
            Section {
                if let repositories {
                    if repositories.isEmpty {
                        Text("No repositories on \(server.name) yet.")
                            .foregroundStyle(.secondary)
                    } else {
                        Picker("Repository", selection: $repository) {
                            ForEach(repositories) { repository in
                                Text(repository.name).tag(Optional(repository.path))
                            }
                        }
                        .pickerStyle(.navigationLink)
                    }
                } else if let repositoriesError {
                    Label(repositoriesError, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.secondary)
                } else {
                    ProgressView()
                }
            }

            if repository != nil {
                Section {
                    if original != nil {
                        TextEditor(text: $rules)
                            .font(.mono)
                            .frame(minHeight: 260)
                            .focused($editing)
                            .textInputAutocapitalization(.never)
                    } else if let rulesError {
                        Label(rulesError, systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.secondary)
                    } else {
                        ProgressView()
                    }
                } header: {
                    Text("Rules")
                } footer: {
                    Text("Instructions every new session in this repository follows. Running sessions keep the rules they started with.")
                }
            }
        }
        .navigationTitle("Repository Rules")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                if saving {
                    ProgressView()
                } else {
                    Button("Save") { save() }
                        .disabled(original == nil || rules == original)
                }
            }
        }
        .task { await loadRepositories() }
        .task(id: repository) { await loadRules() }
        .errorAlert($error, title: "Couldn't save the rules")
    }

    private func loadRepositories() async {
        do {
            var list = try await server.api.repositories()
            if let initialRepository, !list.contains(where: { $0.path == initialRepository }) {
                list.insert(AgentRepository(path: initialRepository), at: 0)
            }
            repositories = list
            if repository == nil {
                repository = initialRepository ?? (list.first(where: \.isDefault) ?? list.first)?.path
            }
        } catch {
            repositoriesError = error.localizedDescription
        }
    }

    private func loadRules() async {
        guard let repository else { return }
        original = nil
        rulesError = nil
        do {
            let text = try await server.api.rules(for: repository)
            guard repository == self.repository else { return }
            rules = text
            original = text
        } catch {
            rulesError = error.localizedDescription
        }
    }

    private func save() {
        guard let repository else { return }
        saving = true
        let text = rules
        Task {
            defer { saving = false }
            do {
                try await server.api.setRules(text, for: repository)
                original = text
                editing = false
                Haptics.success()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}
