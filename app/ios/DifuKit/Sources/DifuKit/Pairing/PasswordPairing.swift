import CommonCrypto
import CryptoKit
import Foundation

/// Password pairing (docs/cloud-agents.md): both sides prove they know the
/// server password with HMACs bound to the certificate the client actually
/// received, so a man in the middle can neither impersonate the server nor
/// complete pairing.
public enum PasswordPairing {
    static let label = "difu-pair-v1"
    /// Work factors outside this range are refused (same bounds as difu).
    public static let iterationRange = 100_000...10_000_000

    /// `difu-pair-v1|<role>|<fingerprint>|<server nonce>|<client nonce>`.
    static func message(
        _ role: Role, fingerprint: String, serverNonce: String, clientNonce: String
    ) -> Data {
        Data([label, role.rawValue, fingerprint, serverNonce, clientNonce].joined(separator: "|").utf8)
    }

    enum Role: String {
        case client, server
    }

    /// Base64url of 32 random bytes, for the `nonce` fields.
    public static func nonce() -> String {
        SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) }.base64URLEncodedString()
    }

    /// `PBKDF2-HMAC-SHA256(password, salt, iterations)`, 32 bytes.
    static func derive(password: String, salt: Data, iterations: Int) throws -> Data {
        var key = Data(count: 32)
        let length = key.count
        let status = key.withUnsafeMutableBytes { output in
            salt.withUnsafeBytes { saltBytes in
                password.withCString { passwordBytes in
                    CCKeyDerivationPBKDF(
                        CCPBKDFAlgorithm(kCCPBKDF2),
                        passwordBytes, password.utf8.count,
                        saltBytes.bindMemory(to: UInt8.self).baseAddress, salt.count,
                        CCPseudoRandomAlgorithm(kCCPRFHmacAlgSHA256),
                        UInt32(iterations),
                        output.bindMemory(to: UInt8.self).baseAddress, length)
                }
            }
        }
        guard status == kCCSuccess else {
            throw RemoteError.protocolError("couldn't derive the password key (\(status))")
        }
        return key
    }
}

/// The key derived from the server password and the server's challenge.
public struct PasswordKey: Sendable {
    private let key: SymmetricKey

    /// Derives the key. Slow on purpose (hundreds of milliseconds); call it off
    /// the main actor.
    public init(password: String, salt: String, iterations: Int) throws {
        guard PasswordPairing.iterationRange.contains(iterations) else {
            throw RemoteError.protocolError("the server asked for an unusual password work factor")
        }
        guard let salt = Data(base64URLEncoded: salt), !salt.isEmpty else {
            throw RemoteError.protocolError("invalid password salt from the server")
        }
        self.init(rawKey: try PasswordPairing.derive(password: password, salt: salt, iterations: iterations))
    }

    init(rawKey: Data) {
        key = SymmetricKey(data: rawKey)
    }

    var rawKey: Data {
        key.withUnsafeBytes { Data($0) }
    }

    /// What the client sends in `proof`.
    public func clientProof(fingerprint: String, serverNonce: String, clientNonce: String) -> String {
        sign(.client, fingerprint: fingerprint, serverNonce: serverNonce, clientNonce: clientNonce)
    }

    /// What a genuine server sends in `welcome.proof`.
    public func serverProof(fingerprint: String, serverNonce: String, clientNonce: String) -> String {
        sign(.server, fingerprint: fingerprint, serverNonce: serverNonce, clientNonce: clientNonce)
    }

    /// Checks `welcome.proof` in constant time.
    public func verifyServerProof(
        _ proof: String, fingerprint: String, serverNonce: String, clientNonce: String
    ) -> Bool {
        guard let tag = Data(base64URLEncoded: proof), !tag.isEmpty else { return false }
        let message = PasswordPairing.message(
            .server, fingerprint: fingerprint, serverNonce: serverNonce, clientNonce: clientNonce)
        return HMAC<SHA256>.isValidAuthenticationCode(tag, authenticating: message, using: key)
    }

    private func sign(
        _ role: PasswordPairing.Role, fingerprint: String, serverNonce: String, clientNonce: String
    ) -> String {
        let message = PasswordPairing.message(
            role, fingerprint: fingerprint, serverNonce: serverNonce, clientNonce: clientNonce)
        return Data(HMAC<SHA256>.authenticationCode(for: message, using: key)).base64URLEncodedString()
    }
}

/// The server's `challenge`.
struct PasswordChallenge: Equatable, Sendable {
    var salt: String
    var iterations: Int
    var nonce: String
}

/// The client side of the exchange, independent of the socket so it can be
/// tested: `hello` → (challenge) → `proof(answering:)` → (welcome) → `finish`.
struct PasswordHandshake: Sendable {
    let deviceId: String
    let deviceName: String
    let access: DeviceAccess
    let password: String
    let clientNonce: String

    private var key: PasswordKey?
    private var challenge: PasswordChallenge?

    init(
        deviceId: String, deviceName: String, access: DeviceAccess, password: String,
        clientNonce: String = PasswordPairing.nonce()
    ) {
        self.deviceId = deviceId
        self.deviceName = deviceName
        self.access = access
        self.password = password
        self.clientNonce = clientNonce
    }

    /// The first frame.
    var hello: Hello {
        .password(deviceId: deviceId, deviceName: deviceName, access: access, nonce: clientNonce)
    }

    /// Answers the server's first reply with the `proof` frame. `fingerprint`
    /// is the certificate this connection actually presented.
    mutating func proof(answering reply: HelloReply, fingerprint: String) throws -> String {
        switch reply.type {
        case "challenge":
            guard let challenge = reply.challenge else {
                throw RemoteError.protocolError("incomplete password challenge")
            }
            let key = try PasswordKey(
                password: password, salt: challenge.salt, iterations: challenge.iterations)
            self.key = key
            self.challenge = challenge
            let proof = key.clientProof(
                fingerprint: fingerprint, serverNonce: challenge.nonce, clientNonce: clientNonce)
            return JSONValue.object(["type": "proof", "proof": .string(proof)]).jsonString()
        case "denied":
            throw RemoteError.denied(
                reply.message ?? "This server doesn't accept password pairing right now.")
        case "welcome":
            // Only a server that checked the password may welcome us.
            throw RemoteError.untrustedServer(
                "The server accepted the connection without checking the password.")
        default:
            throw RemoteError.protocolError("expected a password challenge, got “\(reply.type)”")
        }
    }

    /// Checks the server's final reply and returns its `welcome` only when its
    /// proof is valid for this certificate and these nonces.
    func finish(_ reply: HelloReply, fingerprint: String) throws -> Welcome {
        switch reply.type {
        case "welcome":
            guard let key, let challenge else {
                throw RemoteError.protocolError("welcome before the password challenge")
            }
            guard let welcome = reply.welcome, let proof = welcome.proof,
                key.verifyServerProof(
                    proof, fingerprint: fingerprint, serverNonce: challenge.nonce,
                    clientNonce: clientNonce)
            else {
                throw RemoteError.untrustedServer(
                    "The server couldn't prove it knows the password. Someone may be "
                        + "intercepting the connection.")
            }
            guard welcome.token != nil else {
                throw RemoteError.protocolError("the server didn't send a device token")
            }
            return welcome
        case "denied":
            throw RemoteError.denied(reply.message ?? "Wrong server password.")
        default:
            throw RemoteError.protocolError("expected welcome, got “\(reply.type)”")
        }
    }
}
