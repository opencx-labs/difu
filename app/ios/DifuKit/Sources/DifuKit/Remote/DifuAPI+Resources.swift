import Foundation

// Session resources (artifacts, shells, sub-agents), the sidebar (pins and
// folders) and the session actions beyond messages. Computers with an older
// difu answer `Unknown method` (`RemoteError.isUnknownMethod`).

extension DifuAPI {
    /// The list with its folders, then `sessions` and `folders` events.
    public func subscribeSessionList() async throws -> SessionList {
        try await request("sessions.subscribe", as: SessionList.self)
    }

    public func sessionList() async throws -> SessionList {
        try await request("sessions.list", as: SessionList.self)
    }

    // MARK: Artifacts

    /// A presented artifact's HTML, read by the computer or server running
    /// the session (registered artifacts only, up to 32 MB).
    public func artifact(_ id: String, artifact: SessionArtifact) async throws -> ArtifactFile {
        try await request(
            "session.artifact",
            ["id": .string(id), "path": .string(artifact.path), "workspace": JSONValue(artifact.workspace)],
            as: ArtifactFile.self)
    }

    // MARK: Shells

    public func shells(_ id: String) async throws -> [ShellInfo] {
        try await request("session.shells", ["id": .string(id)], as: ShellsResult.self).shells
    }

    /// The shell's text without colours (the last 64 KB). With the
    /// `revision` from the last read, unchanged output isn't sent again.
    public func readShell(_ id: String, shell: String, revision: Int? = nil) async throws -> ShellOutput {
        var params: [String: JSONValue] = ["id": .string(id), "shell": .string(shell)]
        if let revision { params["revision"] = JSONValue(revision) }
        return try await request("session.shell", params, as: ShellOutput.self)
    }

    /// Opens a shared shell in the session's worktree, or runs `command` like
    /// a `!` message. Returns the shell id.
    public func openShell(_ id: String, command: String? = nil) async throws -> String {
        var params: [String: JSONValue] = ["id": .string(id)]
        if let command = command?.trimmingCharacters(in: .whitespacesAndNewlines), !command.isEmpty {
            params["command"] = .string(command)
        }
        return try await request("session.shell.open", params, as: IdentifierResult.self).id
    }

    /// Types `text` (a newline is Enter) and Enter when `enter`. Control keys
    /// go as they are: `ShellKey`.
    public func writeShell(_ id: String, shell: String, text: String, enter: Bool = false) async throws {
        try await perform(
            "session.shell.write",
            ["id": .string(id), "shell": .string(shell), "text": .string(text), "enter": .bool(enter)])
    }

    public func closeShell(_ id: String, shell: String) async throws {
        try await perform("session.shell.close", ["id": .string(id), "shell": .string(shell)])
    }

    // MARK: Sub-agents

    /// A sub-agent's transcript; `refresh` reads a Codex sub-agent's history again.
    public func subagent(_ id: String, agent: String, refresh: Bool = false) async throws -> SubagentTranscript {
        try await request(
            "session.subagent",
            ["id": .string(id), "agent": .string(agent), "refresh": .bool(refresh)],
            as: SubagentTranscript.self)
    }

    // MARK: Pins and folders

    public func pin(_ id: String, pinned: Bool) async throws {
        try await perform("session.pin", ["id": .string(id), "pinned": .bool(pinned)])
    }

    public func folders() async throws -> [SessionFolder] {
        try await request("folders.list", as: FoldersResult.self).folders
    }

    /// Creates a folder holding `sessions`; returns the folders as saved.
    @discardableResult
    public func createFolder(name: String, sessions: [String] = []) async throws -> [SessionFolder] {
        try await request(
            "folders.create",
            ["name": .string(name), "ids": .array(sessions.map(JSONValue.string))],
            as: FoldersResult.self
        ).folders
    }

    @discardableResult
    public func renameFolder(_ folder: String, name: String) async throws -> [SessionFolder] {
        try await request(
            "folders.rename", ["folder": .string(folder), "name": .string(name)], as: FoldersResult.self
        ).folders
    }

    /// Removes the folder; its sessions return to the main list.
    @discardableResult
    public func deleteFolder(_ folder: String) async throws -> [SessionFolder] {
        try await request("folders.delete", ["folder": .string(folder)], as: FoldersResult.self).folders
    }

    /// Files sessions into `folder`, or back to the main list when nil.
    @discardableResult
    public func moveToFolder(_ sessions: [String], folder: String?) async throws -> [SessionFolder] {
        try await request(
            "folders.move",
            ["ids": .array(sessions.map(JSONValue.string)), "folder": JSONValue(folder)],
            as: FoldersResult.self
        ).folders
    }

    /// Moves a folder to `position` (0 is first).
    @discardableResult
    public func reorderFolder(_ folder: String, position: Int) async throws -> [SessionFolder] {
        try await request(
            "folders.reorder", ["folder": .string(folder), "position": JSONValue(position)],
            as: FoldersResult.self
        ).folders
    }

    // MARK: Session actions

    /// Replaces a queued message; `expected` is the text the phone showed.
    public func editQueued(_ id: String, message: QueuedMessage, text: String) async throws {
        try await perform(
            "session.editQueued",
            [
                "id": .string(id), "index": JSONValue(message.index), "expected": .string(message.text),
                "text": .string(text),
            ])
    }

    /// Attaches a pull request from any repository by its GitHub URL.
    public func attachPr(_ id: String, url: String) async throws {
        try await perform("session.attachPr", ["id": .string(id), "url": .string(url)])
    }

    /// The saved cloud servers this computer can move sessions to.
    public func moveTargets() async throws -> [MoveTarget] {
        try await request("agents.servers", as: MoveTargetsResult.self).servers
    }

    /// Moves a session on this computer to a saved server; returns the new
    /// `<server>:<session>` id. Can take minutes.
    public func move(_ id: String, to server: String) async throws -> String {
        try await request(
            "session.move", ["id": .string(id), "server": .string(server)], as: IdentifierResult.self
        ).id
    }
}

private struct ShellsResult: Decodable {
    var shells: [ShellInfo]

    init(from decoder: Decoder) throws {
        shells = try decoder.fields().list("shells")
    }
}

private struct FoldersResult: Decodable {
    var folders: [SessionFolder]

    init(from decoder: Decoder) throws {
        folders = try decoder.fields().list("folders")
    }
}

private struct MoveTargetsResult: Decodable {
    var servers: [MoveTarget]

    init(from decoder: Decoder) throws {
        servers = try decoder.fields().list("servers")
    }
}

private struct IdentifierResult: Decodable {
    var id: String

    init(from decoder: Decoder) throws {
        guard let id = try decoder.fields().string("id"), !id.isEmpty else {
            throw RemoteError.protocolError("missing id")
        }
        self.id = id
    }
}
