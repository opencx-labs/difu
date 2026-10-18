import Foundation

/// The contents of the `/remote-control` QR code:
/// `difu://pair?v=1&n=<name>&h=<host>,<host>&p=<port>&f=<fingerprint>&c=<code>`.
public struct PairingLink: Equatable, Sendable {
    public var version: Int
    public var name: String
    public var hosts: [String]
    public var port: Int
    public var fingerprint: String
    public var code: String

    public init(
        version: Int = remoteProtocolVersion, name: String, hosts: [String], port: Int,
        fingerprint: String, code: String
    ) {
        self.version = version
        self.name = name
        self.hosts = hosts
        self.port = port
        self.fingerprint = fingerprint
        self.code = code
    }

    /// Parses a pairing link. Surrounding text is ignored, so a pasted message
    /// that contains the link works too.
    public init(_ text: String) throws {
        guard let start = text.range(of: "difu://", options: .caseInsensitive) else {
            throw RemoteError.invalidPairingLink("it should start with difu://pair")
        }
        let link = text[start.lowerBound...].prefix { !$0.isWhitespace && $0 != "\"" && $0 != "'" }
        let rest = link.dropFirst("difu://".count)
        guard let question = rest.firstIndex(of: "?") else {
            throw RemoteError.invalidPairingLink("it has no parameters")
        }
        let path = rest[..<question].trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard path.lowercased() == "pair" else {
            throw RemoteError.invalidPairingLink("difu://\(path) is not a pairing link")
        }
        let query = Self.parseQuery(rest[rest.index(after: question)...])

        let version = query["v"].flatMap { Int($0) } ?? remoteProtocolVersion
        guard version <= remoteProtocolVersion else {
            throw RemoteError.unsupportedProtocol(version)
        }
        let hosts = (query["h"] ?? "")
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        guard !hosts.isEmpty else { throw RemoteError.invalidPairingLink("no host addresses") }
        guard let port = query["p"].flatMap({ Int($0) }), (1...65535).contains(port) else {
            throw RemoteError.invalidPairingLink("missing or invalid port")
        }
        guard let fingerprint = query["f"], !fingerprint.isEmpty else {
            throw RemoteError.invalidPairingLink("missing certificate fingerprint")
        }
        guard let code = query["c"], !code.isEmpty else {
            throw RemoteError.invalidPairingLink("missing pairing code")
        }
        let name = query["n"]?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        self.init(
            version: version, name: name.isEmpty ? "Computer" : name, hosts: hosts,
            port: port, fingerprint: fingerprint, code: code)
    }

    /// Decodes `application/x-www-form-urlencoded` pairs (`+` is a space).
    static func parseQuery(_ query: Substring) -> [String: String] {
        var result: [String: String] = [:]
        for pair in query.split(separator: "&") {
            let parts = pair.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            guard let rawKey = parts.first else { continue }
            let key = decode(rawKey)
            let value = parts.count > 1 ? decode(parts[1]) : ""
            if result[key] == nil { result[key] = value }
        }
        return result
    }

    private static func decode(_ text: Substring) -> String {
        let spaced = text.replacingOccurrences(of: "+", with: " ")
        return spaced.removingPercentEncoding ?? spaced
    }
}

enum Endpoint {
    /// `wss://host:port/`, bracketing IPv6 literals.
    static func url(host: String, port: Int) -> URL? {
        let bare = host.trimmingCharacters(in: CharacterSet(charactersIn: "[] "))
        guard !bare.isEmpty, (1...65535).contains(port),
            bare.allSatisfy({ $0.isLetter || $0.isNumber || ".-:_%".contains($0) })
        else { return nil }
        let authority = bare.contains(":")
            ? "[\(bare.replacingOccurrences(of: "%", with: "%25"))]"
            : bare
        return URL(string: "wss://\(authority):\(port)/")
    }
}
