import Foundation

/// The new-session defaults saved on a computer or server (`agents.defaults`):
/// the same ones its terminal edits, used for sessions started without a choice.
public struct AgentDefaults: Hashable, Sendable, Decodable {
    /// `codex` or `claude`: the saved provider, or the saved model's.
    public var provider: String
    /// The explicitly saved provider; nil when automatic (or an older computer).
    public var savedProvider: String?
    /// nil: the provider's configured model.
    public var model: String?
    /// nil: the model's default reasoning.
    public var effort: String?
    /// nil: no default repository.
    public var repository: String?
    /// A `providers[].startingModes[].id`; nil: the provider's own default.
    public var startingMode: String?
    /// The saved Codex approval preset (`Ask`, `Auto`, `Full`, `ReadOnly`); nil: automatic.
    public var codexPreset: String?
    /// Sessions get their own worktree.
    public var isolated: Bool
    /// Whether the location is a difu server (servers start sessions in Auto);
    /// nil from older computers.
    public var server: Bool?
    /// The providers with the starting modes each offers.
    public var providers: [DefaultsProvider]
    /// What a new session gets when the client chooses nothing; nil from
    /// older computers, which leave the phone to preselect.
    public var effective: EffectiveDefaults?

    public init(
        provider: String = "codex", savedProvider: String? = nil, model: String? = nil,
        effort: String? = nil, repository: String? = nil, startingMode: String? = nil,
        codexPreset: String? = nil, isolated: Bool = true, server: Bool? = nil,
        providers: [DefaultsProvider] = [], effective: EffectiveDefaults? = nil
    ) {
        self.provider = provider
        self.savedProvider = savedProvider
        self.model = model
        self.effort = effort
        self.repository = repository
        self.startingMode = startingMode
        self.codexPreset = codexPreset
        self.isolated = isolated
        self.server = server
        self.providers = providers
        self.effective = effective
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        model = f.string("model").flatMap(Self.nonEmpty)
        provider = f.string("provider").flatMap(Self.nonEmpty)
            ?? model.map { ModelPicker.provider(of: $0, in: []) } ?? "codex"
        savedProvider = f.string("savedProvider").flatMap(Self.nonEmpty)
        effort = f.string("effort").flatMap(Self.nonEmpty)
        repository = f.string("repository").flatMap(Self.nonEmpty)
        startingMode = f.string("startingMode").flatMap(Self.nonEmpty)
        codexPreset = f.string("codexPreset").flatMap(Self.nonEmpty)
        isolated = f.bool("isolated") ?? true
        server = f.bool("server")
        providers = f.list("providers")
        effective = f.value("effective", as: EffectiveDefaults.self)
    }

    /// The starting modes `provider` offers.
    public func startingModes(for provider: String) -> [StartingMode] {
        providers.first { $0.id == provider }?.startingModes ?? []
    }

    /// Codex's approval presets; empty from older computers.
    public var codexPresets: [StartingMode] {
        providers.first { $0.id == "codex" }?.presets ?? []
    }

    /// The provider entry for `provider`, when listed.
    public func entry(for provider: String) -> DefaultsProvider? {
        providers.first { $0.id == provider }
    }

    /// Whether the location reports effective defaults and starting modes
    /// (newer computers and servers).
    public var reportsEffective: Bool { effective != nil }

    /// The model is left to the location (newer computers): nothing saved,
    /// so new sessions get the newest Claude Opus where Claude Code has one.
    public var automaticModel: Bool { reportsEffective && savedProvider == nil && model == nil }

    /// The provider a saved starting mode is checked against: the saved
    /// provider, the saved model's, or Codex.
    public var modeProvider: String {
        savedProvider ?? model.map { ModelPicker.provider(of: $0, in: []) } ?? "codex"
    }

    /// What the location's automatic rule starts `provider`'s sessions in,
    /// e.g. "Auto" for Claude Code on servers, "Manual" on computers.
    public func automaticModeLabel(for provider: String) -> String {
        guard let entry = entry(for: provider) else { return "Default" }
        if entry.startingModeAutomatic { return entry.modeLabel(entry.startingMode) }
        if server == true, provider == "claude", entry.startingModes.contains(where: { $0.id == "auto" }) {
            return entry.modeLabel("auto")
        }
        return entry.modeLabel(nil)
    }

