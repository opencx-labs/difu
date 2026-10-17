import XCTest

@testable import DifuKit

func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
    try JSONDecoder().decode(T.self, from: Data(json.utf8))
}

let summaryJSON = """
    {
      "id": "1791875325706-02acb407", "title": "Fix login redirect", "kind": "Coding",
      "orchestrator": false, "status": "Running", "statusLabel": "Working", "active": true,
      "archived": false, "pinned": false, "updated": 1790000000000, "turnStartedAt": 1790000000000,
      "repository": "difu", "repositoryPath": "/Users/ali/code/difu",
      "branch": "difu/agent-1791875325706-02acb407",
      "workspace": "/Users/ali/Library/Application Support/difu/agents/worktrees/x",
      "worktree": true, "pending": 1, "queued": 0, "canReadChanges": true
    }
    """

let viewJSON = """
    {
      "id": "s1", "title": "Fix login redirect", "kind": "Coding", "orchestrator": false,
      "status": "Running", "statusLabel": "Working", "active": true, "archived": false,
      "pinned": false, "updated": 1790000000000, "turnStartedAt": 1790000000000,
      "repository": "difu", "branch": "b", "worktree": true, "queued": 1, "canReadChanges": true,
      "version": 412, "provider": "codex", "model": "gpt-5.6-luna", "effort": "high",
      "error": null, "preparing": false, "canSend": true, "canInterruptAndSend": false,
      "activity": {"label": "Running cargo test", "since": 1790000000000},
      "queue": [{"index": 0, "text": "Also update the docs"}],
      "pending": [{
        "request": 3, "kind": "questions", "async": false, "title": "Allow command?",
        "detail": "cargo test --locked",
        "questions": [{
          "id": "difu-decision", "header": "Permission", "question": "Allow Bash?\\nrm -rf target",
          "options": [{"label": "Allow once", "description": ""}, {"label": "Deny", "description": ""}],
          "multiSelect": false, "secret": false, "answered": false
        }],
        "choices": [{"id": "accept", "label": "Allow once"}, {"id": "decline", "label": "Decline"}]
      }],
      "entries": [
        {"id": "u1", "kind": "userMessage", "role": "user", "text": "hi", "delivery": "sending"},
        {"id": "item_12", "kind": "commandExecution", "role": "tool", "text": "ok",
         "title": "Ran cargo test", "detail": "cargo test --locked", "state": "done",
         "startedAt": 1790000000000, "finishedAt": 1790000001000,
         "files": [{"path": "src/main.rs", "additions": 3, "deletions": 1, "diff": "@@ -1,3 +1,5 @@\\n+a"}],
         "plan": [{"step": "Add tests", "status": "completed"}, {"step": "Ship", "status": "inProgress"}],
         "delivery": null, "truncated": false}
      ]
    }
    """

final class ModelDecodingTests: XCTestCase {
    func testSessionSummary() throws {
        let summary = try decode(SessionSummary.self, summaryJSON)
        XCTAssertEqual(summary.id, "1791875325706-02acb407")
        XCTAssertEqual(summary.kind, .coding)
        XCTAssertEqual(summary.status, .running)
        XCTAssertEqual(summary.statusLabel, "Working")
        XCTAssertTrue(summary.active)
        XCTAssertEqual(summary.turnStartedAt, 1_790_000_000_000)
        XCTAssertEqual(summary.repository, "difu")
        XCTAssertEqual(summary.pending, 1)
        XCTAssertTrue(summary.canReadChanges)
    }

    func testSummaryToleratesUnknownAndMissingFields() throws {
        let summary = try decode(
            SessionSummary.self,
            #"{"id": "x", "status": "Hibernating", "kind": "Robot", "futureField": [1, 2], "updated": 1.79e12, "pending": "2", "repository": null}"#
        )
        XCTAssertEqual(summary.status, .unknown)
        XCTAssertEqual(summary.kind, .unknown)
        XCTAssertEqual(summary.statusLabel, "Unknown")
        XCTAssertEqual(summary.updated, 1_790_000_000_000)
        XCTAssertEqual(summary.pending, 2)
        XCTAssertNil(summary.repository)
        XCTAssertEqual(summary.displayTitle, "New session")
        XCTAssertFalse(summary.archived)
    }

