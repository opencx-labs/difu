import Foundation

/// Why connecting to one host failed.
public struct HostFailure: Sendable, Equatable {
    public var host: String
    public var error: RemoteError

    public init(host: String, error: RemoteError) {
        self.host = host
        self.error = error
    }
}

public enum RemoteError: Error, Equatable, Sendable {
    case invalidPairingLink(String)
    case unsupportedProtocol(Int)
    case invalidHost(String)
    /// The server's certificate isn't the one in the QR code.
    case pinMismatch(host: String, expected: String, actual: String)
    case tls(host: String, message: String)
    case timeout(host: String)
    case unreachable(host: String, message: String)
    /// Every candidate host failed.
    case unreachableComputer(name: String, failures: [HostFailure])
    /// The server refused `pair` / `auth`; the message is user-facing.
    case denied(String)
    /// An RPC returned `{"error": ...}`.
    case server(String)
    case requestTimedOut(method: String)
    case connectionLost(String)
    case notConnected
    case notPaired
    case protocolError(String)
}

extension RemoteError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .invalidPairingLink(let reason):
            "That isn't a difu pairing link: \(reason)"
        case .unsupportedProtocol(let version):
            "This computer uses remote-control protocol v\(version). Update the difu app."
        case .invalidHost(let host):
            "Invalid host name “\(host)”."
        case .pinMismatch(let host, let expected, let actual):
            "\(host) presented a different certificate than the paired computer "
                + "(expected \(expected.prefix(12))…, got \(actual.prefix(12))…). "
                + "If you reset remote control on your computer, pair again."
        case .tls(let host, let message):
            "Secure connection to \(host) failed: \(message)"
        case .timeout(let host):
            "\(host) didn't answer in time."
        case .unreachable(let host, let message):
            "\(host): \(message)"
        case .unreachableComputer(let name, let failures):
            failures.isEmpty
                ? "Can't reach \(name)."
                : "Can't reach \(name):\n"
                    + failures.map { "• \($0.error.localizedDescription)" }.joined(separator: "\n")
        case .denied(let message):
            message
        case .server(let message):
            message
        case .requestTimedOut(let method):
            "The computer took too long to answer (\(method))."
        case .connectionLost(let message):
            message
        case .notConnected:
            "Not connected to your computer."
        case .notPaired:
            "This phone isn't paired with a computer."
        case .protocolError(let message):
            "Unexpected reply from the computer: \(message)"
        }
    }

    /// Whether the failure means the stored pairing is no longer valid.
    public var isDenied: Bool {
        if case .denied = self { return true }
        return false
    }
}
