import XCTest

@testable import DifuKit

final class ServerStoreTests: XCTestCase {
    /// A credential exactly as the single-computer app version stored it
    /// (default JSONEncoder: dates as seconds since 2001).
    let legacyJSON = """
        {"deviceId":"dev-1","token":"secret-token","computerName":"Ali's MacBook Pro",
         "hosts":["Alis-MacBook-Pro.local","192.168.1.20"],"port":51234,
         "fingerprint":"0AZiLUm8qDpULKH_l6XWgdl1XcBvQB74qO2n4QEdUV8",
         "preferredHost":"192.168.1.20","serverVersion":"0.20.0","pairedAt":783000000}
        """

    func credential(_ fingerprint: String, name: String = "Mac") -> Credential {
        Credential(
            deviceId: "dev", token: "t-\(fingerprint)", computerName: name, hosts: ["a.local"],
            port: 1, fingerprint: fingerprint, pairedAt: Date(timeIntervalSince1970: 1_700_000_000))
    }

    func testMigratesTheSinglePairedComputer() throws {
        let slot = MemorySlot()
        let legacy = MemorySlot(Data(legacyJSON.utf8))
        let store = SavedServerStore(slot: slot, legacy: legacy)

        let servers = try store.load()
        XCTAssertEqual(servers.count, 1)
        let migrated = try XCTUnwrap(servers.first)
        XCTAssertEqual(migrated.token, "secret-token")
        XCTAssertEqual(migrated.computerName, "Ali's MacBook Pro")
        XCTAssertEqual(migrated.candidateHosts, ["192.168.1.20", "Alis-MacBook-Pro.local"])
        XCTAssertEqual(migrated.port, 51234)
        XCTAssertEqual(migrated.pairedAt, Date(timeIntervalSinceReferenceDate: 783_000_000))
        XCTAssertEqual(migrated.id, "0AZiLUm8qDpULKH_l6XWgdl1XcBvQB74qO2n4QEdUV8")
        XCTAssertNil(migrated.mode)
        XCTAssertNil(migrated.address)

        // The legacy item is gone and the list is stored in its place.
        XCTAssertNil(try legacy.read())
        XCTAssertNotNil(try slot.read())
        XCTAssertEqual(try SavedServerStore(slot: slot, legacy: legacy).load(), servers)
    }

    func testNothingToMigrate() throws {
        let slot = MemorySlot()
        XCTAssertEqual(try SavedServerStore(slot: slot, legacy: MemorySlot()).load(), [])
        XCTAssertNil(try slot.read(), "nothing is written until a server is saved")
    }

    func testUnreadableLegacyItemIsLeftAlone() throws {
        let legacy = MemorySlot(Data("{not json".utf8))
        XCTAssertEqual(try SavedServerStore(slot: MemorySlot(), legacy: legacy).load(), [])
        XCTAssertNotNil(try legacy.read())
    }

    func testSavedListWinsOverLegacyItem() throws {
        let slot = MemorySlot()
        let store = SavedServerStore(slot: slot, legacy: MemorySlot(Data(legacyJSON.utf8)))
        try store.save([])
        XCTAssertEqual(try store.load(), [], "forgetting every server must not resurrect the old one")
    }

    func testSavesInOrderOnePerServerId() throws {
        let store = SavedServerStore(slot: MemorySlot())
        var server = credential("fp-b", name: "Server")
        server.address = ServerAddress(host: "203.0.113.5", port: 443)
        server.mode = .server
        server.access = .standard
        server.serverId = "fp-b"
        let list = [credential("fp-a"), server, credential("fp-a", name: "Duplicate")]
        try store.save(list)
        let loaded = try store.load()
        XCTAssertEqual(loaded.map(\.id), ["fp-a", "fp-b"])
        XCTAssertEqual(loaded.first?.computerName, "Mac")
        XCTAssertEqual(loaded.last, server)
    }

    func testDropsUndecodableEntries() throws {
        let good = String(decoding: try JSONEncoder().encode(credential("fp-a")), as: UTF8.self)
        let slot = MemorySlot(Data("[{\"token\": 1}, \(good)]".utf8))
        XCTAssertEqual(try SavedServerStore(slot: slot).load().map(\.id), ["fp-a"])
    }

    func testUnknownModeAndAccessStillDecode() throws {
        var stored = try JSONSerialization.jsonObject(
            with: JSONEncoder().encode(credential("fp-a"))) as? [String: Any] ?? [:]
        stored["mode"] = "spaceship"
        stored["access"] = "root"
        let data = try JSONSerialization.data(withJSONObject: stored)
        let decoded = try JSONDecoder().decode(Credential.self, from: data)
        XCTAssertEqual(decoded.mode, .computer)
        XCTAssertEqual(decoded.access, .standard)
    }

    // MARK: Credential

    func testServerIdAndSameServer() {
        var a = credential("abc+/=")
        XCTAssertEqual(a.id, "abc-_")
        let b = credential("abc-_")
        XCTAssertTrue(a.isSameServer(as: b))
        a.serverId = "server-id"
        XCTAssertEqual(a.id, "server-id")
        XCTAssertTrue(a.isSameServer(as: b), "the pinned certificate still matches")
        XCTAssertFalse(a.isSameServer(as: credential("other")))
    }

    func testTypedAddressKeepsItsOwnPort() {
        var server = credential("fp")
        server.hosts = ["10.0.0.4", "203.0.113.5"]
        server.port = 3438
        server.address = ServerAddress(host: "home.example.com", port: 443)
        XCTAssertEqual(
            server.candidateEndpoints,
            [
                HostPort(host: "home.example.com", port: 443), HostPort(host: "10.0.0.4", port: 3438),
                HostPort(host: "203.0.113.5", port: 3438),
            ])
        server.preferredHost = "203.0.113.5"
        XCTAssertEqual(server.candidateEndpoints.first, HostPort(host: "203.0.113.5", port: 3438))
        XCTAssertEqual(server.candidateEndpoints.count, 3)
        server.preferredHost = "home.example.com"
        XCTAssertEqual(server.candidateEndpoints.first, HostPort(host: "home.example.com", port: 443))
        XCTAssertEqual(server.candidateHosts, ["home.example.com", "10.0.0.4", "203.0.113.5"])
    }

    func testWelcomeRefreshesServerDetails() {
        let welcome = Welcome(
            protocolVersion: 1, device: "dev", token: nil,
            server: ServerInfo(
                name: "hetzner-1", version: "0.35.0", hosts: ["203.0.113.5"], port: 3438,
                fingerprint: "other", id: "other", mode: .server, passwordPairing: true),
            access: .standard)
        let updated = credential("fp").updated(from: welcome, host: "203.0.113.5")
        XCTAssertEqual(updated.mode, .server)
        XCTAssertTrue(updated.isServer)
        XCTAssertEqual(updated.access, .standard)
        XCTAssertEqual(updated.passwordPairing, true)
        XCTAssertEqual(updated.computerName, "hetzner-1")
        XCTAssertEqual(updated.fingerprint, "fp")
        XCTAssertEqual(updated.id, "fp", "the saved id doesn't change on reconnect")
    }
}
