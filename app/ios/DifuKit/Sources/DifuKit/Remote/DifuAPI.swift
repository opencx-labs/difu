import Foundation

/// Typed wrappers for every remote-control method.
public struct DifuAPI: Sendable {
    public typealias Call = @Sendable (_ method: String, _ params: [String: JSONValue]) async throws -> Data

    private let call: Call

    public init(call: @escaping Call) {
        self.call = call
    }

    public init(_ connection: RemoteConnection) {
        self.init { method, params in try await connection.call(method, params) }
    }

    /// Calls `method` and decodes `result` as `T`.
    public func request<T: Decodable>(
        _ method: String, _ params: [String: JSONValue] = [:], as type: T.Type = T.self
    ) async throws -> T {
        let data = try await call(method, params)
        do {
            return try JSONDecoder().decode(ResultEnvelope<T>.self, from: data).result
        } catch {
            throw RemoteError.protocolError("\(method) returned an unexpected result")
        }
    }

    /// Calls `method` and returns `result` as raw JSON.
    public func raw(_ method: String, _ params: [String: JSONValue] = [:]) async throws -> JSONValue {
        let frame = try JSONDecoder().decode(JSONValue.self, from: try await call(method, params))
        return frame["result"] ?? .null
    }

    /// Calls a method whose result is `{}`.
    public func perform(_ method: String, _ params: [String: JSONValue] = [:]) async throws {
        _ = try await call(method, params)
    }
}

// MARK: Agents

extension DifuAPI {
    public func subscribeSessions() async throws -> [SessionSummary] {
        try await request("sessions.subscribe", as: SessionsResult.self).sessions
    }

    public func listSessions() async throws -> [SessionSummary] {
        try await request("sessions.list", as: SessionsResult.self).sessions
    }

    public func watch(_ id: String) async throws {
        try await perform("session.watch", ["id": .string(id)])
    }

    public func unwatch(_ id: String) async throws {
        try await perform("session.unwatch", ["id": .string(id)])
    }

    /// The untruncated entry.
    public func entry(session: String, entry: String) async throws -> EntryView {
        try await request(
            "session.entry", ["id": .string(session), "entry": .string(entry)],
            as: EntryResult.self
        ).entry
    }

    public func send(
        _ id: String, text: String, queue: Bool = false, attachments: [Attachment] = []
    ) async throws {
        var params: [String: JSONValue] = [
            "id": .string(id), "text": .string(text), "queue": .bool(queue),
        ]
        if !attachments.isEmpty {
            params["attachments"] = .array(attachments.map(\.json))
        }
        try await perform("session.send", params)
    }

    public func interrupt(_ id: String) async throws {
        try await perform("session.interrupt", ["id": .string(id)])
    }

    /// Interrupts and sends the queued messages now.
    public func sendNow(_ id: String) async throws {
        try await perform("session.sendNow", ["id": .string(id)])
    }

    /// "Continue session".
    public func resume(_ id: String) async throws {
        try await perform("session.resume", ["id": .string(id)])
    }

    public func compact(_ id: String) async throws {
        try await perform("session.compact", ["id": .string(id)])
    }

    public func unqueue(_ id: String, index: Int) async throws {
        try await perform("session.unqueue", ["id": .string(id), "index": JSONValue(index)])
    }

    /// Answers one question; `answer == nil` skips it.
    public func answer(
        _ id: String, request: JSONValue, question: String, answer: String?
    ) async throws {
        try await perform(
            "session.answer",
            [
                "id": .string(id), "request": request, "question": .string(question),
                "answer": JSONValue(answer),
            ])
    }

    public func decide(_ id: String, request: JSONValue, choice: String) async throws {
        try await perform(
            "session.decide", ["id": .string(id), "request": request, "choice": .string(choice)])
    }

    public func setModel(_ id: String, model: String?, effort: String?) async throws {
        try await perform(
            "session.model",
            ["id": .string(id), "model": JSONValue(model), "effort": JSONValue(effort)])
    }

    public func rename(_ id: String, title: String) async throws {
        try await perform("session.rename", ["id": .string(id), "title": .string(title)])
    }

    public func archive(_ id: String, archived: Bool) async throws {
        try await perform("session.archive", ["id": .string(id), "archived": .bool(archived)])
    }

    public func delete(_ id: String) async throws {
        try await perform("session.delete", ["id": .string(id)])
    }

    public func clear(_ id: String) async throws {
        try await perform("session.clear", ["id": .string(id)])
    }

    public func changes(_ id: String) async throws -> [DiffFile] {
        try await request("session.changes", ["id": .string(id)], as: FilesResult.self).files
    }

