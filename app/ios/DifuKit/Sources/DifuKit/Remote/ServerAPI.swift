import Foundation

// Methods every paired device can call on a difu server (docs/cloud-agents.md):
// devices, server repositories, repository rules, environment profiles,
// logins and disk space.

extension DifuAPI {
    // MARK: Devices

    /// Revokes this device; the server then closes the socket. Call it from
    /// "Forget" before deleting the credential.
    public func unpair() async throws {
        try await perform("device.unpair")
    }

    public func devices() async throws -> [PairedDevice] {
        try await request("server.devices", as: DevicesResult.self).devices
    }

    /// Revokes another device.
    public func forgetDevice(_ id: String) async throws {
        try await perform("server.devices.forget", ["device": .string(id)])
    }

    // MARK: Repositories

    public func serverRepositories() async throws -> ServerRepositories {
        try await request("server.repos", as: ServerRepositories.self)
    }

    /// Repositories the server's GitHub login can clone.
    public func availableRepositories() async throws -> [AvailableRepository] {
        try await request("server.repos.available", as: AvailableRepositoriesResult.self).repositories
    }

    /// Adds `owner/name`; it clones in the background (see `serverRepositories`).
    public func addRepository(_ name: String) async throws {
        try await perform("server.repos.add", ["name": .string(name)])
    }

    public func removeRepository(_ name: String) async throws {
        try await perform("server.repos.remove", ["name": .string(name)])
    }

    /// Fetches one repository, or every one when `name` is nil.
    public func refreshRepositories(_ name: String? = nil) async throws {
        var params: [String: JSONValue] = [:]
        if let name { params["name"] = .string(name) }
        try await perform("server.repos.refresh", params)
    }

    // MARK: Repository rules

    /// `repository` is `owner/name` or a path from `agents.repositories`.
    public func rules(for repository: String) async throws -> String {
        try await request(
            "server.rules.get", ["repository": .string(repository)], as: RulesResult.self
        ).rules
    }

    public func setRules(_ rules: String, for repository: String) async throws {
        try await perform(
            "server.rules.set", ["repository": .string(repository), "rules": .string(rules)])
    }

    // MARK: Environment profiles

    /// A server repository's environment profile and the versions its files
    /// declare. `repository` is `owner/name`.
    public func environment(for repository: String) async throws -> RepositoryEnvironment {
        try await request(
            "server.environment.get", ["repository": .string(repository)],
            as: RepositoryEnvironment.self)
    }

    /// Replaces the profile's tools, setup commands, variables and notes (the
    /// server keeps its history) and returns what it saved. Running sessions of
    /// the repository are told.
    @discardableResult
    public func setEnvironment(_ profile: EnvironmentProfile, for repository: String) async throws
        -> RepositoryEnvironment
    {
        try await request(
            "server.environment.set", ["repository": .string(repository), "profile": profile.json],
            as: RepositoryEnvironment.self)
    }

    // MARK: Logins

    public func logins() async throws -> [ServerLogin] {
        try await request("server.logins", as: LoginsResult.self).logins
    }

    /// Starts a GitHub or Codex device-code login; poll `logins()` until done.
    public func startLogin(_ tool: LoginTool) async throws -> DeviceLogin {
        try await request("server.login.start", ["tool": .string(tool.rawValue)], as: DeviceLogin.self)
    }

    /// Stores a `claude setup-token` token for the service.
    public func setLoginToken(_ token: String, for tool: LoginTool = .claude) async throws {
        try await perform(
            "server.login.token", ["tool": .string(tool.rawValue), "token": .string(token)])
    }

    public func logout(_ tool: String) async throws {
        try await perform("server.logout", ["tool": .string(tool)])
    }

    // MARK: Disk

    /// Disk space and what uses it. `refresh` re-measures the breakdown in the
    /// background: the reply has `measuring` and the previous breakdown.
    public func disk(refresh: Bool = false) async throws -> ServerDisk {
        try await request("server.disk", refresh ? ["refresh": .bool(true)] : [:], as: ServerDisk.self)
    }

    /// Runs a cleanup; it can take minutes.
    public func cleanDisk(_ action: DiskCleanAction) async throws -> DiskCleanResult {
        try await request(
            "server.disk.clean", ["action": .string(action.rawValue)], as: DiskCleanResult.self)
    }
}

private struct DevicesResult: Decodable {
    var devices: [PairedDevice]

    init(from decoder: Decoder) throws {
        devices = try decoder.fields().list("devices")
    }
}

private struct AvailableRepositoriesResult: Decodable {
    var repositories: [AvailableRepository]

    init(from decoder: Decoder) throws {
        repositories = try decoder.fields().list("repositories")
    }
}

private struct RulesResult: Decodable {
    var rules: String

    init(from decoder: Decoder) throws {
        rules = try decoder.fields().string("rules") ?? ""
    }
}

private struct LoginsResult: Decodable {
    var logins: [ServerLogin]

    init(from decoder: Decoder) throws {
        logins = try decoder.fields().list("logins")
    }
}
