import XCTest

@testable import DifuKit

private let commandsJSON = """
    {"commands": [
      {"name": "compact", "trigger": "/compact", "description": "Compact the current conversation",
       "hint": null, "kind": "action", "screen": null, "confirm": false, "arguments": "none", "source": "difu"},
      {"name": "model", "trigger": "/model", "description": "Choose a model", "hint": "[model] [effort]",
       "kind": "screen", "screen": "model", "confirm": false, "arguments": "optional", "source": "difu"},
      {"name": "rename", "trigger": "/rename", "description": "Rename this session", "hint": "[title]",
       "kind": "screen", "screen": "rename", "confirm": false, "arguments": "optional", "source": "difu"},
      {"name": "skills", "trigger": "/skills", "description": "Browse skills for this workspace",
       "kind": "screen", "screen": "skills", "arguments": "none", "source": "difu"},
      {"name": "clear", "trigger": "/clear", "description": "Start over: fresh worktree from main",
       "kind": "action", "confirm": true, "arguments": "none", "source": "difu"},
      {"name": "repo", "trigger": "/repo", "description": "Change repository", "hint": "<path>",
       "kind": "action", "confirm": true, "arguments": "required", "source": "difu"},
      {"name": "review", "trigger": "/review", "description": "Review a pull request", "hint": "[pr]",
       "kind": "prompt", "arguments": "optional", "source": "claude"},
      {"name": "lint", "trigger": "$lint", "description": "Lint the repository",
       "kind": "prompt", "arguments": "optional", "source": "codex"},
      {"trigger": "/nameless"},
      {"name": "future", "kind": "teleport", "screen": "hologram", "arguments": "maybe"}
    ], "errors": ["Skill discovery: bad SKILL.md"]}
    """

private func model(_ id: String, _ provider: String?, _ efforts: [String]) throws -> AgentModel {
    var fields = "\"id\": \"\(id)\", \"efforts\": [\(efforts.map { "\"\($0)\"" }.joined(separator: ","))]"
    if let provider { fields += ", \"provider\": \"\(provider)\"" }
    return try decode(AgentModel.self, "{\(fields)}")
}

private func session(
    provider: String = "codex", kind: String = "Coding", status: String = "Idle",
    active: Bool = false, betweenTurns: Bool? = nil, model: String? = nil, effort: String? = nil,
    queue: Int = 0
) throws -> SessionView {
    var json: [String: Any] = [
        "id": "s1", "title": "t", "kind": kind, "status": status, "active": active,
        "provider": provider, "version": 1, "entries": [],
        "queue": (0..<queue).map { ["index": $0, "text": "later"] },
    ]
    if let betweenTurns { json["betweenTurns"] = betweenTurns }
    if let model { json["model"] = model }
    if let effort { json["effort"] = effort }
    let data = try JSONSerialization.data(withJSONObject: json)
    return try JSONDecoder().decode(SessionView.self, from: data)
}

final class SessionCommandDecodingTests: XCTestCase {
    func testCommandListDecodesKindsScreensAndSkipsBrokenRows() throws {
        let list = try decode(SessionCommandList.self, commandsJSON)
        XCTAssertFalse(list.isFallback)
        XCTAssertEqual(list.errors, ["Skill discovery: bad SKILL.md"])
        XCTAssertEqual(
            list.commands.map(\.trigger),
            ["/compact", "/model", "/rename", "/skills", "/clear", "/repo", "/review", "$lint", "/future"])
        let model = try XCTUnwrap(list.commands.first { $0.name == "model" })
        XCTAssertEqual(model.kind, .screen)
        XCTAssertEqual(model.screen, .model)
        XCTAssertEqual(model.hint, "[model] [effort]")
        XCTAssertEqual(model.arguments, .optional)
        XCTAssertTrue(model.isDifu)
        let clear = try XCTUnwrap(list.commands.first { $0.name == "clear" })
        XCTAssertTrue(clear.confirm)
        let lint = try XCTUnwrap(list.commands.first { $0.name == "lint" })
        XCTAssertTrue(lint.isAttachedSkill)
        XCTAssertEqual(lint.source, "codex")
        let review = try XCTUnwrap(list.commands.first { $0.name == "review" })
        XCTAssertFalse(review.isAttachedSkill)
        let future = try XCTUnwrap(list.commands.last)
        XCTAssertEqual(future.kind, .prompt, "unknown kinds are inserted, never run")
        XCTAssertEqual(future.screen, .unknown)
        XCTAssertEqual(future.arguments, .optional)
    }

