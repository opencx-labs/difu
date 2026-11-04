import XCTest

@testable import DifuKit

final class SessionResourcesTests: XCTestCase {
    func testSummariesCountResourcesAndOlderComputersSendNone() throws {
        let summary = try decode(
            SessionSummary.self,
            """
            {"id": "s1", "title": "Fix", "artifacts": 2, "shells": 1, "subagents": 3,
             "runningSubagents": 1, "folder": "auth", "movedTo": "box:s1"}
            """)
        XCTAssertEqual(summary.artifacts, 2)
        XCTAssertEqual(summary.shells, 1)
        XCTAssertEqual(summary.subagents, 3)
        XCTAssertEqual(summary.runningSubagents, 1)
        XCTAssertEqual(summary.folder, "auth")
        XCTAssertEqual(summary.movedTo, "box:s1")
        let old = try decode(SessionSummary.self, summaryJSON)
        XCTAssertEqual(old.artifacts, 0)
        XCTAssertEqual(old.shells, 0)
        XCTAssertEqual(old.subagents, 0)
        XCTAssertNil(old.folder)
        XCTAssertNil(old.movedTo)
    }

    func testViewsListArtifactsSubAgentsAndQueueEdits() throws {
        let view = try decode(
            SessionView.self,
            """
            {"id": "s1", "title": "Fix", "version": 3, "shells": 2,
             "queue": [{"index": 0, "text": "short", "truncated": false},
                       {"index": 1, "text": "long…", "truncated": true}],
             "artifacts": [{"title": "Report", "path": "out/report.html",
                            "workspace": "/w", "name": "report.html"},
                           {"path": "plain.html"}, {"title": "no path"}],
             "subagents": [{"id": "claude:a", "name": "Explore", "task": "Find it",
                            "title": "Explore · Find it", "state": "Running",
                            "stateLabel": "Running", "active": true, "provider": "claude",
                            "providerLabel": "Claude Code", "background": false, "entries": 4},
                           {"id": "codex:b", "state": "Completed", "provider": "codex"}],
             "entries": []}
            """)
        XCTAssertEqual(view.artifacts.map(\.title), ["Report", "plain.html"])
        XCTAssertEqual(view.artifacts.first?.workspace, "/w")
        XCTAssertEqual(view.artifacts.last?.name, "plain.html")
        XCTAssertNotEqual(view.artifacts.first?.id, view.artifacts.last?.id)
        XCTAssertEqual(view.summary.artifacts, 2)
        XCTAssertEqual(view.summary.shells, 2)
        XCTAssertEqual(view.summary.subagents, 2)
        XCTAssertEqual(view.summary.runningSubagents, 1)
        let first = try XCTUnwrap(view.subagents.first)
        XCTAssertEqual(first.title, "Explore · Find it")
        XCTAssertTrue(first.active)
        XCTAssertEqual(first.providerLabel, "Claude Code")
        XCTAssertEqual(first.entries, 4)
        let second = try XCTUnwrap(view.subagents.last)
        XCTAssertFalse(second.active)
        XCTAssertEqual(second.providerLabel, "Codex")
        XCTAssertEqual(second.title, "Sub-agent")
        XCTAssertEqual(view.queue.map(\.truncated), [false, true])
    }

