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
    /// The session's server, shown as a badge when several are saved.
    var server: ServerConnection?
    /// The session runs on a cloud server (☁️).
    var onServer = false
    /// The folder holding it, shown in search results.
    var folder: String?

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
                FlowLayout {
                    StatusBadge(summary: summary)
                    ForEach(badges.filter { !$0.isPlaceMarker }, id: \.self) { badge in
                        SessionBadgePill(badge: badge)
                    }
                    if let pr = summary.prs.first {
                        let style = SessionPrStyle(pr)
                        let more = summary.prs.count > 1 ? " +\(summary.prs.count - 1)" : ""
                        Pill(text: "#\(pr.number)\(more)", systemImage: style.icon, color: style.color)
                            .lineLimit(1)
                            .fixedSize()
                            .accessibilityLabel("Pull request \(pr.number), \(pr.label)")
                    }
                    if let server {
                        ServerBadge(server: server)
                    }
                }
                SessionPlaceText(summary: summary, badges: badges)
                if let folder {
                    Label(folder, systemImage: "folder")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
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
                .foregroundStyle(Color.accent)
        } else if summary.pinned {
            Text("◆")
                .font(.callout)
                .foregroundStyle(Color.accent)
        } else if summary.kind == .conflicts {
            Image(systemName: "arrow.triangle.merge")
                .foregroundStyle(.warning)
        } else {
            Image(systemName: "chevron.left.forwardslash.chevron.right")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
        }
    }

    private var badges: [SessionBadge] {
        SessionBadge.row(summary, onServer: onServer)
    }
}

/// The server a session runs on (only shown when several are saved).
struct ServerBadge: View {
    let server: ServerConnection

    var body: some View {
        Pill(text: server.name, systemImage: server.isServer ? "server.rack" : "laptopcomputer", color: .violet)
            .lineLimit(1)
            .truncationMode(.tail)
            .layoutPriority(-1)
            .accessibilityLabel("On \(server.name)")
    }
}
