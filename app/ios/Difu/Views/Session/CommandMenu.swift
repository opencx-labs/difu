import DifuKit
import SwiftUI

/// A session's slash commands, loaded from its own computer or server the
/// first time `/` is typed. Computers without `session.commands` get the
/// commands existing methods can run.
@MainActor
@Observable
final class SlashCommandModel {
    private(set) var list: SessionCommandList?
    private(set) var error: String?
    private(set) var loading = false
    /// Lists only the provider's skills and commands (`/skills`).
    var skillsOnly = false

    /// The loaded commands, or the fallback set until they arrive.
    var commands: [SessionCommand] { list?.commands ?? SessionCommandList.fallback.commands }

    func load(_ api: DifuAPI, session: String, refresh: Bool = false) async {
        guard !loading, refresh || list == nil else { return }
        loading = true
        defer { loading = false }
        do {
            list = try await api.commands(session, refresh: refresh)
            error = nil
        } catch let remote as RemoteError where remote.isUnknownMethod {
            list = .fallback
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }
}

/// The filterable `/` list above the composer.
struct CommandMenu: View {
    let commands: SlashCommandModel
    let text: String
    let serverName: String
    let choose: (SessionCommand) -> Void
    let reload: () -> Void

    private static let rowHeight: CGFloat = 52

    var body: some View {
        let matches = SlashCommands.matches(
            commands.commands, query: query, skillsOnly: commands.skillsOnly)
        VStack(alignment: .leading, spacing: 0) {
            if commands.skillsOnly {
                HStack {
                    Text("Skills").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                    Spacer()
                    Button("Done") { commands.skillsOnly = false }
                        .font(.caption.weight(.semibold))
                }
                .padding(.horizontal, 12)
                .padding(.top, 8)
            }
            if !matches.isEmpty {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(matches) { command in
                            Button { choose(command) } label: { CommandRow(command: command) }
                                .buttonStyle(.plain)
                                .frame(height: Self.rowHeight)
                            if command.id != matches.last?.id { Divider().padding(.leading, 12) }
                        }
                    }
                }
                .frame(height: min(CGFloat(matches.count), 4.5) * Self.rowHeight)
            }
            footer(empty: matches.isEmpty)
        }
        .background(Color.codeBackground, in: RoundedRectangle(cornerRadius: 14))
        .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(Color.secondary.opacity(0.15)))
    }

    /// What is typed after `/`; in the skills list a bare word filters too.
    private var query: SlashCommands.Query {
        if let query = SlashCommands.query(text) { return query }
        let word = text.trimmingCharacters(in: .whitespaces)
        return SlashCommands.Query(name: word.hasPrefix("$") ? String(word.dropFirst()) : word)
    }

    @ViewBuilder private func footer(empty: Bool) -> some View {
        let message: String? =
            if commands.loading {
                "Loading commands and skills…"
            } else if let error = commands.error {
                error
            } else if let first = commands.list?.errors.first {
                "Skill discovery: \(first)"
            } else if commands.list?.isFallback == true {
                "Update difu on \(serverName) for every command and the agent's skills."
            } else if empty {
                commands.skillsOnly ? "No skills for this workspace" : "No matching commands"
            } else {
                nil
            }
        if let message {
            HStack(spacing: 8) {
                if commands.loading { ProgressView().controlSize(.mini) }
                Text(message)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                Spacer(minLength: 0)
                if !commands.loading {
                    Button(action: reload) {
                        Image(systemName: "arrow.clockwise")
                    }
                    .font(.caption)
                    .accessibilityLabel("Reload commands and skills")
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
        }
    }
}

private struct CommandRow: View {
    let command: SessionCommand

    var body: some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(command.trigger)
                        .font(.monoBody.weight(.semibold))
                    if let hint = command.hint {
                        Text(hint)
                            .font(.monoCaption)
                            .foregroundStyle(.secondary)
                    }
                }
                .lineLimit(1)
                if !command.description.isEmpty {
                    Text(command.description)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
            if command.confirm {
                Image(systemName: "exclamationmark.triangle")
                    .imageScale(.small)
                    .foregroundStyle(.warning)
                    .accessibilityLabel("Asks before running")
            }
            if !command.isDifu {
                Pill(text: ModelPicker.providerLabel(command.source))
            } else if command.kind == .screen {
                Image(systemName: "chevron.right")
                    .imageScale(.small)
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.horizontal, 12)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityHint(command.description)
    }
}
