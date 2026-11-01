import DifuKit
import Observation
import SwiftUI
import UIKit

/// A session on one of the saved servers.
struct SessionRef: Hashable {
    let server: String
    let session: String
}

/// A row of the combined agents list.
struct ServerSession: Identifiable {
    let server: ServerConnection
    let summary: SessionSummary

    var id: SessionRef { SessionRef(server: server.id, session: summary.id) }
}

/// "New device paired" banner contents.
struct PairedDeviceNotice: Identifiable, Equatable {
    let id = UUID()
    let serverID: String
    let serverName: String
    let device: PairedDevice
}

/// Root app state: the saved computers and servers (one connection each, all
/// connected while the app is active), pairing, the app lock and banners.
@MainActor
@Observable
final class AppModel {
    enum PairingState: Equatable {
        case idle
        case pairing(String)
        case failed(String)
    }

    private(set) var servers: [ServerConnection] = []
    var pairing: PairingState = .idle
    /// Shown on the welcome screen or in an alert, e.g. after a server revoked this phone.
    var notice: String?
    /// A `difu://pair` link opened while already paired, awaiting confirmation.
    var pendingLink: PairingLink?
    /// `device.paired` events not dismissed yet, oldest first.
    private(set) var pairedDevices: [PairedDeviceNotice] = []
    /// The server the Reviews tab uses (nil: the first one).
    var reviewsServerID: String?

    let lock: AppLock

    @ObservationIgnored private let store: ServerStore
    @ObservationIgnored private var isActive = false

    init(store: ServerStore = SavedServerStore.keychain()) {
        self.store = store
        let saved = (try? store.load()) ?? []
        lock = AppLock(hasServers: !saved.isEmpty)
        servers = saved.map(makeConnection)
    }

    var isPaired: Bool { !servers.isEmpty }

    /// Server badges are only useful with more than one server.
    var showsServerBadges: Bool { servers.count > 1 }

    var isAnyConnected: Bool { servers.contains(where: \.isConnected) }

    func server(_ id: String?) -> ServerConnection? {
        guard let id else { return nil }
        return servers.first { $0.id == id }
    }

    /// "Ali's MacBook Pro", or "your servers" when there are several.
    var serversLabel: String {
        servers.count == 1 ? servers[0].name : "your servers"
    }

    var reviewsServer: ServerConnection? {
        server(reviewsServerID) ?? servers.first
    }

    // MARK: Sessions

    /// Every server's sessions in one list. With one server this is the
    /// server's own order; with several, pinned rows (and orchestrators) come
    /// first, then the rest by last update.
    var sessions: [ServerSession] {
        let rows = servers.flatMap { server in
            server.sessions.summaries.map { ServerSession(server: server, summary: $0) }
        }
        guard servers.count > 1 else { return rows }
        let top = rows.filter { $0.summary.orchestrator || $0.summary.pinned }
        let rest = rows.filter { !$0.summary.orchestrator && !$0.summary.pinned }
            .enumerated()
            .sorted { a, b in
                let (left, right) = (a.element.summary.updated ?? 0, b.element.summary.updated ?? 0)
                return left != right ? left > right : a.offset < b.offset
            }
            .map(\.element)
        return top + rest
    }

    var sessionsLoaded: Bool { servers.contains { $0.sessions.loaded } }

    var sessionsError: String? {
        servers.lazy.compactMap(\.sessions.error).first
    }

    func refreshSessions() async {
        await withTaskGroup(of: Void.self) { group in
            for server in servers where server.isConnected {
                group.addTask { await server.sessions.refresh() }
            }
        }
    }

    // MARK: Lifecycle

    /// Connects to every saved server (scene became active).
    func activate() {
        isActive = true
        servers.forEach { $0.start() }
    }

    /// Disconnects everything (scene went to the background).
    func deactivate() {
        isActive = false
        servers.forEach { $0.stop() }
    }

    /// Skips every server's backoff delay.
    func retryAll() {
        servers.forEach { $0.retryNow() }
    }

    private func makeConnection(_ credential: Credential) -> ServerConnection {
        let server = ServerConnection(credential: credential)
        server.onCredentialChange = { [weak self] _ in self?.persist() }
        server.onRevoked = { [weak self] server, message in self?.revoked(server, message: message) }
        server.onEvent = { [weak self] server, event in self?.handle(event, from: server) }
        return server
    }

    private func persist() {
        try? store.save(servers.map(\.credential))
    }

    private func handle(_ event: RemoteEvent, from server: ServerConnection) {
        guard case .devicePaired(let device) = event, !device.current else { return }
        pairedDevices.append(
            PairedDeviceNotice(serverID: server.id, serverName: server.name, device: device))
    }

    func dismiss(_ notice: PairedDeviceNotice) {
        pairedDevices.removeAll { $0.id == notice.id }
    }

