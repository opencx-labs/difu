// difu-remote-probe: exercises the remote-control protocol against a real difu.
//
//   difu-remote-probe pair '<difu://pair?...>'
//   difu-remote-probe info | sessions | inbox [--refresh]
//   difu-remote-probe watch <session-id> [seconds]
//   difu-remote-probe send <session-id> <text...> [--queue]
//   difu-remote-probe overview <owner/repo> <number>
//   difu-remote-probe call <method> ['<json params>']
//   difu-remote-probe forget
//
// The device token and hosts are stored in ~/.difu-remote-probe.json
// (override with --store <path> or DIFU_PROBE_STORE).

import DifuKit
import Foundation

#if canImport(Darwin)
    import Darwin
#endif

setvbuf(stdout, nil, _IOLBF, 0)

struct Usage: Error {}

struct ProbeError: Error, CustomStringConvertible {
    var description: String
}

func log(_ message: String) {
    FileHandle.standardError.write(Data((message + "\n").utf8))
}

let (arguments, storePath): ([String], String) = {
    var arguments = Array(CommandLine.arguments.dropFirst())
    var path = ProcessInfo.processInfo.environment["DIFU_PROBE_STORE"]
        ?? FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent(".difu-remote-probe.json").path
    if let flag = arguments.firstIndex(of: "--store"), flag + 1 < arguments.count {
        path = (arguments[flag + 1] as NSString).expandingTildeInPath
        arguments.removeSubrange(flag...(flag + 1))
    }
    return (arguments, path)
}()
let store = FileCredentialStore(url: URL(fileURLWithPath: storePath))

func deviceName() -> String {
    #if os(macOS)
        let name = Host.current().localizedName ?? "Mac"
    #else
        let name = ProcessInfo.processInfo.hostName
    #endif
    return "\(name) (probe)"
}

func describe(_ error: Error) -> String {
    switch error {
    case let remote as RemoteError:
        switch remote {
        case .unreachableComputer(let name, let failures):
            return "Can't reach \(name). Tried:\n"
                + failures.map { "  ✗ \($0.host): \(hostFailure($0.error))" }.joined(separator: "\n")
        case .denied(let message):
            return "Denied by the computer: \(message)"
        case .pinMismatch(let host, let expected, let actual):
            return "TLS pin mismatch on \(host):\n  expected \(expected)\n  received \(actual)"
        default:
            return remote.localizedDescription
        }
    case let probe as ProbeError:
        return probe.description
    default:
        return error.localizedDescription
    }
}

func hostFailure(_ error: RemoteError) -> String {
    switch error {
    case .timeout: return "timed out after \(Int(RemoteConnector.hostTimeout))s"
    case .unreachable(_, let message): return message
    case .pinMismatch(_, let expected, let actual):
        return "TLS pin mismatch (expected \(expected), received \(actual))"
    case .tls(_, let message): return "TLS failed: \(message)"
    default: return error.localizedDescription
    }
}

func loadCredential() throws -> Credential {
    guard let credential = try store.load() else {
        throw ProbeError(description: "Not paired. Run: difu-remote-probe pair '<difu://pair?...>'")
    }
    return credential
}

/// Connects with the stored credential and saves refreshed hosts/port.
func connect() async throws -> RemoteConnection {
    let credential = try loadCredential()
    log("Connecting to \(credential.computerName) (port \(credential.port))…")
    let (connection, updated) = try await RemoteConnector.connect(credential) { host in
        log("  → trying \(host)")
    }
    try store.save(updated)
    log("  ✓ connected via \(connection.host) — difu \(connection.welcome.server.version ?? "?")")
    return connection
}

func printJSON(_ value: JSONValue) {
    print(value.jsonString(pretty: true))
}

func snippet(_ text: String, _ limit: Int = 100) -> String {
    let flat = text.replacingOccurrences(of: "\n", with: " ⏎ ")
    return flat.count > limit ? String(flat.prefix(limit)) + "…" : flat
}

