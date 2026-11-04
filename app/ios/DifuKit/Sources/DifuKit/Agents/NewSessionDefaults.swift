import Foundation

/// What a new session starts with on the phone: the location's effective
/// defaults as it reports them (saved ones, else the newest Claude Opus with
/// extra-high reasoning, and Auto modes on servers). For older computers that
/// don't report them, the phone picks Claude Opus itself when nothing is saved.
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

    /// The model and effort to preselect: the location's effective defaults
    /// when it reports them. For older computers, saved defaults win; without
    /// a saved model, the newest Claude Opus in `models` with `xhigh` (or its
    /// highest effort); without Claude models (or before they load), the
    /// location's default.
    public static func choice(saved: AgentDefaults?, models: [AgentModel]?) -> Choice {
        if let effective = saved?.effective {
            return Choice(model: effective.model, effort: effective.effort)
        }
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

    /// The model and effort automatic defaults pick from `models`: the newest
    /// Claude Opus with `xhigh` (or its highest), else the provider default.
    /// For previews before the computer has saved and answered.
    public static func automaticChoice(models: [AgentModel]?) -> Choice {
        guard let models, let opus = newestOpus(in: models) else { return Choice(model: nil, effort: nil) }
        return Choice(model: opus.id, effort: effort(for: opus))
    }

    /// The provider a new session runs on with `model` (nil: the location's
    /// effective provider, else its saved one).
    public static func provider(model: String?, models: [AgentModel]?, defaults: AgentDefaults?) -> String {
        if let model { return ModelPicker.provider(of: model, in: models ?? []) }
        return defaults?.effective?.provider ?? defaults?.provider ?? "codex"
    }

    /// How `provider`'s sessions start at the location when the person picks
    /// nothing; nil from older computers.
    public struct Start: Equatable, Sendable {
        /// nil: the provider's default mode.
        public var mode: String?
        public var modeAutomatic: Bool
        /// Codex only; nil: Codex's configured approvals.
        public var preset: String?
        public var presetAutomatic: Bool

        public init(mode: String? = nil, modeAutomatic: Bool = false, preset: String? = nil, presetAutomatic: Bool = false) {
            self.mode = mode
            self.modeAutomatic = modeAutomatic
            self.preset = preset
            self.presetAutomatic = presetAutomatic
        }
    }

    public static func start(for provider: String, defaults: AgentDefaults?) -> Start? {
        guard let defaults, defaults.reportsEffective, let entry = defaults.entry(for: provider) else { return nil }
        return Start(
            mode: entry.startingMode, modeAutomatic: entry.startingModeAutomatic,
            preset: provider == "codex" ? entry.codexPreset : nil,
            presetAutomatic: provider == "codex" && entry.codexPresetAutomatic)
    }

    /// The label of the "leave it to the location" mode choice: "Auto ·
    /// automatic" on servers, "Plan · default" when saved, "Manual · default".
    public static func startLabel(for provider: String, defaults: AgentDefaults?) -> String {
        let start = start(for: provider, defaults: defaults) ?? Start()
        let label = defaults?.entry(for: provider)?.modeLabel(start.mode) ?? "Default"
        return "\(label) · \(start.modeAutomatic ? "automatic" : "default")"
    }

    /// The label of the "leave it to the location" Codex approvals choice.
    public static func presetLabel(defaults: AgentDefaults?) -> String {
        let start = start(for: "codex", defaults: defaults) ?? Start()
        let label = defaults?.entry(for: "codex")?.presetLabel(start.preset) ?? "Codex's configured approvals"
        return "\(label) · \(start.presetAutomatic ? "automatic" : "default")"
    }

    /// What `agents.new` sends for the mode pickers: only choices the person
    /// made that `provider` offers (nil leaves the location's start).
    public static func startRequest(
        provider: String, mode: String?, preset: String?, defaults: AgentDefaults?
    ) -> (startingMode: String?, codexPreset: String?) {
        guard let defaults, defaults.reportsEffective else { return (nil, nil) }
        let mode = mode.flatMap { id in defaults.startingModes(for: provider).contains { $0.id == id } ? id : nil }
        let preset = provider == "codex"
            ? preset.flatMap { id in defaults.codexPresets.contains { $0.id == id } ? id : nil } : nil
        return (mode, preset)
    }

    /// A chosen mode `provider` doesn't offer goes back to the location's start.
    public static func keptMode(_ mode: String?, provider: String, defaults: AgentDefaults?) -> String? {
        guard let mode, defaults?.startingModes(for: provider).contains(where: { $0.id == mode }) == true else {
            return nil
        }
        return mode
    }

    /// Starting a session this way skips permission checks (Claude's bypass
    /// mode or Codex's Full Access), so the phone asks first.
    public static func skipsChecks(mode: String?, preset: String?) -> Bool {
        mode == "bypassPermissions" || preset == "Full"
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
