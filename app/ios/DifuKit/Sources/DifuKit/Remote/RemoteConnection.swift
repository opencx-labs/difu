import Foundation

/// One authenticated WebSocket connection to a difu computer.
///
/// Requests are correlated by id and answered through `call`; push events are
/// delivered on `events`, which finishes when the connection closes (see
/// `closeReason`).
public final class RemoteConnection: @unchecked Sendable {
    public let host: String
    public let port: Int
    public let welcome: Welcome
    public let events: AsyncStream<RemoteEvent>

    private let continuation: AsyncStream<RemoteEvent>.Continuation
    private let session: URLSession
    private let task: URLSessionWebSocketTask
    private let lock = NSLock()
    private var nextId = 0
    private var waiting: [Int: CheckedContinuation<Data, Error>] = [:]
    private var closed: RemoteError?
    private var lastHeard = Date()
    private var loops: [Task<Void, Never>] = []

    /// Largest frame accepted from the server (initial transcripts are big).
    public static let maximumMessageSize = 64 << 20

    private init(
        host: String, port: Int, welcome: Welcome, session: URLSession,
        task: URLSessionWebSocketTask
    ) {
        self.host = host
        self.port = port
        self.welcome = welcome
        self.session = session
        self.task = task
        (events, continuation) = AsyncStream.makeStream(
            of: RemoteEvent.self, bufferingPolicy: .unbounded)
    }

    deinit {
        shutdown(.connectionLost("Closed."))
    }

    /// Why the connection ended, once it has.
    public var closeReason: RemoteError? {
        lock.withLock { closed }
    }

    public var isClosed: Bool { closeReason != nil }

    // MARK: Opening

    /// Connects to one host, sends `hello`, and waits for `welcome`.
    static func open(
        host: String, port: Int, fingerprint: String, hello: Hello, timeout: TimeInterval
    ) async throws -> RemoteConnection {
        guard let url = Endpoint.url(host: host, port: port) else {
            throw RemoteError.invalidHost(host)
        }
        let delegate = PinningDelegate(pinned: fingerprint)
        let configuration = URLSessionConfiguration.ephemeral
        configuration.waitsForConnectivity = false
        configuration.urlCache = nil
        configuration.httpCookieStorage = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        let session = URLSession(configuration: configuration, delegate: delegate, delegateQueue: nil)
        let task = session.webSocketTask(with: url)
        task.maximumMessageSize = maximumMessageSize
        task.resume()

        let timedOut = Flag()
        do {
            // `receive()` may ignore task cancellation, so cancelling the
            // socket itself is what unblocks the handshake on timeout/cancel.
            let welcome = try await withTaskCancellationHandler {
                try await withThrowingTaskGroup(of: Welcome.self) { group in
                    group.addTask { try await handshake(task, hello: hello) }
                    group.addTask {
                        try await Task.sleep(for: .seconds(timeout))
                        timedOut.set()
                        task.cancel(with: .goingAway, reason: nil)
                        throw RemoteError.timeout(host: host)
                    }
                    defer { group.cancelAll() }
                    guard let welcome = try await group.next() else {
                        throw RemoteError.timeout(host: host)
                    }
                    return welcome
                }
            } onCancel: {
                task.cancel(with: .goingAway, reason: nil)
            }
            guard welcome.protocolVersion <= remoteProtocolVersion else {
                throw RemoteError.unsupportedProtocol(welcome.protocolVersion)
            }
            let connection = RemoteConnection(
                host: host, port: port, welcome: welcome, session: session, task: task)
            connection.start()
            return connection
        } catch {
            task.cancel(with: .goingAway, reason: nil)
            session.invalidateAndCancel()
            if Task.isCancelled { throw CancellationError() }
            throw describe(error, host: host, delegate: delegate, timedOut: timedOut.value)
        }
    }

    private static func handshake(_ task: URLSessionWebSocketTask, hello: Hello) async throws -> Welcome {
        try await task.send(.string(hello.json))
        let data: Data
        switch try await task.receive() {
        case .string(let text): data = Data(text.utf8)
        case .data(let bytes): data = bytes
        @unknown default: throw RemoteError.protocolError("unsupported first frame")
        }
        let reply: HelloReply
        do {
            reply = try JSONDecoder().decode(HelloReply.self, from: data)
        } catch {
            throw RemoteError.protocolError("the first message wasn't JSON")
        }
        switch reply.type {
        case "welcome":
            guard let welcome = reply.welcome else { throw RemoteError.protocolError("empty welcome") }
            return welcome
        case "denied":
            throw RemoteError.denied(reply.message ?? "The computer refused this phone.")
        default:
            throw RemoteError.protocolError("expected welcome, got “\(reply.type)”")
        }
    }