func line(for entry: EntryView, index: Int) -> String {
    var parts = ["[\(index)]", entry.role.rawValue]
    if !entry.kind.isEmpty, entry.kind != entry.role.rawValue { parts.append("(\(entry.kind))") }
    if let state = entry.state { parts.append("·\(state.rawValue)") }
    if let delivery = entry.delivery { parts.append("·\(delivery.rawValue)") }
    if entry.truncated { parts.append("·truncated") }
    if let title = entry.title, !title.isEmpty { parts.append("“\(snippet(title, 60))”") }
    if !entry.text.isEmpty { parts.append(snippet(entry.text)) }
    if !entry.files.isEmpty {
        parts.append(entry.files.map { "\($0.path) +\($0.additions) -\($0.deletions)" }.joined(separator: ", "))
    }
    if !entry.plan.isEmpty { parts.append("plan: \(entry.plan.count) steps") }
    return parts.joined(separator: " ")
}

func describe(_ summary: SessionSummary) -> String {
    var parts = [summary.orchestrator ? "◇" : (summary.pinned ? "◆" : "•"), summary.id]
    parts.append("[\(summary.statusLabel)]")
    parts.append(summary.displayTitle)
    if let repository = summary.repository { parts.append("— \(repository)") }
    if let branch = summary.branch { parts.append("@ \(branch)") }
    if summary.pending > 0 { parts.append("· \(summary.pending) pending") }
    if summary.queued > 0 { parts.append("· \(summary.queued) queued") }
    if summary.archived { parts.append("· archived") }
    if let started = summary.turnStartedAt, summary.active {
        parts.append("· \(Formatting.elapsed(since: started))")
    }
    return parts.joined(separator: " ")
}

func describe(_ view: SessionView) -> String {
    var lines = [
        "session \(view.id) v\(view.version): \(view.displayTitle) [\(view.statusLabel)]",
        "  provider \(view.provider ?? "?") · model \(view.model ?? "default") · effort \(view.effort ?? "default")",
        "  canSend \(view.canSend) · preparing \(view.preparing) · canInterruptAndSend \(view.canInterruptAndSend)",
        "  \(view.entries.count) entries · \(view.queue.count) queued · \(view.pending.count) pending",
    ]
    if let activity = view.activity {
        let since = activity.since.map { " (\(Formatting.elapsed(since: $0)))" } ?? ""
        lines.append("  activity: \(activity.label)\(since)")
    }
    if let error = view.error { lines.append("  error: \(error)") }
    for pending in view.pending {
        lines.append(
            "  pending \(pending.kind.rawValue) request=\(pending.request.jsonString()) "
                + "\(pending.title ?? "") questions=\(pending.questions.map(\.id)) "
                + "choices=\(pending.choices.map(\.id))")
    }
    for message in view.queue {
        lines.append("  queued[\(message.index)] \(snippet(message.text))")
    }
    let tail = view.entries.suffix(8)
    if view.entries.count > tail.count { lines.append("  …") }
    for (offset, entry) in tail.enumerated() {
        lines.append("  " + line(for: entry, index: view.entries.count - tail.count + offset))
    }
    return lines.joined(separator: "\n")
}

func describe(_ pr: Pr) -> String {
    var parts = ["\(pr.repo)#\(pr.number)", pr.title]
    if let author = pr.author { parts.append("by \(author)") }
    if let additions = pr.additions, let deletions = pr.deletions {
        parts.append("+\(additions) -\(deletions)")
    }
    if pr.draft { parts.append("[draft]") }
    if pr.conflicts { parts.append("[conflicts]") }
    if let updated = pr.updated { parts.append("· \(Formatting.relative(updated))") }
    return parts.joined(separator: " ")
}

// MARK: Commands

