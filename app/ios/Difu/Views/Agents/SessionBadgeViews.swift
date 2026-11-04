import DifuKit
import SwiftUI

/// How a `SessionBadge` is drawn: a symbol and a semantic colour.
extension SessionBadge {
    var symbol: String {
        switch self {
        case .pending: "questionmark.bubble.fill"
        case .queued: "tray.full"
        case .artifacts: "doc.richtext"
        case .shells: "terminal"
        case .subagents: "person.2"
        case .worktree: "tree"
        case .cloud: "cloud"
        case .moved: "arrow.up.forward.app"
        case .archived: "archivebox"
        }
    }

    var tint: Color {
        switch self {
        case .pending: .warning
        case .subagents(let running, _) where running > 0: .accent
        case .moved: .violet
        default: .secondary
        }
    }

    /// The pill's text: counts get a word where a bare number would be unclear.
    var pillText: String {
        switch self {
        case .queued(let count): "\(count) queued"
        default: text
        }
    }

    /// Drawn as a marker after the repository, like the terminal's 🌳 and ☁️.
    var isPlaceMarker: Bool {
        switch self {
        case .worktree, .cloud: true
        default: false
        }
    }

    var marker: String {
        switch self {
        case .worktree: "🌳"
        case .cloud: "☁️"
        default: ""
        }
    }
}

struct SessionBadgePill: View {
    let badge: SessionBadge

    var body: some View {
        Pill(text: badge.pillText, systemImage: badge.symbol, color: badge.tint)
            .lineLimit(1)
            .fixedSize()
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(badge.accessibilityLabel)
    }
}

/// "difu 🌳 ☁️ · branch": where the session works.
struct SessionPlaceText: View {
    let summary: SessionSummary
    let badges: [SessionBadge]

    var body: some View {
        if let text {
            Text(text)
                .font(.monoCaption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }

    private var text: String? {
        let markers = badges.filter(\.isPlaceMarker).map(\.marker).joined(separator: " ")
        var place = summary.repository ?? ""
        if !markers.isEmpty { place = place.isEmpty ? markers : "\(place) \(markers)" }
        let parts = [place, summary.branch ?? ""].filter { !$0.isEmpty }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

/// Lays chips out in rows, wrapping to the next line when one doesn't fit,
/// so a session with many badges never overflows a narrow phone.
struct FlowLayout: Layout {
    var spacing: CGFloat = 6
    var lineSpacing: CGFloat = 4

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let rows = rows(width: proposal.width ?? .infinity, subviews: subviews)
        let height = rows.reduce(0) { $0 + $1.height } + lineSpacing * CGFloat(max(0, rows.count - 1))
        let width = rows.map(\.width).max() ?? 0
        return CGSize(width: proposal.width ?? width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for row in rows(width: bounds.width, subviews: subviews) {
            var x = bounds.minX
            for index in row.indices {
                var size = subviews[index].sizeThatFits(.unspecified)
                // A chip wider than the row (a long server name) truncates.
                size.width = min(size.width, bounds.width)
                subviews[index].place(
                    at: CGPoint(x: x, y: y + (row.height - size.height) / 2), anchor: .topLeading,
                    proposal: ProposedViewSize(size))
                x += size.width + spacing
            }
            y += row.height + lineSpacing
        }
    }

    private struct Row {
        var indices: [Int] = []
        var width: CGFloat = 0
        var height: CGFloat = 0
    }

    private func rows(width: CGFloat, subviews: Subviews) -> [Row] {
        var rows: [Row] = []
        var row = Row()
        for index in subviews.indices {
            var size = subviews[index].sizeThatFits(.unspecified)
            size.width = min(size.width, width)
            let needed = row.indices.isEmpty ? size.width : row.width + spacing + size.width
            if needed > width, !row.indices.isEmpty {
                rows.append(row)
                row = Row()
            }
            row.width = row.indices.isEmpty ? size.width : row.width + spacing + size.width
            row.height = max(row.height, size.height)
            row.indices.append(index)
        }
        if !row.indices.isEmpty { rows.append(row) }
        return rows
    }
}
