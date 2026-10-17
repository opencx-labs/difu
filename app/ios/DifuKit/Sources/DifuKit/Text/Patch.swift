import Foundation

/// How a line of a unified diff should be drawn.
public enum PatchLineKind: Sendable, Hashable {
    /// `@@ -a,b +c,d @@`
    case hunk
    case addition
    case deletion
    case context
    /// File headers before the first hunk, `\ No newline at end of file`, ...
    case meta
}

public struct PatchLine: Identifiable, Hashable, Sendable {
    public var id: Int
    public var kind: PatchLineKind
    /// The raw line, including its `+`/`-`/space prefix.
    public var text: String
    public var oldNumber: Int?
    public var newNumber: Int?

    /// The line without its diff prefix.
    public var content: Substring {
        switch kind {
        case .addition, .deletion, .context:
            text.isEmpty ? Substring(text) : text.dropFirst()
        case .hunk, .meta:
            Substring(text)
        }
    }
}

public enum Patch {
    /// Classifies one line. `inHunk` is false before the first `@@`, where
    /// `---`/`+++`, `diff --git` and friends are file headers, not changes.
    public static func classify(_ line: Substring, inHunk: Bool) -> PatchLineKind {
        if line.hasPrefix("@@") { return .hunk }
        if !inHunk, isFileHeader(line) { return .meta }
        switch line.first {
        case "+": return .addition
        case "-": return .deletion
        case " ", nil: return .context
        case "\\": return .meta
        default: return .context
        }
    }

    /// Splits a patch into classified, numbered lines.
    public static func lines(_ patch: String) -> [PatchLine] {
        var result: [PatchLine] = []
        var raw = patch.split(omittingEmptySubsequences: false, whereSeparator: { $0 == "\n" || $0 == "\r\n" })
        if raw.last?.isEmpty == true { raw.removeLast() }
        result.reserveCapacity(raw.count)
        var inHunk = false
        var old = 1
        var new = 1
        for (index, var line) in raw.enumerated() {
            if line.hasSuffix("\r") { line = line.dropLast() }
            let kind = classify(line, inHunk: inHunk)
            var entry = PatchLine(id: index, kind: kind, text: String(line))
            switch kind {
            case .hunk:
                inHunk = true
                if let start = hunkStart(line) {
                    old = start.old
                    new = start.new
                }
            case .addition:
                entry.newNumber = new
                new += 1
            case .deletion:
                entry.oldNumber = old
                old += 1
            case .context:
                entry.oldNumber = old
                entry.newNumber = new
                old += 1
                new += 1
            case .meta:
                break
            }
            result.append(entry)
        }
        return result
    }

    private static let fileHeaders = [
        "diff ", "index ", "--- ", "+++ ", "new file", "deleted file", "similarity ",
        "dissimilarity ", "rename ", "copy ", "old mode", "new mode", "Binary files",
    ]

    static func isFileHeader(_ line: Substring) -> Bool {
        line == "---" || line == "+++" || fileHeaders.contains { line.hasPrefix($0) }
    }

    /// The starting old/new line numbers of a hunk header.
    public static func hunkStart(_ header: Substring) -> (old: Int, new: Int)? {
        // @@ -12,7 +12,9 @@ optional section
        let parts = header.split(separator: " ")
        guard parts.count >= 3, parts[0] == "@@" else { return nil }
        func start(_ range: Substring, _ sign: Character) -> Int? {
            guard range.first == sign else { return nil }
            return Int(range.dropFirst().split(separator: ",").first ?? "")
        }
        guard let old = start(parts[1], "-"), let new = start(parts[2], "+") else { return nil }
        return (old, new)
    }

    /// Counts added and removed lines.
    public static func stats(_ patch: String) -> (additions: Int, deletions: Int) {
        var additions = 0
        var deletions = 0
        var inHunk = false
        for line in patch.split(omittingEmptySubsequences: false, whereSeparator: { $0 == "\n" || $0 == "\r\n" }) {
            switch classify(line, inHunk: inHunk) {
            case .hunk: inHunk = true
            case .addition: additions += 1
            case .deletion: deletions += 1
            default: break
            }
        }
        return (additions, deletions)
    }
}