    /// What the automatic rule gives Codex sessions: "Approve for me" on
    /// servers, Codex's configured approvals on computers.
    public var automaticPresetLabel: String {
        guard let entry = entry(for: "codex") else { return "Codex's configured approvals" }
        if entry.codexPresetAutomatic { return entry.presetLabel(entry.codexPreset) }
        return entry.presetLabel(server == true ? "Auto" : nil)
    }

    /// "Claude Code · claude-opus-4-7 · xhigh", for a settings row: what new
    /// sessions get, marked when picked automatically.
    public var summary: String {
        if let effective {
            let text = Self.describe(provider: effective.provider, model: effective.model, effort: effective.effort)
            return effective.modelAutomatic ? "\(text) · automatic" : text
        }
        return Self.describe(provider: provider, model: model, effort: effort)
    }

    static func describe(provider: String, model: String?, effort: String?) -> String {
        let model = model.map { $0.hasPrefix("claude/") ? String($0.dropFirst("claude/".count)) : $0 }
        return [ModelPicker.providerLabel(provider), model ?? "Default model", effort]
            .compactMap { $0 }
            .joined(separator: " · ")
    }

    private static func nonEmpty(_ text: String) -> String? {
        text.isEmpty ? nil : text
    }
}

/// What a location gives a new session the client chose nothing for
/// (`agents.defaults` › `effective`).
public struct EffectiveDefaults: Hashable, Sendable, Decodable {
    public var provider: String
    /// nil: the provider's configured model.
    public var model: String?
    /// nil: the model's default reasoning.
    public var effort: String?
    /// Picked automatically (the newest Claude Opus there), not saved.
    public var modelAutomatic: Bool
    public var effortAutomatic: Bool

    public init(
        provider: String, model: String? = nil, effort: String? = nil,
        modelAutomatic: Bool = false, effortAutomatic: Bool = false
    ) {
        self.provider = provider
        self.model = model
        self.effort = effort
        self.modelAutomatic = modelAutomatic
        self.effortAutomatic = effortAutomatic
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        model = f.string("model").flatMap { $0.isEmpty ? nil : $0 }
        provider = f.string("provider").flatMap { $0.isEmpty ? nil : $0 }
            ?? model.map { ModelPicker.provider(of: $0, in: []) } ?? "codex"
        effort = f.string("effort").flatMap { $0.isEmpty ? nil : $0 }
        modelAutomatic = f.bool("modelAutomatic") ?? false
        effortAutomatic = f.bool("effortAutomatic") ?? false
    }
}

/// A provider in `agents.defaults`, with how its new sessions start there.
public struct DefaultsProvider: Identifiable, Hashable, Sendable, Decodable {
    public var id: String
    public var label: String
    public var startingModes: [StartingMode]
    /// Codex's approval presets; empty for Claude Code and older computers.
    public var presets: [StartingMode]
    /// The mode this provider's new sessions start in (nil: its default).
    public var startingMode: String?
    /// `startingMode` comes from the location's automatic rule, not saved.
    public var startingModeAutomatic: Bool
    /// Codex only: the preset its new sessions get (nil: Codex's configured approvals).
    public var codexPreset: String?
    public var codexPresetAutomatic: Bool

    public init(
        id: String, label: String? = nil, startingModes: [StartingMode] = [], presets: [StartingMode] = [],
        startingMode: String? = nil, startingModeAutomatic: Bool = false, codexPreset: String? = nil,
        codexPresetAutomatic: Bool = false
    ) {
        self.id = id
        self.label = label ?? ModelPicker.providerLabel(id)
        self.startingModes = startingModes
        self.presets = presets
        self.startingMode = startingMode
        self.startingModeAutomatic = startingModeAutomatic
        self.codexPreset = codexPreset
        self.codexPresetAutomatic = codexPresetAutomatic
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let id = f.string("id"), !id.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("id"), .init(codingPath: decoder.codingPath, debugDescription: "provider id"))
        }
        self.id = id
        label = f.string("label") ?? ModelPicker.providerLabel(id)
        startingModes = f.list("startingModes")
        presets = f.list("presets")
        startingMode = f.string("startingMode").flatMap { $0.isEmpty ? nil : $0 }
        startingModeAutomatic = f.bool("startingModeAutomatic") ?? false
        codexPreset = f.string("codexPreset").flatMap { $0.isEmpty ? nil : $0 }
        codexPresetAutomatic = f.bool("codexPresetAutomatic") ?? false
    }

    /// The label of mode `id`, or of the provider's default mode for nil.
    public func modeLabel(_ id: String?) -> String {
        let id = id ?? "default"
        return startingModes.first { $0.id == id }?.label ?? (id == "default" ? "Default" : id)
    }

    /// The label of preset `id`; nil is Codex's own configuration.
    public func presetLabel(_ id: String?) -> String {
        guard let id else { return "Codex's configured approvals" }
        return presets.first { $0.id == id }?.label ?? id
    }
}