    /// The server no longer accepts this phone: forget it and explain why.
    private func revoked(_ server: ServerConnection, message: String) {
        guard !server.isForgetting, servers.contains(where: { $0 === server }) else { return }
        remove(server)
        notice = servers.isEmpty ? message : "\(server.name): \(message)"
    }

    private func remove(_ server: ServerConnection) {
        server.stop()
        server.sessions.reset()
        server.reviews.reset()
        servers.removeAll { $0 === server }
        pairedDevices.removeAll { $0.serverID == server.id }
        if reviewsServerID == server.id { reviewsServerID = nil }
        lock.serversChanged(hasServers: !servers.isEmpty)
        persist()
    }

    // MARK: Adding servers

    /// Pairs with the computer or server in `link`. Pairing again with a saved
    /// server replaces its credential; anything else is added.
    @discardableResult
    func pair(_ link: PairingLink) async -> Bool {
        pairing = .pairing(link.name)
        do {
            let (socket, credential) = try await RemoteConnector.pair(
                link, deviceId: deviceId(reusing: savedServer(for: link)),
                deviceName: UIDevice.current.name)
            add(credential, connection: socket)
            return true
        } catch {
            pairing = .failed(Self.pairingMessage(for: error))
            return false
        }
    }

    /// Parses and pairs with a scanned or pasted link.
    @discardableResult
    func pair(text: String) async -> Bool {
        do {
            return await pair(try PairingLink(text))
        } catch {
            pairing = .failed(error.localizedDescription)
            return false
        }
    }

    /// Pairs with a server using its address and server password.
    @discardableResult
    func join(address text: String, password: String) async -> Bool {
        let address: ServerAddress
        do {
            address = try ServerAddress(parsing: text)
        } catch {
            pairing = .failed(error.localizedDescription)
            return false
        }
        pairing = .pairing(address.display)
        do {
            let (socket, credential) = try await RemoteConnector.join(
                address, password: password, deviceId: deviceId(reusing: nil),
                deviceName: UIDevice.current.name)
            add(credential, connection: socket)
            return true
        } catch {
            pairing = .failed(Self.pairingMessage(for: error))
            return false
        }
    }

    /// Handles `difu://pair?...` opened from another app.
    func open(_ url: URL) {
        guard url.scheme?.lowercased() == "difu", let link = try? PairingLink(url.absoluteString)
        else { return }
        if isPaired {
            pendingLink = link
        } else {
            Task { await pair(link) }
        }
    }

    /// The saved server `link` would pair with again, if any.
    func savedServer(for link: PairingLink) -> ServerConnection? {
        servers.first { CertificateFingerprint.matches($0.credential.fingerprint, pinned: link.fingerprint) }
    }

    private func add(_ credential: Credential, connection: RemoteConnection) {
        notice = nil
        pairing = .idle
        if let existing = servers.first(where: { $0.credential.isSameServer(as: credential) }) {
            existing.replace(credential, connection: connection)
        } else {
            let server = makeConnection(credential)
            servers.append(server)
            if isActive {
                server.start(handoff: connection)
            } else {
                connection.close()
            }
        }
        lock.serversChanged(hasServers: true)
        persist()
    }

    /// One device id per phone, shared by its pairings.
    private func deviceId(reusing existing: ServerConnection?) -> String {
        existing?.credential.deviceId ?? servers.first?.credential.deviceId ?? DeviceIdentity.newId()
    }

    // MARK: Forgetting

    /// Revokes this phone on the server (`device.unpair`), then deletes the
    /// credential. Returns a warning when the server couldn't be told.
    func forget(_ server: ServerConnection) async -> String? {
        server.isForgetting = true
        var warning: String?
        do {
            try await Self.withTimeout(seconds: 10) { try await server.api.unpair() }
        } catch RemoteError.connectionLost {
            // The server closes the socket right after revoking.
        } catch {
            warning = "Couldn't reach \(server.name) to revoke this phone, so it's still listed there. "
                + "Remove it on the server with `difu remote forget`, or in /remote-control on a Mac."
        }
        remove(server)
        return warning
    }

    private static func withTimeout(
        seconds: Double, _ operation: @escaping @Sendable () async throws -> Void
    ) async throws {
        try await withThrowingTaskGroup(of: Void.self) { group in
            group.addTask { try await operation() }
            group.addTask {
                try await Task.sleep(for: .seconds(seconds))
                throw RemoteError.requestTimedOut(method: "device.unpair")
            }
            defer { group.cancelAll() }
            try await group.next()
        }
    }

    private static func pairingMessage(for error: Error) -> String {
        guard let remote = error as? RemoteError else { return error.localizedDescription }
        switch remote {
        case .unreachableComputer(let name, let failures):
            let details = failures.map { "• \($0.error.localizedDescription)" }.joined(separator: "\n")
            return "Can't reach \(name). Check that it's online and reachable from this phone; "
                + "for a computer on your local network, also check the Wi-Fi and the Local "
                + "Network permission.\n\(details)"
        default:
            return remote.localizedDescription
        }
    }
}
