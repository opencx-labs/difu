import DifuKit
import SwiftUI

extension PatchLineKind {
    var foreground: Color {
        switch self {
        case .addition: .addition
        case .deletion: .deletion
        case .hunk: .accentColor
        case .meta: .secondary
        case .context: .primary
        }
    }

    var background: Color {
        switch self {
        case .addition: Color.addition.opacity(0.13)
        case .deletion: Color.deletion.opacity(0.13)
        case .hunk: Color.accentColor.opacity(0.08)
        case .meta, .context: .clear
        }
    }
}

/// One diff line, optionally with old/new line numbers.
struct PatchLineRow: View {
    let line: PatchLine
    var numbers = false

    var body: some View {
        HStack(spacing: 0) {
            if numbers {
                Group {
                    Text(line.oldNumber.map(String.init) ?? "")
                    Text(line.newNumber.map(String.init) ?? "")
                }
                .frame(width: 34, alignment: .trailing)
                .foregroundStyle(.tertiary)
                .padding(.trailing, 4)
            }
            Text(line.text.isEmpty ? " " : line.text)
                .foregroundStyle(line.kind.foreground)
                .padding(.leading, 6)
                .padding(.trailing, 12)
        }
        .font(.monoCaption)
        .lineLimit(1)
        .fixedSize(horizontal: true, vertical: false)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(line.kind.background)
    }
}

/// A compact patch for transcript rows: scrolls horizontally, caps long diffs.
struct InlinePatchView: View {
    let patch: String
    var limit = 200
    @State private var showAll = false
    @State private var width: CGFloat = 0

    var body: some View {
        let lines = Patch.lines(patch)
        let shown = showAll ? lines : Array(lines.prefix(limit))
        VStack(alignment: .leading, spacing: 0) {
            ScrollView(.horizontal, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(shown) { PatchLineRow(line: $0) }
                }
                .padding(.vertical, 6)
                .frame(minWidth: width, alignment: .leading)
            }
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
            if lines.count > shown.count {
                Button("Show all \(lines.count) lines") { showAll = true }
                    .font(.caption)
                    .padding(8)
            }
        }
        .background(Color.codeBackground, in: RoundedRectangle(cornerRadius: 8))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }
}

/// A full-screen, lazily rendered patch with line numbers.
struct FilePatchView: View {
    let file: DiffFile

    var body: some View {
        let lines = Patch.lines(file.patch)
        Group {
            if lines.isEmpty {
                ContentUnavailableView(
                    "No textual changes", systemImage: "doc",
                    description: Text(file.status == .renamed ? "The file was renamed." : "Binary or empty diff."))
            } else {
                ScrollView([.horizontal, .vertical]) {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(lines) { PatchLineRow(line: $0, numbers: true) }
                        if file.truncated {
                            Text("Diff truncated — open it on your computer to see the rest.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .padding()
                        }
                    }
                    .padding(.vertical, 8)
                }
            }
        }
        .navigationTitle((file.path as NSString).lastPathComponent)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 0) {
                    Text((file.path as NSString).lastPathComponent).font(.headline)
                    Text(file.path).font(.caption2.monospaced()).foregroundStyle(.secondary)
                        .lineLimit(1).truncationMode(.head)
                }
            }
        }
    }
}

/// A changed-file row: status icon, path, +/- counts.
struct DiffFileRow: View {
    let file: DiffFile

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: icon)
                .foregroundStyle(color)
                .frame(width: 18)
            VStack(alignment: .leading, spacing: 2) {
                Text((file.path as NSString).lastPathComponent)
                    .font(.callout.weight(.medium))
                    .lineLimit(1)
                Text(subtitle)
                    .font(.caption2.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.head)
            }
            Spacer(minLength: 6)
            DiffStat(additions: file.additions, deletions: file.deletions)
        }
    }

    private var subtitle: String {
        if let oldPath = file.oldPath, oldPath != file.path { return "\(oldPath) → \(file.path)" }
        return file.path
    }

    private var icon: String {
        switch file.status {
        case .added: "plus.square"
        case .deleted: "minus.square"
        case .renamed: "arrow.right.square"
        case .modified: "square.and.pencil"
        }
    }

    private var color: Color {
        switch file.status {
        case .added: .green
        case .deleted: .red
        case .renamed: .blue
        case .modified: .orange
        }
    }
}

/// Files list with totals; each row opens the file's patch.
struct DiffFileList: View {
    let files: [DiffFile]

    var body: some View {
        if files.isEmpty {
            ContentUnavailableView("No changes", systemImage: "checkmark.circle", description: Text("Nothing has changed yet."))
        } else {
            List {
                Section {
                    ForEach(files) { file in
                        NavigationLink {
                            FilePatchView(file: file)
                        } label: {
                            DiffFileRow(file: file)
                        }
                    }
                } header: {
                    HStack {
                        Text("\(files.count) file\(files.count == 1 ? "" : "s") changed")
                        Spacer()
                        DiffStat(
                            additions: files.reduce(0) { $0 + $1.additions },
                            deletions: files.reduce(0) { $0 + $1.deletions })
                    }
                }
            }
            .listStyle(.insetGrouped)
        }
    }
}
