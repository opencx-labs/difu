import DifuKit
import SwiftUI

/// "• Running cargo test (12s)" while the session is working, with Stop.
struct ActivityLine: View {
    let view: SessionView
    let stop: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            VStack(alignment: .leading, spacing: 3) {
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    Text(heading(now: context.date))
                        .font(.subheadline.monospaced())
                        .foregroundStyle(Color.accentColor)
                        .lineLimit(2)
                }
                .phaseAnimator([0.55, 1.0]) { content, opacity in
                    content.opacity(opacity)
                } animation: { _ in
                    .easeInOut(duration: 1.1)
                }
                if view.preparing {
                    Text("You can keep typing. Messages sent now will be queued.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 8)
            Button(role: .destructive, action: stop) {
                Label("Stop", systemImage: "stop.fill")
                    .font(.caption.weight(.semibold))
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .tint(.red)
        }
        .padding(.vertical, 2)
    }

    private func heading(now: Date) -> String {
        let label = view.activity?.label
            ?? (view.preparing ? "Preparing worktree" : nil)
            ?? (view.status == .waiting ? "Waiting for your answer" : "Working")
        if view.preparing { return "• \(label)" }
        guard let since = view.activity?.since ?? view.turnStartedAt else { return "• \(label)" }
        return "• \(label) (\(Formatting.elapsed(since: since, now: now)))"
    }
}
