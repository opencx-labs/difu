import DifuKit
import SwiftUI

/// `/status` and `/usage`: the session's provider, model, mode and workspace,
/// and the provider's rate limits, from the session's computer or server.
struct SessionInfoSheet: View {
    enum Tab: String, CaseIterable, Identifiable {
        case status = "Status"
        case usage = "Usage"
        var id: String { rawValue }
    }

    let sessionID: String
    @State var tab: Tab

    @Environment(ServerConnection.self) private var server
    @Environment(\.dismiss) private var dismiss
    @State private var status: SessionStatusInfo?
    @State private var statusError: String?
    @State private var usage: UsageReport?
    @State private var usageError: String?

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                Picker("Show", selection: $tab) {
                    ForEach(Tab.allCases) { Text($0.rawValue).tag($0) }
                }
                .pickerStyle(.segmented)
                .padding(.horizontal)
                .padding(.vertical, 8)
                switch tab {
                case .status:
                    Loadable(value: status, error: statusError, loadingText: "Loading status…", retry: loadStatus) {
                        StatusForm(status: $0)
                    }
                case .usage:
                    Loadable(value: usage, error: usageError, loadingText: "Asking the provider…", retry: loadUsage) {
                        UsageForm(usage: $0)
                    }
                }
            }
            .themedBackground()
            .navigationTitle(tab == .status ? "Status" : "Usage")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .task(id: tab) {
                switch tab {
                case .status: if status == nil { await loadStatus() }
                case .usage: if usage == nil { await loadUsage() }
                }
            }
        }
        .presentationDetents([.medium, .large])
    }

    private func loadStatus() async {
        do {
            status = try await server.api.runCommand(sessionID, name: "/status").status
            statusError = status == nil ? "difu didn't return the session status." : nil
        } catch {
            statusError = error.localizedDescription
        }
    }

    private func loadUsage() async {
        do {
            usage = try await server.api.runCommand(sessionID, name: "/usage").usage
            usageError = usage == nil ? "difu didn't return the provider's usage." : nil
        } catch {
            usageError = error.localizedDescription
        }
    }
}

private struct StatusForm: View {
    let status: SessionStatusInfo

    var body: some View {
        ThemedForm {
            Section {
                row("Title", status.title)
                row("State", status.state)
            }
            Section("Agent") {
                row("Provider", status.providerLabel)
                row("Model", status.model ?? "Default")
                row("Effort", status.effort?.capitalized ?? "Default")
                if let mode = status.mode { row("Mode", mode) }
                if let preset = status.preset { row("Approvals", preset) }
                row("Provider session", status.providerSession ?? "Not connected", mono: true)
            }
            Section("Workspace") {
                if let repository = status.repository { row("Repository", repository, mono: true) }
                row("Workspace", status.workspace ?? "Not prepared yet", mono: true)
                row("Branch", status.branch ?? "Existing directory", mono: true)
            }
        }
    }

    private func row(_ label: String, _ value: String, mono: Bool = false) -> some View {
        LabeledContent(label) {
            Text(value)
                .font(mono ? .monoCaption : .body)
                .multilineTextAlignment(.trailing)
                .textSelection(.enabled)
        }
    }
}

private struct UsageForm: View {
    let usage: UsageReport

    var body: some View {
        ThemedForm {
            if usage.limits.isEmpty && usage.details.isEmpty {
                Text("The provider reported no usage.")
                    .foregroundStyle(.secondary)
            }
            ForEach(sections(usage.limits.map(\.section)), id: \.self) { section in
                Section(section ?? usage.title) {
                    ForEach(usage.limits.filter { $0.section == section }) { limit in
                        LimitRow(limit: limit)
                    }
                }
            }
            ForEach(sections(usage.details.map(\.section)), id: \.self) { section in
                Section(section ?? "Details") {
                    ForEach(usage.details.filter { $0.section == section }) { detail in
                        LabeledContent(detail.label) {
                            Text(detail.value).multilineTextAlignment(.trailing)
                        }
                    }
                }
            }
        }
    }

    /// Section names in first-seen order.
    private func sections(_ names: [String?]) -> [String?] {
        var seen: [String?] = []
        for name in names where !seen.contains(name) { seen.append(name) }
        return seen
    }
}

