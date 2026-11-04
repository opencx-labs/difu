import Foundation

/// One row of the agents list (`sessions` event, `sessions.list`).
public struct SessionSummary: Identifiable, Hashable, Sendable {
    public var id: String
    public var title: String
    public var kind: SessionKind
    public var orchestrator: Bool
    public var status: SessionStatus
    public var statusLabel: String
    public var active: Bool
    public var archived: Bool
    public var pinned: Bool
    /// Unix milliseconds.
    public var updated: Int?
    /// Unix milliseconds; nil when no turn is running.
    public var turnStartedAt: Int?
    public var repository: String?
    public var repositoryPath: String?
    public var branch: String?
    public var workspace: String?
    public var worktree: Bool
    /// Unanswered questions plus approvals.
    public var pending: Int
    public var queued: Int
    public var canReadChanges: Bool
    /// Attached and discovered pull requests, current open ones first.
    public var prs: [SessionPr]
    /// Presented HTML artifacts (`session.artifact`); 0 from older computers.
    public var artifacts: Int
    /// Open shells: shared shells, running `!` commands and Codex's
    /// background shells (`session.shells`).
    public var shells: Int
    /// Sub-agents, and how many still work (`session.subagent`).
    public var subagents: Int
    public var runningSubagents: Int
    /// The id of the sidebar folder holding the session (`folders.list`).
    public var folder: String?
    /// The `<server>:<session>` this session moved to; it takes no messages.
    public var movedTo: String?

    public var displayTitle: String {
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty { return trimmed }
        return orchestrator ? "Difu orchestrator" : "New session"
    }
}

extension SessionSummary: Decodable {
    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let id = f.string("id"), !id.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("id"), .init(codingPath: decoder.codingPath, debugDescription: "Session id"))
        }
        self.id = id
        title = f.string("title") ?? ""
        kind = SessionKind(lenient: f.string("kind"))
        orchestrator = f.bool("orchestrator") ?? (kind == .orchestrator)
        status = SessionStatus(lenient: f.string("status"))
        statusLabel = f.string("statusLabel") ?? status.defaultLabel
        active = f.bool("active") ?? status.isActive
        archived = f.bool("archived") ?? false
        pinned = f.bool("pinned") ?? false
        updated = f.int("updated")
        turnStartedAt = f.int("turnStartedAt")
        repository = f.string("repository")
        repositoryPath = f.string("repositoryPath")
        branch = f.string("branch")
        workspace = f.string("workspace")
        worktree = f.bool("worktree") ?? false
        // In a SessionView `pending` is an array of requests rather than a count.
        pending = f.int("pending") ?? f.value("pending", as: [JSONValue].self)?.count ?? 0
        queued = f.int("queued") ?? f.value("queue", as: [JSONValue].self)?.count ?? 0
        canReadChanges = f.bool("canReadChanges") ?? false
        prs = f.list("prs")
        // In a SessionView these are lists rather than counts.
        artifacts = f.int("artifacts") ?? f.value("artifacts", as: [JSONValue].self)?.count ?? 0
        shells = f.int("shells") ?? 0
        let agents = f.value("subagents", as: [JSONValue].self)
        subagents = f.int("subagents") ?? agents?.count ?? 0
        runningSubagents = f.int("runningSubagents")
            ?? agents?.filter { $0["active"] == .bool(true) }.count ?? 0
        folder = f.string("folder").flatMap { $0.isEmpty ? nil : $0 }
        movedTo = f.string("movedTo").flatMap { $0.isEmpty ? nil : $0 }
    }
}
