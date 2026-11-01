import Foundation

/// What kind of difu a saved server is.
public enum ServerMode: String, LenientEnum, Codable {
    /// difu on a Mac (`/remote-control`).
    case computer
    /// A cloud server (`difu serve`).
    case server

    public static let fallback = ServerMode.computer

    public init(from decoder: Decoder) throws {
        self.init(lenient: try? decoder.singleValueContainer().decode(String.self))
    }
}

/// A device's access level: `standard` is the phone API, `full` adds `rpc`.
public enum DeviceAccess: String, LenientEnum, Codable {
    case standard, full

    public static let fallback = DeviceAccess.standard

    public init(from decoder: Decoder) throws {
        self.init(lenient: try? decoder.singleValueContainer().decode(String.self))
    }
}

/// A tool the server logs in to.
public enum LoginTool: String, LenientEnum {
    case github, codex, claude
    case unknown

    public static let fallback = LoginTool.unknown

    public var displayName: String {
        switch self {
        case .github: "GitHub"
        case .codex: "Codex"
        case .claude: "Claude Code"
        case .unknown: "Other"
        }
    }

    /// GitHub and Codex log in with a device code; Claude Code takes a token
    /// from `claude setup-token`.
    public var usesDeviceCode: Bool { self == .github || self == .codex }
}

/// A device paired with a server (`server.devices`, `device.paired`).
public struct PairedDevice: Identifiable, Hashable, Sendable, Decodable {
    public var id: String
    public var name: String
    public var access: DeviceAccess
    /// Unix milliseconds.
    public var paired: Int?
    /// Unix milliseconds; last time the device connected.
    public var seen: Int?
    public var connected: Bool
    /// The device making the request.
    public var current: Bool

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let id = f.string("id"), !id.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("id"), .init(codingPath: decoder.codingPath, debugDescription: "device id"))
        }
        self.id = id
        let name = f.string("name")?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        self.name = name.isEmpty ? "Unnamed device" : name
        access = DeviceAccess(lenient: f.string("access"))
        paired = f.timestamp("paired")
        seen = f.timestamp("seen")
        connected = f.bool("connected") ?? false
        current = f.bool("current") ?? false
    }
}

/// A repository the server keeps a base clone of (`server.repos`).
public struct ServerRepository: Identifiable, Hashable, Sendable, Decodable {
    /// `owner/name`.
    public var name: String
    public var path: String?
    public var cloned: Bool
    /// Unix milliseconds of the last successful fetch.
    public var fetchedAt: Int?
    /// The last clone or fetch error.
    public var error: String?

    public var id: String { name }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let name = f.string("name"), !name.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("name"),
                .init(codingPath: decoder.codingPath, debugDescription: "repository name"))
        }
        self.name = name
        path = f.string("path").flatMap { $0.isEmpty ? nil : $0 }
        cloned = f.bool("cloned") ?? (path != nil)
        fetchedAt = f.timestamp("fetchedAt")
        error = f.string("error").flatMap { $0.isEmpty ? nil : $0 }
    }
}

/// `server.repos`.
public struct ServerRepositories: Hashable, Sendable, Decodable {
    public var repositories: [ServerRepository]
    /// How often the server fetches every repository in the background.
    public var refreshMinutes: Int?

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        repositories = f.list("repositories")
        refreshMinutes = f.int("refreshMinutes")
    }
}

/// A repository the server's GitHub login can clone (`server.repos.available`).
public struct AvailableRepository: Identifiable, Hashable, Sendable, Decodable {
    /// `owner/name`.
    public var name: String
    public var description: String?
    public var isPrivate: Bool?

    public var id: String { name }

    public init(from decoder: Decoder) throws {
        if let name = try? decoder.singleValueContainer().decode(String.self) {
            self.name = name
            return
        }
        let f = try decoder.fields()
        guard let name = f.string("name") ?? f.string("nameWithOwner"), !name.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("name"),
                .init(codingPath: decoder.codingPath, debugDescription: "repository name"))
        }
        self.name = name
        description = f.string("description").flatMap { $0.isEmpty ? nil : $0 }
        isPrivate = f.bool("private") ?? f.bool("isPrivate")
    }
}

/// One tool's login on the server (`server.logins`).
public struct ServerLogin: Identifiable, Hashable, Sendable, Decodable {
    public var tool: LoginTool
    /// The tool name as the server sent it.
    public var toolName: String
    public var loggedIn: Bool
    public var account: String?
    public var detail: String?

    public var id: String { toolName }

    public var displayName: String {
        tool == .unknown ? toolName.capitalized : tool.displayName
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let toolName = f.string("tool"), !toolName.isEmpty else {
            throw DecodingError.keyNotFound(
                AnyKey("tool"), .init(codingPath: decoder.codingPath, debugDescription: "login tool"))
        }
        self.toolName = toolName
        tool = LoginTool(lenient: toolName)
        loggedIn = f.bool("loggedIn") ?? false
        account = f.string("account").flatMap { $0.isEmpty ? nil : $0 }
        detail = f.string("detail").flatMap { $0.isEmpty ? nil : $0 }
    }
}

/// A device-code login that finishes in the background (`server.login.start`).
public struct DeviceLogin: Hashable, Sendable, Decodable {
    /// Where to enter the code.
    public var url: URL?
    public var code: String?
    public var message: String?

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        url = (f.string("url") ?? f.string("verificationUri")).flatMap(URL.init(string:))
        code = (f.string("code") ?? f.string("userCode")).flatMap { $0.isEmpty ? nil : $0 }
        message = f.string("message").flatMap { $0.isEmpty ? nil : $0 }
    }
}
