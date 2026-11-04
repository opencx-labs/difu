import Foundation

/// Keys a phone keyboard lacks, sent as a terminal sends them.
public enum ShellKey: String, CaseIterable, Identifiable, Sendable {
    case interrupt = "Ctrl-C"
    case endOfInput = "Ctrl-D"
    case tab = "Tab"
    case escape = "Esc"
    case up = "↑"
    case down = "↓"
    case left = "←"
    case right = "→"
    case clear = "Ctrl-L"

    public var id: String { rawValue }

    /// The bytes the key sends.
    public var sequence: String {
        switch self {
        case .interrupt: "\u{03}"
        case .endOfInput: "\u{04}"
        case .tab: "\t"
        case .escape: "\u{1B}"
        case .up: "\u{1B}[A"
        case .down: "\u{1B}[B"
        case .right: "\u{1B}[C"
        case .left: "\u{1B}[D"
        case .clear: "\u{0C}"
        }
    }
}

/// How often an open shell is read on a phone: quickly while output moves or
/// right after typing, then backing off while it's quiet, so an idle shell
/// costs little battery and data. A shell that ended isn't read again.
public enum ShellPolling {
    public static let fast: Duration = .milliseconds(700)
    public static let normal: Duration = .seconds(2)
    public static let idle: Duration = .seconds(5)

    /// The delay before the next read; nil stops reading.
    public static func next(running: Bool, sinceChange: TimeInterval, sinceInput: TimeInterval) -> Duration? {
        guard running else { return nil }
        if sinceInput < 10 || sinceChange < 5 { return fast }
        if sinceChange < 60 { return normal }
        return idle
    }
}

public enum TerminalText {
    /// Removes ANSI escape sequences (colours, cursor moves, titles) and
    /// carriage returns that overwrite a line, keeping the visible text.
    public static func plain(_ text: String) -> String {
        // Scalars: "\r\n" is one Character, so `contains("\r")` misses it.
        guard text.unicodeScalars.contains(where: { $0 == "\u{1B}" || $0 == "\r" }) else { return text }
        var result = ""
        result.reserveCapacity(text.count)
        var scalars = text.unicodeScalars.makeIterator()
        var line = ""
        while let scalar = scalars.next() {
            switch scalar {
            case "\u{1B}":
                guard let kind = scalars.next() else { break }
                if kind == "[" {
                    // CSI: parameters, then one final byte in @...~.
                    while let next = scalars.next(), !(0x40...0x7E).contains(next.value) {}
                } else if kind == "]" {
                    // OSC: until BEL or ESC \.
                    while let next = scalars.next() {
                        if next == "\u{07}" { break }
                        if next == "\u{1B}" {
                            _ = scalars.next()
                            break
                        }
                    }
                }
            case "\r":
                // "\r\n" ends a line; a lone "\r" starts it over.
                var lookahead = scalars
                if lookahead.next() == "\n" {
                    break
                }
                line = ""
            case "\n":
                result += line
                result += "\n"
                line = ""
            default:
                line.unicodeScalars.append(scalar)
            }
        }
        return result + line
    }
}