func pair(_ text: String) async throws {
    let link = try PairingLink(text)
    log("Pairing with \(link.name) on port \(link.port) (hosts: \(link.hosts.joined(separator: ", ")))")
    let deviceId = (try? store.load())?.deviceId ?? DeviceIdentity.newId()
    let (connection, credential) = try await RemoteConnector.pair(
        link, deviceId: deviceId, deviceName: deviceName()
    ) { host in log("  → trying \(host)") }
    defer { connection.close() }
    try store.save(credential)
    print("Paired with \(credential.computerName) via \(connection.host).")
    print("  device id: \(credential.deviceId)")
    print("  server: difu \(credential.serverVersion ?? "?"), hosts \(credential.hosts), port \(credential.port)")
    print("  saved to \(storePath)")
}

func info() async throws {
    let connection = try await connect()
    defer { connection.close() }
    let server = try await DifuAPI(connection).serverInfo()
    print("name:    \(server.name)")
    print("version: \(server.version ?? "?")")
    print("hosts:   \(server.hosts.joined(separator: ", "))")
    print("port:    \(server.port.map(String.init) ?? "?")")
}

func sessions() async throws {
    let connection = try await connect()
    defer { connection.close() }
    let list = try await DifuAPI(connection).listSessions()
    if list.isEmpty { print("(no sessions)") }
    for summary in list {
        print(describe(summary))
    }
}

func watch(_ id: String, seconds: Double) async throws {
    let connection = try await connect()
    let api = DifuAPI(connection)
    var view: SessionView?
    var deltas = 0
    var rewatches = 0
    let started = Date()
    func stamp() -> String { String(format: "+%5.2fs", Date().timeIntervalSince(started)) }

    let timer = Task {
        try? await Task.sleep(for: .seconds(seconds))
        connection.close()
    }
    try await api.watch(id)
    log("Watching \(id) for \(Int(seconds))s… (Ctrl-C to stop)")
    for await event in connection.events {
        switch event {
        case .session(let next) where next.id == id:
            view = next
            print("\(stamp()) session\n\(describe(next))")
        case .sessionDelta(let delta) where delta.id == id:
            deltas += 1
            let changed = delta.entries.map { line(for: $0.entry, index: $0.index) }
            print(
                "\(stamp()) delta base \(delta.base) → v\(delta.session?.version ?? -1), "
                    + "count \(delta.count.map(String.init) ?? "?"), \(delta.entries.count) changed")
            changed.forEach { print("    " + $0) }
            if var current = view {
                if current.apply(delta) == .applied {
                    view = current
                    if let activity = current.activity {
                        print("    activity: \(activity.label)")
                    }
                } else {
                    rewatches += 1
                    print("    ! base mismatch (have v\(current.version)) — re-watching")
                    try? await api.watch(id)
                }
            } else {
                print("    ! delta before the initial session event — re-watching")
                rewatches += 1
                try? await api.watch(id)
            }
        case .sessionClosed(let closed, let message) where closed == id:
            print("\(stamp()) session.closed: \(message ?? "")")
        case .sessions(let list):
            print("\(stamp()) sessions event: \(list.count) sessions")
        case .unknown(let name):
            print("\(stamp()) unknown event \(name)")
        default:
            break
        }
    }
    timer.cancel()
    if let reason = connection.closeReason, reason != .connectionLost("Disconnected.") {
        log("Connection ended: \(reason.localizedDescription)")
    }
    print(
        "Done: \(deltas) deltas, \(rewatches) re-watches, final "
            + (view.map { "v\($0.version) with \($0.entries.count) entries" } ?? "no session view"))
}

func send(_ id: String, text: String, queue: Bool) async throws {
    let connection = try await connect()
    defer { connection.close() }
    try await DifuAPI(connection).send(id, text: text, queue: queue)
    print(queue ? "Queued." : "Sent.")
}

func inbox(refresh: Bool) async throws {
    let connection = try await connect()
    defer { connection.close() }
    if refresh { log("Refreshing from GitHub…") }
    let inbox = try await DifuAPI(connection).inbox(refresh: refresh)
    print("viewer: \(inbox.viewer ?? "?")")
    print("Mine (\(inbox.mine.count)):")
    inbox.mine.forEach { print("  " + describe($0)) }
    print("Requested (\(inbox.reviewing.count)):")
    inbox.reviewing.forEach { print("  " + describe($0)) }
}

