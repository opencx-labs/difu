import XCTest

@testable import DifuKit

final class ServerAddressTests: XCTestCase {
    func testParsesHostsPortsAndIPv6() throws {
        let cases: [(String, String, Int)] = [
            ("203.0.113.5", "203.0.113.5", 3438),
            ("  my-server.example.com \n", "my-server.example.com", 3438),
            ("my-server.example.com:8443", "my-server.example.com", 8443),
            ("box.tail1234.ts.net:3438", "box.tail1234.ts.net", 3438),
            ("[2001:db8::1]", "2001:db8::1", 3438),
            ("[2001:db8::1]:9000", "2001:db8::1", 9000),
            ("2001:db8::1", "2001:db8::1", 3438),
            ("wss://203.0.113.5:4000/", "203.0.113.5", 4000),
            ("https://[::1]:5/path?x=1", "::1", 5),
            ("root@203.0.113.5", "203.0.113.5", 3438),
            ("Alis-MacBook-Pro.local", "Alis-MacBook-Pro.local", 3438),
            ("[fe80::1%en0]:3438", "fe80::1%en0", 3438),
        ]
        for (text, host, port) in cases {
            let address = try ServerAddress(parsing: text)
            XCTAssertEqual(address.host, host, text)
            XCTAssertEqual(address.port, port, text)
        }
        XCTAssertEqual(ServerAddress.defaultPort, 3438)
    }

    func testRejectsInvalidAddresses() {
        let bad = [
            "", "   ", "host:", "host:0", "host:70000", "host:abc", "[2001:db8::1",
            "[2001:db8::1]9000", "bad host", "wss://", "host:12:34x",
        ]
        for text in bad {
            XCTAssertThrowsError(try ServerAddress(parsing: text), text) { error in
                guard case .invalidAddress = error as? RemoteError else {
                    return XCTFail("\(text): \(error)")
                }
            }
        }
    }

    func testDisplayAndEndpoint() throws {
        XCTAssertEqual(try ServerAddress(parsing: "[::1]:9").display, "[::1]:9")
        XCTAssertEqual(try ServerAddress(parsing: "example.com").display, "example.com:3438")
        XCTAssertEqual(
            Endpoint.url(host: try ServerAddress(parsing: "2001:db8::1").host, port: 3438)?.absoluteString,
            "wss://[2001:db8::1]:3438/")
    }
}
