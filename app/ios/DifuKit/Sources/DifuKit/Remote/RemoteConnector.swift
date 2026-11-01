import Foundation

/// Opens connections to a computer or server, trying each candidate host in order.
public enum RemoteConnector {
    /// Per-host connect + handshake timeout.
    public static let hostTimeout: TimeInterval = 4
    /// Password pairing derives a slow key on both sides, so it gets longer.
    public static let passwordTimeout: TimeInterval = 25

    /// Pairs with the computer in `link`. Returns the live connection and the
    /// credential to store (with the device token from `welcome`).
    public static func pair(
        _ link: PairingLink, deviceId: String, deviceName: String,
        timeout: TimeInterval = hostTimeout,
        progress: (@Sendable (String) -> Void)? = nil
    ) async throws -> (RemoteConnection, Credential) {
        let hello = Hello.pair(
            code: link.code, deviceId: deviceId, deviceName: deviceName, access: .standard)
        let connection = try await firstReachable(
            link.hosts.map { HostPort(host: $0, port: link.port) }, name: link.name,
            fingerprint: link.fingerprint, handshake: .hello(hello), timeout: timeout,
            progress: progress)
        guard let token = connection.welcome.token else {
            connection.close()
            throw RemoteError.protocolError("the computer didn't send a device token")
        }
        let credential = Credential(
            deviceId: connection.welcome.device ?? deviceId, token: token,
            computerName: link.name, hosts: link.hosts, port: link.port,
            fingerprint: link.fingerprint,
            serverId: connection.welcome.server.id ?? link.fingerprint
        ).updated(from: connection.welcome, host: connection.host)
        return (connection, credential)
    }

    /// Pairs with a server using only its address and server password
    /// (`standard` access). The certificate the server presents is pinned once
    /// both password proofs check out.
    public static func join(
        _ address: ServerAddress, password: String, deviceId: String, deviceName: String,
        timeout: TimeInterval = passwordTimeout,
        progress: (@Sendable (String) -> Void)? = nil
    ) async throws -> (RemoteConnection, Credential) {
        let exchange = PasswordHandshake(
            deviceId: deviceId, deviceName: deviceName, access: .standard, password: password)
        let connection = try await firstReachable(
            [address.endpoint], name: address.display, fingerprint: nil,
            handshake: .password(exchange), timeout: timeout, progress: progress)
        let welcome = connection.welcome
        guard let token = welcome.token else {
            connection.close()
            throw RemoteError.protocolError("the server didn't send a device token")
        }
        let credential = Credential(
            deviceId: welcome.device ?? deviceId, token: token, computerName: address.host,
            hosts: [], port: address.port, fingerprint: connection.fingerprint,
            serverId: welcome.server.id ?? connection.fingerprint, address: address
        ).updated(from: welcome, host: connection.host)
        return (connection, credential)
    }

    /// Reconnects with a stored credential. Returns the connection and the
    /// credential refreshed from `welcome` (hosts, port, preferred host).
    public static func connect(
        _ credential: Credential, timeout: TimeInterval = hostTimeout,
        progress: (@Sendable (String) -> Void)? = nil
    ) async throws -> (RemoteConnection, Credential) {
        let connection = try await firstReachable(
            credential.candidateEndpoints, name: credential.computerName,
            fingerprint: credential.fingerprint, handshake: .hello(.auth(token: credential.token)),
            timeout: timeout, progress: progress)
        return (connection, credential.updated(from: connection.welcome, host: connection.host))
    }

    private static func firstReachable(
        _ endpoints: [HostPort], name: String, fingerprint: String?,
        handshake: RemoteConnection.Handshake, timeout: TimeInterval,
        progress: (@Sendable (String) -> Void)?
    ) async throws -> RemoteConnection {
        var failures: [HostFailure] = []
        for endpoint in endpoints {
            try Task.checkCancellation()
            progress?(endpoint.host)
            do {
                return try await RemoteConnection.open(
                    host: endpoint.host, port: endpoint.port, fingerprint: fingerprint,
                    handshake: handshake, timeout: timeout)
            } catch let error as RemoteError {
                switch error {
                case .denied, .unsupportedProtocol, .untrustedServer:
                    // The computer answered: other hosts would say the same.
                    throw error
                default:
                    failures.append(HostFailure(host: endpoint.host, error: error))
                }
            }
            try Task.checkCancellation()
        }
        throw RemoteError.unreachableComputer(name: name, failures: failures)
    }
}
