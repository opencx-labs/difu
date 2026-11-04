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
    /// No turn, pending request or queued message: provider switches (and
    /// Claude Code model changes) are accepted now. Nil from older computers.
    public var betweenTurns: Bool?
    public var activity: Activity?
    public var queue: [QueuedMessage]
    public var pending: [PendingView]
    /// The provider mode (and Codex approval preset) the session runs in;
    /// nil for other sessions and from older computers.
    public var permissionMode: PermissionMode?
    public var entries: [EntryView]
    /// Presented HTML artifacts; empty from older computers.
    public var artifacts: [SessionArtifact]
    /// Provider sub-agents, without their transcripts.
    public var subagents: [SubagentSummary]

    public var id: String { summary.id }

    public subscript<T>(dynamicMember keyPath: KeyPath<SessionSummary, T>) -> T {
        summary[keyPath: keyPath]
    }

    /// Whether the session is between turns, estimated from its state when
    /// the computer doesn't say.
    public var isBetweenTurns: Bool {
        betweenTurns ?? (!summary.active && !preparing && pending.isEmpty && queue.isEmpty)
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
        betweenTurns = f.bool("betweenTurns")
        activity = f.value("activity", as: Activity.self)
        queue = f.list("queue")
        pending = f.list("pending")
        permissionMode = f.value("permissionMode", as: PermissionMode.self)
        artifacts = f.list("artifacts")
        subagents = f.list("subagents")
        // Entries stay index-aligned with the server, so bad rows become
        // placeholders instead of being dropped.
        entries = (f.value("entries", as: [EntryOrPlaceholder].self) ?? []).map(\.entry)
        summary.pending = pending.count
        summary.queued = queue.count
        if f.has("artifacts") { summary.artifacts = artifacts.count }
        if f.has("subagents") {
            summary.subagents = subagents.count
            summary.runningSubagents = subagents.filter(\.active).count
        }
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
    /// `text` was shortened, so it can't be edited on the phone.
    public var truncated: Bool

    public var id: Int { index }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let index = f.int("index") else {
            throw DecodingError.keyNotFound(
                AnyKey("index"), .init(codingPath: decoder.codingPath, debugDescription: "index"))
        }
        self.index = index
        text = f.string("text") ?? ""
        truncated = f.bool("truncated") ?? false
    }
}

/// A session's provider mode and, for Codex, approval preset, as the mode
/// chip shows them (`SessionView.permissionMode`).
public struct PermissionMode: Hashable, Sendable, Decodable {
    /// The active mode, nil until the provider reports one.
    public var id: String?
    public var label: String?
    /// A mode waiting for the next turn (or for the agent to restart).
    public var pending: String?
    public var pendingLabel: String?
    /// Codex: the detected approval preset (nil: custom or inherited).
    public var preset: String?
    public var presetLabel: String?
    public var pendingPreset: String?
    public var pendingPresetLabel: String?

    public init(
        id: String? = nil, label: String? = nil, pending: String? = nil, pendingLabel: String? = nil,
        preset: String? = nil, presetLabel: String? = nil, pendingPreset: String? = nil,
        pendingPresetLabel: String? = nil
    ) {
        self.id = id
        self.label = label
        self.pending = pending
        self.pendingLabel = pendingLabel
        self.preset = preset
        self.presetLabel = presetLabel
        self.pendingPreset = pendingPreset
        self.pendingPresetLabel = pendingPresetLabel
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        func text(_ key: String) -> String? { f.string(key).flatMap { $0.isEmpty ? nil : $0 } }
        id = text("id")
        label = text("label")
        pending = text("pending")
        pendingLabel = text("pendingLabel")
        preset = text("preset")
        presetLabel = text("presetLabel")
        pendingPreset = text("pendingPreset")
        pendingPresetLabel = text("pendingPresetLabel")
    }

    /// A change waits for the next turn.
    public var isPending: Bool {
        (pending != nil && pending != id) || (pendingPreset != nil && pendingPreset != preset)
    }

    /// The mode the next turn uses.
    public var nextMode: String? { pending ?? id }
    /// The preset the next turn uses.
    public var nextPreset: String? { pendingPreset ?? preset }

    /// Skips permission checks: Claude's bypass mode or Codex's Full Access.
    public var skipsChecks: Bool {
        nextMode == "bypassPermissions" || nextPreset == "Full"
    }

    /// "Auto", "Regular · Approve for me", "Plan · next turn".
    public var chipText: String {
        let modeText = (pending != nil && pending != id ? pendingLabel ?? pending : nil) ?? label ?? id
        let presetText =
            (pendingPreset != nil && pendingPreset != preset ? pendingPresetLabel ?? pendingPreset : nil)
            ?? presetLabel ?? preset
        var parts = [modeText ?? (presetText == nil ? "Default mode" : nil), presetText].compactMap { $0 }
        if isPending { parts.append("next turn") }
        return parts.joined(separator: " · ")
    }
}
