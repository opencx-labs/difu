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
}
