import Foundation

/// A question set or approval the agent is waiting on.
public struct PendingView: Identifiable, Hashable, Sendable, Decodable {
    /// Opaque; echo back unchanged in `session.answer` / `session.decide`.
    public var request: JSONValue
    public var kind: PendingKind
    /// The agent keeps working while it waits.
    public var async: Bool
    public var title: String?
    public var detail: String?
    public var questions: [PendingQuestion]
    public var choices: [PendingChoice]

    public var id: String { request.jsonString() }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        request = f.value("request", as: JSONValue.self) ?? .null
        kind = PendingKind(lenient: f.string("kind"))
        async = f.bool("async") ?? false
        title = f.string("title")
        detail = f.string("detail")
        questions = f.list("questions")
        choices = f.list("choices")
    }
}

public struct PendingQuestion: Identifiable, Hashable, Sendable, Decodable {
    public var id: String
    public var header: String?
    public var question: String
    public var options: [QuestionOption]
    public var multiSelect: Bool
    public var secret: Bool
    public var answered: Bool

    /// Permission questions must be answered with an option label exactly.
    public var isPermission: Bool { id == "difu-decision" }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        id = f.string("id") ?? ""
        header = f.string("header")
        question = f.string("question") ?? ""
        options = f.list("options")
        multiSelect = f.bool("multiSelect") ?? false
        secret = f.bool("secret") ?? false
        answered = f.bool("answered") ?? false
    }
}

public struct QuestionOption: Hashable, Sendable, Decodable {
    public var label: String
    public var description: String?

    public init(from decoder: Decoder) throws {
        if let label = try? decoder.singleValueContainer().decode(String.self) {
            self.label = label
            return
        }
        let f = try decoder.fields()
        label = f.string("label") ?? ""
        description = f.string("description").flatMap { $0.isEmpty ? nil : $0 }
    }
}

public struct PendingChoice: Identifiable, Hashable, Sendable, Decodable {
    public var id: String
    public var label: String

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let id = f.string("id") else {
            throw DecodingError.keyNotFound(
                AnyKey("id"), .init(codingPath: decoder.codingPath, debugDescription: "choice id"))
        }
        self.id = id
        label = f.string("label") ?? id
    }
}
