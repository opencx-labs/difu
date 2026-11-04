import Foundation

/// A small marker on a session row or the session screen, as the terminal's
/// sidebar and resource controls show them.
public enum SessionBadge: Hashable, Sendable {
    /// Unanswered questions and approvals.
    case pending(Int)
    case queued(Int)
    case artifacts(Int)
    case shells(Int)
    case subagents(running: Int, total: Int)
    /// The session works in its own worktree (🌳).
    case worktree
    /// The session runs on a cloud server (☁️).
    case cloud
    /// Moved to `<server>:<session>`; it takes no more messages.
    case moved(String)
    case archived

    /// A short label: "2", "1/3", "Moved".
    public var text: String {
        switch self {
        case .pending(let count), .queued(let count), .artifacts(let count), .shells(let count):
            "\(count)"
        case .subagents(let running, let total):
            running > 0 ? "\(running)/\(total)" : "\(total)"
        case .worktree: "worktree"
        case .cloud: "cloud"
        case .moved: "Moved"
        case .archived: "Archived"
        }
    }

    /// What VoiceOver reads.
    public var accessibilityLabel: String {
        switch self {
        case .pending(let count): count == 1 ? "1 request waiting" : "\(count) requests waiting"
        case .queued(let count): "\(count) queued"
        case .artifacts(let count): count == 1 ? "1 artifact" : "\(count) artifacts"
        case .shells(let count): count == 1 ? "1 shell" : "\(count) shells"
        case .subagents(let running, let total):
            running > 0
                ? "\(running) of \(total) sub-agents working"
                : (total == 1 ? "1 sub-agent" : "\(total) sub-agents")
        case .worktree: "In its own worktree"
        case .cloud: "On a cloud server"
        case .moved(let target): "Moved to \(target)"
        case .archived: "Archived"
        }
    }

    /// The badges a session row shows, in the terminal's order: requests (or
    /// queued messages), resources, then where the session runs.
    public static func row(_ summary: SessionSummary, onServer: Bool) -> [SessionBadge] {
        var badges: [SessionBadge] = []
        if summary.pending > 0 {
            badges.append(.pending(summary.pending))
        } else if summary.queued > 0 {
            badges.append(.queued(summary.queued))
        }
        badges += resources(summary)
        if summary.worktree { badges.append(.worktree) }
        if onServer { badges.append(.cloud) }
        if let target = summary.movedTo {
            badges.append(.moved(target))
        } else if summary.archived {
            badges.append(.archived)
        }
        return badges
    }

    /// Artifacts, shells and sub-agents, when the session has any.
    public static func resources(_ summary: SessionSummary) -> [SessionBadge] {
        var badges: [SessionBadge] = []
        if summary.artifacts > 0 { badges.append(.artifacts(summary.artifacts)) }
        if summary.shells > 0 { badges.append(.shells(summary.shells)) }
        if summary.subagents > 0 {
            badges.append(.subagents(running: summary.runningSubagents, total: summary.subagents))
        }
        return badges
    }
}
