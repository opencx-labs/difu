import Foundation

/// How choosing a slash command behaves.
public enum CommandKind: String, LenientEnum {
    /// Runs on the computer with `session.command`.
    case action
    /// Sent to the agent as a message (provider skills and commands).
    case prompt
    /// Opens a phone screen.
    case screen

    public static let fallback = CommandKind.prompt
}

/// Whether a command takes text after its name.
public enum CommandArguments: String, LenientEnum {
    case none
    case optional
    case required

    public static let fallback = CommandArguments.optional
}

/// The phone screen a `screen` command opens.
public enum CommandScreen: String, LenientEnum {
    case model, rename, rules, changes, usage, status, permissions, new, skills
    case unknown

    public static let fallback = CommandScreen.unknown
}

/// One entry of the composer's `/` list (`session.commands`).
public struct SessionCommand: Identifiable, Hashable, Sendable, Decodable {
    /// Without the trigger: `compact`, `review`.
    public var name: String
    /// What the list shows and the composer inserts: `/compact`, `/review`, `$lint`.
    public var trigger: String
    public var description: String
    /// An argument hint such as `<path>` or `[model] [effort]`.
    public var hint: String?
    public var kind: CommandKind
    public var screen: CommandScreen?
    /// Destructive: ask before running it.
    public var confirm: Bool
    public var arguments: CommandArguments
    /// `difu`, or the provider (`codex`, `claude`) for its skills and commands.
    public var source: String

    public var id: String { trigger }
    public var isDifu: Bool { source == "difu" }
    /// A Codex skill: sent with the message as well as named in its text.
    public var isAttachedSkill: Bool { !isDifu && trigger.hasPrefix("$") }

    public init(
        name: String, trigger: String? = nil, description: String, hint: String? = nil,
        kind: CommandKind, screen: CommandScreen? = nil, confirm: Bool = false,
        arguments: CommandArguments = .none, source: String = "difu"
    ) {
        self.name = name
        self.trigger = trigger ?? "/\(name)"
        self.description = description
        self.hint = hint
        self.kind = kind
        self.screen = screen
        self.confirm = confirm
        self.arguments = arguments
        self.source = source
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let name = f.string("name"), !name.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("name"), .init(codingPath: decoder.codingPath, debugDescription: "command name"))
        }
        self.name = name
        trigger = f.string("trigger").flatMap { $0.isEmpty ? nil : $0 } ?? "/\(name)"
        description = f.string("description") ?? ""
        hint = f.string("hint").flatMap { $0.isEmpty ? nil : $0 }
        kind = CommandKind(lenient: f.string("kind"))
        screen = f.string("screen").map { CommandScreen(lenient: $0) }
        confirm = f.bool("confirm") ?? false
        arguments = CommandArguments(lenient: f.string("arguments"))
        source = f.string("source") ?? "difu"
    }
}

/// A session's commands and any skill discovery problems.
public struct SessionCommandList: Hashable, Sendable, Decodable {
    public var commands: [SessionCommand]
    public var errors: [String]
    /// Built on the phone because the computer predates `session.commands`:
    /// only commands existing methods can run.
    public var isFallback: Bool

    public init(commands: [SessionCommand], errors: [String] = [], isFallback: Bool = false) {
        self.commands = commands
        self.errors = errors
        self.isFallback = isFallback
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        commands = f.list("commands")
        errors = f.strings("errors")
        isFallback = false
    }

    /// What a computer without `session.commands` can still do from the phone.
    public static let fallback = SessionCommandList(
        commands: [
            SessionCommand(name: "compact", description: "Compact the current conversation", kind: .action),
            SessionCommand(
                name: "model", description: "Choose a model", kind: .screen, screen: .model),
            SessionCommand(
                name: "effort", description: "Choose the reasoning level", kind: .screen, screen: .model),
            SessionCommand(
                name: "diff", description: "Show current session changes", kind: .screen, screen: .changes),
            SessionCommand(name: "new", description: "Launch a new coding session", kind: .screen, screen: .new),
            SessionCommand(
                name: "rename", description: "Rename this session", hint: "[title]", kind: .screen,
                screen: .rename, arguments: .optional),
            SessionCommand(
                name: "clear",
                description: "Start over: clear context, PRs and artifacts; fresh worktree from main",
                kind: .action, confirm: true),
        ],
        isFallback: true)
}

