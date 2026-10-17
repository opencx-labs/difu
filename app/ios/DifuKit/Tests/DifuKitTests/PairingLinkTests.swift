import XCTest

@testable import DifuKit

final class PairingLinkTests: XCTestCase {
    let fingerprint = "0AZiLUm8qDpULKH_l6XWgdl1XcBvQB74qO2n4QEdUV8"

    func testParsesSpecExample() throws {
        let link = try PairingLink(
            "difu://pair?v=1&n=Ali%27s+MacBook+Pro&h=Alis-MacBook-Pro.local,192.168.1.20&p=51234&f=\(fingerprint)&c=abc123"
        )
        XCTAssertEqual(link.version, 1)
        XCTAssertEqual(link.name, "Ali's MacBook Pro")
        XCTAssertEqual(link.hosts, ["Alis-MacBook-Pro.local", "192.168.1.20"])
        XCTAssertEqual(link.port, 51234)
        XCTAssertEqual(link.fingerprint, fingerprint)
        XCTAssertEqual(link.code, "abc123")
    }

    func testPlusAndPercentTwentyAreSpaces() throws {
        let plus = try PairingLink("difu://pair?n=Ali+Mac+Pro&h=a.local&p=1&f=x&c=y")
        let percent = try PairingLink("difu://pair?n=Ali%20Mac%20Pro&h=a.local&p=1&f=x&c=y")
        XCTAssertEqual(plus.name, "Ali Mac Pro")
        XCTAssertEqual(percent.name, "Ali Mac Pro")
        // A literal plus is percent-encoded.
        let literal = try PairingLink("difu://pair?n=C%2B%2B+Box&h=a.local&p=1&f=x&c=y")
        XCTAssertEqual(literal.name, "C++ Box")
    }

    func testEncodedHostListAndUnicodeName() throws {
        let link = try PairingLink(
            "difu://pair?v=1&n=%E2%9C%A8+Studio&h=studio.local%2C10.0.0.5%2C+10.0.0.6&p=443&f=x&c=y")
        XCTAssertEqual(link.name, "✨ Studio")
        XCTAssertEqual(link.hosts, ["studio.local", "10.0.0.5", "10.0.0.6"])
    }

    func testFindsLinkInsideSurroundingText() throws {
        let link = try PairingLink("Scan this: difu://pair?n=Mac&h=m.local&p=9&f=x&c=y  thanks")
        XCTAssertEqual(link.code, "y")
        XCTAssertEqual(link.hosts, ["m.local"])
    }

    func testCaseInsensitiveScheme() throws {
        XCTAssertEqual(try PairingLink("DIFU://pair?h=a&p=1&f=x&c=y").hosts, ["a"])
    }

    func testMissingNameFallsBack() throws {
        XCTAssertEqual(try PairingLink("difu://pair?h=a&p=1&f=x&c=y").name, "Computer")
    }

    func testRejectsInvalidLinks() {
        let bad = [
            "https://example.com",
            "difu://open?h=a&p=1&f=x&c=y",
            "difu://pair",
            "difu://pair?p=1&f=x&c=y",  // no hosts
            "difu://pair?h=a&p=0&f=x&c=y",  // bad port
            "difu://pair?h=a&p=70000&f=x&c=y",
            "difu://pair?h=a&p=1&c=y",  // no fingerprint
            "difu://pair?h=a&p=1&f=x",  // no code
        ]
        for text in bad {
            XCTAssertThrowsError(try PairingLink(text), text)
        }
    }

    func testRejectsNewerProtocol() {
        XCTAssertThrowsError(try PairingLink("difu://pair?v=2&h=a&p=1&f=x&c=y")) { error in
            XCTAssertEqual(error as? RemoteError, .unsupportedProtocol(2))
        }
    }

    func testEndpointURLs() {
        XCTAssertEqual(Endpoint.url(host: "Alis-MacBook-Pro.local", port: 51234)?.absoluteString,
                       "wss://Alis-MacBook-Pro.local:51234/")
        XCTAssertEqual(Endpoint.url(host: "192.168.1.20", port: 1)?.absoluteString, "wss://192.168.1.20:1/")
        XCTAssertEqual(Endpoint.url(host: "fe80::1", port: 8)?.absoluteString, "wss://[fe80::1]:8/")
        XCTAssertNil(Endpoint.url(host: "bad host/../", port: 8))
        XCTAssertNil(Endpoint.url(host: "a.local", port: 0))
    }

    func testCredentialHostOrderAndWelcomeRefresh() {
        var credential = Credential(
            deviceId: "dev", token: "t", computerName: "Mac", hosts: ["a.local", "10.0.0.1"],
            port: 1, fingerprint: "f")
        XCTAssertEqual(credential.candidateHosts, ["a.local", "10.0.0.1"])
        credential.preferredHost = "10.0.0.1"
        XCTAssertEqual(credential.candidateHosts, ["10.0.0.1", "a.local"])

        let welcome = Welcome(
            protocolVersion: 1, device: "dev", token: nil,
            server: ServerInfo(
                name: "Ali's Mac", version: "0.20.0", hosts: ["b.local", "10.0.0.9"], port: 2,
                fingerprint: "other"))
        let updated = credential.updated(from: welcome, host: "10.0.0.9")
        XCTAssertEqual(updated.hosts, ["b.local", "10.0.0.9"])
        XCTAssertEqual(updated.port, 2)
        XCTAssertEqual(updated.computerName, "Ali's Mac")
        XCTAssertEqual(updated.preferredHost, "10.0.0.9")
        XCTAssertEqual(updated.token, "t")
        XCTAssertEqual(updated.fingerprint, "f", "the pin only ever comes from the QR code")
    }

    func testDeviceIdShape() {
        let id = DeviceIdentity.newId()
        XCTAssertTrue((8...64).contains(id.count))
        XCTAssertTrue(id.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") })
        XCTAssertNotEqual(id, DeviceIdentity.newId())
    }

    func testFingerprint() {
        XCTAssertEqual(
            CertificateFingerprint.of(der: Data("abc".utf8)),
            "ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0")
        XCTAssertTrue(CertificateFingerprint.matches(
            "ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0",
            pinned: "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0="))
        XCTAssertFalse(CertificateFingerprint.matches("abc", pinned: "abd"))
    }

    func testHelloFrames() throws {
        let pair = try JSONDecoder().decode(
            JSONValue.self,
            from: Data(Hello.pair(code: "c", deviceId: "d", deviceName: "Ali's iPhone").json.utf8))
        XCTAssertEqual(pair, ["type": "pair", "code": "c", "device": ["id": "d", "name": "Ali's iPhone"]])
        let auth = try JSONDecoder().decode(JSONValue.self, from: Data(Hello.auth(token: "t").json.utf8))
        XCTAssertEqual(auth, ["type": "auth", "token": "t"])
    }

    func testStores() throws {
        let credential = Credential(
            deviceId: "dev", token: "t", computerName: "Mac", hosts: ["a"], port: 1, fingerprint: "f",
            pairedAt: Date(timeIntervalSince1970: 1_700_000_000))
        let memory = InMemoryCredentialStore()
        XCTAssertNil(try memory.load())
        try memory.save(credential)
        XCTAssertEqual(try memory.load(), credential)
        try memory.delete()
        XCTAssertNil(try memory.load())

        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("difukit-\(UUID().uuidString).json")
        let file = FileCredentialStore(url: url)
        try file.save(credential)
        XCTAssertEqual(try file.load(), credential)
        try file.delete()
        XCTAssertNil(try file.load())
    }
}
