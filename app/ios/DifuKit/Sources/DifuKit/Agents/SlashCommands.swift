import Foundation

/// The composer's `/` list: when it shows, what it lists, and what choosing a
/// command or sending a command draft does.
public enum SlashCommands {
    /// A command being typed: `/name` and, once a space follows, its arguments.
    public struct Query: Equatable, Sendable {
        public var name: String
        /// Nil while the name is still being typed.
        public var arguments: String?

        public init(name: String, arguments: String? = nil) {
            self.name = name
            self.arguments = arguments
        }
    }

    /// The query when `text` starts with `/` on its first line.
    public static func query(_ text: String) -> Query? {
        guard text.hasPrefix("/"), !text.contains("\n") else { return nil }
        let body = text.dropFirst()
        guard let space = body.firstIndex(where: \.isWhitespace) else {
            return Query(name: String(body))
        }
        return Query(
            name: String(body[..<space]),
            arguments: String(body[space...]).trimmingCharacters(in: .whitespaces))
    }

    /// Commands matching `query`: names starting with it first, then names and
    /// descriptions containing it. Once arguments follow a complete name, only
    /// that command. `skillsOnly` lists the provider's skills and commands.
    public static func matches(
        _ commands: [SessionCommand], query: Query, skillsOnly: Bool = false
    ) -> [SessionCommand] {
        let pool = skillsOnly ? commands.filter { !$0.isDifu } : commands
        let name = query.name.lowercased()
        if query.arguments != nil {
            return Array(pool.filter { $0.name.lowercased() == name }.prefix(1))
        }
        guard !name.isEmpty else { return pool }
        let prefixed = pool.filter { $0.name.lowercased().hasPrefix(name) }
        let named = pool.filter {
            !$0.name.lowercased().hasPrefix(name) && $0.name.lowercased().contains(name)
        }
        let described = pool.filter {
            !$0.name.lowercased().contains(name) && $0.description.lowercased().contains(name)
        }
        return prefixed + named + described
    }

    /// What choosing a command from the list does.
    public enum Choice: Equatable, Sendable {
        /// Run it now (ask first when `command.confirm`).
        case run(SessionCommand, arguments: String)
        /// Open its screen.
        case open(CommandScreen, SessionCommand)
        /// Put this text in the composer to add arguments and send.
        case insert(String)
    }

    /// `text` is the composer's current draft, whose typed arguments are kept.
    public static func choose(_ command: SessionCommand, text: String) -> Choice {
        let typed = query(text).flatMap { $0.name.lowercased() == command.name.lowercased() ? $0.arguments : nil }
            ?? ""
        let insertion = typed.isEmpty ? "\(command.trigger) " : "\(command.trigger) \(typed)"
        switch command.kind {
        case .prompt:
            return .insert(insertion)
        case .action:
            if command.arguments == .required && typed.isEmpty { return .insert(insertion) }
            return .run(command, arguments: typed)
        case .screen:
            if !typed.isEmpty && command.arguments != .none { return .run(command, arguments: typed) }
            return .open(command.screen ?? .unknown, command)
        }
    }

    /// What sending the composer's text does.
    public enum Submission: Equatable, Sendable {
        /// A difu command: run it with `session.command` (ask first when it
        /// needs confirmation).
        case command(SessionCommand, arguments: String)
        /// A message, with the Codex skills it names attached.
        case message(String, skills: [String])
    }

    public static func submission(_ text: String, commands: [SessionCommand]) -> Submission {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if let query = query(trimmed),
            let command = commands.first(where: { $0.isDifu && $0.name.lowercased() == query.name.lowercased() })
        {
            return .command(command, arguments: query.arguments ?? "")
        }
        let words = Set(trimmed.split(whereSeparator: \.isWhitespace).map(String.init))
        var skills: [String] = []
        for command in commands where command.isAttachedSkill && words.contains(command.trigger) {
            if !skills.contains(command.name) { skills.append(command.name) }
        }
        return .message(trimmed, skills: skills)
    }
}
