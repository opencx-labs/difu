import DifuKit
import SwiftUI

/// Renders Markdown: blocks from `Markdown.blocks`, inline formatting via
/// `AttributedString(markdown:)`.
struct MarkdownText: View {
    let source: String
    var font: Font = .body

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(Markdown.blocks(source).enumerated()), id: \.offset) { _, block in
                MarkdownBlockView(block: block, font: font)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// Inline Markdown with code spans drawn in monospace and tinted.
    static func inline(_ text: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(
            interpretedSyntax: .inlineOnlyPreservingWhitespace, failurePolicy: .returnPartiallyParsedIfPossible)
        guard var attributed = try? AttributedString(markdown: text, options: options) else {
            return AttributedString(text)
        }
        for run in attributed.runs where run.inlinePresentationIntent?.contains(.code) == true {
            attributed[run.range].font = .system(.callout, design: .monospaced)
            attributed[run.range].foregroundColor = .accentColor
        }
        return attributed
    }
}

private struct MarkdownBlockView: View {
    let block: MarkdownBlock
    let font: Font

    var body: some View {
        switch block {
        case .heading(let level, let text):
            Text(MarkdownText.inline(text))
                .font(headingFont(level))
                .padding(.top, level <= 2 ? 4 : 0)
        case .paragraph(let text):
            Text(MarkdownText.inline(text)).font(font)
        case .code(let language, let code):
            VStack(alignment: .leading, spacing: 0) {
                if let language {
                    Text(language)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                        .padding([.top, .horizontal], 10)
                }
                CodeBlock(text: code)
            }
            .background(Color.codeBackground, in: RoundedRectangle(cornerRadius: 8))
        case .table(let text):
            CodeBlock(text: text)
        case .list(let items):
            VStack(alignment: .leading, spacing: 5) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        if let checked = item.checked {
                            Image(systemName: checked ? "checkmark.square.fill" : "square")
                                .foregroundStyle(checked ? Color.green : .secondary)
                        }
                        if !item.marker.isEmpty {
                            Text(item.marker)
                                .foregroundStyle(.secondary)
                                .monospacedDigit()
                        }
                        Text(MarkdownText.inline(item.text))
                    }
                    .font(font)
                    .padding(.leading, CGFloat(item.depth) * 16)
                }
            }
        case .quote(let text):
            HStack(alignment: .top, spacing: 8) {
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(.secondary.opacity(0.5))
                    .frame(width: 3)
                Text(MarkdownText.inline(text))
                    .font(font)
                    .foregroundStyle(.secondary)
            }
            .fixedSize(horizontal: false, vertical: true)
        case .rule:
            Divider()
        }
    }

    private func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: .title3.weight(.bold)
        case 2: .headline
        default: .subheadline.weight(.semibold)
        }
    }
}
