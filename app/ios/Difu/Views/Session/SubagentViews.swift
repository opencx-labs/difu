import DifuKit
import SwiftUI

/// The session's sub-agents (name, state, provider); each opens read-only.
struct SubagentsSheet: View {
    let sessionID: String
    let subagents: [SubagentSummary]

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List(subagents) { agent in
                NavigationLink {
                    SubagentTranscriptView(sessionID: sessionID, agent: agent)
                } label: {
                    SubagentRow(agent: agent)
                }
            }
            .overlay {
                if subagents.isEmpty {
                    ContentUnavailableView(
                        "No sub-agents", systemImage: "person.2",
                        description: Text("Agents the session starts for parts of its task appear here."))
                }
            }
            .navigationTitle("Sub-agents")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }
}

private struct SubagentRow: View {
    let agent: SubagentSummary

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Group {
                if agent.active {
                    PulsingDot(size: 8)
                } else {
                    Image(systemName: symbol).foregroundStyle(.secondary)
                }
            }
            .frame(width: 22, height: 20)
            VStack(alignment: .leading, spacing: 3) {
                Text(agent.title)
                    .font(.body.weight(.medium))
                    .lineLimit(2)
                Text([agent.stateLabel, agent.providerLabel, agent.background ? "Background" : nil]
                    .compactMap { $0 }.joined(separator: " · "))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .combine)
    }

    private var symbol: String {
        switch agent.state {
        case "Completed": "checkmark.circle"
        case "Failed": "exclamationmark.circle"
        case "Interrupted", "Closed": "stop.circle"
        default: "circle.dotted"
        }
    }
}

/// A sub-agent's conversation, read-only, rendered like the session's own.
/// While it works it's read again every few seconds (Codex's history is read
/// from its thread, as the terminal's pane does).
struct SubagentTranscriptView: View {
    let sessionID: String
    let agent: SubagentSummary

    @Environment(ServerConnection.self) private var server
    @State private var transcript: SubagentTranscript?
    @State private var error: String?
    @State private var expanded: Set<String> = []

    var body: some View {
        Loadable(value: transcript, error: error, loadingText: "Loading the sub-agent's conversation…", retry: {
            await load(refresh: true)
        }) { transcript in
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    header(transcript)
                    if transcript.earlier > 0 {
                        Text("\(transcript.earlier) earlier messages aren't shown.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    ForEach(Array(transcript.entries.enumerated()), id: \.offset) { index, entry in
                        let key = "\(index)·\(entry.id)"
                        EntryRow(
                            entry: entry, active: transcript.subagent.active, expanded: expanded.contains(key),
                            loadingFull: false,
                            toggle: {
                                withAnimation(.snappy(duration: 0.2)) {
                                    if expanded.contains(key) { expanded.remove(key) } else { expanded.insert(key) }
                                }
                            },
                            loadFull: {})
                    }
                    if transcript.entries.isEmpty {
                        Text(
                            transcript.subagent.active
                                ? "Waiting for sub-agent messages…" : "No messages were recorded for this sub-agent."
                        )
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    }
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
            }
            .defaultScrollAnchor(.bottom)
            .refreshable { await load(refresh: true) }
        }
        .navigationTitle(agent.name)
        .navigationBarTitleDisplayMode(.inline)
        .task(id: server.generation) {
            await load(refresh: false)
            while !Task.isCancelled, transcript?.subagent.active ?? agent.active {
                try? await Task.sleep(for: .seconds(3))
                await load(refresh: true)
            }
        }
    }

    private func header(_ transcript: SubagentTranscript) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                if transcript.subagent.active { PulsingDot(size: 6) }
                Text([transcript.subagent.stateLabel, transcript.subagent.providerLabel].joined(separator: " · "))
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.secondary)
            }
            if !transcript.subagent.task.isEmpty {
                Text(transcript.subagent.task)
                    .font(.callout)
                    .textSelection(.enabled)
            }
            if let error = transcript.error {
                Label("Couldn't refresh: \(error)", systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.red)
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.cardBackground, in: RoundedRectangle(cornerRadius: 10))
    }

    private func load(refresh: Bool) async {
        do {
            transcript = try await server.api.subagent(sessionID, agent: agent.id, refresh: refresh)
            error = nil
        } catch let remote as RemoteError where remote.isUnknownMethod {
            error = "Update difu on \(server.name) to read sub-agents from the phone."
        } catch {
            if transcript == nil { self.error = error.localizedDescription }
        }
    }
}
