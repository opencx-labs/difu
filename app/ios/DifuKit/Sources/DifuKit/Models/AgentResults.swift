import Foundation

/// A changed file (`session.changes`, `pr.diff`).
public struct DiffFile: Identifiable, Hashable, Sendable, Decodable {
    public var path: String
    public var oldPath: String?
    public var status: DiffStatus
    public var additions: Int
    public var deletions: Int
    /// Hunks only, starting at the first `@@`.
    public var patch: String
    public var truncated: Bool

    public var id: String { path }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        path = f.string("path") ?? f.string("filename") ?? ""
        oldPath = f.string("oldPath").flatMap { $0.isEmpty ? nil : $0 }
        status = DiffStatus(lenient: f.string("status"))
        patch = f.string("patch") ?? f.string("diff") ?? ""
        let counted = Patch.stats(patch)
        additions = f.int("additions") ?? counted.additions
        deletions = f.int("deletions") ?? counted.deletions
        truncated = f.bool("truncated") ?? false
    }
}

/// A repository a new session can be started in.
public struct AgentRepository: Identifiable, Hashable, Sendable, Decodable {
    public var path: String
    public var name: String
    public var isDefault: Bool

    public var id: String { path }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        path = f.string("path") ?? ""
        name = f.string("name") ?? URL(fileURLWithPath: path).lastPathComponent
        isDefault = f.bool("default") ?? false
    }
}

/// A model offered for new sessions and `session.model`.
public struct AgentModel: Identifiable, Hashable, Sendable, Decodable {
    public var id: String
    public var name: String
    public var provider: String?
    public var efforts: [String]

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        guard let id = f.string("id") else {
            throw DecodingError.keyNotFound(
                AnyKey("id"), .init(codingPath: decoder.codingPath, debugDescription: "model id"))
        }
        self.id = id
        name = f.string("name") ?? id
        provider = f.string("provider")
        efforts = f.strings("efforts")
    }
}

/// The computer, as described by `welcome.server` and `server.info`.
public struct ServerInfo: Hashable, Sendable, Decodable {
    public var name: String
    public var version: String?
    public var hosts: [String]
    public var port: Int?
    public var fingerprint: String?

    public init(name: String, version: String?, hosts: [String], port: Int?, fingerprint: String?) {
        self.name = name
        self.version = version
        self.hosts = hosts
        self.port = port
        self.fingerprint = fingerprint
    }

    public init(from decoder: Decoder) throws {
        let f = try decoder.fields()
        name = f.string("name") ?? "Computer"
        version = f.string("version")
        hosts = f.strings("hosts")
        port = f.int("port")
        fingerprint = f.string("fingerprint")
    }
}

/// An image sent with `session.send`.
public struct Attachment: Hashable, Sendable {
    public var name: String
    /// Base64-encoded file contents.
    public var data: String

    public init(name: String, bytes: Data) {
        self.name = name
        data = bytes.base64EncodedString()
    }

    var json: JSONValue { ["name": .string(name), "data": .string(data)] }
}
