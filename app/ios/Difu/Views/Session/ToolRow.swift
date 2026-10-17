import DifuKit
import SwiftUI

/// A collapsed tool call (icon, title, state, duration) that expands to its
/// command/arguments, output and file diffs.
struct ToolRow: View {
    let entry: EntryView
    let expanded: Bool
    let loadingFull: Bool
    let toggle: () -> Void
    let loadFull: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button(action: toggle) {
                header.contentShape(Rectangle())
            }
            .buttonStyle(.plain)

            if expanded {
                if let detail = entry.detail, !detail.isEmpty, detail != entry.title {
                    CodeBlock(text: detail, color: .secondary)
                }
                if !entry.text.isEmpty {
                    OutputBlock(text: entry.text)
                }
                ForEach(Array(entry.files.enumerated()), id: \.offset) { _, file in
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: 6) {
                            Text(file.path)
                                .font(.monoCaption)
                                .lineLimit(1)
                                .truncationMode(.head)
                            Spacer(minLength: 4)
                            DiffStat(additions: file.additions, deletions: file.deletions)
                        }
                        if !file.diff.isEmpty {
                            InlinePatchView(patch: file.diff)
                        }
                    }
                }
                TruncationNote(entry: entry, loading: loadingFull, loadFull: loadFull)
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.codeBackground.opacity(0.6), in: RoundedRectangle(cornerRadius: 10))
        .overlay {
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(entry.state == .failed ? Color.red.opacity(0.45) : Color.primary.opacity(0.06))
        }
        .contextMenu {
            if let detail = entry.detail { Button("Copy Command", systemImage: "terminal") { UIPasteboard.general.string = detail } }
            if !entry.text.isEmpty { CopyButton(text: entry.text) }
        }
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: icon)
                .font(.footnote.weight(.semibold))
                .foregroundStyle(tint)
                .frame(width: 18)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(entry.state == .failed ? Color.red : .primary)
                    .lineLimit(expanded ? 4 : 1)
                    .multilineTextAlignment(.leading)
                if !expanded, let summary = fileSummary {
                    Text(summary)
                        .font(.monoCaption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 4)
            stateLabel
            Image(systemName: "chevron.right")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(.tertiary)
                .rotationEffect(.degrees(expanded ? 90 : 0))
        }
    }

    @ViewBuilder private var stateLabel: some View {
        HStack(spacing: 4) {
            switch entry.state {
            case .running:
                ProgressView().controlSize(.mini)
                if let started = entry.startedAt {
                    TimelineView(.periodic(from: .now, by: 1)) { context in
                        Text(Formatting.duration(milliseconds: context.date.milliseconds - started))
                    }
                }
            case .failed:
                Image(systemName: "xmark.circle.fill").foregroundStyle(.red)
                durationText
            case .interrupted:
                Image(systemName: "stop.circle").foregroundStyle(.secondary)
                durationText
            case .done:
                durationText
            case .unknown, nil:
                EmptyView()
            }
        }
        .font(.caption.monospacedDigit())
        .foregroundStyle(.secondary)
    }

    @ViewBuilder private var durationText: some View {
        if let duration = entry.duration(now: Date().milliseconds) {
            Text(Formatting.duration(milliseconds: duration))
        }
    }

    private var title: String {
        if let title = entry.title, !title.isEmpty { return title }
        if let detail = entry.detail, !detail.isEmpty { return detail }
        return entry.kind.isEmpty ? "Tool" : entry.kind
    }

    private var fileSummary: String? {
        guard !entry.files.isEmpty else { return nil }
        if entry.files.count == 1, let file = entry.files.first {
            return "\((file.path as NSString).lastPathComponent)  +\(file.additions) −\(file.deletions)"
        }
        let additions = entry.files.reduce(0) { $0 + $1.additions }
        let deletions = entry.files.reduce(0) { $0 + $1.deletions }
        return "\(entry.files.count) files  +\(additions) −\(deletions)"
    }

    private var tint: Color {
        switch entry.state {
        case .running: .accentColor
        case .failed: .red
        default: .secondary
        }
    }

    private var icon: String {
        switch entry.kind {
        case "commandExecution": "terminal"
        case "fileChange": "pencil.line"
        case "webSearch": "globe"
        case "mcpToolCall", "dynamicToolCall": "puzzlepiece.extension"
        case "imageView", "imageGeneration": "photo"
        case "contextCompaction": "arrow.down.right.and.arrow.up.left"
        case "collabAgentToolCall": "person.2"
        case "autoApprovalReview": "checkmark.shield"
        case "todoList", "plan": "checklist"
        default:
            entry.files.isEmpty ? "wrench.and.screwdriver" : "pencil.line"
        }
    }
}
