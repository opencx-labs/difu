import Foundation

// What the terminal shows around a session's chat: presented HTML artifacts,
// shells and sub-agents, plus the sidebar folders. See `docs/remote-protocol.md`
// › Session resources.

/// A presented HTML artifact (`SessionView.artifacts`); open it with
/// `session.artifact` (`DifuAPI.artifact`).
public struct SessionArtifact: Identifiable, Hashable, Sendable, Decodable {
    public var title: String
    /// Relative to `workspace` (or to the session's workspace when nil).
    public var path: String
    public var workspace: String?
    /// The file name, e.g. `report.html`.
    public var name: String

    public var id: String { "\(workspace ?? "")\u{0}\(path)" }

    public init(title: String, path: String, workspace: String? = nil, name: String? = nil) {
        self.title = title
        self.path = path
        self.workspace = workspace
        self.name = name ?? (path as NSString).lastPathComponent
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let path = f.string("path"), !path.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("path"), .init(codingPath: decoder.codingPath, debugDescription: "Artifact path"))
        }
        let name = f.string("name").flatMap { $0.isEmpty ? nil : $0 }
        let title = f.string("title").flatMap { $0.isEmpty ? nil : $0 }
        self.init(
            title: title ?? name ?? (path as NSString).lastPathComponent, path: path,
            workspace: f.string("workspace").flatMap { $0.isEmpty ? nil : $0 }, name: name)
    }
}

/// An artifact's file (`session.artifact`): HTML up to 32 MB.
public struct ArtifactFile: Sendable, Equatable, Decodable {
    public var name: String
    public var mime: String
    public var data: Data

    public var html: String {
        String(decoding: data, as: UTF8.self)
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let encoded = f.string("data"), let data = Data(base64Encoded: encoded) else {
            throw RemoteError.protocolError("the artifact wasn't valid base64")
        }
        self.data = data
        name = f.string("name") ?? "artifact.html"
        mime = f.string("mime") ?? "text/html"
    }
}

/// A shell in the session's Shells list (`session.shells`).
public struct ShellInfo: Identifiable, Hashable, Sendable, Decodable {
    public enum Kind: String, LenientEnum {
        /// A shared interactive shell in the session's worktree.
        case shell
        /// A `!` command.
        case command
        /// One of Codex's background shells: read-only.
        case background

        public static let fallback = Kind.background
    }

    public var id: String
    public var title: String
    public var kind: Kind
    public var running: Bool
    public var exitCode: Int?
    public var cwd: String?
    /// Changes with every output; `session.shell` skips unchanged output.
    public var revision: Int?
    /// Someone types in it at the terminal (or on a phone) right now.
    public var humanFocused: Bool
    public var canWrite: Bool
    public var canClose: Bool

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let id = f.string("id"), !id.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("id"), .init(codingPath: decoder.codingPath, debugDescription: "Shell id"))
        }
        self.id = id
        kind = Kind(lenient: f.string("kind"))
        title = f.string("title").flatMap { $0.isEmpty ? nil : $0 }
            ?? (kind == .shell ? "Interactive shell" : "Shell")
        running = f.bool("running") ?? true
        exitCode = f.int("exitCode")
        cwd = f.string("cwd").flatMap { $0.isEmpty ? nil : $0 }
        revision = f.int("revision")
        humanFocused = f.bool("humanFocused") ?? false
        canWrite = f.bool("canWrite") ?? false
        canClose = f.bool("canClose") ?? false
    }

    /// "Running", "Exited 0", "Finished".
    public var stateLabel: String {
        if running { return "Running" }
        if let exitCode { return "Exited \(exitCode)" }
        return "Finished"
    }
}

/// One read of a shell (`session.shell`). `output` is nil when it didn't
/// change since the revision the phone had.
public struct ShellOutput: Sendable, Decodable {
    public var shell: ShellInfo
    public var output: String?
    /// Earlier output was left out.
    public var truncated: Bool
    public var error: String?

