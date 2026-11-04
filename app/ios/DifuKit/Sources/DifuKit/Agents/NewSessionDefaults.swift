import Foundation

/// What a new session starts with on the phone: the location's saved defaults,
/// or Claude Opus with extra-high reasoning when it saved no model.
public enum NewSessionDefaults {
    /// A model and effort for the pickers (nil: the location's default).
    public struct Choice: Equatable, Sendable {
        public var model: String?
        public var effort: String?

        public init(model: String?, effort: String?) {
            self.model = model
            self.effort = effort
        }
    }

    /// The effort the phone prefers for Claude Opus.
    public static let preferredEffort = "xhigh"

    /// Efforts from lowest to highest, for picking a model's highest.
    static let effortOrder = ["none", "minimal", "low", "medium", "high", "xhigh", "max"]

    /// The model and effort to preselect. Saved defaults win; without a saved
    /// model, the newest Claude Opus in `models` with `xhigh` (or its highest
    /// effort); without Claude models (or before they load), the location's default.
    public static func choice(saved: AgentDefaults?, models: [AgentModel]?) -> Choice {
        if let model = saved?.model {
            return Choice(model: model, effort: saved?.effort)
        }
        if let models, let opus = newestOpus(in: models) {
            return Choice(model: opus.id, effort: effort(for: opus))
        }
        return Choice(model: nil, effort: saved?.effort)
    }

    /// The newest Claude Opus: the highest version (from the id, else the
    /// name), then the `opus` alias. Plan-mode and other variants of the name
    /// don't count; a 1M-context variant loses to the plain model.
    public static func newestOpus(in models: [AgentModel]) -> AgentModel? {
        struct Candidate {
            var model: AgentModel
            var version: [Int]
            var fromID: Bool
            var variant: Bool
            var alias: Bool
            var index: Int
        }
        let candidates = models.enumerated().compactMap { index, model -> Candidate? in
            guard (model.provider ?? ModelPicker.provider(of: model.id, in: [])) == "claude" else {
                return nil
            }
            let bare = bareID(model.id)
            let base = bare.split(separator: "[", maxSplits: 1).first.map(String.init) ?? bare
            let tokens = words(base)
            guard tokens.contains("opus") else { return nil }
            let idVersion = version(tokens)
            let nameTokens = words(model.name.lowercased())
            let nameVersion = nameTokens.contains("opus") ? version(nameTokens) : []
            return Candidate(
                model: model, version: idVersion.isEmpty ? nameVersion : idVersion,
                fromID: !idVersion.isEmpty, variant: base != bare, alias: base == "opus", index: index)
        }
        return candidates.min { a, b in
            if a.version != b.version { return b.version.lexicographicallyPrecedes(a.version) }
            if a.fromID != b.fromID { return a.fromID }
            if a.variant != b.variant { return !a.variant }
            if a.alias != b.alias { return a.alias }
            return a.index < b.index
        }?.model
    }

    /// `xhigh` when `model` supports it, otherwise its highest effort (nil
    /// when it lists none).
    public static func effort(for model: AgentModel, preferring preferred: String = preferredEffort) -> String? {
        if model.efforts.contains(preferred) { return preferred }
        return model.efforts.enumerated().max { a, b in
            let (left, right) = (rank(a.element), rank(b.element))
            return left != right ? left < right : a.offset < b.offset
        }?.element
    }

    /// The repository to preselect: the saved default, then the one the
    /// location marks as default, then the first.
    public static func repository(saved: String?, in repositories: [AgentRepository]) -> String? {
        if let saved, !saved.isEmpty { return saved }
        return (repositories.first(where: \.isDefault) ?? repositories.first)?.path
    }

    /// `repositories` plus the saved default when the list doesn't have it,
    /// so the picker can show it.
    public static func repositories(_ repositories: [AgentRepository], saved: String?) -> [AgentRepository] {
        guard let saved, !saved.isEmpty, !repositories.contains(where: { $0.path == saved }) else {
            return repositories
        }
        return [AgentRepository(path: saved, isDefault: true)] + repositories
    }

    /// The model id without the provider prefix, lowercased.
    static func bareID(_ id: String) -> String {
        let lower = id.lowercased()
        return lower.hasPrefix("claude/") ? String(lower.dropFirst("claude/".count)) : lower
    }

    private static func words(_ text: String) -> [String] {
        text.split { !$0.isLetter && !$0.isNumber }.map(String.init)
    }

    /// Version numbers in `tokens`, ignoring dates (`20250514`).
    private static func version(_ tokens: [String]) -> [Int] {
        tokens.compactMap { $0.count < 6 ? Int($0) : nil }
    }

    private static func rank(_ effort: String) -> Int {
        effortOrder.firstIndex(of: effort.lowercased()) ?? -1
    }
}
