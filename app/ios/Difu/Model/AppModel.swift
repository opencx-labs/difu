import DifuKit
import Observation
import SwiftUI
import UIKit

/// Root app state: the paired computer, the live connection and its
/// lifecycle (connect when active, disconnect in the background, reconnect
/// with backoff), plus the agents and reviews stores.
@MainActor
@Observable
final class AppModel {
    enum ConnectionState: Equatable {
        /// Not trying (unpaired or in the background).
        case idle
        /// Trying to connect; `failures` counts consecutive failed attempts.
        case connecting(failures: Int, lastError: String?)
        case connected(host: String)
    }

    enum PairingState: Equatable {
        case idle
        case pairing(computer: String)
        case failed(String)
    }

    private(set) var credential: Credential?
    private(set) var connection: ConnectionState = .idle
    private(set) var server: ServerInfo?
    /// Increments every time a connection is established, so screens can reload.
    private(set) var generation = 0
    var pairing: PairingState = .idle
    /// Shown on the welcome screen, e.g. after the computer revoked this phone.
    var notice: String?
    /// A `difu://pair` link opened while already paired, awaiting confirmation.
    var pendingLink: PairingLink?

    let sessions = SessionStore()
    let reviews = ReviewsStore()

    @ObservationIgnored private let store: CredentialStore
    @ObservationIgnored private var socket: RemoteConnection?
    @ObservationIgnored private var loop: Task<Void, Never>?
    @ObservationIgnored private var retryRequested = false

    init(store: CredentialStore = KeychainCredentialStore()) {
        self.store = store
        credential = try? store.load()
        sessions.api = api
        reviews.api = api
    }

    var isPaired: Bool { credential != nil }

    var isConnected: Bool {
        if case .connected = connection { return true }
        return false
    }

    var computerName: String {
        credential?.computerName ?? server?.name ?? "your computer"
    }

    /// API bound to whichever connection is current; calls made while
    /// reconnecting wait briefly for the connection.
    var api: DifuAPI {
        DifuAPI { [weak self] method, params in
            guard let self else { throw RemoteError.notConnected }
            let socket = try await self.connectedSocket()
            return try await socket.call(method, params)
        }
    }

    // MARK: Lifecycle

    /// Starts connecting (scene became active).
    func activate() {
        guard credential != nil, loop == nil else { return }
        loop = Task { await run(initial: nil) }
    }

    /// Disconnects (scene went to the background).
    func deactivate() {
        loop?.cancel()
        loop = nil
        socket?.close()
        socket = nil
        if credential != nil { connection = .idle }
    }

    /// Skips the current backoff delay.
    func retryNow() {
        if loop == nil {
            activate()
        } else {
            retryRequested = true
        }
    }

    private func run(initial: (RemoteConnection, Credential)?) async {
        var failures = 0
        var handoff = initial
        while !Task.isCancelled, let credential {
            let opened: (RemoteConnection, Credential)
            if let ready = handoff {
                opened = ready
                handoff = nil
            } else {
                connection = .connecting(failures: failures, lastError: lastError)
                do {
                    opened = try await RemoteConnector.connect(credential)
                } catch let error as RemoteError where error.isDenied {
                    revoked(error.localizedDescription)
                    return
                } catch {
                    if Task.isCancelled { return }
                    failures += 1
                    lastError = error.localizedDescription
                    connection = .connecting(failures: failures, lastError: lastError)
                    await backoff(failures)
                    continue
                }
            }
            let (socket, updated) = opened
            guard !Task.isCancelled else {
                socket.close()
                return
            }
            failures = 0
            lastError = nil
            save(updated)
            self.socket = socket
            server = socket.welcome.server
            connection = .connected(host: socket.host)
            generation += 1
            Task { await sessions.connected() }

            for await event in socket.events {
                handle(event)
            }

            self.socket = nil
            if Task.isCancelled { return }
            lastError = socket.closeReason?.localizedDescription
            connection = .connecting(failures: 0, lastError: lastError)
            try? await Task.sleep(for: .milliseconds(400))
        }
    }

    @ObservationIgnored private var lastError: String?

    private func backoff(_ failures: Int) async {
        let seconds = min(30, 0.5 * pow(2, Double(min(failures, 6)))) + .random(in: 0...0.5)
        let deadline = ContinuousClock.now + .seconds(seconds)
        while ContinuousClock.now < deadline, !retryRequested, !Task.isCancelled {
            try? await Task.sleep(for: .milliseconds(200))
        }
        retryRequested = false
    }

    private func connectedSocket() async throws -> RemoteConnection {
        if let socket { return socket }
        guard credential != nil else { throw RemoteError.notPaired }
        let deadline = ContinuousClock.now + .seconds(8)
        while ContinuousClock.now < deadline, loop != nil {
            try await Task.sleep(for: .milliseconds(150))
            if let socket { return socket }
        }
        throw RemoteError.notConnected
    }

    private func handle(_ event: RemoteEvent) {
        switch event {
        case .sessions(let list): sessions.receive(list)
        case .session(let view): sessions.receive(view)
        case .sessionDelta(let delta): sessions.apply(delta)
        case .sessionClosed(let id, let message): sessions.closed(id, message: message)
        case .unknown: break
        }
    }

    private func save(_ updated: Credential) {
        guard updated != credential else { return }
        credential = updated
        try? store.save(updated)
    }

    /// The computer no longer accepts this phone: forget it and explain why.
    private func revoked(_ message: String) {
        loop = nil
        forgetLocally()
        notice = message
    }

    // MARK: Pairing

    /// Pairs with the computer in `link`, replacing any current pairing.
    func pair(_ link: PairingLink) async {
        pairing = .pairing(computer: link.name)
        let deviceId = credential?.deviceId ?? DeviceIdentity.newId()
        do {
            let (socket, credential) = try await RemoteConnector.pair(
                link, deviceId: deviceId, deviceName: UIDevice.current.name)
            deactivate()
            sessions.reset()
            reviews.reset()
            self.credential = credential
            try? store.save(credential)
            notice = nil
            pairing = .idle
            loop = Task { await run(initial: (socket, credential)) }
        } catch {
            pairing = .failed(Self.pairingMessage(for: error))
        }
    }

    /// Parses and pairs with a scanned or pasted link.
    func pair(text: String) async {
        do {
            await pair(try PairingLink(text))
        } catch {
            pairing = .failed(error.localizedDescription)
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

    func forget() {
        forgetLocally()
        notice = nil
    }

    private func forgetLocally() {
        deactivate()
        try? store.delete()
        credential = nil
        server = nil
        connection = .idle
        sessions.reset()
        reviews.reset()
    }

    private static func pairingMessage(for error: Error) -> String {
        guard let remote = error as? RemoteError else { return error.localizedDescription }
        switch remote {
        case .unreachableComputer(let name, let failures):
            let details = failures.map { "• \($0.error.localizedDescription)" }.joined(separator: "\n")
            return "Can't reach \(name). Make sure your phone is on the same Wi-Fi and "
                + "allowed to use the local network.\n\(details)"
        default:
            return remote.localizedDescription
        }
    }
}
