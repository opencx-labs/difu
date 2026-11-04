import Foundation

// A server repository's environment profile (`server.environment.get/set`,
// docs/cloud-agents.md, *Environments*): the tools its sandboxes install, the
// setup commands new worktrees run, non-secret variables and notes for agents.

/// One `name = value` line of a profile: a tool and its version, or a variable.
public struct EnvironmentEntry: Hashable, Sendable {
    public var name: String
    public var value: String

    public init(name: String, value: String) {
        self.name = name
        self.value = value
    }
}

/// One recorded change to a profile.
public struct EnvironmentChange: Hashable, Sendable, Decodable {
    /// Unix milliseconds.
    public var at: Int?
    /// A device, an agent in a session, or difu itself.
    public var by: String
    public var summary: String

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        at = f.timestamp("at")
        by = f.string("by") ?? ""
        summary = f.string("summary") ?? ""
    }
}

/// What a repository's sandboxes need, as the server remembers it.
public struct EnvironmentProfile: Hashable, Sendable, Decodable {
    /// mise tool names and versions (`node` = `24`), sorted by name.
    public var tools: [EnvironmentEntry]
    /// Commands run in each new worktree before the agent starts.
    public var setup: [String]
    /// Non-secret variables, sorted by name.
    public var env: [EnvironmentEntry]
    public var notes: [String]
    /// The latest changes, oldest first. Only the server writes it.
    public var history: [EnvironmentChange]
    /// Unix milliseconds of the last change.
    public var updatedAt: Int?

    public init(
        tools: [EnvironmentEntry] = [], setup: [String] = [], env: [EnvironmentEntry] = [],
        notes: [String] = []
    ) {
        self.tools = tools
        self.setup = setup
        self.env = env
        self.notes = notes
        history = []
        updatedAt = nil
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        tools = Self.entries(f.value("tools", as: [String: Lossy<LossyString>].self))
        setup = f.strings("setup")
        env = Self.entries(f.value("env", as: [String: Lossy<LossyString>].self))
        notes = f.strings("notes")
        history = f.list("history")
        updatedAt = f.timestamp("updatedAt")
    }

    private static func entries(_ map: [String: Lossy<LossyString>]?) -> [EnvironmentEntry] {
        (map ?? [:])
            .compactMap { name, value in value.value.map { EnvironmentEntry(name: name, value: $0.value) } }
            .sorted { $0.name < $1.name }
    }

    public var isEmpty: Bool {
        tools.isEmpty && setup.isEmpty && env.isEmpty && notes.isEmpty
    }

    /// The editable part as `server.environment.set` takes it: trimmed, blank
    /// lines dropped, later duplicates of a name win.
    public var json: JSONValue {
        func map(_ entries: [EnvironmentEntry]) -> JSONValue {
            var object: [String: JSONValue] = [:]
            for entry in entries {
                let name = entry.name.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !name.isEmpty else { continue }
                object[name] = .string(entry.value.trimmingCharacters(in: .whitespacesAndNewlines))
            }
            return .object(object)
        }
        func lines(_ items: [String]) -> JSONValue {
            .array(
                items.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                    .filter { !$0.isEmpty }
                    .map(JSONValue.string))
        }
        return .object(["tools": map(tools), "setup": lines(setup), "env": map(env), "notes": lines(notes)])
    }
}

/// A tool version the repository's files declare.
public struct DetectedTool: Hashable, Sendable, Decodable {
    public var tool: String
    public var version: String
    /// The file it comes from (`.nvmrc`, `package.json packageManager`).
    public var source: String
    /// Provided by corepack rather than mise.
    public var corepack: Bool

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let tool = f.string("tool"), !tool.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("tool"), .init(codingPath: decoder.codingPath, debugDescription: "tool name"))
        }
        self.tool = tool
        version = f.string("version") ?? ""
        source = f.string("source") ?? ""
        corepack = f.bool("corepack") ?? false
    }
}

/// `server.environment.get`: a repository's profile and what its base clone declares.
public struct RepositoryEnvironment: Hashable, Sendable, Decodable {
    /// `owner/name`.
    public var repository: String
    public var profile: EnvironmentProfile
    /// The version files on the base clone's default branch; they win over
    /// the profile's tools.
    public var detected: [DetectedTool]

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        repository = f.string("repository") ?? ""
        profile = f.value("profile", as: EnvironmentProfile.self) ?? EnvironmentProfile()
        detected = f.list("detected")
    }
}