/// A mode new sessions can start in (plan, accept edits, …), or a Codex
/// approval preset.
public struct StartingMode: Identifiable, Hashable, Sendable, Decodable {
    public var id: String
    public var label: String
    public var description: String?

    public init(id: String, label: String, description: String? = nil) {
        self.id = id
        self.label = label
        self.description = description
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let id = f.string("id"), !id.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("id"), .init(codingPath: decoder.codingPath, debugDescription: "mode id"))
        }
        self.id = id
        label = f.string("label") ?? id
        description = f.string("description").flatMap { $0.isEmpty ? nil : $0 }
    }
}

/// What `agents.defaults.set` changes. A nil property leaves that default as
/// it is; `.some(nil)` sets it back to automatic.
public struct AgentDefaultsChange: Hashable, Sendable {
    /// `.some(nil)` clears the saved provider (newer computers).
    public var provider: String??
    public var model: String??
    public var effort: String??
    public var repository: String??
    public var startingMode: String??
    public var codexPreset: String??
    public var isolated: Bool?

    public init(
        provider: String?? = nil, model: String?? = nil, effort: String?? = nil,
        repository: String?? = nil, startingMode: String?? = nil, codexPreset: String?? = nil,
        isolated: Bool? = nil
    ) {
        self.provider = provider
        self.model = model
        self.effort = effort
        self.repository = repository
        self.startingMode = startingMode
        self.codexPreset = codexPreset
        self.isolated = isolated
    }

    /// Every default, as edited on a settings screen.
    public init(all defaults: AgentDefaults) {
        self.init(
            provider: .some(defaults.provider), model: .some(defaults.model), effort: .some(defaults.effort),
            repository: .some(defaults.repository), startingMode: .some(defaults.startingMode),
            isolated: defaults.isolated)
    }

    /// Returns the model and effort to automatic (the newest Claude Opus
    /// where Claude Code has one).
    public static let automaticModel = AgentDefaultsChange(
        provider: .some(nil), model: .some(nil), effort: .some(nil))

    /// What editing `saved` into `draft` changes. Another provider sends the
    /// model, effort and starting mode too, since the computer would otherwise
    /// drop the ones that belonged to the old provider; clearing the saved
    /// provider (back to automatic) sends `provider: null` with them.
    public init(from saved: AgentDefaults, to draft: AgentDefaults) {
        // Computers that report effective defaults leave the provider unsaved
        // (automatic) unless the person picked one.
        let automatic = draft.reportsEffective && draft.savedProvider == nil
        let cleared = automatic && (saved.savedProvider != nil || draft.provider != saved.provider)
        let switched = !automatic && draft.provider != saved.provider
        let pinned = draft.savedProvider != nil && draft.savedProvider != saved.savedProvider
        let provider: String??
        if cleared {
            provider = .some(nil)
        } else if switched || pinned {
            provider = .some(draft.savedProvider ?? draft.provider)
        } else {
            provider = nil
        }
        let moved = cleared || switched
        self.init(
            provider: provider,
            model: moved || draft.model != saved.model ? .some(draft.model) : nil,
            effort: moved || draft.effort != saved.effort ? .some(draft.effort) : nil,
            repository: draft.repository != saved.repository ? .some(draft.repository) : nil,
            startingMode: moved || draft.startingMode != saved.startingMode ? .some(draft.startingMode) : nil,
            codexPreset: draft.codexPreset != saved.codexPreset ? .some(draft.codexPreset) : nil,
            isolated: draft.isolated != saved.isolated ? draft.isolated : nil)
    }

    /// Whether saving it would change nothing.
    public var isEmpty: Bool { params.isEmpty }

    var params: [String: JSONValue] {
        var params: [String: JSONValue] = [:]
        if let provider { params["provider"] = JSONValue(provider) }
        if let model { params["model"] = JSONValue(model) }
        if let effort { params["effort"] = JSONValue(effort) }
        if let repository { params["repository"] = JSONValue(repository) }
        if let startingMode { params["startingMode"] = JSONValue(startingMode) }
        if let codexPreset { params["codexPreset"] = JSONValue(codexPreset) }
        if let isolated { params["isolated"] = .bool(isolated) }
        return params
    }
}