    func testOutcomesCarryScreensUsageStatusAndPermissions() throws {
        let usage = try decode(
            CommandOutcome.self,
            """
            {"screen": "usage", "usage": {"provider": "codex", "title": "OpenAI Codex · Usage",
              "limits": [{"section": "Codex", "title": "Weekly limit", "percent": 96.0, "remaining": true,
                          "resetsAt": 1800000000000}],
              "details": [{"section": null, "label": "Account", "value": "pro"}]}}
            """)
        XCTAssertEqual(usage.screen, .usage)
        let limit = try XCTUnwrap(usage.usage?.limits.first)
        XCTAssertEqual(limit.caption, "96% left")
        XCTAssertEqual(limit.section, "Codex")
        XCTAssertEqual(limit.resetsAt, 1_800_000_000_000)
        XCTAssertEqual(usage.usage?.details.first?.label, "Account")

        let status = try decode(
            CommandOutcome.self,
            """
            {"screen": "status", "status": {"title": "Fix login", "provider": "claude",
              "providerLabel": "Claude Code", "model": null, "effort": "high", "state": "Ready",
              "repository": "/repo", "workspace": "/worktree", "branch": "b", "providerSession": null,
              "mode": "Plan", "preset": null}}
            """)
        XCTAssertEqual(status.status?.providerLabel, "Claude Code")
        XCTAssertEqual(status.status?.mode, "Plan")
        XCTAssertNil(status.status?.model)

        let permissions = try decode(
            CommandOutcome.self,
            """
            {"screen": "permissions", "permissions": {"provider": "codex", "activeMode": "default",
              "pendingMode": "plan", "activePreset": "Auto", "pendingPreset": null, "reloadPending": false,
              "modes": [{"id": "default", "label": "Regular", "description": "", "available": true, "reason": null},
                        {"id": "plan", "label": "Plan", "description": "", "available": false,
                         "reason": "Provider has not reported support for this mode"}],
              "presets": [{"id": "Auto", "label": "Approve for me", "description": "", "available": true}],
              "message": null}}
            """)
        let info = try XCTUnwrap(permissions.permissions)
        XCTAssertEqual(info.selectedMode, "plan")
        XCTAssertEqual(info.selectedPreset, "Auto")
        XCTAssertEqual(info.modes.last?.available, false)
        XCTAssertEqual(info.presets.first?.label, "Approve for me")

        let done = try decode(CommandOutcome.self, #"{"message": "Compacting the conversation"}"#)
        XCTAssertEqual(done.message, "Compacting the conversation")
        XCTAssertNil(done.screen)
    }

    func testSessionViewsSayWhetherTheyAreBetweenTurns() throws {
        XCTAssertEqual(try session(betweenTurns: false).betweenTurns, false)
        XCTAssertFalse(try session(betweenTurns: false).isBetweenTurns)
        // Older computers: estimated from the state.
        XCTAssertTrue(try session().isBetweenTurns)
        XCTAssertFalse(try session(status: "Running", active: true).isBetweenTurns)
        XCTAssertFalse(try session(queue: 1).isBetweenTurns)
    }

    func testAPISendsCommandsSkillsAndConfirmation() async throws {
        let recorder = CallRecorder(result: #"{"message": "Started over"}"#)
        let api = DifuAPI(call: recorder.call)
        let outcome = try await api.runCommand("s1", name: "/clear", confirm: true)
        XCTAssertEqual(outcome.message, "Started over")
        try await api.send("s1", text: "$lint docs", skills: ["lint"])
        let calls = await recorder.calls
        XCTAssertEqual(calls.first?.method, "session.command")
        XCTAssertEqual(calls.first?.params["confirm"], .bool(true))
        XCTAssertEqual(calls.first?.params["name"], .string("/clear"))
        XCTAssertNil(calls.first?.params["args"])
        XCTAssertEqual(calls.last?.method, "session.send")
        XCTAssertEqual(calls.last?.params["skills"], .array([.string("lint")]))
    }
}

/// Records calls and answers each with the same result.
private actor CallRecorder {
    struct Call {
        var method: String
        var params: [String: JSONValue]
    }

    private(set) var calls: [Call] = []
    private let result: String

    init(result: String) {
        self.result = result
    }

    nonisolated var call: DifuAPI.Call {
        { method, params in await self.record(method, params) }
    }

    private func record(_ method: String, _ params: [String: JSONValue]) -> Data {
        calls.append(Call(method: method, params: params))
        return Data(#"{"id": 1, "result": \#(result)}"#.utf8)
    }
}

final class SlashCommandTests: XCTestCase {
    private var commands: [SessionCommand] = []

    override func setUpWithError() throws {
        commands = try decode(SessionCommandList.self, commandsJSON).commands
    }

    private func names(_ query: String, skillsOnly: Bool = false) throws -> [String] {
        let query = try XCTUnwrap(SlashCommands.query(query))
        return SlashCommands.matches(commands, query: query, skillsOnly: skillsOnly).map(\.trigger)
    }

    private func command(_ name: String) throws -> SessionCommand {
        try XCTUnwrap(commands.first { $0.name == name })
    }

    func testTheListShowsOnlyForASlashAtTheStart() {
        XCTAssertNil(SlashCommands.query("hello /compact"))
        XCTAssertNil(SlashCommands.query("/compact\nmore"))
        XCTAssertEqual(SlashCommands.query("/"), .init(name: ""))
        XCTAssertEqual(SlashCommands.query("/mo"), .init(name: "mo"))
        XCTAssertEqual(SlashCommands.query("/model  gpt-5.6 "), .init(name: "model", arguments: "gpt-5.6"))
        XCTAssertEqual(SlashCommands.query("/rename "), .init(name: "rename", arguments: ""))
    }

    func testFilteringRanksNamePrefixesThenNamesThenDescriptions() throws {
        XCTAssertEqual(try names("/").count, commands.count)
        XCTAssertEqual(
            try names("/re"),
            ["/rename", "/repo", "/review", "/future", "/compact", "/clear", "$lint"],
            "name prefixes, then names, then descriptions")
        XCTAssertEqual(try names("/REV"), ["/review"])
        XCTAssertEqual(try names("/lint"), ["$lint"], "Codex skills are found by name")
        XCTAssertEqual(try names("/conversation"), ["/compact"])
        XCTAssertEqual(try names("/model gpt"), ["/model"], "arguments narrow to the named command")
        XCTAssertEqual(try names("/mod gpt"), [])
        XCTAssertEqual(try names("/", skillsOnly: true), ["/review", "$lint"])
    }

    func testChoosingRunsOpensOrInserts() throws {
        XCTAssertEqual(
            SlashCommands.choose(try command("compact"), text: "/comp"),
            .run(try command("compact"), arguments: ""))
        XCTAssertEqual(
            SlashCommands.choose(try command("clear"), text: "/cl"),
            .run(try command("clear"), arguments: ""), "the view asks before running it")
        XCTAssertEqual(SlashCommands.choose(try command("repo"), text: "/re"), .insert("/repo "))
        XCTAssertEqual(
            SlashCommands.choose(try command("repo"), text: "/repo owner/app"),
            .run(try command("repo"), arguments: "owner/app"))
        XCTAssertEqual(
            SlashCommands.choose(try command("model"), text: "/mo"), .open(.model, try command("model")))
        XCTAssertEqual(
            SlashCommands.choose(try command("model"), text: "/model gpt-5.6 low"),
            .run(try command("model"), arguments: "gpt-5.6 low"))
        XCTAssertEqual(
            SlashCommands.choose(try command("skills"), text: "/sk"), .open(.skills, try command("skills")))
        XCTAssertEqual(SlashCommands.choose(try command("review"), text: "/rev"), .insert("/review "))
        XCTAssertEqual(SlashCommands.choose(try command("review"), text: "/review 45"), .insert("/review 45"))
        XCTAssertEqual(SlashCommands.choose(try command("lint"), text: "/li"), .insert("$lint "))
    }

    func testSendingRunsDifuCommandsAndAttachesNamedCodexSkills() throws {
        XCTAssertEqual(
            SlashCommands.submission(" /rename  Fix login ", commands: commands),
            .command(try command("rename"), arguments: "Fix login"))
        XCTAssertEqual(
            SlashCommands.submission("/Compact", commands: commands),
            .command(try command("compact"), arguments: ""))
        XCTAssertEqual(
            SlashCommands.submission("/review 45", commands: commands),
            .message("/review 45", skills: []), "Claude Code runs its commands from the text")
        XCTAssertEqual(
            SlashCommands.submission("$lint the docs, then $lint again", commands: commands),
            .message("$lint the docs, then $lint again", skills: ["lint"]))
        XCTAssertEqual(
            SlashCommands.submission("explain $linting", commands: commands),
            .message("explain $linting", skills: []))
        XCTAssertEqual(
            SlashCommands.submission("/unknown thing", commands: commands),
            .message("/unknown thing", skills: []))
    }

    func testOlderComputersGetCommandsExistingMethodsRun() {
        let fallback = SessionCommandList.fallback
        XCTAssertTrue(fallback.isFallback)
        XCTAssertEqual(
            fallback.commands.map(\.trigger),
            ["/compact", "/model", "/effort", "/diff", "/new", "/rename", "/clear"])
        XCTAssertTrue(fallback.commands.allSatisfy(\.isDifu))
        XCTAssertEqual(fallback.commands.last?.confirm, true)
    }
}

final class ModelPickerTests: XCTestCase {
    private var models: [AgentModel] = []

    override func setUpWithError() throws {
        models = [
            try model("gpt-5.6", "codex", ["low", "medium", "high", "xhigh"]),
            try model("gpt-5.5-mini", "codex", ["low", "medium"]),
            try model("claude/opus", "claude", ["low", "high", "max"]),
            try model("claude/sonnet", "claude", ["low", "high"]),
        ]
    }

    func testEveryProvidersModelsAreGroupedWithTheSessionsFirst() {
        let groups = ModelPicker.groups(models, first: "claude")
        XCTAssertEqual(groups.map(\.title), ["Claude Code", "Codex"])
        XCTAssertEqual(groups.first?.models.map(\.id), ["claude/opus", "claude/sonnet"])
        XCTAssertEqual(ModelPicker.groups(models).map(\.provider), ["codex", "claude"])
    }

    func testProvidersComeFromTheListOrTheModelName() {
        XCTAssertEqual(ModelPicker.provider(of: "claude/opus", in: models), "claude")
        XCTAssertEqual(ModelPicker.provider(of: "claude-sonnet-5", in: []), "claude")
        XCTAssertEqual(ModelPicker.provider(of: "opus", in: []), "claude")
        XCTAssertEqual(ModelPicker.provider(of: "gpt-7", in: []), "codex")
    }

    func testEffortsFollowTheChosenModel() {
        XCTAssertEqual(ModelPicker.efforts(models, model: "claude/opus", provider: "codex"), ["low", "high", "max"])
        XCTAssertEqual(
            ModelPicker.efforts(models, model: nil, provider: "codex"), ["low", "medium", "high", "xhigh"])
        XCTAssertEqual(ModelPicker.efforts(models, model: nil, provider: nil).last, "max")
        XCTAssertEqual(
            ModelPicker.syncedEffort("xhigh", models: models, model: "claude/opus", provider: "codex"), nil)
        XCTAssertEqual(
            ModelPicker.syncedEffort("high", models: models, model: "claude/opus", provider: "codex"), "high")
        XCTAssertEqual(
            ModelPicker.syncedEffort("max", models: models, model: nil, provider: "claude"), "max")
        XCTAssertNil(ModelPicker.syncedEffort(nil, models: models, model: "gpt-5.6", provider: nil))
    }

    func testSwitchingProvidersNeedsAnIdleCodingSession() throws {
        let idle = try session(model: "gpt-5.6", effort: "high")
        XCTAssertEqual(
            ModelChange.evaluate(session: idle, model: "gpt-5.6", effort: "high", models: models), .unchanged)
        XCTAssertEqual(
            ModelChange.evaluate(session: idle, model: "gpt-5.5-mini", effort: "low", models: models), .allowed)
        XCTAssertEqual(
            ModelChange.evaluate(session: idle, model: "claude/opus", effort: nil, models: models),
            .switchesProvider(from: "codex", to: "claude"))
        XCTAssertTrue(
            ModelChange.evaluate(session: idle, model: "claude/opus", effort: nil, models: models).canSave)

        let working = try session(status: "Running", active: true, betweenTurns: false, model: "gpt-5.6")
        XCTAssertEqual(
            ModelChange.evaluate(session: working, model: "gpt-5.5-mini", effort: nil, models: models), .allowed,
            "Codex changes models while it works")
        guard
            case .blocked(let reason) = ModelChange.evaluate(
                session: working, model: "claude/opus", effort: nil, models: models)
        else { return XCTFail("switching while working is refused") }
        XCTAssertTrue(reason.contains("Finish the current turn and pending requests"))

        let claude = try session(provider: "claude", betweenTurns: false, model: "claude/opus")
        guard
            case .blocked(let claudeReason) = ModelChange.evaluate(
                session: claude, model: "claude/opus", effort: "max", models: models)
        else { return XCTFail("Claude Code changes models between turns") }
        XCTAssertTrue(claudeReason.contains("between turns"))

        let orchestrator = try session(kind: "Orchestrator")
        XCTAssertEqual(
            ModelChange.evaluate(session: orchestrator, model: "claude/opus", effort: nil, models: models),
            .blocked("Only coding sessions can switch providers."))
    }
}
