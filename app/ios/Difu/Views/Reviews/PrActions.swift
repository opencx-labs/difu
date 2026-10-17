import DifuKit
import SwiftUI

/// Comment / approve / request changes / merge / close / ready-or-draft.
/// Every write passes the overview's `head` and reloads the overview after.
struct PrActionsMenu: View {
    let model: PrModel

    @Environment(\.openURL) private var openURL
    @State private var compose: Compose?
    @State private var showingMerge = false
    @State private var draftChange: Bool?
    @State private var result: String?
    @State private var error: String?

    enum Compose: String, Identifiable {
        case comment, approve, requestChanges, close
        var id: String { rawValue }

        var title: String {
            switch self {
            case .comment: "Comment"
            case .approve: "Approve"
            case .requestChanges: "Request Changes"
            case .close: "Close Pull Request"
            }
        }

        var action: String {
            switch self {
            case .comment: "Comment"
            case .approve: "Approve"
            case .requestChanges: "Request Changes"
            case .close: "Close"
            }
        }

        var bodyRequired: Bool { self == .comment || self == .requestChanges }

        var placeholder: String {
            switch self {
            case .comment: "Leave a comment"
            case .approve: "Optional comment"
            case .requestChanges: "What needs to change?"
            case .close: "Optional closing comment"
            }
        }
    }

    var body: some View {
        let overview = model.overview
        Menu {
            Button("Comment", systemImage: "text.bubble") { compose = .comment }
            Button("Approve", systemImage: "checkmark.seal") { compose = .approve }
            Button("Request Changes", systemImage: "exclamationmark.bubble") { compose = .requestChanges }
            if overview?.isOpen ?? true {
                Divider()
                Button("Merge…", systemImage: "arrow.triangle.merge") { showingMerge = true }
                if overview?.draft == true {
                    Button("Mark Ready for Review", systemImage: "eye") { draftChange = false }
                } else {
                    Button("Convert to Draft", systemImage: "pencil.circle") { draftChange = true }
                }
                Button("Close Pull Request", systemImage: "xmark.circle", role: .destructive) { compose = .close }
            }
            if let url = overview?.url {
                Divider()
                Button("Open on GitHub", systemImage: "safari") { openURL(url) }
            }
        } label: {
            Image(systemName: "ellipsis.circle")
        }
        .disabled(overview == nil)
        .sheet(item: $compose) { kind in
            ComposeSheet(kind: kind) { body in
                try await submit(kind, body: body)
            }
        }
        .sheet(isPresented: $showingMerge) {
            MergeSheet(overview: overview) { squash, admin in
                try await model.write { api, head in
                    try await api.merge(repo: model.repo, number: model.number, head: head, squash: squash, admin: admin)
                }
            } done: { message in
                result = message
            }
        }
        .confirmationDialog(
            draftChange == true ? "Convert to draft?" : "Mark ready for review?",
            isPresented: Binding(get: { draftChange != nil }, set: { if !$0 { draftChange = nil } }),
            titleVisibility: .visible,
            presenting: draftChange
        ) { draft in
            Button(draft ? "Convert to Draft" : "Mark Ready") {
                Task { await setDraft(draft) }
            }
        }
        .alert("Done", isPresented: Binding(get: { result != nil }, set: { if !$0 { result = nil } })) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(result ?? "")
        }
        .errorAlert($error)
    }

    private func submit(_ kind: Compose, body: String) async throws {
        let message = try await model.write { api, head in
            switch kind {
            case .comment:
                try await api.comment(repo: model.repo, number: model.number, head: head, body: body)
            case .approve:
                try await api.review(repo: model.repo, number: model.number, head: head, event: .approve, body: body)
            case .requestChanges:
                try await api.review(
                    repo: model.repo, number: model.number, head: head, event: .requestChanges, body: body)
            case .close:
                try await api.close(repo: model.repo, number: model.number, head: head, body: body)
            }
        }
        Haptics.success()
        result = message
    }

    private func setDraft(_ draft: Bool) async {
        do {
            result = try await model.write { api, head in
                try await api.setReady(repo: model.repo, number: model.number, head: head, draft: draft)
            }
        } catch {
            self.error = error.localizedDescription
        }
    }
}

/// Text body for a comment, review or close.
private struct ComposeSheet: View {
    let kind: PrActionsMenu.Compose
    let submit: (String) async throws -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var sending = false
    @State private var error: String?
    @FocusState private var focused: Bool

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField(kind.placeholder, text: $text, axis: .vertical)
                        .lineLimit(6...16)
                        .focused($focused)
                } footer: {
                    if kind == .close {
                        Text("Closes the pull request without merging.")
                    }
                }
            }
            .navigationTitle(kind.title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if sending {
                        ProgressView()
                    } else {
                        Button(kind.action) { send() }
                            .disabled(kind.bodyRequired && trimmed.isEmpty)
                    }
                }
            }
            .onAppear { focused = true }
            .errorAlert($error)
        }
        .presentationDetents([.medium, .large])
    }

    private var trimmed: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    private func send() {
        sending = true
        Task {
            defer { sending = false }
            do {
                try await submit(trimmed)
                dismiss()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

/// Merge method and admin override.
private struct MergeSheet: View {
    let overview: PrOverview?
    let merge: (_ squash: Bool, _ admin: Bool) async throws -> String
    let done: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var squash = true
    @State private var admin = false
    @State private var merging = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                if let overview {
                    Section {
                        Text(overview.title).font(.headline)
                        if let head = overview.headBranch, let base = overview.baseBranch {
                            Text("\(head) → \(base)").font(.monoCaption).foregroundStyle(.secondary)
                        }
                        if overview.mergeable?.uppercased() == "CONFLICTING" {
                            Label("This branch has conflicts.", systemImage: "exclamationmark.triangle.fill")
                                .foregroundStyle(.red)
                        }
                    }
                }
                Section("Method") {
                    Picker("Method", selection: $squash) {
                        Text("Squash and merge").tag(true)
                        Text("Create a merge commit").tag(false)
                    }
                    .pickerStyle(.inline)
                    .labelsHidden()
                }
                Section {
                    Toggle("Merge as administrator", isOn: $admin)
                } footer: {
                    Text("Bypasses branch protection rules. Only works if you are a repository admin.")
                }
                Section {
                    Button {
                        run()
                    } label: {
                        HStack {
                            Spacer()
                            if merging { ProgressView() } else { Text("Merge Pull Request").fontWeight(.semibold) }
                            Spacer()
                        }
                    }
                    .disabled(merging)
                }
            }
            .navigationTitle("Merge")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
            .errorAlert($error, title: "Couldn't merge")
        }
        .presentationDetents([.medium, .large])
    }

    private func run() {
        merging = true
        Task {
            defer { merging = false }
            do {
                let message = try await merge(squash, admin)
                Haptics.success()
                dismiss()
                done(message)
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}