    func testOrchestratorTitleFallback() throws {
        let summary = try decode(SessionSummary.self, #"{"id": "o", "kind": "Orchestrator", "title": ""}"#)
        XCTAssertTrue(summary.orchestrator)
        XCTAssertEqual(summary.displayTitle, "Difu orchestrator")
    }

    func testSessionListDropsOnlyBadRows() throws {
        let frame = try decode(
            IncomingFrame.self,
            #"{"event": "sessions", "data": [{"id": "a"}, {"title": "no id"}, 42, {"id": "b"}]}"#)
        guard case .sessions(let list)? = frame.event else { return XCTFail("no sessions event") }
        XCTAssertEqual(list.map(\.id), ["a", "b"])
    }

    func testSessionView() throws {
        let view = try decode(SessionView.self, viewJSON)
        XCTAssertEqual(view.id, "s1")
        XCTAssertEqual(view.version, 412)
        XCTAssertEqual(view.model, "gpt-5.6-luna")
        XCTAssertEqual(view.activity?.label, "Running cargo test")
        XCTAssertEqual(view.queue.first?.text, "Also update the docs")
        XCTAssertEqual(view.pending.count, 1)
        XCTAssertEqual(view.summary.pending, 1, "pending is an array in a SessionView")
        XCTAssertEqual(view.statusLabel, "Working")

        let pending = try XCTUnwrap(view.pending.first)
        XCTAssertEqual(pending.request, .int(3))
        XCTAssertEqual(pending.kind, .questions)
        XCTAssertEqual(pending.questions.first?.isPermission, true)
        XCTAssertEqual(pending.questions.first?.options.map(\.label), ["Allow once", "Deny"])
        XCTAssertNil(pending.questions.first?.options.first?.description)
        XCTAssertEqual(pending.choices.map(\.id), ["accept", "decline"])

        XCTAssertEqual(view.entries.count, 2)
        XCTAssertEqual(view.entries[0].role, .user)
        XCTAssertEqual(view.entries[0].delivery, .sending)
        let tool = view.entries[1]
        XCTAssertEqual(tool.role, .tool)
        XCTAssertEqual(tool.state, .done)
        XCTAssertNil(tool.delivery)
        XCTAssertEqual(tool.duration(now: 0), 1000)
        XCTAssertEqual(tool.files.first?.path, "src/main.rs")
        XCTAssertEqual(tool.files.first?.additions, 3)
        XCTAssertEqual(tool.plan.map(\.status), [.completed, .inProgress])
    }

    func testEntriesStayIndexAligned() throws {
        let view = try decode(
            SessionView.self,
            #"{"id": "s", "version": 1, "entries": [{"id": "a", "role": "user"}, 7, {"id": "c", "role": "telepathy", "state": "floating", "delivery": "pigeon"}]}"#
        )
        XCTAssertEqual(view.entries.count, 3)
        XCTAssertEqual(view.entries[1].role, .system)
        XCTAssertEqual(view.entries[2].role, .unknown)
        XCTAssertEqual(view.entries[2].state, .unknown)
        XCTAssertEqual(view.entries[2].delivery, .unknown)
    }

    func testOpaqueRequestRoundTrips() throws {
        for raw in [#"3"#, #""req-1""#, #"{"turn": 4, "call": "x"}"#, #"[1, "a"]"#] {
            let pending = try decode(PendingView.self, #"{"request": \#(raw), "kind": "approval"}"#)
            let reencoded = try decode(JSONValue.self, pending.request.jsonString())
            XCTAssertEqual(reencoded, try decode(JSONValue.self, raw))
        }
        let pending = try decode(PendingView.self, #"{"request": 3, "kind": "approval"}"#)
        XCTAssertEqual(pending.request.jsonString(), "3", "integers must not become 3.0")
    }

    func testUnknownPendingKind() throws {
        let pending = try decode(PendingView.self, #"{"request": 1, "kind": "telepathy", "choices": [{"id": "stop"}]}"#)
        XCTAssertEqual(pending.kind, .unknown)
        XCTAssertEqual(pending.choices.first?.label, "stop")
    }

    func testDiffFile() throws {
        let file = try decode(
            DiffFile.self,
            #"{"path": "src/a.rs", "oldPath": null, "status": "modified", "additions": 3, "deletions": 1, "patch": "@@ -1 +1 @@\n-a\n+b", "truncated": false}"#
        )
        XCTAssertEqual(file.status, .modified)
        XCTAssertNil(file.oldPath)
        let counted = try decode(DiffFile.self, #"{"path": "b", "status": "copied", "patch": "@@ -1 +1,2 @@\n-a\n+b\n+c"}"#)
        XCTAssertEqual(counted.status, .modified)
        XCTAssertEqual(counted.additions, 2)
        XCTAssertEqual(counted.deletions, 1)
    }

    func testAgentLists() throws {
        let repositories = try decode(
            ResultEnvelope<[AgentRepository]>.self,
            #"{"result": [{"path": "/x/difu", "name": "difu", "default": true}, {"path": "/x/other"}]}"#
        ).result
        XCTAssertEqual(repositories.map(\.name), ["difu", "other"])
        XCTAssertTrue(repositories[0].isDefault)

        let model = try decode(
            AgentModel.self,
            #"{"id": "gpt-5.6-luna", "name": "GPT-5.6 Luna", "provider": "codex", "efforts": ["low", "medium", "high"]}"#
        )
        XCTAssertEqual(model.efforts, ["low", "medium", "high"])
    }

    func testPrAndInbox() throws {
        let inbox = try decode(
            Inbox.self,
            #"""
            {"viewer": "octocat", "mine": [{"repo": "opencx-labs/difu", "number": 45, "title": "T", "author": "octocat",
             "updated": "2026-10-17T12:00:00Z", "created": "2026-10-16T12:00:00.250Z", "draft": false,
             "state": "OPEN", "conflicts": false, "reviewers": ["alice", {"login": "bob"}],
             "additions": 10, "deletions": 2, "changedFiles": 3, "url": "https://github.com/opencx-labs/difu/pull/45"}],
             "reviewing": [{"title": "missing repo"}]}
            """#)
        XCTAssertEqual(inbox.viewer, "octocat")
        XCTAssertEqual(inbox.reviewing.count, 0)
        let pr = try XCTUnwrap(inbox.mine.first)
        XCTAssertEqual(pr.id, "opencx-labs/difu#45")
        XCTAssertEqual(pr.updated, Date(timeIntervalSince1970: 1_792_238_400))
        XCTAssertNotNil(pr.created)
        XCTAssertEqual(pr.reviewers, ["alice", "bob"])
        XCTAssertEqual(pr.url?.host, "github.com")
    }

    func testPrOverview() throws {
        let overview = try decode(
            PrOverview.self,
            #"""
            {"repo": "opencx-labs/difu", "number": 45, "url": "https://x", "title": "T", "body": "# Hi",
             "author": "octocat", "state": "open", "draft": false, "head": "abc", "base": "def",
             "headBranch": "feature", "baseBranch": "main", "additions": 10, "deletions": 2, "changedFiles": 3,
             "mergeable": "MERGEABLE", "mergeState": "CLEAN",
             "reviewers": [{"name": "alice", "state": "approved"}],
             "checks": [{"name": "check (macos-latest)", "state": "pass", "url": "https://c"}, {"name": "x", "state": "exploded"}],
             "timeline": [{"date": "2026-10-17T12:00:00Z", "author": "alice", "kind": "review · approved", "body": "LGTM", "url": "https://t"}]}
            """#)
        XCTAssertTrue(overview.isOpen)
        XCTAssertEqual(overview.head, "abc")
        XCTAssertEqual(overview.checks.map(\.state), [.pass, .unknown])
        XCTAssertEqual(overview.reviewers.first?.state, "approved")
        XCTAssertEqual(overview.timeline.first?.kind, "review · approved")
        XCTAssertNotNil(overview.timeline.first?.date)
    }

    func testGuide() throws {
        let guide = try decode(
            Guide.self,
            #"""
            {"status": "ready", "message": null, "session": "1790-abcd", "outdated": true,
             "chapters": [{"category": "regular", "title": "Core", "explanation": "**why**",
               "hunks": [{"file": "src/a.rs", "header": "@@ -1,3 +1,4 @@",
                 "lines": [{"kind": "add", "old": null, "new": 2, "text": "let x = 1;"}, {"kind": "glitter", "text": "?"}]}]},
              {"category": "astrology", "title": "Odd", "explanation": "", "hunks": []}]}
            """#)
        XCTAssertEqual(guide.status, .ready)
        XCTAssertTrue(guide.outdated)
        XCTAssertEqual(guide.chapters.count, 2)
        XCTAssertEqual(guide.chapters[1].category, .regular)
        let lines = guide.chapters[0].hunks[0].lines
        XCTAssertEqual(lines.map(\.kind), [.add, .context])
        XCTAssertNil(lines[0].old)
        XCTAssertEqual(lines[0].new, 2)

        XCTAssertEqual(try decode(Guide.self, #"{"status": "none"}"#).status, .notGenerated)
        XCTAssertEqual(try decode(Guide.self, #"{"status": "brewing"}"#).status, .unknown)
    }

    func testFrames() throws {
        let response = try decode(IncomingFrame.self, #"{"id": 7, "result": {"sessions": []}}"#)
        XCTAssertEqual(response.id, 7)
        XCTAssertNil(response.error)

        let failure = try decode(IncomingFrame.self, #"{"id": 8, "error": "Session not found"}"#)
        XCTAssertEqual(failure.error, "Session not found")
        let nested = try decode(IncomingFrame.self, #"{"id": 9, "error": {"message": "Nope"}}"#)
        XCTAssertEqual(nested.error, "Nope")

        let closed = try decode(IncomingFrame.self, #"{"event": "session.closed", "data": {"id": "s", "message": "Session not found"}}"#)
        guard case .sessionClosed(let id, let message)? = closed.event else { return XCTFail() }
        XCTAssertEqual(id, "s")
        XCTAssertEqual(message, "Session not found")

        let unknown = try decode(IncomingFrame.self, #"{"event": "future.thing", "data": 1}"#)
        guard case .unknown("future.thing")? = unknown.event else { return XCTFail() }

        let welcome = try decode(
            HelloReply.self,
            #"{"type":"welcome","protocol":1,"device":"d","token":null,"server":{"name":"Ali's MacBook Pro","version":"0.20.0","hosts":["Alis-MacBook-Pro.local","192.168.1.20"],"port":51234,"fingerprint":"f"}}"#
        )
        XCTAssertNil(welcome.welcome?.token)
        XCTAssertEqual(welcome.welcome?.server.hosts.count, 2)
        XCTAssertEqual(welcome.welcome?.server.port, 51234)

        let denied = try decode(HelloReply.self, #"{"type":"denied","message":"No longer paired."}"#)
        XCTAssertEqual(denied.type, "denied")
        XCTAssertEqual(denied.message, "No longer paired.")
    }

    func testAPIEncodesParams() async throws {
        let recorder = Recorder()
        let api = DifuAPI { method, params in
            await recorder.record(method, params)
            return Data(#"{"id": 1, "result": {"id": "new-session"}}"#.utf8)
        }
        try await api.send("s", text: "hi", queue: true, attachments: [Attachment(name: "p.jpg", bytes: Data([1, 2, 3]))])
        try await api.answer("s", request: .int(3), question: "difu-decision", answer: nil)
        let id = try await api.newSession(repository: nil, model: "m", effort: nil, text: "")
        XCTAssertEqual(id, "new-session")

        let calls = await recorder.calls
        XCTAssertEqual(calls[0].0, "session.send")
        XCTAssertEqual(calls[0].1["queue"], .bool(true))
        XCTAssertEqual(calls[0].1["attachments"], [["name": "p.jpg", "data": "AQID"]])
        XCTAssertEqual(calls[1].1["answer"], .null)
        XCTAssertEqual(calls[1].1["request"], .int(3))
        XCTAssertEqual(calls[2].0, "agents.new")
        XCTAssertEqual(calls[2].1["repository"], .null)
        XCTAssertNil(calls[2].1["text"], "an empty first message is omitted")
    }
}

actor Recorder {
    var calls: [(String, [String: JSONValue])] = []

    func record(_ method: String, _ params: [String: JSONValue]) {
        calls.append((method, params))
    }
}