    func testShellsAndTheirOutputDecode() throws {
        let shell = try decode(
            ShellInfo.self,
            """
            {"id": "difu-shell-1", "title": "Interactive shell", "kind": "shell", "running": true,
             "exitCode": null, "cwd": "/w", "revision": 12, "humanFocused": false,
             "canWrite": true, "canClose": true}
            """)
        XCTAssertEqual(shell.kind, .shell)
        XCTAssertEqual(shell.revision, 12)
        XCTAssertEqual(shell.stateLabel, "Running")
        let finished = try decode(
            ShellInfo.self, #"{"id": "c1", "kind": "command", "running": false, "exitCode": 2}"#)
        XCTAssertEqual(finished.stateLabel, "Exited 2")
        XCTAssertFalse(finished.canWrite)
        XCTAssertEqual(try decode(ShellInfo.self, #"{"id": "x", "kind": "future"}"#).kind, .background)
        XCTAssertThrowsError(try decode(ShellInfo.self, #"{"title": "no id"}"#))

        let read = try decode(
            ShellOutput.self,
            #"{"shell": {"id": "s", "kind": "shell"}, "output": "$ ls\nREADME", "truncated": true}"#)
        XCTAssertEqual(read.output, "$ ls\nREADME")
        XCTAssertTrue(read.truncated)
        XCTAssertFalse(read.unchanged)
        let same = try decode(ShellOutput.self, #"{"shell": {"id": "s"}, "unchanged": true}"#)
        XCTAssertTrue(same.unchanged)
        XCTAssertNil(same.output)
    }

    func testArtifactFilesAndTranscriptsDecode() throws {
        let file = try decode(
            ArtifactFile.self,
            #"{"name": "report.html", "mime": "text/html", "size": 15, "data": "PGgxPlJlcG9ydDwvaDE+"}"#)
        XCTAssertEqual(file.html, "<h1>Report</h1>")
        XCTAssertThrowsError(try decode(ArtifactFile.self, #"{"data": "%%%"}"#))

        let transcript = try decode(
            SubagentTranscript.self,
            """
            {"subagent": {"id": "claude:a", "state": "Completed"}, "earlier": 5, "error": "offline",
             "entries": [{"id": "a1", "kind": "agentMessage", "role": "assistant", "text": "Done"}, 7]}
            """)
        XCTAssertEqual(transcript.entries.count, 2, "bad rows become placeholders")
        XCTAssertEqual(transcript.entries.first?.text, "Done")
        XCTAssertEqual(transcript.earlier, 5)
        XCTAssertEqual(transcript.error, "offline")
    }

    func testFoldersArriveWithTheListAndAsEvents() throws {
        let list = try decode(
            SessionList.self,
            """
            {"sessions": [{"id": "a"}], "folders": [{"id": "auth", "name": "Auth", "sessions": ["a"]},
                                                     {"name": "no id"}]}
            """)
        XCTAssertEqual(list.sessions.map(\.id), ["a"])
        XCTAssertEqual(list.folders, [SessionFolder(id: "auth", name: "Auth", sessions: ["a"])])
        XCTAssertNil(try decode(SessionList.self, #"{"sessions": []}"#).folders, "older computers")

        let frame = try decode(
            IncomingFrame.self, #"{"event": "folders", "data": [{"id": "x", "name": "X"}]}"#)
        guard case .folders(let folders)? = frame.event else {
            return XCTFail("expected a folders event")
        }
        XCTAssertEqual(folders.map(\.name), ["X"])
        let targets = try decode(
            [Lossy<MoveTarget>].self, #"[{"name": "box", "connected": true, "problem": ""}, {"connected": true}]"#
        ).compactMap(\.value)
        XCTAssertEqual(targets.map(\.name), ["box"])
        XCTAssertNil(targets.first?.problem)
    }

    private func summary(_ id: String, orchestrator: Bool = false, folder: String? = nil) throws -> SessionSummary {
        var json: [String: JSONValue] = ["id": .string(id), "title": .string(id.uppercased())]
        if orchestrator { json["orchestrator"] = .bool(true) }
        if let folder { json["folder"] = .string(folder) }
        return try decode(SessionSummary.self, JSONValue.object(json).jsonString())
    }

    func testGroupingFilesSessionsLikeTheTerminal() throws {
        let rows = try [
            summary("orch", orchestrator: true), summary("a"), summary("b"), summary("c", folder: "later"),
            summary("d", folder: "gone"),
        ]
        let folders = [
            SessionFolder(id: "auth", name: "Auth", sessions: ["b", "orch"]),
            SessionFolder(id: "later", name: "Later"),
            SessionFolder(id: "empty", name: "Empty"),
        ]
        let active = SessionGrouping.arrange(rows, folders: folders, archived: false) { $0 }
        XCTAssertEqual(active.loose.map(\.id), ["orch", "a", "d"], "orchestrator never filed; unknown folder ignored")
        XCTAssertEqual(active.groups.map(\.folder.id), ["auth", "later", "empty"])
        XCTAssertEqual(active.groups.map { $0.items.map(\.id) }, [["b"], ["c"], []])
        let archived = SessionGrouping.arrange(rows, folders: folders, archived: true) { $0 }
        XCTAssertEqual(archived.groups.map(\.folder.id), ["auth", "later"], "empty folders hide when archived")
    }

    func testSearchMatchesTitleRepositoryServerAndFolder() throws {
        let row = try decode(
            SessionSummary.self,
            #"{"id": "s1", "title": "Fix login", "repository": "difu", "repositoryPath": "/code/difu", "branch": "feat/x"}"#)
        XCTAssertTrue(SessionGrouping.matches(row, query: ""))
        XCTAssertTrue(SessionGrouping.matches(row, query: "LOGIN"))
        XCTAssertTrue(SessionGrouping.matches(row, query: "/code"))
        XCTAssertTrue(SessionGrouping.matches(row, query: "feat"))
        XCTAssertTrue(SessionGrouping.matches(row, query: "box", extra: ["Box server", nil]))
        XCTAssertTrue(SessionGrouping.matches(row, query: "auth", extra: [nil, "Auth"]))
        XCTAssertFalse(SessionGrouping.matches(row, query: "nothing"))
    }

    func testFolderDetailsSummarizeWhatSessionsDo() throws {
        let working = try decode(SessionSummary.self, #"{"id": "a", "status": "Running"}"#)
        let asking = try decode(SessionSummary.self, #"{"id": "b", "status": "Running", "pending": 2}"#)
        let idle = try decode(SessionSummary.self, #"{"id": "c", "status": "Idle"}"#)
        XCTAssertEqual(SessionGrouping.folderDetail([]), "Empty")
        XCTAssertEqual(SessionGrouping.folderDetail([idle]), "1 session")
        XCTAssertEqual(
            SessionGrouping.folderDetail([working, asking, idle]), "3 sessions · 1 working · 1 needs input")
    }

    func testRowBadgesFollowTheTerminalsOrder() throws {
        let row = try decode(
            SessionSummary.self,
            """
            {"id": "s", "pending": 2, "queued": 1, "artifacts": 1, "shells": 2, "subagents": 3,
             "runningSubagents": 1, "worktree": true, "archived": true}
            """)
        let badges = SessionBadge.row(row, onServer: true)
        XCTAssertEqual(
            badges,
            [.pending(2), .artifacts(1), .shells(2), .subagents(running: 1, total: 3), .worktree, .cloud, .archived])
        XCTAssertEqual(SessionBadge.subagents(running: 1, total: 3).text, "1/3")
        XCTAssertEqual(SessionBadge.subagents(running: 0, total: 3).text, "3")
        XCTAssertEqual(SessionBadge.subagents(running: 0, total: 1).accessibilityLabel, "1 sub-agent")
        let queued = try decode(SessionSummary.self, #"{"id": "q", "queued": 1, "movedTo": "box:q", "archived": true}"#)
        XCTAssertEqual(SessionBadge.row(queued, onServer: false), [.queued(1), .moved("box:q")])
        XCTAssertEqual(SessionBadge.resources(queued), [])
    }

    func testShellKeysAndPolling() {
        XCTAssertEqual(ShellKey.interrupt.sequence, "\u{03}")
        XCTAssertEqual(ShellKey.up.sequence, "\u{1B}[A")
        XCTAssertNil(ShellPolling.next(running: false, sinceChange: 0, sinceInput: 0))
        XCTAssertEqual(ShellPolling.next(running: true, sinceChange: 100, sinceInput: 1), ShellPolling.fast)
        XCTAssertEqual(ShellPolling.next(running: true, sinceChange: 2, sinceInput: 100), ShellPolling.fast)
        XCTAssertEqual(ShellPolling.next(running: true, sinceChange: 30, sinceInput: 100), ShellPolling.normal)
        XCTAssertEqual(ShellPolling.next(running: true, sinceChange: 300, sinceInput: 300), ShellPolling.idle)
    }

    func testTerminalTextDropsEscapes() {
        XCTAssertEqual(TerminalText.plain("plain"), "plain")
        XCTAssertEqual(TerminalText.plain("\u{1B}[1;32mok\u{1B}[0m done"), "ok done")
        XCTAssertEqual(TerminalText.plain("\u{1B}]0;title\u{07}prompt$ "), "prompt$ ")
        XCTAssertEqual(TerminalText.plain("a\r\nb"), "a\nb")
        XCTAssertEqual(TerminalText.plain("10%\r50%\r100%\ndone"), "100%\ndone")
    }

    func testResourceMethodsSendTheProtocolsParameters() async throws {
        let recorder = CallRecorder()
        let api = DifuAPI { method, params in
            await recorder.record(method, params)
            let result: String
            switch method {
            case "session.shell.open", "session.move": result = #"{"id": "new"}"#
            case "folders.create", "folders.move": result = #"{"folders": [{"id": "f", "name": "F"}]}"#
            default: result = "{}"
            }
            return Data(#"{"id": 1, "result": \#(result)}"#.utf8)
        }
        let artifact = SessionArtifact(title: "R", path: "out/r.html", workspace: nil)
        _ = try? await api.artifact("s", artifact: artifact)
        let opened = try await api.openShell("s", command: "  ls ")
        XCTAssertEqual(opened, "new")
        try await api.writeShell("s", shell: "sh", text: ShellKey.interrupt.sequence)
        let filed = try await api.moveToFolder(["a"], folder: nil)
        XCTAssertEqual(filed.map(\.id), ["f"])
        let moved = try await api.move("s", to: "box")
        XCTAssertEqual(moved, "new")
        let message = try decode(QueuedMessage.self, #"{"index": 1, "text": "Old"}"#)
        try await api.editQueued("s", message: message, text: "New")
        let calls = await recorder.calls
        XCTAssertEqual(
            calls.map(\.0),
            ["session.artifact", "session.shell.open", "session.shell.write", "folders.move", "session.move",
             "session.editQueued"])
        XCTAssertEqual(calls[0].1["workspace"], .null)
        XCTAssertEqual(calls[1].1["command"], .string("ls"))
        XCTAssertEqual(calls[2].1["text"], .string("\u{03}"))
        XCTAssertEqual(calls[2].1["enter"], .bool(false))
        XCTAssertEqual(calls[3].1["folder"], .null)
        XCTAssertEqual(calls[3].1["ids"], .array([.string("a")]))
        XCTAssertEqual(calls[5].1["expected"], .string("Old"))
        XCTAssertEqual(calls[5].1["index"], .int(1))
        XCTAssertEqual(RemoteConnection.timeout(for: "session.move"), 15 * 60)
    }
}

private actor CallRecorder {
    var calls: [(String, [String: JSONValue])] = []

    func record(_ method: String, _ params: [String: JSONValue]) {
        calls.append((method, params))
    }
}
