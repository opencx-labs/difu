import DifuKit
import SwiftUI

/// Status label in the session's colour; working sessions pulse and show a
/// live timer from `turnStartedAt`.
struct StatusBadge: View {
    let summary: SessionSummary

    var body: some View {
        let tone = StatusTone(summary)
        HStack(spacing: 5) {
            if tone == .working {
                PulsingDot(color: tone.color, size: 6)
            } else {
                Circle().fill(tone.color).frame(width: 6, height: 6)
            }
            Text(summary.statusLabel)
            if summary.active, let started = summary.turnStartedAt {
                ElapsedText(since: started)
                    .foregroundStyle(tone.color.opacity(0.75))
            }
        }
        .font(.caption.weight(.medium))
        .foregroundStyle(tone.color)
        .opacity(tone == .working ? 0.9 : 1)
        .padding(.horizontal, 7)
        .padding(.vertical, 3)
        .background(tone.color.opacity(0.13), in: Capsule())
    }
}

struct SessionRow: View {
    let summary: SessionSummary

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            marker
                .frame(width: 22, height: 22)
                .padding(.top, 1)
            VStack(alignment: .leading, spacing: 5) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(summary.displayTitle)
                        .font(.body.weight(.semibold))
                        .lineLimit(1)
                    Spacer(minLength: 4)
                    if let updated = summary.updated, !summary.active {
                        Text(Formatting.relative(Date(milliseconds: updated)))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                HStack(spacing: 6) {
                    StatusBadge(summary: summary)
                    if summary.pending > 0 {
                        Pill(text: "\(summary.pending)", systemImage: "questionmark.bubble.fill", color: .orange)
                    }
                    if summary.queued > 0 {
                        Pill(text: "\(summary.queued) queued", systemImage: "tray.full")
                    }
                    if summary.archived {
                        Pill(text: "Archived", systemImage: "archivebox")
                    }
                }
                if let subtitle {
                    Text(subtitle)
                        .font(.monoCaption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder private var marker: some View {
        if summary.orchestrator {
            Image(systemName: "sparkles")
                .font(.callout.weight(.semibold))
                .foregroundStyle(Color.accentColor)
        } else if summary.pinned {
            Text("◆")
                .font(.callout)
                .foregroundStyle(Color.accentColor)
        } else if summary.kind == .conflicts {
            Image(systemName: "arrow.triangle.merge")
                .foregroundStyle(.orange)
        } else {
            Image(systemName: "chevron.left.forwardslash.chevron.right")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
        }
    }

    private var subtitle: String? {
        let parts = [summary.repository, summary.branch].compactMap { $0 }.filter { !$0.isEmpty }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}
