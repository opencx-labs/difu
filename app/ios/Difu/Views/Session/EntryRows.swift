import DifuKit
import SwiftUI

/// One transcript row, drawn according to its role. Rows compare by content
/// (not their callbacks), so a streaming update redraws only changed rows.
struct EntryRow: View, Equatable {
    let entry: EntryView
    /// The session is running; in-progress plan steps animate only then.
    let active: Bool
    let expanded: Bool
    let loadingFull: Bool
    let toggle: () -> Void
    let loadFull: () -> Void

    static func == (a: EntryRow, b: EntryRow) -> Bool {
        a.entry == b.entry && a.active == b.active && a.expanded == b.expanded
            && a.loadingFull == b.loadingFull
    }

    var body: some View {
        switch entry.role {
        case .user:
            UserBubble(entry: entry)
        case .assistant:
            VStack(alignment: .leading, spacing: 4) {
                MarkdownText(source: entry.text)
                    .textSelection(.enabled)
                TruncationNote(entry: entry, loading: loadingFull, loadFull: loadFull)
            }
        case .reasoning:
            ReasoningRow(entry: entry, expanded: expanded, toggle: toggle)
        case .tool:
            ToolRow(entry: entry, expanded: expanded, loadingFull: loadingFull, toggle: toggle, loadFull: loadFull)
        case .plan:
            PlanRow(entry: entry, active: active)
        case .error:
            ErrorRow(text: entry.text.isEmpty ? (entry.title ?? "Error") : entry.text)
        case .system, .unknown:
            SystemRow(entry: entry)
        }
    }
}

struct UserBubble: View {
    let entry: EntryView

    var body: some View {
        VStack(alignment: .trailing, spacing: 4) {
            Text(entry.text.isEmpty ? " " : entry.text)
                .textSelection(.enabled)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(Color.accentColor.opacity(0.18), in: RoundedRectangle(cornerRadius: 16))
                .overlay {
                    if entry.delivery == .unsent {
                        RoundedRectangle(cornerRadius: 16).strokeBorder(Color.red.opacity(0.6))
                    }
                }
            if let delivery = entry.delivery {
                deliveryLabel(delivery)
                    .font(.caption2)
            }
        }
        .frame(maxWidth: .infinity, alignment: .trailing)
        .padding(.leading, 40)
    }

    @ViewBuilder private func deliveryLabel(_ delivery: Delivery) -> some View {
        switch delivery {
        case .sending:
            HStack(spacing: 4) {
                ProgressView().controlSize(.mini)
                Text("Sending…")
            }
            .foregroundStyle(.secondary)
        case .unsent:
            Label("Not sent", systemImage: "exclamationmark.circle.fill")
                .foregroundStyle(.red)
        case .awaiting:
            Label("Waiting to send", systemImage: "clock")
                .foregroundStyle(.secondary)
        case .unknown:
            EmptyView()
        }
    }
}

/// A dim one-line heading that expands to the reasoning summary.
struct ReasoningRow: View {
    let entry: EntryView
    let expanded: Bool
    let toggle: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button(action: toggle) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Image(systemName: "brain")
                        .imageScale(.small)
                    Text(heading)
                        .italic()
                        .lineLimit(expanded ? nil : 1)
                        .multilineTextAlignment(.leading)
                    Spacer(minLength: 0)
                }
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if expanded, !entry.text.isEmpty, entry.text != heading {
                MarkdownText(source: entry.text, font: .subheadline)
                    .foregroundStyle(.secondary)
                    .padding(.leading, 20)
            }
        }
    }

    private var heading: String {
        if let title = entry.title, !title.isEmpty { return title }
        let first = entry.text.split(separator: "\n").first.map(String.init) ?? ""
        return first.isEmpty ? "Thinking" : first.replacingOccurrences(of: "**", with: "")
    }
}

/// A plan checklist.
struct PlanRow: View {
    let entry: EntryView
    let active: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(entry.title ?? "Plan", systemImage: "list.bullet.clipboard")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.secondary)
            ForEach(Array(entry.plan.enumerated()), id: \.offset) { _, step in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    icon(step.status)
                    Text(step.step)
                        .strikethrough(step.status == .completed, color: .secondary)
                        .foregroundStyle(step.status == .completed ? .secondary : .primary)
                }
                .font(.callout)
            }
            if entry.plan.isEmpty, !entry.text.isEmpty {
                MarkdownText(source: entry.text, font: .callout)
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.codeBackground, in: RoundedRectangle(cornerRadius: 10))
    }

    @ViewBuilder private func icon(_ status: PlanStatus) -> some View {
        switch status {
        case .completed:
            Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .inProgress:
            Image(systemName: "circle.dotted.circle").foregroundStyle(Color.accentColor)
                .symbolEffect(.pulse, isActive: active)
        case .pending:
            Image(systemName: "circle").foregroundStyle(.secondary)
        }
    }
}

struct ErrorRow: View {
    let text: String

    var body: some View {
        Label {
            Text(text).textSelection(.enabled)
        } icon: {
            Image(systemName: "exclamationmark.triangle.fill")
        }
        .font(.callout)
        .foregroundStyle(.red)
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.red.opacity(0.1), in: RoundedRectangle(cornerRadius: 10))
    }
}

struct SystemRow: View {
    let entry: EntryView

    var body: some View {
        let text = [entry.title, entry.text.isEmpty ? nil : entry.text]
            .compactMap { $0 }
            .joined(separator: "\n")
        Text(text.isEmpty ? entry.kind : text)
            .font(.caption)
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// "Show full message" for truncated rows.
struct TruncationNote: View {
    let entry: EntryView
    let loading: Bool
    let loadFull: () -> Void

    var body: some View {
        if entry.truncated {
            Button(action: loadFull) {
                HStack(spacing: 6) {
                    if loading { ProgressView().controlSize(.mini) }
                    Text(entry.role == .tool ? "Show full output" : "Show full message")
                }
                .font(.caption.weight(.medium))
            }
            .buttonStyle(.borderless)
            .disabled(loading)
        }
    }
}

struct CopyButton: View {
    let text: String

    var body: some View {
        Button("Copy", systemImage: "doc.on.doc") {
            UIPasteboard.general.string = text
        }
    }
}
