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
    }
}
