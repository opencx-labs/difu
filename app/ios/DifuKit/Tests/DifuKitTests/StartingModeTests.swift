import XCTest

@testable import DifuKit

/// `agents.defaults` from a server with nothing saved, as difu 0.38 reports it.
private let serverDefaultsJSON = """
    {"provider": "codex", "savedProvider": null, "model": null, "effort": null, "repository": "/srv/repos/acme/api",
     "startingMode": null, "codexPreset": null, "isolated": true, "server": true,
     "providers": [
       {"id": "codex", "label": "Codex",
        "startingModes": [{"id": "default", "label": "Regular", "description": "Native coding mode"},
                          {"id": "plan", "label": "Plan", "description": "Native planning mode"}],
        "presets": [{"id": "Ask", "label": "Ask for approval"}, {"id": "Auto", "label": "Approve for me"},
                    {"id": "Full", "label": "Full Access"}, {"id": "ReadOnly", "label": "Read Only"}],
        "startingMode": null, "startingModeAutomatic": false, "codexPreset": "Auto", "codexPresetAutomatic": true},
       {"id": "claude", "label": "Claude Code",
        "startingModes": [{"id": "default", "label": "Manual"}, {"id": "acceptEdits", "label": "Accept edits"},
                          {"id": "plan", "label": "Plan"}, {"id": "bypassPermissions", "label": "Bypass permissions"},
                          {"id": "auto", "label": "Auto"}, {"id": "dontAsk", "label": "Don't ask"}],
        "startingMode": "auto", "startingModeAutomatic": true}
     ],
     "effective": {"provider": "claude", "model": "claude/claude-opus-4-7", "effort": "xhigh",
                   "modelAutomatic": true, "effortAutomatic": true}}
    """

private func defaults(_ json: String) throws -> AgentDefaults {
    try decode(AgentDefaults.self, json)
}

private func catalog() throws -> [AgentModel] {
    try decode(
        [AgentModel].self,
        """
        [{"id": "gpt-5.6", "name": "GPT-5.6", "provider": "codex", "efforts": ["low", "high"]},
         {"id": "claude/claude-opus-4-6", "name": "Claude Code · Opus 4.6", "provider": "claude", "efforts": ["high", "xhigh"]},
         {"id": "claude/claude-opus-4-7", "name": "Claude Code · Opus 4.7", "provider": "claude", "efforts": ["high", "xhigh", "max"]}]
        """)
}

