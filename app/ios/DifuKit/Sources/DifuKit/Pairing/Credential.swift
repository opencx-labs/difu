import Foundation

/// Everything needed to reconnect to a paired computer or server.
///
/// Stored as JSON in the Keychain; fields added after the first release are
/// optional so credentials saved by older app versions still decode.
public struct Credential: Codable, Equatable, Sendable, Identifiable {
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
    /// `welcome.server.id` at pairing time (the certificate fingerprint).
    public var serverId: String?
    /// `server` for `difu serve`, `computer` for difu on a Mac; nil when the
    /// computer predates server mode.
    public var mode: ServerMode?
    public var access: DeviceAccess?
    /// The address typed when joining with a password. It is tried with its own
    /// port (which can differ from the advertised one behind port forwarding).
    public var address: ServerAddress?
    public var passwordPairing: Bool?

    public init(
        deviceId: String, token: String, computerName: String, hosts: [String], port: Int,
        fingerprint: String, preferredHost: String? = nil, serverVersion: String? = nil,
        pairedAt: Date = Date(), serverId: String? = nil, mode: ServerMode? = nil,
        access: DeviceAccess? = nil, address: ServerAddress? = nil, passwordPairing: Bool? = nil
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
        self.serverId = serverId
        self.mode = mode
        self.access = access
        self.address = address
        self.passwordPairing = passwordPairing
    }

    /// Saved servers are keyed by the server id, which is the certificate
    /// fingerprint (also for computers paired before servers reported an id).
    public var id: String {
        serverId.flatMap { $0.isEmpty ? nil : $0 } ?? CertificateFingerprint.normalize(fingerprint)
    }

    /// Whether `other` describes the same server (same id or same pinned certificate).
    public func isSameServer(as other: Credential) -> Bool {
        id == other.id || CertificateFingerprint.matches(fingerprint, pinned: other.fingerprint)
    }

    /// Hosts in the order to try: the last one that worked, then the rest.
    public var candidateHosts: [String] {
        candidateEndpoints.map(\.host).uniqued()
    }

    /// Host and port pairs in the order to try: the last host that worked, the
    /// address typed when joining, then the hosts the server advertises.
    public var candidateEndpoints: [HostPort] {
        var endpoints: [HostPort] = []
        if let preferredHost {
            if let address, address.host == preferredHost {
                endpoints.append(address.endpoint)
            } else {
                endpoints.append(HostPort(host: preferredHost, port: port))
            }
        }
        if let address { endpoints.append(address.endpoint) }
        endpoints += hosts.map { HostPort(host: $0, port: port) }
        return endpoints.uniqued()
    }

    /// Refreshes hosts, port, name, version and server details from a `welcome`.
    /// The pinned fingerprint never changes here.
    public func updated(from welcome: Welcome, host: String) -> Credential {
        var next = self
        let server = welcome.server
        if !server.hosts.isEmpty { next.hosts = server.hosts }
        if let port = server.port, (1...65535).contains(port) { next.port = port }
        if !server.name.isEmpty, server.name != "Computer" { next.computerName = server.name }
        next.serverVersion = server.version ?? serverVersion
        next.preferredHost = host
        if let token = welcome.token { next.token = token }
        if let mode = server.mode { next.mode = mode }
        if let access = welcome.access { next.access = access }
        if let passwordPairing = server.passwordPairing { next.passwordPairing = passwordPairing }
        return next
    }

    /// Whether this is a `difu serve` cloud server rather than a computer.
    public var isServer: Bool { mode == .server }
}

/// One address to try.
public struct HostPort: Hashable, Sendable {
    public var host: String
    public var port: Int

    public init(host: String, port: Int) {
        self.host = host
        self.port = port
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

    /// Decodes base64url (padding optional; standard base64 is accepted too).
    public init?(base64URLEncoded text: String) {
        var base64 = text.trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
            .replacingOccurrences(of: "=", with: "")
        switch base64.count % 4 {
        case 1: return nil
        case 2: base64 += "=="
        case 3: base64 += "="
        default: break
        }
        self.init(base64Encoded: base64)
    }
}

extension Array where Element: Hashable {
    /// The elements in order, without repeats.
    func uniqued() -> [Element] {
        var seen = Set<Element>()
        return filter { seen.insert($0).inserted }
    }
}
