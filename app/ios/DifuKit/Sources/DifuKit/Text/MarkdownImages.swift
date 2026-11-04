import Foundation

/// An image in Markdown (`![alt](src)`, `[![alt](src)](link)`) or HTML
/// (`<img src alt>`).
public struct MarkdownImage: Hashable, Sendable {
    /// As written: an absolute URL, a GitHub root path, or a repository path.
    public var source: String
    public var alt: String
    /// Where a linked image (a badge) points.
    public var link: String?

    public init(source: String, alt: String = "", link: String? = nil) {
        self.source = source
        self.alt = alt
        self.link = link
    }

    /// The alt text, or "Image".
    public var label: String {
        let trimmed = alt.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "Image" : trimmed
    }
}

/// Finds images in paragraph text the way the terminal's PR previews do:
/// Markdown image syntax and HTML `<img>` tags, never inside code spans.
public enum MarkdownImages {
    public enum Segment: Hashable, Sendable {
        case text(String)
        case image(MarkdownImage)
    }

    /// `text` split into prose and images, in order. Around images, leftover
    /// pieces that are only HTML tags (`<p align="center">`, `</a>`) are dropped.
    public static func split(_ text: String) -> [Segment] {
        let chars = Array(text)
        var segments: [Segment] = []
        var prose: [Character] = []
        var index = 0
        var found = false

        func flush() {
            let piece = String(prose)
            prose.removeAll()
            if !piece.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                segments.append(.text(piece))
            }
        }