    /// Turns URLSession failures into errors a person can act on.
    static func describe(
        _ error: Error, host: String, delegate: PinningDelegate, timedOut: Bool
    ) -> RemoteError {
        if let actual = delegate.mismatch {
            return .pinMismatch(host: host, expected: delegate.pinned, actual: actual)
        }
        if let remote = error as? RemoteError { return remote }
        if timedOut { return .timeout(host: host) }
        let nsError = error as NSError
        if nsError.domain == NSURLErrorDomain {
            switch nsError.code {
            case NSURLErrorTimedOut:
                return .timeout(host: host)
            case NSURLErrorCannotFindHost, NSURLErrorDNSLookupFailed:
                return .unreachable(host: host, message: "host not found on this network")
            case NSURLErrorCannotConnectToHost:
                return .unreachable(
                    host: host, message: "connection refused (is /remote-control enabled?)")
            case NSURLErrorNotConnectedToInternet:
                return .unreachable(
                    host: host,
                    message: "no network (check Wi-Fi and the Local Network permission)")
            case NSURLErrorNetworkConnectionLost:
                return .unreachable(host: host, message: "the connection dropped")
            case NSURLErrorBadServerResponse:
                return .unreachable(host: host, message: "not a difu remote-control server")
            case NSURLErrorSecureConnectionFailed, NSURLErrorServerCertificateUntrusted,
                NSURLErrorServerCertificateHasBadDate, NSURLErrorServerCertificateNotYetValid,
                NSURLErrorServerCertificateHasUnknownRoot, NSURLErrorClientCertificateRejected:
                return .tls(host: host, message: nsError.localizedDescription)
            case NSURLErrorCancelled:
                return .unreachable(host: host, message: "connection cancelled")
            default:
                break
            }
        }
        if nsError.domain == NSPOSIXErrorDomain, nsError.code == Int(ECONNREFUSED) {
            return .unreachable(host: host, message: "connection refused (is /remote-control enabled?)")
        }
        return .unreachable(host: host, message: nsError.localizedDescription)
    }

    // MARK: Running

    private func start() {
        let receive = Task { [weak self] in
            guard let self else { return }
            await self.receiveLoop()
        }
        let heartbeat = Task { [weak self] in
            guard let self else { return }
            await self.heartbeatLoop()
        }
        lock.withLock { loops = [receive, heartbeat] }
    }

    private func receiveLoop() async {
        while true {
            let message: URLSessionWebSocketTask.Message
            do {
                message = try await task.receive()
            } catch {
                let reason: RemoteError
                if task.closeCode != .invalid {
                    let text = task.closeReason.map { String(decoding: $0, as: UTF8.self) } ?? ""
                    reason = .connectionLost(
                        text.isEmpty ? "The computer closed the connection." : text)
                } else {
                    reason = .connectionLost("Lost the connection to the computer.")
                }
                shutdown(reason)
                return
            }
            lock.withLock { lastHeard = Date() }
            switch message {
            case .string(let text): handle(Data(text.utf8))
            case .data(let data): handle(data)
            @unknown default: break
            }
        }
    }

    private func handle(_ data: Data) {
        guard let frame = try? JSONDecoder().decode(IncomingFrame.self, from: data) else { return }
        if let id = frame.id {
            let waiter = lock.withLock { waiting.removeValue(forKey: id) }
            if let message = frame.error {
                waiter?.resume(throwing: RemoteError.server(message))
            } else {
                waiter?.resume(returning: data)
            }
        } else if let event = frame.event {
            continuation.yield(event)
        }
    }

    /// Pings every 15 s; closes when nothing has been heard for 40 s so a
    /// silently dropped Wi-Fi connection is noticed.
    private func heartbeatLoop() async {
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(15))
            if Task.isCancelled || isClosed { return }
            let silent = lock.withLock { Date().timeIntervalSince(lastHeard) }
            if silent > 40 {
                shutdown(.connectionLost("The computer stopped responding."))
                return
            }
            task.sendPing { [weak self] error in
                guard let self, error == nil else { return }
                self.lock.withLock { self.lastHeard = Date() }
            }
        }
    }

    // MARK: Requests

    /// Sends a request and returns the raw response frame (`{"id":..,"result":..}`).
    public func call(
        _ method: String, _ params: [String: JSONValue] = [:], timeout: TimeInterval = 120
    ) async throws -> Data {
        let id: Int = lock.withLock {
            nextId += 1
            return nextId
        }
        let frame = JSONValue.object([
            "id": .int(Int64(id)), "method": .string(method), "params": .object(params),
        ]).jsonString()

        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (waiter: CheckedContinuation<Data, Error>) in
                let failure: RemoteError? = lock.withLock {
                    if let closed { return closed }
                    waiting[id] = waiter
                    return nil
                }
                if let failure {
                    waiter.resume(throwing: failure)
                    return
                }
                task.send(.string(frame)) { [weak self] error in
                    guard let error else { return }
                    self?.fail(id, RemoteError.connectionLost(
                        "Couldn't send to the computer: \(error.localizedDescription)"))
                }
                Task { [weak self] in
                    try? await Task.sleep(for: .seconds(timeout))
                    self?.fail(id, RemoteError.requestTimedOut(method: method))
                }
            }
        } onCancel: { [weak self] in
            self?.fail(id, CancellationError())
        }
    }

    private func fail(_ id: Int, _ error: Error) {
        let waiter = lock.withLock { waiting.removeValue(forKey: id) }
        waiter?.resume(throwing: error)
    }

    // MARK: Closing

    public func close() {
        shutdown(.connectionLost("Disconnected."))
    }

    private func shutdown(_ reason: RemoteError) {
        let pending: ([CheckedContinuation<Data, Error>], [Task<Void, Never>])? = lock.withLock {
            guard closed == nil else { return nil }
            closed = reason
            let waiters = Array(waiting.values)
            waiting.removeAll()
            let loops = self.loops
            self.loops = []
            return (waiters, loops)
        }
        guard let (waiters, loops) = pending else { return }
        for waiter in waiters {
            waiter.resume(throwing: reason)
        }
        continuation.finish()
        loops.forEach { $0.cancel() }
        task.cancel(with: .normalClosure, reason: nil)
        session.invalidateAndCancel()
    }
}

/// A thread-safe one-way boolean.
final class Flag: @unchecked Sendable {
    private let lock = NSLock()
    private var isSet = false

    var value: Bool { lock.withLock { isSet } }

    func set() {
        lock.withLock { isSet = true }
    }
}
