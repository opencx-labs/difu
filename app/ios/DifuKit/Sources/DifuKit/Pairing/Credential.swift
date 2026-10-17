import Foundation

/// Everything needed to reconnect to a paired computer.
public struct Credential: Codable, Equatable, Sendable {
    /// Stable random id this phone identifies itself with.
    public var deviceId: String
    /// Long-lived device token from the pairing `welcome`.
    public var token: String
    public var computerName: String
    public var hosts: [String]
    public var port: Int
    /// Pinned certificate fingerprint (base64url SHA-256 of the DER).
    public var fingerprint: String
    /// The host that worked last; tried first.
    public var preferredHost: String?
    public var serverVersion: String?
    public var pairedAt: Date

    public init(
        deviceId: String, token: String, computerName: String, hosts: [String], port: Int,
        fingerprint: String, preferredHost: String? = nil, serverVersion: String? = nil,
        pairedAt: Date = Date()
    ) {
        self.deviceId = deviceId
        self.token = token
        self.computerName = computerName
        self.hosts = hosts
        self.port = port
        self.fingerprint = fingerprint
        self.preferredHost = preferredHost
        self.serverVersion = serverVersion
        self.pairedAt = pairedAt
    }

    /// Hosts in the order to try: the last one that worked, then the rest.
    public var candidateHosts: [String] {
        var seen = Set<String>()
        return ([preferredHost].compactMap { $0 } + hosts).filter { seen.insert($0).inserted }
    }

    /// Refreshes hosts, port, name and version from a `welcome`.
    public func updated(from welcome: Welcome, host: String) -> Credential {
        var next = self
        let server = welcome.server
        if !server.hosts.isEmpty { next.hosts = server.hosts }
        if let port = server.port, (1...65535).contains(port) { next.port = port }
        if !server.name.isEmpty, server.name != "Computer" { next.computerName = server.name }
        next.serverVersion = server.version ?? serverVersion
        next.preferredHost = host
        if let token = welcome.token { next.token = token }
        return next
    }
}

public enum DeviceIdentity {
    /// A new random device id: 22 characters of `[A-Za-z0-9_-]`.
    public static func newId() -> String {
        var generator = SystemRandomNumberGenerator()
        let bytes = (0..<16).map { _ in UInt8.random(in: .min ... .max, using: &generator) }
        return Data(bytes).base64URLEncodedString()
    }
}

extension Data {
    /// Base64url without padding.
    public func base64URLEncodedString() -> String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
