import DifuKit
import Observation
import SwiftUI

/// One saved computer or server: its credential, live connection and
/// lifecycle (connect when the app is active, disconnect in the background,
/// reconnect with backoff), plus its own agents and reviews stores.
@MainActor
@Observable
final class ServerConnection: Identifiable {
    enum State: Equatable {
        /// Not trying (in the background, or forgotten).
        case idle
        /// Trying to connect; `failures` counts consecutive failed attempts.
        case connecting(failures: Int, lastError: String?)
        case connected(host: String)
    }

    /// The saved-server id (the certificate fingerprint). Fixed for the
    /// lifetime of this object, so session references stay valid.
    let id: String
    private(set) var credential: Credential
    private(set) var state: State = .idle
    private(set) var server: ServerInfo?
    /// Increments every time a connection is established, so screens can reload.
    private(set) var generation = 0

    let sessions = SessionStore()
    let reviews = ReviewsStore()

    /// The credential changed (refreshed hosts, re-pairing); persist it.
    @ObservationIgnored var onCredentialChange: ((Credential) -> Void)?
    /// The server refused this phone's token: it was revoked there.
    @ObservationIgnored var onRevoked: ((ServerConnection, String) -> Void)?
    @ObservationIgnored var onEvent: ((ServerConnection, RemoteEvent) -> Void)?

    @ObservationIgnored private var socket: RemoteConnection?
    @ObservationIgnored private var loop: Task<Void, Never>?
    @ObservationIgnored private var retryRequested = false
    @ObservationIgnored private var lastError: String?
    /// Set while "Forget" revokes this phone, so the resulting `denied` on
    /// reconnect isn't reported as a remote revocation.
    @ObservationIgnored var isForgetting = false

    init(credential: Credential) {
        id = credential.id
        self.credential = credential
        sessions.api = api
        reviews.api = api
    }

    var name: String { credential.computerName }

    var isConnected: Bool {
        if case .connected = state { return true }
        return false
    }

    /// Whether the last attempts failed (not just a quick reconnect).
    var isUnreachable: Bool {
        if case .connecting(let failures, _) = state { return failures > 0 }
        return false
    }

    var isServer: Bool { (server?.mode ?? credential.mode) == .server }

    var version: String? { server?.version ?? credential.serverVersion }

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

    /// Starts connecting, optionally adopting a connection that pairing just opened.
    func start(handoff: RemoteConnection? = nil) {
        guard loop == nil else {
            handoff?.close()
            return
        }
        let initial = handoff.map { ($0, credential) }
        loop = Task { await run(initial: initial) }
    }

    /// Disconnects (the app went to the background or the server was forgotten).
    func stop() {
        loop?.cancel()
        loop = nil
        socket?.close()
        socket = nil
        state = .idle
    }

    /// Skips the current backoff delay.
    func retryNow() {
        if loop == nil {
            start()
        } else {
            retryRequested = true
        }
    }

    /// Adopts a fresh pairing with the same server (new token, maybe new hosts).
    func replace(_ credential: Credential, connection: RemoteConnection) {
        stop()
        sessions.reset()
        reviews.reset()
        self.credential = credential
        onCredentialChange?(credential)
        start(handoff: connection)
    }

    private func run(initial: (RemoteConnection, Credential)?) async {
        var failures = 0
        var handoff = initial
        while !Task.isCancelled {
            let opened: (RemoteConnection, Credential)
            if let ready = handoff {
                opened = ready
                handoff = nil
            } else {
                state = .connecting(failures: failures, lastError: lastError)
                do {
                    opened = try await RemoteConnector.connect(credential)
                } catch let error as RemoteError where error.isDenied {
                    if Task.isCancelled { return }
                    loop = nil
                    state = .idle
                    onRevoked?(self, error.localizedDescription)
                    return
                } catch {
                    if Task.isCancelled { return }
                    failures += 1
                    lastError = error.localizedDescription
                    state = .connecting(failures: failures, lastError: lastError)
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
            state = .connected(host: socket.host)
            generation += 1
            Task { await sessions.connected() }

            for await event in socket.events {
                handle(event)
            }

            self.socket = nil
            if Task.isCancelled { return }
            lastError = socket.closeReason?.localizedDescription
            state = .connecting(failures: 0, lastError: lastError)
            try? await Task.sleep(for: .milliseconds(400))
        }
    }

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
        case .devicePaired, .unknown: onEvent?(self, event)
        }
    }

    private func save(_ updated: Credential) {
        guard updated != credential else { return }
        credential = updated
        onCredentialChange?(updated)
    }
}