        while index < chars.count {
            let char = chars[index]
            if char == "`" {
                let end = codeSpanEnd(chars, from: index)
                prose.append(contentsOf: chars[index..<end])
                index = end
                continue
            }
            if char == "[", let (image, end) = linkedImage(chars, at: index) {
                flush()
                segments.append(.image(image))
                index = end
                found = true
                continue
            }
            if char == "!", let (image, end) = markdownImage(chars, at: index) {
                flush()
                segments.append(.image(image))
                index = end
                found = true
                continue
            }
            if char == "<", let (image, end) = htmlImage(chars, at: index) {
                flush()
                if let image { segments.append(.image(image)) }
                index = end
                found = true
                continue
            }
            prose.append(char)
            index += 1
        }
        flush()
        guard found else { return segments }
        return segments.compactMap { segment in
            if case .text(let piece) = segment {
                let cleaned = piece.trimmingCharacters(in: .whitespacesAndNewlines)
                return isOnlyTags(cleaned) ? nil : .text(cleaned)
            }
            return segment
        }
    }

    /// Where the image opens in a browser: absolute http(s) URLs as they are,
    /// GitHub root paths on github.com, repository paths at the PR's head.
    public static func browserURL(_ source: String, repo: String?, head: String?) -> URL? {
        let source = source.trimmingCharacters(in: .whitespaces)
        if let url = URL(string: source), let scheme = url.scheme?.lowercased() {
            return scheme == "http" || scheme == "https" ? url : nil
        }
        if source.hasPrefix("/") { return URL(string: "https://github.com\(source)") }
        guard let repo, let head, !repo.isEmpty, !head.isEmpty else { return nil }
        var path = source
        while path.hasPrefix("./") { path.removeFirst(2) }
        return URL(string: "https://github.com/\(repo)/blob/\(head)/\(path)")
    }

    /// SVG images aren't drawn (as in the terminal); they open in a browser.
    public static func isVector(_ source: String) -> Bool {
        let path = source.split(whereSeparator: { $0 == "?" || $0 == "#" }).first.map(String.init) ?? source
        let lower = path.lowercased()
        return lower.hasSuffix(".svg") || lower.hasSuffix(".svgz")
    }

    // MARK: Scanning

    /// The index after a code span starting at `start` (a run of backticks and
    /// its matching closing run), or after the opening run when unclosed.
    private static func codeSpanEnd(_ chars: [Character], from start: Int) -> Int {
        var run = start
        while run < chars.count, chars[run] == "`" { run += 1 }
        let width = run - start
        var index = run
        while index < chars.count {
            if chars[index] == "`" {
                var close = index
                while close < chars.count, chars[close] == "`" { close += 1 }
                if close - index == width { return close }
                index = close
            } else {
                index += 1
            }
        }
        return run
    }

    /// `![alt](destination "title")` at `start`.
    private static func markdownImage(_ chars: [Character], at start: Int) -> (MarkdownImage, Int)? {
        guard start + 1 < chars.count, chars[start + 1] == "[" else { return nil }
        guard let close = closingBracket(chars, from: start + 1) else { return nil }
        let alt = String(chars[(start + 2)..<close])
        guard let (destination, end) = destination(chars, from: close + 1) else { return nil }
        return (MarkdownImage(source: destination, alt: alt), end)
    }

    /// `[![alt](src)](link)` at `start`.
    private static func linkedImage(_ chars: [Character], at start: Int) -> (MarkdownImage, Int)? {
        guard start + 1 < chars.count, chars[start + 1] == "!",
            var (image, end) = markdownImage(chars, at: start + 1),
            end < chars.count, chars[end] == "]",
            let (link, after) = destination(chars, from: end + 1)
        else { return nil }
        image.link = link
        end = after
        return (image, end)
    }

    /// The `]` matching the `[` at `open`, allowing nested brackets.
    private static func closingBracket(_ chars: [Character], from open: Int) -> Int? {
        var depth = 0
        var index = open
        while index < chars.count {
            switch chars[index] {
            case "\\": index += 1
            case "[": depth += 1
            case "]":
                depth -= 1
                if depth == 0 { return index }
            case "\n" where index + 1 < chars.count && chars[index + 1] == "\n": return nil
            default: break
            }
            index += 1
        }
        return nil
    }

    /// `(destination "optional title")` starting at `open`.
    private static func destination(_ chars: [Character], from open: Int) -> (String, Int)? {
        guard open < chars.count, chars[open] == "(" else { return nil }
        var index = open + 1
        while index < chars.count, chars[index] == " " { index += 1 }
        var url: [Character] = []
        if index < chars.count, chars[index] == "<" {
            index += 1
            while index < chars.count, chars[index] != ">", chars[index] != "\n" {
                url.append(chars[index])
                index += 1
            }
            guard index < chars.count, chars[index] == ">" else { return nil }
            index += 1
        } else {
            var depth = 0
            while index < chars.count, !chars[index].isWhitespace {
                if chars[index] == "(" { depth += 1 }
                if chars[index] == ")" {
                    if depth == 0 { break }
                    depth -= 1
                }
                url.append(chars[index])
                index += 1
            }
        }
        // An optional title, then the closing parenthesis.
        while index < chars.count, chars[index] != ")" {
            if chars[index] == "\n" && index + 1 < chars.count && chars[index + 1] == "\n" { return nil }
            index += 1
        }
        guard index < chars.count, !url.isEmpty else { return nil }
        return (String(url), index + 1)
    }

    /// An `<img …>` tag at `start`: the image (nil without a `src`) and the
    /// index after the tag. Other tags aren't images.
    private static func htmlImage(_ chars: [Character], at start: Int) -> (MarkdownImage?, Int)? {
        let name = String(chars[start..<min(start + 4, chars.count)]).lowercased()
        guard name == "<img", start + 4 < chars.count,
            chars[start + 4].isWhitespace || chars[start + 4] == "/" || chars[start + 4] == ">"
        else { return nil }
        var quote: Character?
        var index = start + 4
        while index < chars.count {
            let char = chars[index]
            if let open = quote {
                if char == open { quote = nil }
            } else if char == "\"" || char == "'" {
                quote = char
            } else if char == ">" {
                break
            }
            index += 1
        }
        guard index < chars.count else { return nil }
        let attributes = Self.attributes(String(chars[(start + 4)..<index]))
        let image = attributes["src"].flatMap { $0.isEmpty ? nil : $0 }.map {
            MarkdownImage(source: $0, alt: attributes["alt"] ?? "")
        }
        return (image, index + 1)
    }

    /// `name="value"`, `name='value'` and `name=value` pairs, entities decoded.
    static func attributes(_ text: String) -> [String: String] {
        let chars = Array(text)
        var result: [String: String] = [:]
        var index = 0
        while index < chars.count {
            while index < chars.count, chars[index].isWhitespace || chars[index] == "/" { index += 1 }
            var name: [Character] = []
            while index < chars.count, !chars[index].isWhitespace, chars[index] != "=", chars[index] != "/" {
                name.append(chars[index])
                index += 1
            }
            while index < chars.count, chars[index].isWhitespace { index += 1 }
            var value: [Character] = []
            if index < chars.count, chars[index] == "=" {
                index += 1
                while index < chars.count, chars[index].isWhitespace { index += 1 }
                if index < chars.count, chars[index] == "\"" || chars[index] == "'" {
                    let quote = chars[index]
                    index += 1
                    while index < chars.count, chars[index] != quote {
                        value.append(chars[index])
                        index += 1
                    }
                    index += 1
                } else {
                    while index < chars.count, !chars[index].isWhitespace {
                        value.append(chars[index])
                        index += 1
                    }
                }
            }
            if !name.isEmpty {
                let key = String(name).lowercased()
                if result[key] == nil { result[key] = decodeEntities(String(value)) }
            } else {
                index += 1
            }
        }
        return result
    }

    static func decodeEntities(_ text: String) -> String {
        guard text.contains("&") else { return text }
        return text
            .replacingOccurrences(of: "&quot;", with: "\"")
            .replacingOccurrences(of: "&#39;", with: "'")
            .replacingOccurrences(of: "&apos;", with: "'")
            .replacingOccurrences(of: "&lt;", with: "<")
            .replacingOccurrences(of: "&gt;", with: ">")
            .replacingOccurrences(of: "&amp;", with: "&")
    }

    /// Only HTML tags (and whitespace), such as `<p align="center">` or `</a>`.
    private static func isOnlyTags(_ text: String) -> Bool {
        var rest = Substring(text)
        guard !rest.isEmpty else { return true }
        while let first = rest.first {
            if first.isWhitespace {
                rest = rest.dropFirst()
                continue
            }
            guard first == "<", let close = rest.firstIndex(of: ">") else { return false }
            let tag = rest[rest.index(after: rest.startIndex)..<close]
            guard let letter = tag.first, letter.isLetter || letter == "/" || letter == "!" else { return false }
            rest = rest[rest.index(after: close)...]
        }
        return true
    }
}
