import Foundation

/// Opens connections to a computer, trying each candidate host in order.
public enum RemoteConnector {
    /// Per-host connect + handshake timeout.
    public static let hostTimeout: TimeInterval = 4

    /// Pairs with the computer in `link`. Returns the live connection and the
    /// credential to store (with the device token from `welcome`).
    public static func pair(
        _ link: PairingLink, deviceId: String, deviceName: String,
        timeout: TimeInterval = hostTimeout,
        progress: (@Sendable (String) -> Void)? = nil
    ) async throws -> (RemoteConnection, Credential) {
        let hello = Hello.pair(code: link.code, deviceId: deviceId, deviceName: deviceName)
        let connection = try await firstReachable(
            link.hosts, name: link.name, port: link.port, fingerprint: link.fingerprint,
            hello: hello, timeout: timeout, progress: progress)
        guard let token = connection.welcome.token else {
            connection.close()
            throw RemoteError.protocolError("the computer didn't send a device token")
        }
        let credential = Credential(
            deviceId: connection.welcome.device ?? deviceId, token: token,
            computerName: link.name, hosts: link.hosts, port: link.port,
            fingerprint: link.fingerprint
        ).updated(from: connection.welcome, host: connection.host)
        return (connection, credential)
    }

    /// Reconnects with a stored credential. Returns the connection and the
    /// credential refreshed from `welcome` (hosts, port, preferred host).
    public static func connect(
        _ credential: Credential, timeout: TimeInterval = hostTimeout,
        progress: (@Sendable (String) -> Void)? = nil
    ) async throws -> (RemoteConnection, Credential) {
        let connection = try await firstReachable(
            credential.candidateHosts, name: credential.computerName, port: credential.port,
            fingerprint: credential.fingerprint, hello: .auth(token: credential.token),
            timeout: timeout, progress: progress)
        return (connection, credential.updated(from: connection.welcome, host: connection.host))
    }

    private static func firstReachable(
        _ hosts: [String], name: String, port: Int, fingerprint: String, hello: Hello,
        timeout: TimeInterval, progress: (@Sendable (String) -> Void)?
    ) async throws -> RemoteConnection {
        var failures: [HostFailure] = []
        for host in hosts {
            try Task.checkCancellation()
            progress?(host)
            do {
                return try await RemoteConnection.open(
                    host: host, port: port, fingerprint: fingerprint, hello: hello,
                    timeout: timeout)
            } catch let error as RemoteError {
                switch error {
                case .denied, .unsupportedProtocol:
                    // The computer answered: other hosts would say the same.
                    throw error
                default:
                    failures.append(HostFailure(host: host, error: error))
                }
            }
            try Task.checkCancellation()
        }
        throw RemoteError.unreachableComputer(name: name, failures: failures)
    }
}
