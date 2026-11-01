import CryptoKit
import Foundation
import Security

public enum CertificateFingerprint {
    /// Base64url (no padding) SHA-256 of DER bytes.
    public static func of(der: Data) -> String {
        Data(SHA256.hash(data: der)).base64URLEncodedString()
    }

    /// The fingerprint of the leaf certificate in `trust`.
    public static func leaf(of trust: SecTrust) -> String? {
        guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate],
            let leaf = chain.first
        else { return nil }
        return of(der: SecCertificateCopyData(leaf) as Data)
    }

    /// Compares fingerprints, tolerating standard-base64 or padded input.
    public static func matches(_ actual: String, pinned: String) -> Bool {
        normalize(actual) == normalize(pinned)
    }

    static func normalize(_ value: String) -> String {
        value.trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

/// Decides which server certificates to accept. System trust is never
/// consulted (difu's certificates are self-signed).
///
/// * Pinned: accept only the leaf whose fingerprint matches the pin.
/// * First use (`pinned == nil`, password pairing only): accept the presented
///   certificate and record its fingerprint; the password proofs then bind the
///   pairing to it, and that fingerprint becomes the pin.
final class PinningDelegate: NSObject, URLSessionWebSocketDelegate, @unchecked Sendable {
    let pinned: String?
    private let lock = NSLock()
    private var seen: String?
    private var accepted: String?
    private var rejected = false

    init(pinned: String?) {
        self.pinned = pinned
    }

    /// The fingerprint the server presented, if it was rejected.
    var mismatch: String? {
        lock.withLock { rejected ? seen : nil }
    }

    /// The fingerprint of the certificate this connection accepted.
    var presented: String? {
        lock.withLock { accepted }
    }

    func urlSession(
        _ session: URLSession, didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping @Sendable (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        evaluate(challenge, completionHandler)
    }

    func urlSession(
        _ session: URLSession, task: URLSessionTask, didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping @Sendable (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        evaluate(challenge, completionHandler)
    }

    private func evaluate(
        _ challenge: URLAuthenticationChallenge,
        _ completionHandler: @escaping @Sendable (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust
        else {
            completionHandler(.performDefaultHandling, nil)
            return
        }
        guard let trust = challenge.protectionSpace.serverTrust,
            let actual = CertificateFingerprint.leaf(of: trust)
        else {
            completionHandler(.cancelAuthenticationChallenge, nil)
            return
        }
        let accept: Bool = lock.withLock {
            seen = actual
            // On first use, only the first certificate is trusted: a second
            // handshake on the same session must present the same one.
            let expected = pinned ?? accepted
            if let expected, !CertificateFingerprint.matches(actual, pinned: expected) {
                rejected = true
                return false
            }
            accepted = actual
            return true
        }
        if accept {
            completionHandler(.useCredential, URLCredential(trust: trust))
        } else {
            completionHandler(.cancelAuthenticationChallenge, nil)
        }
    }
}