    public var unchanged: Bool { output == nil }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let shell = f.value("shell", as: ShellInfo.self) else {
            throw RemoteError.protocolError("the shell read named no shell")
        }
        self.shell = shell
        output = (f.bool("unchanged") ?? false) ? nil : (f.string("output") ?? "")
        truncated = f.bool("truncated") ?? false
        error = f.string("error").flatMap { $0.isEmpty ? nil : $0 }
    }
}

/// A provider sub-agent (`SessionView.subagents`).
public struct SubagentSummary: Identifiable, Hashable, Sendable, Decodable {
    public var id: String
    public var name: String
    public var task: String
    /// The terminal's row title: name and the task's first line.
    public var title: String
    /// Starting | Running | Completed | Interrupted | Failed | Closed | Unknown
    public var state: String
    public var stateLabel: String
    public var active: Bool
    /// `codex` or `claude`.
    public var provider: String
    public var providerLabel: String
    public var background: Bool
    /// Messages recorded so far (Codex loads them when opened).
    public var entries: Int

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let id = f.string("id"), !id.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("id"), .init(codingPath: decoder.codingPath, debugDescription: "Sub-agent id"))
        }
        self.id = id
        name = f.string("name") ?? "Sub-agent"
        task = f.string("task") ?? ""
        title = f.string("title").flatMap { $0.isEmpty ? nil : $0 } ?? name
        state = f.string("state") ?? "Unknown"
        stateLabel = f.string("stateLabel") ?? state
        active = f.bool("active") ?? (state == "Running" || state == "Starting")
        provider = f.string("provider") ?? ""
        providerLabel = f.string("providerLabel")
            ?? (provider == "claude" ? "Claude Code" : provider == "codex" ? "Codex" : provider)
        background = f.bool("background") ?? false
        entries = f.int("entries") ?? 0
    }
}

/// A sub-agent's transcript (`session.subagent`), read-only.
public struct SubagentTranscript: Sendable, Decodable {
    public var subagent: SubagentSummary
    public var entries: [EntryView]
    /// Older messages left out.
    public var earlier: Int
    /// Why Codex's history couldn't be read again; the last copy is shown.
    public var error: String?

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let subagent = f.value("subagent", as: SubagentSummary.self) else {
            throw RemoteError.protocolError("the transcript named no sub-agent")
        }
        self.subagent = subagent
        entries = (f.value("entries", as: [EntryOrPlaceholder].self) ?? []).map(\.entry)
        earlier = f.int("earlier") ?? 0
        error = f.string("error").flatMap { $0.isEmpty ? nil : $0 }
    }
}

/// A sidebar folder on one computer or server (`folders.list`, the `folders`
/// event): a named group of session ids, in the terminal's order.
public struct SessionFolder: Identifiable, Hashable, Sendable, Decodable {
    public var id: String
    public var name: String
    public var sessions: [String]

    public init(id: String, name: String, sessions: [String] = []) {
        self.id = id
        self.name = name
        self.sessions = sessions
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let id = f.string("id"), !id.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("id"), .init(codingPath: decoder.codingPath, debugDescription: "Folder id"))
        }
        self.init(id: id, name: f.string("name") ?? id, sessions: f.strings("sessions"))
    }
}

/// A saved cloud server a computer can move sessions to (`agents.servers`).
public struct MoveTarget: Identifiable, Hashable, Sendable, Decodable {
    public var name: String
    public var connected: Bool
    public var version: String?
    /// Why sessions can't move there now.
    public var problem: String?

    public var id: String { name }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let name = f.string("name"), !name.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("name"), .init(codingPath: decoder.codingPath, debugDescription: "Server name"))
        }
        self.name = name
        connected = f.bool("connected") ?? false
        version = f.string("version")
        problem = f.string("problem").flatMap { $0.isEmpty ? nil : $0 }
    }
}

/// The agents list with its folders (`sessions.subscribe`, `sessions.list`).
public struct SessionList: Sendable {
    public var sessions: [SessionSummary]
    /// Nil from computers without folders.
    public var folders: [SessionFolder]?
}

extension SessionList: Decodable {
    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        sessions = f.list("sessions")
        folders = f.has("folders") ? f.list("folders") : nil
    }
}
