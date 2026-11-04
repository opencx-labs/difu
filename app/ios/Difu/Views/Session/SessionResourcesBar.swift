import DifuKit
import SwiftUI

/// The terminal's row of session controls under the chat: Artifacts, Shells
/// and sub-agents, then the session's pull requests. Hidden when the session
/// has none of them.
struct SessionResourcesBar: View {
    let view: SessionView
    /// The list's copy of the session, which counts shells sooner.
    let listed: SessionSummary?
    let prs: [SessionPr]
    let open: (SessionExtrasModel.Sheet) -> Void
    let openPr: (SessionPr) -> Void

    var body: some View {
        let shells = listed?.shells ?? view.summary.shells
        let running = view.subagents.filter(\.active).count
        if !view.artifacts.isEmpty || shells > 0 || !view.subagents.isEmpty || !prs.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    if !view.artifacts.isEmpty {
                        chip("Artifacts", count: "\(view.artifacts.count)", symbol: "doc.richtext") {
                            open(.artifacts)
                        }
                    }
                    if shells > 0 {
                        chip("Shells", count: "\(shells)", symbol: "terminal") { open(.shells) }
                    }
                    if !view.subagents.isEmpty {
                        chip(
                            running > 0 ? "Sub-agents · Working" : "Sub-agents",
                            count: running > 0 ? "\(running)/\(view.subagents.count)" : "\(view.subagents.count)",
                            symbol: "person.2", working: running > 0
                        ) { open(.subagents) }
                    }
                    ForEach(prs) { pr in
                        Button { openPr(pr) } label: { SessionPrChip(pr: pr) }
                            .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
            }
            .background(.bar)
        }
    }

    private func chip(
        _ title: String, count: String, symbol: String, working: Bool = false, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 5) {
                if working {
                    PulsingDot(size: 6)
                } else {
                    Image(systemName: symbol).imageScale(.small)
                }
                Text(title)
                    .font(.caption.weight(.semibold))
                Text(count)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(Color.secondary.opacity(0.12), in: Capsule())
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(title), \(count)")
    }
}