/// What `session.command` did: a message, or a screen to open with its data.
public struct CommandOutcome: Sendable, Decodable {
    public var message: String?
    public var screen: CommandScreen?
    public var usage: UsageReport?
    public var status: SessionStatusInfo?
    public var permissions: PermissionsInfo?
    /// For `rules`: the session's repository.
    public var repository: String?

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        message = f.string("message")
        screen = f.string("screen").map { CommandScreen(lenient: $0) }
        usage = f.value("usage", as: UsageReport.self)
        status = f.value("status", as: SessionStatusInfo.self)
        permissions = f.value("permissions", as: PermissionsInfo.self)
        repository = f.string("repository")
    }
}

/// The terminal's `/status` as fields.
public struct SessionStatusInfo: Hashable, Sendable, Decodable {
    public var title: String
    public var provider: String?
    public var providerLabel: String
    public var model: String?
    public var effort: String?
    public var state: String
    public var repository: String?
    public var workspace: String?
    public var branch: String?
    public var providerSession: String?
    public var mode: String?
    public var preset: String?

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        title = f.string("title") ?? ""
        provider = f.string("provider")
        providerLabel = f.string("providerLabel") ?? ModelPicker.providerLabel(provider)
        model = f.string("model")
        effort = f.string("effort")
        state = f.string("state") ?? ""
        repository = f.string("repository")
        workspace = f.string("workspace")
        branch = f.string("branch")
        providerSession = f.string("providerSession")
        mode = f.string("mode")
        preset = f.string("preset")
    }
}

/// Provider usage laid out like the terminal's `/usage`.
public struct UsageReport: Hashable, Sendable, Decodable {
    public struct Limit: Identifiable, Hashable, Sendable, Decodable {
        public var section: String?
        public var title: String
        /// Percent left (`remaining`) or used.
        public var percent: Double
        public var remaining: Bool
        /// Unix milliseconds.
        public var resetsAt: Int?

        public var id: String { "\(section ?? "")|\(title)" }
        public var caption: String { "\(Int(percent.rounded()))% \(remaining ? "left" : "used")" }

        public init(from decoder: Decoder) throws {
            let f = try decoder.fields()
            section = f.string("section")
            title = f.string("title") ?? "Limit"
            percent = min(100, max(0, f.value("percent", as: Double.self) ?? Double(f.int("percent") ?? 0)))
            remaining = f.bool("remaining") ?? false
            resetsAt = f.timestamp("resetsAt")
        }
    }

    public struct Detail: Identifiable, Hashable, Sendable, Decodable {
        public var section: String?
        public var label: String
        public var value: String

        public var id: String { "\(section ?? "")|\(label)|\(value)" }

        public init(from decoder: Decoder) throws {
            let f = try decoder.fields()
            section = f.string("section")
            label = f.string("label") ?? ""
            value = f.string("value") ?? ""
        }
    }

    public var provider: String?
    public var title: String
    public var limits: [Limit]
    public var details: [Detail]

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        provider = f.string("provider")
        title = f.string("title") ?? "Usage"
        limits = f.list("limits")
        details = f.list("details")
    }
}

/// A provider mode or Codex approval preset.
public struct PermissionChoice: Identifiable, Hashable, Sendable, Decodable {
    public var id: String
    public var label: String
    public var description: String
    public var available: Bool
    public var reason: String?

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let id = f.string("id") else {
            throw DecodingError.keyNotFound(
                AnyKey("id"), .init(codingPath: decoder.codingPath, debugDescription: "choice id"))
        }
        self.id = id
        label = f.string("label") ?? id
        description = f.string("description") ?? ""
        available = f.bool("available") ?? true
        reason = f.string("reason")
    }
}

/// The session's provider modes and approval presets (`/permissions`).
public struct PermissionsInfo: Hashable, Sendable, Decodable {
    public var provider: String?
    public var activeMode: String?
    public var pendingMode: String?
    public var activePreset: String?
    public var pendingPreset: String?
    /// The provider restarts to apply a change.
    public var reloadPending: Bool
    public var modes: [PermissionChoice]
    public var presets: [PermissionChoice]
    public var message: String?

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        provider = f.string("provider")
        activeMode = f.string("activeMode")
        pendingMode = f.string("pendingMode")
        activePreset = f.string("activePreset")
        pendingPreset = f.string("pendingPreset")
        reloadPending = f.bool("reloadPending") ?? false
        modes = f.list("modes")
        presets = f.list("presets")
        message = f.string("message")
    }

    /// The mode the next turn uses.
    public var selectedMode: String? { pendingMode ?? activeMode }
    /// The preset the next turn uses.
    public var selectedPreset: String? { pendingPreset ?? activePreset }
}