    /// Starts a session and returns its id. `repository == nil` uses the default.
    public func newSession(
        repository: String?, model: String?, effort: String?, text: String?
    ) async throws -> String {
        var params: [String: JSONValue] = [
            "repository": JSONValue(repository), "model": JSONValue(model),
            "effort": JSONValue(effort),
        ]
        if let text, !text.isEmpty { params["text"] = .string(text) }
        return try await request("agents.new", params, as: IdResult.self).id
    }

    public func repositories() async throws -> [AgentRepository] {
        try await request("agents.repositories", as: RepositoriesResult.self).repositories
    }

    /// Slow: the server asks each provider.
    public func models() async throws -> [AgentModel] {
        try await request("agents.models", as: ModelsResult.self).models
    }

    public func serverInfo() async throws -> ServerInfo {
        try await request("server.info", as: ServerInfo.self)
    }
}

// MARK: Reviews

extension DifuAPI {
    /// `refresh: true` fetches from GitHub (takes seconds).
    public func inbox(refresh: Bool = false) async throws -> Inbox {
        try await request("reviews.inbox", ["refresh": .bool(refresh)], as: Inbox.self)
    }

    public func reviewRepositories() async throws -> [ReviewRepository] {
        try await request("reviews.repositories", as: ReviewRepositoriesResult.self).repositories
    }

    public func pullRequests(in repo: String) async throws -> [Pr] {
        try await request("reviews.repository", ["repo": .string(repo)], as: PrsResult.self).prs
    }

    public func overview(repo: String, number: Int) async throws -> PrOverview {
        try await request("pr.overview", pr(repo, number), as: PrOverview.self)
    }

    public func diff(repo: String, number: Int) async throws -> [DiffFile] {
        try await request("pr.diff", pr(repo, number), as: FilesResult.self).files
    }

    public func guide(repo: String, number: Int) async throws -> Guide {
        try await request("pr.guide", pr(repo, number), as: Guide.self)
    }

    public func generateGuide(repo: String, number: Int) async throws -> Guide {
        try await request("pr.generateGuide", pr(repo, number), as: Guide.self)
    }

    public func comment(repo: String, number: Int, head: String, body: String) async throws -> String {
        var params = pr(repo, number, head: head)
        params["body"] = .string(body)
        return try await request("pr.comment", params, as: MessageResult.self).message
    }

    public func review(
        repo: String, number: Int, head: String, event: ReviewEvent, body: String
    ) async throws -> String {
        var params = pr(repo, number, head: head)
        params["event"] = .string(event.rawValue)
        params["body"] = .string(body)
        return try await request("pr.review", params, as: MessageResult.self).message
    }

    public func merge(
        repo: String, number: Int, head: String, squash: Bool, admin: Bool
    ) async throws -> String {
        var params = pr(repo, number, head: head)
        params["squash"] = .bool(squash)
        params["admin"] = .bool(admin)
        return try await request("pr.merge", params, as: MessageResult.self).message
    }

    public func close(repo: String, number: Int, head: String, body: String?) async throws -> String {
        var params = pr(repo, number, head: head)
        if let body, !body.isEmpty { params["body"] = .string(body) }
        return try await request("pr.close", params, as: MessageResult.self).message
    }

    /// `draft: false` marks ready for review; `true` converts back to draft.
    public func setReady(repo: String, number: Int, head: String, draft: Bool) async throws -> String {
        var params = pr(repo, number, head: head)
        params["draft"] = .bool(draft)
        return try await request("pr.ready", params, as: MessageResult.self).message
    }

    private func pr(_ repo: String, _ number: Int, head: String? = nil) -> [String: JSONValue] {
        var params: [String: JSONValue] = ["repo": .string(repo), "number": JSONValue(number)]
        if let head { params["head"] = .string(head) }
        return params
    }
}

// MARK: Result wrappers

private struct EntryResult: Decodable {
    var entry: EntryView
}

private struct FilesResult: Decodable {
    var files: [DiffFile]

    init(from decoder: Decoder) throws {
        files = try decoder.fields().list("files")
    }
}

private struct IdResult: Decodable {
    var id: String

    init(from decoder: Decoder) throws {
        guard let id = try decoder.fields().string("id") else {
            throw RemoteError.protocolError("missing session id")
        }
        self.id = id
    }
}

private struct RepositoriesResult: Decodable {
    var repositories: [AgentRepository]

    init(from decoder: Decoder) throws {
        repositories = try decoder.fields().list("repositories")
    }
}

private struct ModelsResult: Decodable {
    var models: [AgentModel]

    init(from decoder: Decoder) throws {
        models = try decoder.fields().list("models")
    }
}

private struct ReviewRepositoriesResult: Decodable {
    var repositories: [ReviewRepository]

    init(from decoder: Decoder) throws {
        repositories = try decoder.fields().list("repositories")
    }
}

private struct PrsResult: Decodable {
    var prs: [Pr]

    init(from decoder: Decoder) throws {
        prs = try decoder.fields().list("prs")
    }
}