final class StartingModeTests: XCTestCase {
    func testServerDefaultsDecodeTheEffectiveChoicesAndEachProvidersStart() throws {
        let server = try defaults(serverDefaultsJSON)
        XCTAssertTrue(server.reportsEffective)
        XCTAssertEqual(server.server, true)
        XCTAssertNil(server.savedProvider)
        XCTAssertTrue(server.automaticModel)
        XCTAssertEqual(
            server.effective,
            EffectiveDefaults(
                provider: "claude", model: "claude/claude-opus-4-7", effort: "xhigh",
                modelAutomatic: true, effortAutomatic: true))
        XCTAssertEqual(server.codexPresets.map(\.id), ["Ask", "Auto", "Full", "ReadOnly"])
        XCTAssertEqual(server.entry(for: "codex")?.codexPreset, "Auto")
        XCTAssertEqual(server.entry(for: "claude")?.startingMode, "auto")
        XCTAssertEqual(server.summary, "Claude Code · claude-opus-4-7 · xhigh · automatic")
        XCTAssertEqual(server.automaticModeLabel(for: "claude"), "Auto")
        XCTAssertEqual(server.automaticModeLabel(for: "codex"), "Regular")
        XCTAssertEqual(server.automaticPresetLabel, "Approve for me")

        // Older computers report none of it.
        let older = try defaults(#"{"provider": "codex", "model": null, "providers": [{"id": "codex"}]}"#)
        XCTAssertFalse(older.reportsEffective)
        XCTAssertNil(older.server)
        XCTAssertFalse(older.automaticModel)
        XCTAssertEqual(older.codexPresets, [])
        XCTAssertNil(NewSessionDefaults.start(for: "claude", defaults: older))
        XCTAssertEqual(older.summary, "Codex · Default model")

        // A computer: nothing automatic about modes.
        let computer = try defaults(
            """
            {"provider": "claude", "savedProvider": "claude", "server": false, "codexPreset": "Ask",
             "providers": [{"id": "codex", "startingModes": [{"id": "default", "label": "Regular"}], "presets": [{"id": "Ask", "label": "Ask for approval"}],
                            "codexPreset": "Ask", "codexPresetAutomatic": false},
                           {"id": "claude", "startingModes": [{"id": "default", "label": "Manual"}, {"id": "auto", "label": "Auto"}]}],
             "effective": {"provider": "claude", "model": null, "effort": null}}
            """)
        XCTAssertEqual(computer.savedProvider, "claude")
        XCTAssertEqual(computer.codexPreset, "Ask")
        XCTAssertFalse(computer.automaticModel)
        XCTAssertEqual(computer.automaticModeLabel(for: "claude"), "Manual")
        XCTAssertEqual(computer.automaticPresetLabel, "Codex's configured approvals")
        XCTAssertEqual(computer.summary, "Claude Code · Default model")
    }

    func testTheSheetUsesTheServersEffectiveChoiceAndFallsBackOnlyForOlderComputers() throws {
        let models = try catalog()
        let server = try defaults(serverDefaultsJSON)
        XCTAssertEqual(
            NewSessionDefaults.choice(saved: server, models: nil),
            .init(model: "claude/claude-opus-4-7", effort: "xhigh"),
            "the server's choice doesn't wait for the models")
        // The server decides, even when it settles on Codex's configured model.
        let codex = try defaults(
            #"{"provider": "codex", "savedProvider": "codex", "effective": {"provider": "codex", "model": null, "effort": "high"}}"#)
        XCTAssertEqual(NewSessionDefaults.choice(saved: codex, models: models), .init(model: nil, effort: "high"))
        // Older computers: the phone's own Opus preselection.
        let older = try defaults(#"{"provider": "codex"}"#)
        XCTAssertEqual(
            NewSessionDefaults.choice(saved: older, models: models),
            .init(model: "claude/claude-opus-4-7", effort: "xhigh"))
        XCTAssertEqual(
            NewSessionDefaults.automaticChoice(models: models), .init(model: "claude/claude-opus-4-7", effort: "xhigh"))
        XCTAssertEqual(NewSessionDefaults.automaticChoice(models: [models[0]]), .init(model: nil, effort: nil))
        XCTAssertEqual(NewSessionDefaults.automaticChoice(models: nil), .init(model: nil, effort: nil))

        XCTAssertEqual(NewSessionDefaults.provider(model: nil, models: models, defaults: server), "claude")
        XCTAssertEqual(NewSessionDefaults.provider(model: "gpt-5.6", models: models, defaults: server), "codex")
        XCTAssertEqual(NewSessionDefaults.provider(model: nil, models: nil, defaults: older), "codex")
    }

    func testModesPreselectTheLocationsStartAndSendOnlyChoices() throws {
        let server = try defaults(serverDefaultsJSON)
        XCTAssertEqual(
            NewSessionDefaults.start(for: "claude", defaults: server), .init(mode: "auto", modeAutomatic: true))
        XCTAssertEqual(
            NewSessionDefaults.start(for: "codex", defaults: server),
            .init(mode: nil, modeAutomatic: false, preset: "Auto", presetAutomatic: true))
        XCTAssertEqual(NewSessionDefaults.startLabel(for: "claude", defaults: server), "Auto · automatic")
        XCTAssertEqual(NewSessionDefaults.startLabel(for: "codex", defaults: server), "Regular · default")
        XCTAssertEqual(NewSessionDefaults.presetLabel(defaults: server), "Approve for me · automatic")

        // Left on the location's start: nothing is sent, so the server applies it.
        XCTAssertTrue(
            NewSessionDefaults.startRequest(provider: "claude", mode: nil, preset: nil, defaults: server) == (nil, nil))
        XCTAssertTrue(
            NewSessionDefaults.startRequest(provider: "claude", mode: "plan", preset: "Ask", defaults: server)
                == ("plan", nil), "presets are Codex's")
        XCTAssertTrue(
            NewSessionDefaults.startRequest(provider: "codex", mode: "acceptEdits", preset: "ReadOnly", defaults: server)
                == (nil, "ReadOnly"), "Codex has no accept-edits mode")
        let older = try defaults(#"{"provider": "codex"}"#)
        XCTAssertTrue(
            NewSessionDefaults.startRequest(provider: "codex", mode: "plan", preset: "Auto", defaults: older) == (nil, nil))

        // Another provider keeps only a mode it also offers.
        XCTAssertEqual(NewSessionDefaults.keptMode("plan", provider: "codex", defaults: server), "plan")
        XCTAssertNil(NewSessionDefaults.keptMode("auto", provider: "codex", defaults: server))
        XCTAssertNil(NewSessionDefaults.keptMode(nil, provider: "claude", defaults: server))

        XCTAssertTrue(NewSessionDefaults.skipsChecks(mode: "bypassPermissions", preset: nil))
        XCTAssertTrue(NewSessionDefaults.skipsChecks(mode: nil, preset: "Full"))
        XCTAssertFalse(NewSessionDefaults.skipsChecks(mode: "auto", preset: "Auto"))
    }

    func testTheAPISendsChosenModesWithNewSessions() async throws {
        let recorder = Recorder()
        let api = DifuAPI { method, params in
            await recorder.record(method, params)
            return Data(#"{"id": 1, "result": {"id": "s1"}}"#.utf8)
        }
        _ = try await api.newSession(repository: "/r", model: nil, effort: nil, text: nil)
        _ = try await api.newSession(
            repository: "/r", model: "gpt-5.6", effort: "high", startingMode: "plan", codexPreset: "Auto", text: nil)
        let calls = await recorder.calls
        XCTAssertEqual(calls[0].1, ["repository": "/r", "model": .null, "effort": .null])
        XCTAssertEqual(
            calls[1].1,
            ["repository": "/r", "model": "gpt-5.6", "effort": "high", "startingMode": "plan", "codexPreset": "Auto"])
    }

    func testDefaultsChangesClearTheProviderAndSaveCodexApprovals() throws {
        let server = try defaults(serverDefaultsJSON)
        XCTAssertEqual(AgentDefaultsChange.automaticModel.params, ["provider": .null, "model": .null, "effort": .null])

        // Picking Codex approvals sends only them.
        var draft = server
        draft.codexPreset = "ReadOnly"
        XCTAssertEqual(AgentDefaultsChange(from: server, to: draft).params, ["codexPreset": "ReadOnly"])
        draft.codexPreset = nil
        XCTAssertTrue(AgentDefaultsChange(from: server, to: draft).isEmpty)

        // A Claude mode while the model is automatic pins Claude Code; the model stays blank.
        var claude = server
        claude.savedProvider = "claude"
        claude.provider = "claude"
        claude.startingMode = "acceptEdits"
        XCTAssertEqual(
            AgentDefaultsChange(from: server, to: claude).params,
            ["provider": "claude", "model": .null, "effort": .null, "startingMode": "acceptEdits"])

        // Back to automatic from a saved Codex model.
        let saved = try defaults(
            """
            {"provider": "codex", "savedProvider": "codex", "model": "gpt-5.6", "effort": "low", "startingMode": "plan",
             "effective": {"provider": "codex", "model": "gpt-5.6", "effort": "low"}}
            """)
        var automatic = saved
        automatic.savedProvider = nil
        automatic.model = nil
        automatic.effort = nil
        XCTAssertTrue(automatic.automaticModel)
        XCTAssertEqual(
            AgentDefaultsChange(from: saved, to: automatic).params,
            ["provider": .null, "model": .null, "effort": .null, "startingMode": "plan"])

        // Older computers: the provider is still only sent as a switch.
        let older = try defaults(#"{"provider": "codex", "model": "gpt-5.6"}"#)
        var switched = older
        switched.provider = "claude"
        switched.model = "claude/opus"
        XCTAssertEqual(
            AgentDefaultsChange(from: older, to: switched).params,
            ["provider": "claude", "model": "claude/opus", "effort": .null, "startingMode": .null])
    }

    func testTheModeChipNamesTheModePresetAndWhatWaits() throws {
        let view = try decode(
            SessionView.self,
            """
            {"id": "s1", "kind": "Coding", "status": "Idle",
             "permissionMode": {"id": "default", "label": "Regular", "pending": null, "pendingLabel": null,
                                "preset": "Auto", "presetLabel": "Approve for me", "pendingPreset": null, "pendingPresetLabel": null}}
            """)
        let codex = try XCTUnwrap(view.permissionMode)
        XCTAssertEqual(codex.chipText, "Regular · Approve for me")
        XCTAssertFalse(codex.isPending)
        XCTAssertFalse(codex.skipsChecks)

        XCTAssertEqual(PermissionMode(id: "auto", label: "Auto").chipText, "Auto")
        let pending = PermissionMode(id: "default", label: "Manual", pending: "plan", pendingLabel: "Plan")
        XCTAssertEqual(pending.chipText, "Plan · next turn")
        XCTAssertTrue(pending.isPending)
        // A new session before the agent starts: the requested mode waits.
        XCTAssertEqual(PermissionMode(pending: "auto", pendingLabel: "Auto").chipText, "Auto · next turn")
        XCTAssertEqual(
            PermissionMode(
                id: "default", label: "Regular", preset: "Ask", presetLabel: "Ask for approval",
                pendingPreset: "Full", pendingPresetLabel: "Full Access"
            ).chipText,
            "Regular · Full Access · next turn")
        XCTAssertTrue(PermissionMode(id: "default", pendingPreset: "Full").skipsChecks)
        XCTAssertTrue(PermissionMode(id: "bypassPermissions", label: "Bypass permissions").skipsChecks)
        XCTAssertEqual(PermissionMode().chipText, "Default mode")

        // Older computers and other sessions: no chip.
        XCTAssertNil(try decode(SessionView.self, #"{"id": "s1", "kind": "Coding", "status": "Idle"}"#).permissionMode)
        // Deltas carry it too.
        let delta = try decode(
            SessionDelta.self,
            #"{"id": "s1", "base": 1, "session": {"id": "s1", "version": 2, "permissionMode": {"id": "plan", "label": "Plan"}}, "count": 0, "entries": []}"#)
        XCTAssertEqual(delta.session?.permissionMode?.chipText, "Plan")
    }
}
