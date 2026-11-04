import DifuKit
import SwiftUI

/// Edits a server repository's environment profile: the tools its sandboxes
/// install, the setup commands new worktrees run, non-secret variables and
/// notes for agents. Agents extend it too (`difu_environment`).
struct EnvironmentEditorView: View {
    let server: ServerConnection
    /// `owner/name`.
    let repository: String

    @State private var loaded: RepositoryEnvironment?
    @State private var loadError: String?
    @State private var draft = Draft()
    @State private var saved = Draft()
    @State private var saving = false
    @State private var error: String?
    @FocusState private var focused: Bool

    var body: some View {
        Loadable(value: loaded, error: loadError, loadingText: "Loading environment…", retry: load) {
            environment in
            form(environment)
        }
        .navigationTitle("Environment")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                if saving {
                    ProgressView()
                } else {
                    Button("Save") { save() }
                        .disabled(loaded == nil || draft == saved)
                }
            }
        }
        .task { await load() }
        .errorAlert($error, title: "Couldn't save the environment")
    }

    private func form(_ environment: RepositoryEnvironment) -> some View {
        ThemedForm {
            if !environment.detected.isEmpty {
                Section {
                    ForEach(environment.detected, id: \.self) { tool in
                        HStack {
                            Text("\(tool.tool) \(tool.version)").font(.monoBody)
                            Spacer()
                            Text(tool.corepack ? "\(tool.source) · corepack" : tool.source)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                    }
                } header: {
                    Text("Declared by the repository")
                } footer: {
                    Text("Version files on \(environment.repository)'s main branch. They win over the tools below.")
                }
            }

            Section {
                ForEach($draft.tools) { $row in
                    PairRow(name: $row.name, value: $row.value, namePrompt: "tool", valuePrompt: "version")
                        .focused($focused)
                }
                .onDelete { draft.tools.remove(atOffsets: $0) }
                Button("Add Tool", systemImage: "plus") { draft.tools.append(Pair()) }
            } header: {
                Text("Tools")
            } footer: {
                Text("mise names and versions, e.g. node 24, pnpm 9, python 3.12. Installed once into the server's shared cache.")
            }

            Section {
                ForEach($draft.setup) { $line in
                    TextField("pnpm install --frozen-lockfile", text: $line.text, axis: .vertical)
                        .font(.mono)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .focused($focused)
                }
                .onDelete { draft.setup.remove(atOffsets: $0) }
                .onMove { draft.setup.move(fromOffsets: $0, toOffset: $1) }
                Button("Add Command", systemImage: "plus") { draft.setup.append(Line()) }
            } header: {
                Text("Setup")
            } footer: {
                Text("Run in order in each new worktree before the agent starts, and again only when lockfiles, commands or tools change.")
            }

            Section {
                ForEach($draft.env) { $row in
                    PairRow(name: $row.name, value: $row.value, namePrompt: "NAME", valuePrompt: "value")
                        .focused($focused)
                }
                .onDelete { draft.env.remove(atOffsets: $0) }
                Button("Add Variable", systemImage: "plus") { draft.env.append(Pair()) }
            } header: {
                Text("Variables")
            } footer: {
                Label("Never secrets: every device paired with \(server.name) can read them.", systemImage: "lock.slash")
                    .foregroundStyle(Color.warning)
            }

            Section {
                ForEach($draft.notes) { $line in
                    TextField("Backend tests need Postgres via docker compose", text: $line.text, axis: .vertical)
                        .focused($focused)
                }
                .onDelete { draft.notes.remove(atOffsets: $0) }
                Button("Add Note", systemImage: "plus") { draft.notes.append(Line()) }
            } header: {
                Text("Notes for agents")
            }

            if !environment.profile.history.isEmpty {
                Section("Recent Changes") {
                    ForEach(Array(environment.profile.history.reversed().prefix(10).enumerated()), id: \.offset) {
                        _, change in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(change.summary)
                                .font(.callout)
                                .lineLimit(3)
                            Text(changeCaption(change))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(2)
                        }
                    }
                }
            }
        }
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                EditButton()
            }
        }
    }

    private func changeCaption(_ change: EnvironmentChange) -> String {
        guard let at = change.at else { return change.by }
        return "\(change.by) · \(Formatting.relative(Date(milliseconds: at)))"
    }

    private func load() async {
        do {
            let environment = try await server.api.environment(for: repository)
            loaded = environment
            loadError = nil
            draft = Draft(environment.profile)
            saved = draft
        } catch {
            loadError = error.localizedDescription
        }
    }

    private func save() {
        let profile = draft.profile
        saving = true
        Task {
            defer { saving = false }
            do {
                let environment = try await server.api.setEnvironment(profile, for: repository)
                loaded = environment
                draft = Draft(environment.profile)
                saved = draft
                focused = false
                Haptics.success()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

/// An editable profile, with stable row identities for the lists.
private struct Draft: Equatable {
    var tools: [Pair] = []
    var setup: [Line] = []
    var env: [Pair] = []
    var notes: [Line] = []

    init() {}

    init(_ profile: EnvironmentProfile) {
        tools = profile.tools.map { Pair(name: $0.name, value: $0.value) }
        setup = profile.setup.map { Line(text: $0) }
        env = profile.env.map { Pair(name: $0.name, value: $0.value) }
        notes = profile.notes.map { Line(text: $0) }
    }

    var profile: EnvironmentProfile {
        EnvironmentProfile(
            tools: tools.map { EnvironmentEntry(name: $0.name, value: $0.value) },
            setup: setup.map(\.text),
            env: env.map { EnvironmentEntry(name: $0.name, value: $0.value) },
            notes: notes.map(\.text))
    }

    /// Equal when the profiles they would save are equal, whatever the rows' identities.
    static func == (lhs: Draft, rhs: Draft) -> Bool {
        lhs.profile.json == rhs.profile.json
    }
}

private struct Pair: Identifiable {
    let id = UUID()
    var name = ""
    var value = ""
}

private struct Line: Identifiable {
    let id = UUID()
    var text = ""
}

/// `name` and `value` side by side, in monospace.
private struct PairRow: View {
    @Binding var name: String
    @Binding var value: String
    let namePrompt: String
    let valuePrompt: String

    var body: some View {
        HStack {
            TextField(namePrompt, text: $name)
                .font(.monoBody)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Divider()
            TextField(valuePrompt, text: $value)
                .font(.monoBody)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
        }
    }
}