private struct LimitRow: View {
    let limit: UsageReport.Limit

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(limit.title).font(.callout.weight(.semibold))
                Spacer()
                Text(limit.caption).font(.callout.monospacedDigit()).foregroundStyle(.secondary)
            }
            ProgressView(value: limit.percent, total: 100)
                .tint(limit.remaining ? (limit.percent < 15 ? .failure : .accent) : (limit.percent > 85 ? .failure : .violet))
            if let resetsAt = limit.resetsAt {
                Text("Resets \(Date(milliseconds: resetsAt).formatted(date: .abbreviated, time: .shortened))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }
}

/// `/permissions` and the session's mode chip: the provider's modes and, for
/// Codex, approval presets. Unavailable ones say why; the computer's refusal
/// is shown as it words it.
struct PermissionsSheet: View {
    let sessionID: String

    @Environment(ServerConnection.self) private var server
    @Environment(\.dismiss) private var dismiss
    @State private var info: PermissionsInfo?
    @State private var loadError: String?
    @State private var changing: String?
    /// A choice that skips permission checks, waiting for confirmation.
    @State private var confirming: PermissionChoice?
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Loadable(value: info, error: loadError, loadingText: "Asking the provider…", retry: load) { info in
                ThemedForm {
                    Section {
                        ForEach(info.modes) { choice in
                            ChoiceRow(choice: choice, selected: info.selectedMode == choice.id, changing: changing == choice.id) {
                                select(choice)
                            }
                        }
                    } header: {
                        Text("Mode")
                    } footer: {
                        if info.pendingMode != nil && info.pendingMode != info.activeMode {
                            Text("Applies from the next turn.")
                        }
                    }
                    if !info.presets.isEmpty {
                        Section {
                            ForEach(info.presets) { choice in
                                ChoiceRow(choice: choice, selected: info.selectedPreset == choice.id, changing: changing == choice.id) {
                                    select(choice)
                                }
                            }
                        } header: {
                            Text("Approvals")
                        } footer: {
                            Text("Codex's approval and sandbox policy. Edit individual rules in difu on your computer.")
                        }
                    }
                    if let message = info.message {
                        Section { Text(message).foregroundStyle(.secondary) }
                    }
                    if info.reloadPending {
                        Section {
                            Label("The agent restarts to apply this before its next turn.", systemImage: "arrow.clockwise")
                                .font(.callout)
                        }
                    }
                }
            }
            .navigationTitle("Permissions")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .task { if info == nil { await load() } }
            .confirmationDialog(
                "Switch to \(confirming?.label ?? "this mode")?",
                isPresented: Binding(get: { confirming != nil }, set: { if !$0 { confirming = nil } }),
                titleVisibility: .visible,
                presenting: confirming
            ) { choice in
                Button("Switch to \(choice.label)", role: .destructive) { change(choice) }
            } message: { _ in
                Text("The agent runs every command and edit without asking until you switch back.")
            }
            .errorAlert($error, title: "Couldn't change permissions")
        }
        .presentationDetents([.medium, .large])
    }

    private func load() async {
        do {
            info = try await server.api.runCommand(sessionID, name: "/permissions").permissions
            loadError = info == nil ? "difu didn't return the session's permissions." : nil
        } catch {
            loadError = error.localizedDescription
        }
    }

    private func select(_ choice: PermissionChoice) {
        guard changing == nil else { return }
        if NewSessionDefaults.skipsChecks(mode: choice.id, preset: choice.id) {
            confirming = choice
        } else {
            change(choice)
        }
    }

    private func change(_ choice: PermissionChoice) {
        guard changing == nil else { return }
        changing = choice.id
        Task {
            defer { changing = nil }
            do {
                if let updated = try await server.api.runCommand(
                    sessionID, name: "/permissions", arguments: choice.id
                ).permissions {
                    info = updated
                }
                Haptics.success()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

private struct ChoiceRow: View {
    let choice: PermissionChoice
    let selected: Bool
    let changing: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(choice.label).foregroundStyle(choice.available ? Color.primary : Color.secondary)
                    if let reason = choice.reason, !choice.available {
                        Text(reason).font(.caption).foregroundStyle(.secondary)
                    } else if !choice.description.isEmpty {
                        Text(choice.description).font(.caption).foregroundStyle(.secondary)
                    }
                }
                Spacer()
                if changing {
                    ProgressView()
                } else if selected {
                    Image(systemName: "checkmark").foregroundStyle(Color.accent)
                }
            }
        }
        .disabled(!choice.available || selected)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// The session's mode (and Codex approvals) as a chip above the composer, like
/// the terminal's mode indicator; tapping it opens the permissions sheet.
struct PermissionModeChip: View {
    let mode: PermissionMode
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 5) {
                Image(systemName: mode.skipsChecks ? "exclamationmark.shield" : "lock.shield")
                    .imageScale(.small)
                Text(mode.chipText)
                    .lineLimit(1)
                Image(systemName: "chevron.up.chevron.down")
                    .imageScale(.small)
                    .foregroundStyle(.secondary)
            }
            .font(.caption.weight(.medium))
            .foregroundStyle(mode.skipsChecks ? Color.red : (mode.isPending ? Color.secondary : Color.primary))
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(Color.secondary.opacity(0.12), in: Capsule())
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Mode: \(mode.chipText)")
        .accessibilityHint("Changes the session's mode")
    }
}
