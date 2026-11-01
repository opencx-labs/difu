import Foundation

/// An address typed by hand: `host`, `host:port`, `[IPv6]` or `[IPv6]:port`.
/// A bare host uses difu's server port, 3438.
public struct ServerAddress: Codable, Hashable, Sendable {
    /// The port `difu serve` listens on unless configured otherwise.
    public static let defaultPort = 3438

    public var host: String
    public var port: Int

    public init(host: String, port: Int = defaultPort) {
        self.host = host
        self.port = port
    }

    /// Parses an address. A leading scheme (`wss://`), user (`root@`) and any
    /// path are ignored so pasted URLs and SSH targets work too.
    public init(parsing text: String) throws {
        var rest = Substring(text.trimmingCharacters(in: .whitespacesAndNewlines))
        if let scheme = rest.range(of: "://") { rest = rest[scheme.upperBound...] }
        if let end = rest.firstIndex(where: { "/?#".contains($0) }) { rest = rest[..<end] }
        if let at = rest.lastIndex(of: "@") { rest = rest[rest.index(after: at)...] }
        guard !rest.isEmpty else {
            throw RemoteError.invalidAddress("Enter the server's address, like 203.0.113.5 or my-server.example.com.")
        }

        var host: Substring
        var portText: Substring?
        if rest.hasPrefix("[") {
            guard let close = rest.firstIndex(of: "]") else {
                throw RemoteError.invalidAddress("The IPv6 address is missing its closing “]”.")
            }
            host = rest[rest.index(after: rest.startIndex)..<close]
            let after = rest[rest.index(after: close)...]
            if !after.isEmpty {
                guard after.hasPrefix(":") else {
                    throw RemoteError.invalidAddress("Put the port after the IPv6 address, like [2001:db8::1]:3438.")
                }
                portText = after.dropFirst()
            }
        } else {
            switch rest.filter({ $0 == ":" }).count {
            case 0:
                host = rest
            case 1:
                let colon = rest.firstIndex(of: ":")!
                host = rest[..<colon]
                portText = rest[rest.index(after: colon)...]
            default:
                // A bare IPv6 address; a port needs brackets.
                host = rest
            }
        }

        var port = Self.defaultPort
        if let portText {
            guard let value = Int(portText), (1...65535).contains(value) else {
                throw RemoteError.invalidAddress("“\(portText)” isn't a valid port (1–65535).")
            }
            port = value
        }
        let bare = String(host)
        let isIPv6 = bare.contains(":")
        guard !bare.isEmpty, Endpoint.url(host: bare, port: port) != nil,
            !isIPv6 || Self.isIPv6Literal(bare)
        else {
            throw RemoteError.invalidAddress("“\(bare)” isn't a valid host name or IP address.")
        }
        self.init(host: bare, port: port)
    }

    /// Whether `text` is an IPv6 address, optionally with a `%zone`.
    static func isIPv6Literal(_ text: String) -> Bool {
        let address = text.split(separator: "%", maxSplits: 1).first.map(String.init) ?? text
        var storage = in6_addr()
        return inet_pton(AF_INET6, address, &storage) == 1
    }

    public var endpoint: HostPort { HostPort(host: host, port: port) }

    /// `host:port`, with IPv6 hosts in brackets.
    public var display: String {
        host.contains(":") ? "[\(host)]:\(port)" : "\(host):\(port)"
    }
}