func overview(_ repo: String, _ number: Int) async throws {
    let connection = try await connect()
    defer { connection.close() }
    let pr = try await DifuAPI(connection).overview(repo: repo, number: number)
    print("\(pr.repo)#\(pr.number) \(pr.title)")
    print("  \(pr.state)\(pr.draft ? " (draft)" : "") by \(pr.author ?? "?")")
    print("  \(pr.headBranch ?? "?") → \(pr.baseBranch ?? "?") @ \(pr.head.prefix(10))")
    print("  +\(pr.additions) -\(pr.deletions) in \(pr.changedFiles) files")
    print("  mergeable \(pr.mergeable ?? "?") · state \(pr.mergeState ?? "?")")
    for reviewer in pr.reviewers {
        print("  reviewer \(reviewer.name): \(reviewer.state)")
    }
    for check in pr.checks {
        print("  check \(check.state.rawValue): \(check.name)")
    }
    print("  timeline: \(pr.timeline.count) events")
    if !pr.body.isEmpty { print("\n" + pr.body) }
}

func call(_ method: String, _ json: String?) async throws {
    var params: [String: JSONValue] = [:]
    if let json {
        guard case .object(let object)? = try? JSONDecoder().decode(JSONValue.self, from: Data(json.utf8))
        else { throw ProbeError(description: "Params must be a JSON object, e.g. '{\"id\": \"…\"}'") }
        params = object
    }
    let connection = try await connect()
    defer { connection.close() }
    printJSON(try await DifuAPI(connection).raw(method, params))
}

func forget() throws {
    try store.delete()
    print("Forgot the paired computer (\(storePath)).")
}

func run() async throws {
    guard let command = arguments.first else { throw Usage() }
    let rest = Array(arguments.dropFirst())
    switch command {
    case "pair":
        guard !rest.isEmpty else { throw Usage() }
        try await pair(rest.joined(separator: " "))
    case "info":
        try await info()
    case "sessions":
        try await sessions()
    case "watch":
        guard let id = rest.first else { throw Usage() }
        try await watch(id, seconds: rest.dropFirst().first.flatMap(Double.init) ?? 10)
    case "send":
        let queue = rest.contains("--queue")
        let words = rest.filter { $0 != "--queue" }
        guard words.count >= 2 else { throw Usage() }
        try await send(words[0], text: words.dropFirst().joined(separator: " "), queue: queue)
    case "inbox":
        try await inbox(refresh: rest.contains("--refresh"))
    case "overview":
        guard rest.count == 2, let number = Int(rest[1].trimmingCharacters(in: CharacterSet(charactersIn: "#")))
        else { throw Usage() }
        try await overview(rest[0], number)
    case "call":
        guard let method = rest.first else { throw Usage() }
        try await call(method, rest.count > 1 ? rest.dropFirst().joined(separator: " ") : nil)
    case "forget":
        try forget()
    case "help", "-h", "--help":
        throw Usage()
    default:
        log("Unknown command “\(command)”.")
        throw Usage()
    }
}

let usage = """
    usage: difu-remote-probe [--store <path>] <command>

      pair '<difu://pair?...>'        pair using the /remote-control QR link
      info                            server.info
      sessions                        sessions.list
      watch <session-id> [seconds]    print session events (default 10 s)
      send <session-id> <text> [--queue]
      inbox [--refresh]               reviews.inbox
      overview <owner/repo> <number>  pr.overview
      call <method> ['<json params>'] any method; prints the raw result
      forget                          delete the stored credential
    """

do {
    try await run()
    exit(0)
} catch is Usage {
    log(usage)
    exit(2)
} catch {
    log("error: " + describe(error))
    exit(1)
}
