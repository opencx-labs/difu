import XCTest

@testable import DifuKit

/// Records calls made through a `DifuAPI` and answers with a canned result.
private actor CallRecorder {
    var calls: [(String, [String: JSONValue])] = []

    func record(_ method: String, _ params: [String: JSONValue]) {
        calls.append((method, params))
    }
}

private func api(_ recorder: CallRecorder, result: String = "{}") -> DifuAPI {
    DifuAPI { method, params in
        await recorder.record(method, params)
        return Data(#"{"id":1,"result":\#(result)}"#.utf8)
    }
}

final class ServerModelTests: XCTestCase {
    func testWelcomeAdditions() throws {
        let reply = try HelloReply(
            Data(
                #"""
                {"type":"welcome","protocol":1,"device":"dev","token":"tok","access":"full","proof":"p",
                 "server":{"name":"hetzner-1","version":"0.35.0","hosts":["203.0.113.5"],"port":3438,
                           "fingerprint":"fp","id":"fp","mode":"server","passwordPairing":true,"future":1}}
                """#.utf8))
        let welcome = try XCTUnwrap(reply.welcome)
        XCTAssertEqual(welcome.access, .full)
        XCTAssertEqual(welcome.proof, "p")
        XCTAssertEqual(welcome.server.id, "fp")
        XCTAssertEqual(welcome.server.mode, .server)
        XCTAssertEqual(welcome.server.passwordPairing, true)

        let old = try XCTUnwrap(
            HelloReply(Data(#"{"type":"welcome","protocol":1,"server":{"name":"Mac"}}"#.utf8)).welcome)
        XCTAssertNil(old.access)
        XCTAssertNil(old.proof)
        XCTAssertNil(old.server.id)
        XCTAssertNil(old.server.mode)
        XCTAssertNil(old.server.passwordPairing)
    }

    func testPairHelloAsksForStandardAccess() throws {
        let pair = try decode(
            JSONValue.self,
            Hello.pair(code: "c", deviceId: "d", deviceName: "Ali's iPhone", access: .standard).json)
        XCTAssertEqual(
            pair,
            [
                "type": "pair", "code": "c", "device": ["id": "d", "name": "Ali's iPhone"],
                "access": "standard",
            ])
    }

    func testDevicePairedEvent() throws {
        let frame = try decode(
            IncomingFrame.self,
            #"{"event":"device.paired","data":{"id":"ipad","name":"Ali's iPad","access":"standard","paired":1790000000000}}"#
        )
        guard case .devicePaired(let device) = frame.event else {
            return XCTFail("\(String(describing: frame.event))")
        }
        XCTAssertEqual(device.id, "ipad")
        XCTAssertEqual(device.name, "Ali's iPad")
        XCTAssertEqual(device.access, .standard)
        XCTAssertEqual(device.paired, 1_790_000_000_000)

        let broken = try decode(IncomingFrame.self, #"{"event":"device.paired","data":{"name":"x"}}"#)
        guard case .unknown("device.paired") = broken.event else { return XCTFail() }
    }

    func testDevices() throws {
        let devices = try decode(
            [PairedDevice].self,
            #"""
            [{"id":"a","name":"Ali's iPhone","access":"standard","paired":1790000000000,"seen":1790000500,"connected":true,"current":true},
             {"id":"b","name":"","access":"superuser","seen":"2026-10-17T12:00:00Z"},
             {"id":"c","name":"Mac","access":"full","connected":"1"}]
            """#)
        XCTAssertEqual(devices.map(\.id), ["a", "b", "c"])
        XCTAssertTrue(devices[0].current)
        XCTAssertTrue(devices[0].connected)
        XCTAssertEqual(devices[0].seen, 1_790_000_500_000, "seconds become milliseconds")
        XCTAssertEqual(devices[1].name, "Unnamed device")
        XCTAssertEqual(devices[1].access, .standard)
        XCTAssertEqual(devices[1].seen, Dates.parse("2026-10-17T12:00:00Z")?.milliseconds)
        XCTAssertFalse(devices[1].current)
        XCTAssertEqual(devices[2].access, .full)
        XCTAssertTrue(devices[2].connected)
    }

    func testServerRepositories() throws {
        let result = try decode(
            ServerRepositories.self,
            #"""
            {"repositories":[
              {"name":"opencx-labs/difu","path":"/srv/difu/repos/opencx-labs/difu","cloned":true,"fetchedAt":1790000000000,"error":null},
              {"name":"opencx-labs/web","cloned":false,"error":"Repository not found"},
              {"path":"/nameless"}
            ],"refreshMinutes":10}
            """#)
        XCTAssertEqual(result.refreshMinutes, 10)
        XCTAssertEqual(result.repositories.map(\.name), ["opencx-labs/difu", "opencx-labs/web"])
        XCTAssertTrue(result.repositories[0].cloned)
        XCTAssertEqual(result.repositories[0].fetchedAt, 1_790_000_000_000)
        XCTAssertNil(result.repositories[0].error)
        XCTAssertFalse(result.repositories[1].cloned)
        XCTAssertEqual(result.repositories[1].error, "Repository not found")
    }

    func testAvailableRepositoriesLoginsAndDeviceLogin() throws {
        let available = try decode(
            [Lossy<AvailableRepository>].self,
            #"["opencx-labs/difu", {"name":"opencx-labs/web","private":true,"description":"Site"}, {"x":1}]"#
        ).compactMap(\.value)
        XCTAssertEqual(available.map(\.name), ["opencx-labs/difu", "opencx-labs/web"])
        XCTAssertEqual(available[1].isPrivate, true)

        let logins = try decode(
            [ServerLogin].self,
            #"""
            [{"tool":"github","loggedIn":true,"account":"octocat"},
             {"tool":"codex","loggedIn":false,"detail":"Not logged in"},
             {"tool":"claude","loggedIn":"true","account":""},
             {"tool":"gemini","loggedIn":false}]
            """#)
        XCTAssertEqual(logins.map(\.tool), [.github, .codex, .claude, .unknown])
        XCTAssertEqual(logins[0].account, "octocat")
        XCTAssertEqual(logins[1].detail, "Not logged in")
        XCTAssertTrue(logins[2].loggedIn)
        XCTAssertNil(logins[2].account)
        XCTAssertEqual(logins[3].displayName, "Gemini")
        XCTAssertTrue(LoginTool.github.usesDeviceCode)
        XCTAssertFalse(LoginTool.claude.usesDeviceCode)

        let login = try decode(
            DeviceLogin.self,
            #"{"url":"https://github.com/login/device","code":"ABCD-1234","message":"Enter the code"}"#)
        XCTAssertEqual(login.url?.absoluteString, "https://github.com/login/device")
        XCTAssertEqual(login.code, "ABCD-1234")
        XCTAssertEqual(login.message, "Enter the code")
    }

    func testServerMethodsSendTheDocumentedParams() async throws {
        let recorder = CallRecorder()
        let client = api(recorder)
        try await client.unpair()
        try await client.forgetDevice("ipad")
        try await client.addRepository("opencx-labs/difu")
        try await client.removeRepository("opencx-labs/difu")
        try await client.refreshRepositories()
        try await client.refreshRepositories("opencx-labs/difu")
        try await client.setRules("Run cargo test", for: "opencx-labs/difu")
        try await client.setLoginToken("sk-ant-oat01")
        try await client.logout("codex")
        let calls = await recorder.calls
        XCTAssertEqual(
            calls.map(\.0),
            [
                "device.unpair", "server.devices.forget", "server.repos.add", "server.repos.remove",
                "server.repos.refresh", "server.repos.refresh", "server.rules.set",
                "server.login.token", "server.logout",
            ])
        XCTAssertEqual(calls[1].1, ["device": "ipad"])
        XCTAssertEqual(calls[2].1, ["name": "opencx-labs/difu"])
        XCTAssertEqual(calls[4].1, [:])
        XCTAssertEqual(calls[5].1, ["name": "opencx-labs/difu"])
        XCTAssertEqual(calls[6].1, ["repository": "opencx-labs/difu", "rules": "Run cargo test"])
        XCTAssertEqual(calls[7].1, ["tool": "claude", "token": "sk-ant-oat01"])
        XCTAssertEqual(calls[8].1, ["tool": "codex"])
    }

    func testResultsAndNewSessionParams() async throws {
        let recorder = CallRecorder()
        let rules = try await api(recorder, result: #"{"rules":"Be brief"}"#).rules(for: "/srv/r")
        XCTAssertEqual(rules, "Be brief")
        let empty = try await api(recorder, result: #"{"rules":null}"#).rules(for: "o/n")
        XCTAssertEqual(empty, "")
        let started = try await api(recorder, result: #"{"url":"https://x","code":"C"}"#).startLogin(.codex)
        XCTAssertEqual(started.code, "C")
        let devices = try await api(recorder, result: #"{"devices":[{"id":"a"}]}"#).devices()
        XCTAssertEqual(devices.map(\.id), ["a"])

        let id = try await api(recorder, result: #"{"id":"s9"}"#).newSession(
            repository: "opencx-labs/difu", base: " release ", model: nil, effort: "high", text: "Fix it")
        XCTAssertEqual(id, "s9")
        _ = try await api(recorder, result: #"{"id":"s10"}"#).newSession(
            repository: nil, model: nil, effort: nil, text: nil)
        let calls = await recorder.calls
        XCTAssertEqual(calls[0].1, ["repository": "/srv/r"])
        XCTAssertEqual(calls[2].1, ["tool": "codex"])
        XCTAssertEqual(
            calls[4].1,
            [
                "repository": "opencx-labs/difu", "base": "release", "model": nil, "effort": "high",
                "text": "Fix it",
            ])
        XCTAssertEqual(calls[5].1, ["repository": nil, "model": nil, "effort": nil])
    }

    func testRepositoryEnvironment() throws {
        let environment = try decode(
            RepositoryEnvironment.self,
            #"""
            {"repository":"acme/widgets",
             "profile":{"tools":{"pnpm":"9","node":"24","bad":{"x":1}},
                        "setup":["pnpm install --frozen-lockfile"],
                        "env":{"NODE_OPTIONS":"--max-old-space-size=4096","WORKERS":4},
                        "notes":["backend tests need Postgres via docker compose"],
                        "history":[{"at":1790000000000,"by":"Claude Code in session 1 (Billing)","summary":"node 24"}],
                        "updatedAt":1790000000000,"future":true},
             "detected":[{"tool":"pnpm","version":"9.12.0","source":"package.json packageManager","corepack":true},
                         {"tool":"node","version":"24","source":".nvmrc"},
                         {"version":"1"}]}
            """#)
        XCTAssertEqual(environment.repository, "acme/widgets")
        let profile = environment.profile
        XCTAssertEqual(profile.tools.map(\.name), ["node", "pnpm"], "sorted, invalid values dropped")
        XCTAssertEqual(profile.tools.first?.value, "24")
        XCTAssertEqual(profile.setup, ["pnpm install --frozen-lockfile"])
        XCTAssertEqual(
            profile.env,
            [
                EnvironmentEntry(name: "NODE_OPTIONS", value: "--max-old-space-size=4096"),
                EnvironmentEntry(name: "WORKERS", value: "4"),
            ])
        XCTAssertEqual(profile.notes, ["backend tests need Postgres via docker compose"])
        XCTAssertEqual(profile.history.first?.by, "Claude Code in session 1 (Billing)")
        XCTAssertEqual(profile.history.first?.at, 1_790_000_000_000)
        XCTAssertEqual(profile.updatedAt, 1_790_000_000_000)
        XCTAssertEqual(environment.detected.map(\.tool), ["pnpm", "node"])
        XCTAssertEqual(environment.detected.map(\.corepack), [true, false])
        XCTAssertFalse(profile.isEmpty)

        let empty = try decode(RepositoryEnvironment.self, #"{"repository":"a/b","profile":null}"#)
        XCTAssertTrue(empty.profile.isEmpty)
        XCTAssertTrue(empty.detected.isEmpty)
    }

    func testEnvironmentCalls() async throws {
        let recorder = CallRecorder()
        let reply = #"{"repository":"acme/widgets","profile":{"tools":{"node":"24"}},"detected":[]}"#
        let read = try await api(recorder, result: reply).environment(for: "acme/widgets")
        XCTAssertEqual(read.profile.tools, [EnvironmentEntry(name: "node", value: "24")])
        let edited = EnvironmentProfile(
            tools: [
                EnvironmentEntry(name: " node ", value: " 24 "), EnvironmentEntry(name: "", value: "x"),
            ],
            setup: ["pnpm install", "  "],
            env: [EnvironmentEntry(name: "CI", value: "1")],
            notes: ["Postgres via docker compose"])
        let saved = try await api(recorder, result: reply).setEnvironment(edited, for: "acme/widgets")
        XCTAssertEqual(saved.repository, "acme/widgets")
        let calls = await recorder.calls
        XCTAssertEqual(calls.map(\.0), ["server.environment.get", "server.environment.set"])
        XCTAssertEqual(calls[0].1, ["repository": "acme/widgets"])
        XCTAssertEqual(
            calls[1].1,
            [
                "repository": "acme/widgets",
                "profile": [
                    "tools": ["node": "24"], "setup": ["pnpm install"], "env": ["CI": "1"],
                    "notes": ["Postgres via docker compose"],
                ],
            ])
    }

    func testServerDisk() throws {
        let disk = try decode(
            ServerDisk.self,
            #"""
            {"path":"/home/difu/.config/difu/agents","total":80000000000,"used":52000000000,"free":28000000000,
             "status":"ok","measuredAt":1790000000000,"measuring":false,
             "breakdown":{
               "sessions":[
                 {"id":"1790000000000-ab12cd34","title":"Fix login","archived":false,"active":true,"bytes":1234567,"buildOutputs":456789},
                 {"id":"s2","title":"Old spike","archived":true,"active":false,"bytes":9000000,"buildOutputs":0}
               ],
               "repositories":9000000000,"attachments":12000000,"caches":340000000,
               "sandbox":{"available":true,"images":3000000000,"containers":120000000,"volumes":0,"reclaimable":900000000},
               "other":30000000000}}
            """#)
        XCTAssertEqual(disk.path, "/home/difu/.config/difu/agents")
        XCTAssertEqual(disk.total, 80_000_000_000)
        XCTAssertEqual(disk.used, 52_000_000_000)
        XCTAssertEqual(disk.free, 28_000_000_000)
        XCTAssertEqual(disk.status, .ok)
        XCTAssertEqual(disk.measuredAt, 1_790_000_000_000)
        XCTAssertFalse(disk.measuring)
        XCTAssertEqual(disk.usedFraction, 0.65, accuracy: 0.0001)

        let breakdown = disk.breakdown
        XCTAssertEqual(breakdown.sessions.map(\.id), ["s2", "1790000000000-ab12cd34"], "biggest first")
        let session = try XCTUnwrap(breakdown.sessions.last)
        XCTAssertEqual(session.title, "Fix login")
        XCTAssertFalse(session.archived)
        XCTAssertTrue(session.active)
        XCTAssertEqual(session.bytes, 1_234_567)
        XCTAssertEqual(session.buildOutputs, 456_789)
        XCTAssertEqual(breakdown.sessionsTotal, 10_234_567)
        XCTAssertEqual(breakdown.repositories, 9_000_000_000)
        XCTAssertEqual(breakdown.attachments, 12_000_000)
        XCTAssertEqual(breakdown.caches, 340_000_000)
        XCTAssertEqual(breakdown.other, 30_000_000_000)
        XCTAssertTrue(breakdown.sandbox.available)
        XCTAssertEqual(breakdown.sandbox.images, 3_000_000_000)
        XCTAssertEqual(breakdown.sandbox.containers, 120_000_000)
        XCTAssertEqual(breakdown.sandbox.volumes, 0)
        XCTAssertEqual(breakdown.sandbox.reclaimable, 900_000_000)
        XCTAssertEqual(breakdown.sandbox.total, 3_120_000_000)
    }

    func testServerDiskIsLenient() throws {
        let disk = try decode(
            ServerDisk.self,
            #"""
            {"total":8.0e10,"used":"52000000000","free":2.8e10,"status":"nearly-full","measuredAt":null,
             "measuring":"1","future":{"x":1},
             "breakdown":{
               "sessions":[{"id":"a"},{"title":"No id","bytes":1},{"id":"b","title":"  ","bytes":5.5e9,"archived":1,"future":true}],
               "repositories":-5,"sandbox":null,"extra":[1,2]}}
            """#)
        XCTAssertNil(disk.path)
        XCTAssertEqual(disk.total, 80_000_000_000)
        XCTAssertEqual(disk.used, 52_000_000_000)
        XCTAssertEqual(disk.free, 28_000_000_000)
        XCTAssertEqual(disk.status, .unknown)
        XCTAssertNil(disk.measuredAt)
        XCTAssertTrue(disk.measuring)
        XCTAssertEqual(disk.breakdown.sessions.map(\.id), ["b", "a"], "id-less sessions are dropped")
        let b = try XCTUnwrap(disk.breakdown.sessions.first)
        XCTAssertEqual(b.bytes, 5_500_000_000)
        XCTAssertTrue(b.archived)
        XCTAssertFalse(b.active)
        XCTAssertEqual(b.buildOutputs, 0)
        XCTAssertEqual(b.displayTitle, "New session")
        XCTAssertEqual(disk.breakdown.sessions.last?.bytes, 0)
        XCTAssertEqual(disk.breakdown.repositories, 0, "negative sizes clamp to zero")
        XCTAssertEqual(disk.breakdown.attachments, 0)
        XCTAssertEqual(disk.breakdown.sandbox, SandboxDiskUsage())
        XCTAssertFalse(disk.breakdown.sandbox.available)

        let empty = try decode(ServerDisk.self, "{}")
        XCTAssertEqual(empty.total, 0)
        XCTAssertEqual(empty.usedFraction, 0)
        XCTAssertEqual(empty.status, .unknown)
        XCTAssertFalse(empty.measuring)
        XCTAssertEqual(empty.breakdown, DiskBreakdown())
        for action in DiskCleanAction.allCases {
            XCTAssertFalse(empty.breakdown.canClean(action))
        }

        let huge = try decode(
            ServerDisk.self,
            #"{"total":18446744073709551615,"used":"9","status":"CRITICAL","measuredAt":1790000000,"breakdown":[]}"#)
        XCTAssertEqual(huge.total, .max, "UInt64 sizes clamp to Int64")
        XCTAssertEqual(huge.status, .critical)
        XCTAssertEqual(huge.measuredAt, 1_790_000_000_000, "seconds become milliseconds")
        XCTAssertEqual(huge.breakdown, DiskBreakdown())
        XCTAssertEqual(DiskStatus(lenient: "low"), .low)
    }

    func testDiskCleanupExpectations() throws {
        let disk = try decode(
            ServerDisk.self,
            #"""
            {"total":100,"used":150,"free":0,"status":"critical","measuredAt":1790000000000,
             "breakdown":{"sessions":[
               {"id":"active-archived","archived":true,"active":true,"bytes":700,"buildOutputs":600},
               {"id":"archived","title":"Old","archived":true,"bytes":300,"buildOutputs":100},
               {"id":"idle","title":"Idle","bytes":900,"buildOutputs":400},
               {"id":"busy","active":true,"bytes":1000,"buildOutputs":800},
               {"id":"gone","archived":true,"bytes":0},
               {"id":"big-archived","title":"Big","archived":true,"bytes":500}
             ],"sandbox":{"available":false,"reclaimable":50}}}
            """#)
        XCTAssertEqual(disk.usedFraction, 1, "clamped")
        XCTAssertEqual(
            disk.breakdown.sessions.map(\.id),
            ["busy", "idle", "active-archived", "big-archived", "archived", "gone"])

        XCTAssertEqual(disk.breakdown.sessions(for: .archivedWorktrees).map(\.id), ["big-archived", "archived"])
        XCTAssertEqual(disk.breakdown.expectedSpace(for: .archivedWorktrees), 800)
        XCTAssertEqual(disk.breakdown.sessions(for: .buildOutputs).map(\.id), ["idle", "archived"])
        XCTAssertEqual(disk.breakdown.expectedSpace(for: .buildOutputs), 500)
        XCTAssertEqual(disk.breakdown.sessions(for: .sandbox), [])
        XCTAssertEqual(disk.breakdown.expectedSpace(for: .sandbox), 0, "no Podman, nothing to prune")
        XCTAssertTrue(disk.breakdown.canClean(.archivedWorktrees))
        XCTAssertTrue(disk.breakdown.canClean(.buildOutputs))
        XCTAssertFalse(disk.breakdown.canClean(.sandbox))

        var podman = disk.breakdown
        podman.sandbox.available = true
        XCTAssertEqual(podman.expectedSpace(for: .sandbox), 50)
        XCTAssertTrue(podman.canClean(.sandbox))
        podman.sandbox.reclaimable = 0
        XCTAssertFalse(podman.canClean(.sandbox))

        let overflowing = try decode(
            DiskBreakdown.self,
            #"{"sessions":[{"id":"a","archived":true,"bytes":9223372036854775807},{"id":"b","archived":true,"bytes":9223372036854775807}]}"#)
        XCTAssertEqual(overflowing.expectedSpace(for: .archivedWorktrees), .max, "sums saturate")
        XCTAssertEqual(overflowing.sessionsTotal, .max)
    }

    func testDiskCleanResult() throws {
        let result = try decode(
            DiskCleanResult.self,
            #"{"freed":2100000000,"summary":"Removed 3 archived worktrees (2.1 GB). Kept 1 with uncommitted or ignored files: Fix login.","future":1}"#)
        XCTAssertEqual(result.freed, 2_100_000_000)
        XCTAssertEqual(
            result.summary,
            "Removed 3 archived worktrees (2.1 GB). Kept 1 with uncommitted or ignored files: Fix login.")

        let bare = try decode(DiskCleanResult.self, #"{"freed":1.5e3,"summary":null}"#)
        XCTAssertEqual(bare.freed, 1500)
        XCTAssertEqual(bare.summary, "")
    }

    func testDiskMethodsSendTheDocumentedParams() async throws {
        XCTAssertEqual(DiskCleanAction.allCases.map(\.rawValue), ["archivedWorktrees", "buildOutputs", "sandbox"])
        let recorder = CallRecorder()
        let client = api(recorder, result: #"{"freed":10,"summary":"Done."}"#)
        _ = try await client.disk()
        _ = try await client.disk(refresh: true)
        for action in DiskCleanAction.allCases {
            let result = try await client.cleanDisk(action)
            XCTAssertEqual(result.freed, 10)
            XCTAssertEqual(result.summary, "Done.")
        }
        let calls = await recorder.calls
        XCTAssertEqual(
            calls.map(\.0),
            ["server.disk", "server.disk", "server.disk.clean", "server.disk.clean", "server.disk.clean"])
        XCTAssertEqual(calls[0].1, [:])
        XCTAssertEqual(calls[1].1, ["refresh": true])
        XCTAssertEqual(calls[2].1, ["action": "archivedWorktrees"])
        XCTAssertEqual(calls[3].1, ["action": "buildOutputs"])
        XCTAssertEqual(calls[4].1, ["action": "sandbox"])

        let disk = try await api(recorder, result: #"{"total":10,"used":4,"free":6,"status":"low","measuring":true}"#).disk()
        XCTAssertEqual(disk.status, .low)
        XCTAssertTrue(disk.measuring)
    }

    func testOlderServersAndSlowCleanups() {
        XCTAssertTrue(RemoteError.server("Unknown method `server.disk`; update difu on your computer").isUnknownMethod)
        XCTAssertFalse(RemoteError.server("Only difu servers report disk space").isUnknownMethod)
        XCTAssertFalse(RemoteError.notConnected.isUnknownMethod)
        XCTAssertEqual(RemoteConnection.timeout(for: "server.disk"), 120)
        XCTAssertGreaterThanOrEqual(RemoteConnection.timeout(for: "server.disk.clean"), 10 * 60)
        XCTAssertEqual(Formatting.bytes(-1), Formatting.bytes(0))
        XCTAssertFalse(Formatting.bytes(0).localizedCaseInsensitiveContains("zero"))
    }
}
