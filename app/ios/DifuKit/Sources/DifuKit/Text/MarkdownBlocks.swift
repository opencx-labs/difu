import Foundation

/// A block of Markdown. Inline formatting inside `text` is left for the UI
/// (`AttributedString(markdown:)` with inline-only syntax).
public enum MarkdownBlock: Hashable, Sendable {
    case heading(level: Int, text: String)
    case paragraph(String)
    case code(language: String?, code: String)
    case list([MarkdownListItem])
    case quote(String)
    /// Pipe tables, kept verbatim so columns stay aligned in monospace.
    case table(String)
    case rule
}

public struct MarkdownListItem: Hashable, Sendable {
    /// `•` for bullets, `3.` for ordered items.
    public var marker: String
    public var text: String
    /// Nesting depth (0 = top level).
    public var depth: Int
    /// Task-list state, when the item is `- [ ]` / `- [x]`.
    public var checked: Bool?
}

public enum Markdown {
    /// Splits Markdown into blocks.
    public static func blocks(_ source: String) -> [MarkdownBlock] {
        let lines = source.replacingOccurrences(of: "\r\n", with: "\n")
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map(String.init)
        var blocks: [MarkdownBlock] = []
        var paragraph: [String] = []
        var index = 0

        func flushParagraph() {
            let text = paragraph.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
            if !text.isEmpty { blocks.append(.paragraph(text)) }
            paragraph.removeAll()
        }

        while index < lines.count {
            let line = lines[index]
            let trimmed = line.trimmingCharacters(in: .whitespaces)

            if let fence = fenceMarker(trimmed) {
                flushParagraph()
                let language = trimmed.dropFirst(fence.count).trimmingCharacters(in: .whitespaces)
                var code: [String] = []
                index += 1
                while index < lines.count,
                    !lines[index].trimmingCharacters(in: .whitespaces).hasPrefix(fence)
                {
                    code.append(lines[index])
                    index += 1
                }
                index += 1  // closing fence
                blocks.append(.code(language: language.isEmpty ? nil : language, code: code.joined(separator: "\n")))
                continue
            }
            if trimmed.isEmpty {
                flushParagraph()
                index += 1
                continue
            }
            if let heading = heading(trimmed) {
                flushParagraph()
                blocks.append(heading)
                index += 1
                continue
            }
            if isRule(trimmed) {
                flushParagraph()
                blocks.append(.rule)
                index += 1
                continue
            }
            if trimmed.hasPrefix("|"), index + 1 < lines.count,
                isTableSeparator(lines[index + 1].trimmingCharacters(in: .whitespaces))
            {
                flushParagraph()
                var rows: [String] = []
                while index < lines.count,
                    lines[index].trimmingCharacters(in: .whitespaces).hasPrefix("|")
                {
                    rows.append(lines[index].trimmingCharacters(in: .whitespaces))
                    index += 1
                }
                blocks.append(.table(rows.joined(separator: "\n")))
                continue
            }
            if trimmed.hasPrefix(">") {
                flushParagraph()
                var quoted: [String] = []
                while index < lines.count {
                    let current = lines[index].trimmingCharacters(in: .whitespaces)
                    guard current.hasPrefix(">") else { break }
                    quoted.append(String(current.dropFirst()).trimmingCharacters(in: .whitespaces))
                    index += 1
                }
                blocks.append(.quote(quoted.joined(separator: "\n")))
                continue
            }
            if listItem(line) != nil {
                flushParagraph()
                var items: [MarkdownListItem] = []
                while index < lines.count {
                    let current = lines[index]
                    if let item = listItem(current) {
                        items.append(item)
                    } else if !current.trimmingCharacters(in: .whitespaces).isEmpty,
                        current.first == " " || current.first == "\t", !items.isEmpty
                    {
                        // Continuation of the previous item.
                        items[items.count - 1].text += "\n"
                            + current.trimmingCharacters(in: .whitespaces)
                    } else {
                        break
                    }
                    index += 1
                }
                blocks.append(.list(items))
                continue
            }
            paragraph.append(line)
            index += 1
        }
        flushParagraph()
        return blocks
    }

    private static func fenceMarker(_ line: String) -> String? {
        for fence in ["```", "~~~"] where line.hasPrefix(fence) {
            return fence
        }
        return nil
    }

    private static func heading(_ line: String) -> MarkdownBlock? {
        let hashes = line.prefix { $0 == "#" }.count
        guard (1...6).contains(hashes) else { return nil }
        let rest = line.dropFirst(hashes)
        guard rest.isEmpty || rest.first == " " else { return nil }
        var text = rest.trimmingCharacters(in: .whitespaces)
        while text.hasSuffix("#") { text.removeLast() }
        return .heading(level: hashes, text: text.trimmingCharacters(in: .whitespaces))
    }

    private static func isRule(_ line: String) -> Bool {
        let compact = line.replacingOccurrences(of: " ", with: "")
        guard compact.count >= 3, let first = compact.first, "-*_".contains(first) else {
            return false
        }
        return compact.allSatisfy { $0 == first }
    }

    private static func isTableSeparator(_ line: String) -> Bool {
        line.hasPrefix("|") && line.contains("-")
            && line.allSatisfy { "|-: ".contains($0) }
    }

    static func listItem(_ line: String) -> MarkdownListItem? {
        let indent = line.prefix { $0 == " " || $0 == "\t" }
        let width = indent.reduce(0) { $0 + ($1 == "\t" ? 4 : 1) }
        let body = line.dropFirst(indent.count)
        var marker: String
        var rest: Substring
        if let first = body.first, "-*+".contains(first), body.dropFirst().first == " " {
            marker = "•"
            rest = body.dropFirst(2)
        } else {
            let digits = body.prefix { $0.isNumber }
            guard !digits.isEmpty, digits.count <= 9 else { return nil }
            let after = body.dropFirst(digits.count)
            guard let punctuation = after.first, punctuation == "." || punctuation == ")",
                after.dropFirst().first == " "
            else { return nil }
            marker = "\(digits)."
            rest = after.dropFirst(2)
        }
        var checked: Bool?
        if rest.hasPrefix("[ ] ") {
            checked = false
            rest = rest.dropFirst(4)
        } else if rest.hasPrefix("[x] ") || rest.hasPrefix("[X] ") {
            checked = true
            rest = rest.dropFirst(4)
        }
        if checked != nil, marker == "•" { marker = "" }
        return MarkdownListItem(
            marker: marker, text: rest.trimmingCharacters(in: .whitespaces),
            depth: min(width / 2, 6), checked: checked)
    }
}
