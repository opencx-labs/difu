import DifuKit
import SwiftUI

/// The generated review guide: chapters grouped by category with coloured hunks.
struct PrGuideTab: View {
    let model: PrModel

    var body: some View {
        Group {
            if let guide = model.guide {
                content(guide)
            } else if let error = model.guideError {
                ContentUnavailableView {
                    Label("Couldn't load the guide", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(error)
                } actions: {
                    Button("Try Again") { Task { await model.loadGuide() } }
                }
            } else {
                ProgressView("Loading guide…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .task {
            if model.guide == nil { await model.loadGuide() }
        }
        .task(id: model.guide?.status == .running) {
            await model.pollGuide()
        }
    }

    @ViewBuilder private func content(_ guide: Guide) -> some View {
        switch guide.status {
        case .ready where !guide.chapters.isEmpty:
            chapters(guide)
        case .running:
            VStack(spacing: 14) {
                ProgressView()
                    .controlSize(.large)
                Text("Generating guide…")
                    .font(.headline)
                if let message = guide.message {
                    Text(message)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
                Text("This takes a minute or two. You can leave this screen.")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
            .padding()
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .failed:
            ContentUnavailableView {
                Label("Guide generation failed", systemImage: "exclamationmark.triangle")
            } description: {
                Text(guide.message ?? "Something went wrong while writing the guide.")
            } actions: {
                generateButton("Try Again")
            }
        default:
            ContentUnavailableView {
                Label("No guide yet", systemImage: "book.pages")
            } description: {
                Text("A guide walks you through this pull request chapter by chapter, in review order.")
            } actions: {
                generateButton("Generate Guide")
            }
        }
    }

    private func generateButton(_ title: String) -> some View {
        Button {
            Task { await model.generateGuide() }
        } label: {
            if model.startingGuide {
                ProgressView()
            } else {
                Text(title)
            }
        }
        .buttonStyle(.borderedProminent)
        .foregroundStyle(Color.onAccent)
        .disabled(model.startingGuide)
    }

    private func chapters(_ guide: Guide) -> some View {
        let groups = Dictionary(grouping: guide.chapters, by: \.category)
            .sorted { $0.key.order < $1.key.order }
        return List {
            if guide.outdated {
                Section {
                    HStack(alignment: .top, spacing: 10) {
                        Image(systemName: "clock.arrow.circlepath").foregroundStyle(.orange)
                        VStack(alignment: .leading, spacing: 6) {
                            Text("This guide was written for an older commit.")
                                .font(.callout)
                            Button("Regenerate") { Task { await model.generateGuide() } }
                                .font(.callout.weight(.semibold))
                                .buttonStyle(.borderless)
                                .disabled(model.startingGuide)
                        }
                    }
                }
            }
            ForEach(groups, id: \.key) { category, chapters in
                Section(category.title) {
                    ForEach(Array(chapters.enumerated()), id: \.offset) { _, chapter in
                        ChapterView(chapter: chapter)
                            .listRowInsets(EdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 14))
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await model.loadGuide() }
    }
}

private struct ChapterView: View {
    let chapter: GuideChapter

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(chapter.title)
                .font(.headline)
            if !chapter.explanation.isEmpty {
                MarkdownText(source: chapter.explanation, font: .callout)
                    .textSelection(.enabled)
            }
            ForEach(Array(chapter.hunks.enumerated()), id: \.offset) { _, hunk in
                GuideHunkView(hunk: hunk)
            }
        }
    }
}

private struct GuideHunkView: View {
    let hunk: GuideHunk
    @State private var width: CGFloat = 0
    @ScaledMetric(relativeTo: .caption) private var digit: CGFloat = 7.5

    /// Wide enough for the hunk's largest line number, so columns never merge.
    private var numberWidth: CGFloat {
        let largest = hunk.lines.map { max($0.old ?? 0, $0.new ?? 0) }.max() ?? 0
        return CGFloat(max(2, String(largest).count)) * digit + 6
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: "doc.text").imageScale(.small)
                Text(hunk.file).lineLimit(1).truncationMode(.head)
            }
            .font(.monoCaption.weight(.semibold))
            .foregroundStyle(.secondary)
            .padding(.horizontal, 8)
            .padding(.vertical, 6)
            ScrollView(.horizontal, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 0) {
                    if !hunk.header.isEmpty {
                        line(text: hunk.header, kind: .meta, old: nil, new: nil, color: .accentColor)
                    }
                    ForEach(Array(hunk.lines.enumerated()), id: \.offset) { _, guideLine in
                        line(
                            text: prefix(guideLine.kind) + guideLine.text, kind: guideLine.kind,
                            old: guideLine.old, new: guideLine.new, color: nil)
                    }
                }
                .frame(minWidth: width, alignment: .leading)
            }
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
        }
        .background(Color.codeBackground, in: RoundedRectangle(cornerRadius: 8))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    private func line(text: String, kind: GuideLine.Kind, old: Int?, new: Int?, color: Color?) -> some View {
        HStack(spacing: 0) {
            Text(old.map(String.init) ?? "").frame(width: numberWidth, alignment: .trailing)
            Text(new.map(String.init) ?? "").frame(width: numberWidth, alignment: .trailing)
            Text(text.isEmpty ? " " : text)
                .foregroundStyle(color ?? foreground(kind))
                .padding(.leading, 8)
                .padding(.trailing, 12)
        }
        .foregroundStyle(.tertiary)
        .font(.monoCaption)
        .lineLimit(1)
        .fixedSize(horizontal: true, vertical: false)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(background(kind))
    }

    private func prefix(_ kind: GuideLine.Kind) -> String {
        switch kind {
        case .add: "+"
        case .remove: "-"
        case .context: " "
        case .meta: ""
        }
    }

    private func foreground(_ kind: GuideLine.Kind) -> Color {
        switch kind {
        case .add: .addition
        case .remove: .deletion
        case .meta: .secondary
        case .context: .primary
        }
    }

    private func background(_ kind: GuideLine.Kind) -> Color {
        switch kind {
        case .add: Color.addition.opacity(0.13)
        case .remove: Color.deletion.opacity(0.13)
        case .meta, .context: .clear
        }
    }
}
