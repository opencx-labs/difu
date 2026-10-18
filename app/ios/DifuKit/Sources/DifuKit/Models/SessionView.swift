import Foundation

/// A watched session: the summary plus transcript and composer state.
@dynamicMemberLookup
public struct SessionView: Identifiable, Equatable, Sendable {
    public var summary: SessionSummary
    public var version: Int
    public var provider: String?
    public var model: String?
    public var effort: String?
    /// Session-level error, shown at the end of the transcript.
    public var error: String?
    /// The worktree or agent is still starting; messages are queued.
    public var preparing: Bool
    public var canSend: Bool
    /// Queued messages exist that "Send now" would flush.
    public var canInterruptAndSend: Bool
    public var activity: Activity?
    public var queue: [QueuedMessage]
    public var pending: [PendingView]
    public var entries: [EntryView]

    public var id: String { summary.id }

    public subscript<T>(dynamicMember keyPath: KeyPath<SessionSummary, T>) -> T {
        summary[keyPath: keyPath]
    }
}

extension SessionView: Decodable {
    public init(from decoder: Decoder) throws {
        summary = try SessionSummary(from: decoder)
        let f = try decoder.fields()
        version = f.int("version") ?? 0
        provider = f.string("provider")
        model = f.string("model")
        effort = f.string("effort")
        error = f.string("error").flatMap { $0.isEmpty ? nil : $0 }
        preparing = f.bool("preparing") ?? false
        canSend = f.bool("canSend") ?? true
        canInterruptAndSend = f.bool("canInterruptAndSend") ?? false
        activity = f.value("activity", as: Activity.self)
        queue = f.list("queue")
        pending = f.list("pending")
        // Entries stay index-aligned with the server, so bad rows become
        // placeholders instead of being dropped.
        entries = (f.value("entries", as: [EntryOrPlaceholder].self) ?? []).map(\.entry)
        summary.pending = pending.count
        summary.queued = queue.count
    }
}

public struct Activity: Hashable, Sendable, Decodable {
    public var label: String
    /// Unix milliseconds.
    public var since: Int?

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        label = f.string("label") ?? "Working"
        since = f.int("since")
    }
}

public struct QueuedMessage: Identifiable, Hashable, Sendable, Decodable {
    public var index: Int
    public var text: String

    public var id: Int { index }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let index = f.int("index") else {
            throw DecodingError.keyNotFound(
                AnyKey("index"), .init(codingPath: decoder.codingPath, debugDescription: "index"))
        }
        self.index = index
        text = f.string("text") ?? ""
    }
}
