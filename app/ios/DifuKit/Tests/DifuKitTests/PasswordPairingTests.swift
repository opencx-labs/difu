import XCTest

@testable import DifuKit

/// The vector shared with difu's Rust implementation (cli/src/cloud/password.rs).
private enum Vector {
    static let password = "difu test password"
    static let salt = "MDEyMzQ1Njc4OWFiY2RlZg"  // base64url("0123456789abcdef")
    static let iterations = 100_000
    static let key = "ZuRZLJbLiD4kaB4n6BBY2rNnWNmwex3mIuH_9tp147Q"
    static let fingerprint = "fingerprint"
    static let serverNonce = "server-nonce"
    static let clientNonce = "client-nonce"
    static let clientProof = "94cIXRPlcWBtrQv3mKJ3qtvZsXrjN6GvnJjZkYvpw2w"
    static let serverProof = "WfFgJvmNYXPD06hqmuyAXT2AHIAgLPA5rLTdVlDFuto"

    static var challenge: String {
        #"{"type":"challenge","salt":"\#(salt)","iterations":\#(iterations),"nonce":"\#(serverNonce)"}"#
    }

    static func welcome(proof: String?, token: String? = "device-token") -> String {
        var fields = [
            #""type":"welcome""#, #""protocol":1"#, #""device":"dev""#, #""access":"standard""#,
            #""server":{"name":"hetzner-1","version":"0.35.0","hosts":["203.0.113.5"],"port":3438,"id":"fingerprint","mode":"server","passwordPairing":true}"#,
        ]
        if let token { fields.append(#""token":"\#(token)""#) }
        if let proof { fields.append(#""proof":"\#(proof)""#) }
        return "{" + fields.joined(separator: ",") + "}"
    }
}

private func reply(_ json: String) throws -> HelloReply {
    try HelloReply(Data(json.utf8))
}

private func handshake(password: String = Vector.password) -> PasswordHandshake {
    PasswordHandshake(
        deviceId: "dev", deviceName: "Ali's iPhone", access: .standard, password: password,
        clientNonce: Vector.clientNonce)
}

final class PasswordPairingTests: XCTestCase {
    func testKnownVectorMatchesRust() throws {
        let key = try PasswordKey(
            password: Vector.password, salt: Vector.salt, iterations: Vector.iterations)
        XCTAssertEqual(key.rawKey.base64URLEncodedString(), Vector.key)
        XCTAssertEqual(
            key.clientProof(
                fingerprint: Vector.fingerprint, serverNonce: Vector.serverNonce,
                clientNonce: Vector.clientNonce),
            Vector.clientProof)
        XCTAssertEqual(
            key.serverProof(
                fingerprint: Vector.fingerprint, serverNonce: Vector.serverNonce,
                clientNonce: Vector.clientNonce),
            Vector.serverProof)
        XCTAssertTrue(
            key.verifyServerProof(
                Vector.serverProof, fingerprint: Vector.fingerprint,
                serverNonce: Vector.serverNonce, clientNonce: Vector.clientNonce))
    }

    func testServerProofRejectsAnythingElse() throws {
        let key = PasswordKey(rawKey: try XCTUnwrap(Data(base64URLEncoded: Vector.key)))
        func verify(
            _ proof: String, fingerprint: String = Vector.fingerprint,
            serverNonce: String = Vector.serverNonce, clientNonce: String = Vector.clientNonce
        ) -> Bool {
            key.verifyServerProof(
                proof, fingerprint: fingerprint, serverNonce: serverNonce, clientNonce: clientNonce)
        }
        XCTAssertTrue(verify(Vector.serverProof))
        // A man in the middle presented its own certificate.
        XCTAssertFalse(verify(Vector.serverProof, fingerprint: "attacker-fingerprint"))
        // Replayed from another exchange.
        XCTAssertFalse(verify(Vector.serverProof, serverNonce: "other-server-nonce"))
        XCTAssertFalse(verify(Vector.serverProof, clientNonce: "other-client-nonce"))
        // The client's own proof reflected back.
        XCTAssertFalse(verify(Vector.clientProof))
        XCTAssertFalse(verify(""))
        XCTAssertFalse(verify("not base64!"))
        XCTAssertFalse(verify(String(Vector.serverProof.dropLast(4))))

        let wrong = try PasswordKey(
            password: Vector.password + "!", salt: Vector.salt, iterations: Vector.iterations)
        XCTAssertFalse(
            wrong.verifyServerProof(
                Vector.serverProof, fingerprint: Vector.fingerprint,
                serverNonce: Vector.serverNonce, clientNonce: Vector.clientNonce))
    }

    func testRefusesOddChallenges() {
        XCTAssertThrowsError(try PasswordKey(password: "p", salt: Vector.salt, iterations: 10))
        XCTAssertThrowsError(try PasswordKey(password: "p", salt: Vector.salt, iterations: 20_000_000))
        XCTAssertThrowsError(try PasswordKey(password: "p", salt: "", iterations: 100_000))
        XCTAssertThrowsError(try PasswordKey(password: "p", salt: "a", iterations: 100_000))
    }

    func testNoncesAreRandom32Bytes() throws {
        let nonce = PasswordPairing.nonce()
        XCTAssertEqual(try XCTUnwrap(Data(base64URLEncoded: nonce)).count, 32)
        XCTAssertFalse(nonce.contains("="))
        XCTAssertNotEqual(nonce, PasswordPairing.nonce())
    }

    func testBase64URLDecoding() {
        XCTAssertEqual(Data(base64URLEncoded: "MDEyMzQ1Njc4OWFiY2RlZg"), Data("0123456789abcdef".utf8))
        XCTAssertEqual(Data(base64URLEncoded: "-_8"), Data([0xfb, 0xff]))
        XCTAssertEqual(Data(base64URLEncoded: "+/8="), Data([0xfb, 0xff]))
        XCTAssertNil(Data(base64URLEncoded: "a"))
    }

    // MARK: Handshake

    func testHandshakeWithGenuineServer() throws {
        var exchange = handshake()
        let hello = try decode(JSONValue.self, exchange.hello.json)
        XCTAssertEqual(
            hello,
            [
                "type": "password", "device": ["id": "dev", "name": "Ali's iPhone"],
                "access": "standard", "nonce": .string(Vector.clientNonce),
            ])

        let proof = try exchange.proof(answering: reply(Vector.challenge), fingerprint: Vector.fingerprint)
        XCTAssertEqual(
            try decode(JSONValue.self, proof), ["type": "proof", "proof": .string(Vector.clientProof)])

        let welcome = try exchange.finish(
            reply(Vector.welcome(proof: Vector.serverProof)), fingerprint: Vector.fingerprint)
        XCTAssertEqual(welcome.token, "device-token")
        XCTAssertEqual(welcome.access, .standard)
        XCTAssertEqual(welcome.server.id, "fingerprint")
        XCTAssertEqual(welcome.server.mode, .server)
    }

    func testHandshakeRejectsServerWithoutValidProof() throws {
        let cases: [(String, String)] = [
            ("no proof", Vector.welcome(proof: nil)),
            ("garbage proof", Vector.welcome(proof: "AAAA")),
            ("client proof reflected", Vector.welcome(proof: Vector.clientProof)),
        ]
        for (label, welcome) in cases {
            var exchange = handshake()
            _ = try exchange.proof(answering: reply(Vector.challenge), fingerprint: Vector.fingerprint)
            XCTAssertThrowsError(
                try exchange.finish(reply(welcome), fingerprint: Vector.fingerprint), label
            ) { error in
                guard case .untrustedServer = error as? RemoteError else {
                    return XCTFail("\(label): \(error)")
                }
            }
        }
    }

    func testHandshakeDetectsSubstitutedCertificate() throws {
        // The client saw the attacker's certificate; the real server proved its own.
        var exchange = handshake()
        _ = try exchange.proof(answering: reply(Vector.challenge), fingerprint: "attacker-fingerprint")
        XCTAssertThrowsError(
            try exchange.finish(
                reply(Vector.welcome(proof: Vector.serverProof)), fingerprint: "attacker-fingerprint")
        ) { error in
            guard case .untrustedServer = error as? RemoteError else { return XCTFail("\(error)") }
        }
    }

    func testHandshakeRejectsWelcomeWithoutChallenge() throws {
        var exchange = handshake()
        XCTAssertThrowsError(
            try exchange.proof(
                answering: reply(Vector.welcome(proof: Vector.serverProof)),
                fingerprint: Vector.fingerprint)
        ) { error in
            guard case .untrustedServer = error as? RemoteError else { return XCTFail("\(error)") }
        }
        XCTAssertThrowsError(
            try handshake().finish(
                reply(Vector.welcome(proof: Vector.serverProof)), fingerprint: Vector.fingerprint))
    }

    func testHandshakeDeniedAndMalformedReplies() throws {
        var exchange = handshake()
        XCTAssertThrowsError(
            try exchange.proof(
                answering: reply(#"{"type":"denied","message":"Password pairing is off."}"#),
                fingerprint: Vector.fingerprint)
        ) { error in
            XCTAssertEqual(error as? RemoteError, .denied("Password pairing is off."))
        }

        _ = try exchange.proof(answering: reply(Vector.challenge), fingerprint: Vector.fingerprint)
        XCTAssertThrowsError(
            try exchange.finish(reply(#"{"type":"denied"}"#), fingerprint: Vector.fingerprint)
        ) { error in
            XCTAssertEqual(error as? RemoteError, .denied("Wrong server password."))
        }
        XCTAssertThrowsError(
            try exchange.finish(
                reply(Vector.welcome(proof: Vector.serverProof, token: nil)),
                fingerprint: Vector.fingerprint))

        var weak = handshake()
        XCTAssertThrowsError(
            try weak.proof(
                answering: reply(#"{"type":"challenge","salt":"MDEy","iterations":1000,"nonce":"n"}"#),
                fingerprint: Vector.fingerprint))
        XCTAssertThrowsError(
            try weak.proof(answering: reply(#"{"type":"challenge"}"#), fingerprint: Vector.fingerprint))
        XCTAssertThrowsError(try HelloReply(Data("not json".utf8)))
    }
}
