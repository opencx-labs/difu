import Foundation

/// Model and effort choices shared by the new-session and session sheets:
/// every provider's models, grouped like the terminal's Alt+M picker.
public enum ModelPicker {
    public struct Group: Identifiable, Hashable, Sendable {
        /// `codex`, `claude`, or `other`.
        public var provider: String
        public var title: String
        public var models: [AgentModel]

        public var id: String { provider }
    }

    /// The display name of a provider key.
    public static func providerLabel(_ provider: String?) -> String {
        switch provider?.lowercased() {
        case "codex": "Codex"
        case "claude": "Claude Code"
        case nil, "": "Other"
        case let other?: other.capitalized
        }
    }

    /// Models grouped by provider in the server's order, with `first` (the
    /// session's provider) on top.
    public static func groups(_ models: [AgentModel], first: String? = nil) -> [Group] {
        var order: [String] = []
        var groups: [String: [AgentModel]] = [:]
        for model in models {
            let key = model.provider ?? provider(of: model.id, in: [])
            if groups[key] == nil { order.append(key) }
            groups[key, default: []].append(model)
        }
        if let first, let index = order.firstIndex(of: first) {
            order.insert(order.remove(at: index), at: 0)
        }
        return order.map { Group(provider: $0, title: providerLabel($0), models: groups[$0] ?? []) }
    }

    /// The provider running `model`: from the list when it's there, otherwise
    /// by name as the computer decides (`claude/…`, `claude-…`, `sonnet`,
    /// `opus`, `haiku` are Claude Code; everything else Codex).
    public static func provider(of model: String, in models: [AgentModel]) -> String {
        if let provider = models.first(where: { $0.id == model })?.provider { return provider }
        if model.hasPrefix("claude/") || model.hasPrefix("claude-")
            || ["sonnet", "opus", "haiku"].contains(model)
        {
            return "claude"
        }
        return "codex"
    }

    /// Efforts offered for `model`; for the provider default (`nil`), those of
    /// `provider`'s models (every model's when the provider isn't known).
    public static func efforts(_ models: [AgentModel], model: String?, provider: String?) -> [String] {
        if let model, let match = models.first(where: { $0.id == model }) {
            return match.efforts
        }
        let relevant = provider.map { provider in models.filter { ($0.provider ?? provider) == provider } }
            ?? models
        var seen = Set<String>()
        return relevant.flatMap(\.efforts).filter { seen.insert($0).inserted }
    }

    /// `effort` when the chosen model supports it, otherwise the default.
    public static func syncedEffort(
        _ effort: String?, models: [AgentModel], model: String?, provider: String?
    ) -> String? {
        guard let effort else { return nil }
        let offered = efforts(models, model: model, provider: provider)
        return offered.isEmpty || offered.contains(effort) ? effort : nil
    }
}

/// What saving a model and effort does to a session.
public enum ModelChange: Equatable, Sendable {
    case unchanged
    case allowed
    /// Allowed: the session moves to another provider, keeping chat and workspace.
    case switchesProvider(from: String, to: String)
    /// Not now, and why.
    case blocked(String)

    public var canSave: Bool {
        switch self {
        case .allowed, .switchesProvider: true
        case .unchanged, .blocked: false
        }
    }

    /// The computer's rules: switching providers needs a coding session between
    /// turns (no turn, pending request or queued message), and Claude Code
    /// changes models only between turns.
    public static func evaluate(
        session: SessionView, model: String?, effort: String?, models: [AgentModel]
    ) -> ModelChange {
        if model == session.model && effort == session.effort { return .unchanged }
        let current = session.provider ?? model.map { ModelPicker.provider(of: $0, in: models) } ?? "codex"
        let target = model.map { ModelPicker.provider(of: $0, in: models) } ?? current
        if target != current {
            if session.kind != .coding {
                return .blocked("Only coding sessions can switch providers.")
            }
            if !session.isBetweenTurns {
                return .blocked(
                    "Finish the current turn and pending requests before switching providers.")
            }
            return .switchesProvider(from: current, to: target)
        }
        if current == "claude" && !session.isBetweenTurns {
            return .blocked(
                "Claude Code changes models between turns. Finish the current turn and pending requests first.")
        }
        return .allowed
    }
}
