import Foundation

/// The new-session defaults saved on a computer or server (`agents.defaults`):
/// the same ones its terminal edits, used for sessions started without a choice.
public struct AgentDefaults: Hashable, Sendable, Decodable {
    /// `codex` or `claude`: the saved provider, or the saved model's.
    public var provider: String
    /// nil: the provider's configured model.
    public var model: String?
    /// nil: the model's default reasoning.
    public var effort: String?
    /// nil: no default repository.
    public var repository: String?
    /// A `providers[].startingModes[].id`; nil: the provider's own default.
    public var startingMode: String?
    /// Sessions get their own worktree.
    public var isolated: Bool
    /// The providers with the starting modes each offers.
    public var providers: [DefaultsProvider]

    public init(
        provider: String = "codex", model: String? = nil, effort: String? = nil,
        repository: String? = nil, startingMode: String? = nil, isolated: Bool = true,
        providers: [DefaultsProvider] = []
    ) {
        self.provider = provider
        self.model = model
        self.effort = effort
        self.repository = repository
        self.startingMode = startingMode
        self.isolated = isolated
        self.providers = providers
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        model = f.string("model").flatMap(Self.nonEmpty)
        provider = f.string("provider").flatMap(Self.nonEmpty)
            ?? model.map { ModelPicker.provider(of: $0, in: []) } ?? "codex"
        effort = f.string("effort").flatMap(Self.nonEmpty)
        repository = f.string("repository").flatMap(Self.nonEmpty)
        startingMode = f.string("startingMode").flatMap(Self.nonEmpty)
        isolated = f.bool("isolated") ?? true
        providers = f.list("providers")
    }

    /// The starting modes `provider` offers.
    public func startingModes(for provider: String) -> [StartingMode] {
        providers.first { $0.id == provider }?.startingModes ?? []
    }

    /// "Claude Code · claude-opus-4-7 · xhigh", for a settings row.
    public var summary: String {
        let model = model.map { $0.hasPrefix("claude/") ? String($0.dropFirst("claude/".count)) : $0 }
        return [ModelPicker.providerLabel(provider), model ?? "Default model", effort]
            .compactMap { $0 }
            .joined(separator: " · ")
    }

    private static func nonEmpty(_ text: String) -> String? {
        text.isEmpty ? nil : text
    }
}

/// A provider in `agents.defaults`.
public struct DefaultsProvider: Identifiable, Hashable, Sendable, Decodable {
    public var id: String
    public var label: String
    public var startingModes: [StartingMode]

    public init(id: String, label: String? = nil, startingModes: [StartingMode] = []) {
        self.id = id
        self.label = label ?? ModelPicker.providerLabel(id)
        self.startingModes = startingModes
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
    }
}

/// A mode new sessions can start in (plan, accept edits, …).
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
    public var provider: String?
    public var model: String??
    public var effort: String??
    public var repository: String??
    public var startingMode: String??
    public var isolated: Bool?

    public init(
        provider: String? = nil, model: String?? = nil, effort: String?? = nil,
        repository: String?? = nil, startingMode: String?? = nil, isolated: Bool? = nil
    ) {
        self.provider = provider
        self.model = model
        self.effort = effort
        self.repository = repository
        self.startingMode = startingMode
        self.isolated = isolated
    }

    /// Every default, as edited on a settings screen.
    public init(all defaults: AgentDefaults) {
        self.init(
            provider: defaults.provider, model: .some(defaults.model), effort: .some(defaults.effort),
            repository: .some(defaults.repository), startingMode: .some(defaults.startingMode),
            isolated: defaults.isolated)
    }

    /// What editing `saved` into `draft` changes. Another provider sends the
    /// model, effort and starting mode too, since the computer would otherwise
    /// drop the ones that belonged to the old provider.
    public init(from saved: AgentDefaults, to draft: AgentDefaults) {
        let switched = draft.provider != saved.provider
        self.init(
            provider: switched ? draft.provider : nil,
            model: switched || draft.model != saved.model ? .some(draft.model) : nil,
            effort: switched || draft.effort != saved.effort ? .some(draft.effort) : nil,
            repository: draft.repository != saved.repository ? .some(draft.repository) : nil,
            startingMode: switched || draft.startingMode != saved.startingMode ? .some(draft.startingMode) : nil,
            isolated: draft.isolated != saved.isolated ? draft.isolated : nil)
    }

    /// Whether saving it would change nothing.
    public var isEmpty: Bool { params.isEmpty }

    var params: [String: JSONValue] {
        var params: [String: JSONValue] = [:]
        if let provider { params["provider"] = .string(provider) }
        if let model { params["model"] = JSONValue(model) }
        if let effort { params["effort"] = JSONValue(effort) }
        if let repository { params["repository"] = JSONValue(repository) }
        if let startingMode { params["startingMode"] = JSONValue(startingMode) }
        if let isolated { params["isolated"] = .bool(isolated) }
        return params
    }
}
