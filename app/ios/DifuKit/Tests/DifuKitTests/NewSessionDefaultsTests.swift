import XCTest

@testable import DifuKit

private func model(_ id: String, name: String? = nil, provider: String? = "claude", _ efforts: [String] = [])
    throws -> AgentModel
{
    var json: [String: Any] = ["id": id, "efforts": efforts]
    if let name { json["name"] = name }
    if let provider { json["provider"] = provider }
    return try JSONDecoder().decode(AgentModel.self, from: JSONSerialization.data(withJSONObject: json))
}

private func defaults(_ json: String) throws -> AgentDefaults {
    try decode(AgentDefaults.self, json)
}

final class NewSessionDefaultsTests: XCTestCase {
    private let claudeEfforts = ["low", "medium", "high", "xhigh", "max"]

    func testTheNewestOpusWinsOverOlderOnesTheAliasAndVariants() throws {
        let catalog = [
            try model("gpt-5.6", provider: "codex", ["low", "high", "xhigh"]),
            try model("claude/default", name: "Claude Code · Default (recommended)", claudeEfforts),
            try model("claude/opus", name: "Claude Code · Opus", claudeEfforts),
            try model("claude/claude-opus-4-6", claudeEfforts),
            try model("claude/claude-opus-4-7[1m]", claudeEfforts),
            try model("claude/claude-opus-4-7", claudeEfforts),
            try model("claude/claude-opus-4-1-20250805", claudeEfforts),
            try model("claude/opusplan", name: "Claude Code · Opus Plan Mode", claudeEfforts),
            try model("claude/sonnet", claudeEfforts),
        ]
        XCTAssertEqual(NewSessionDefaults.newestOpus(in: catalog)?.id, "claude/claude-opus-4-7")
        // A date isn't a version: 4.1 (2025-08-05) is older than 4.6.
        XCTAssertEqual(
            NewSessionDefaults.newestOpus(in: [catalog[6], catalog[3]])?.id, "claude/claude-opus-4-6")
        // Only the 1M variant of the newest: it still beats older versions.
        XCTAssertEqual(
            NewSessionDefaults.newestOpus(in: [catalog[3], catalog[4]])?.id, "claude/claude-opus-4-7[1m]")
        // Without versioned models, the `opus` alias; never plan mode.
        XCTAssertEqual(
            NewSessionDefaults.newestOpus(in: [catalog[1], catalog[7], catalog[2], catalog[8]])?.id,
            "claude/opus")
        XCTAssertNil(NewSessionDefaults.newestOpus(in: [catalog[0], catalog[1], catalog[7], catalog[8]]))
        // An alias whose name carries the version counts as that version.
        let named = try model("claude/opus", name: "Claude Code · Opus 4.8", claudeEfforts)
        XCTAssertEqual(NewSessionDefaults.newestOpus(in: [catalog[5], named])?.id, "claude/opus")
        // Providers the list leaves out are derived from the id.
        let bare = try model("claude-opus-4-5", provider: nil, ["high"])
        XCTAssertEqual(NewSessionDefaults.newestOpus(in: [bare])?.id, "claude-opus-4-5")
        XCTAssertNil(NewSessionDefaults.newestOpus(in: [try model("opus-like", provider: "codex")]))
    }

    func testEffortIsXhighWhenSupportedOtherwiseTheHighest() throws {
        XCTAssertEqual(NewSessionDefaults.effort(for: try model("claude/opus", claudeEfforts)), "xhigh")
        XCTAssertEqual(
            NewSessionDefaults.effort(for: try model("claude/opus", ["low", "max", "high"])), "max")
        XCTAssertEqual(NewSessionDefaults.effort(for: try model("claude/opus", ["medium", "high"])), "high")
        XCTAssertNil(NewSessionDefaults.effort(for: try model("claude/opus")))
        XCTAssertEqual(NewSessionDefaults.effort(for: try model("claude/opus", ["turbo"])), "turbo")
    }

    func testWithoutASavedModelTheSheetPreselectsClaudeOpusXhigh() throws {
        let catalog = [
            try model("gpt-5.6", provider: "codex", ["low", "high"]),
            try model("claude/claude-opus-4-7", claudeEfforts),
        ]
        let saved = try defaults(#"{"provider": "codex", "model": null, "effort": "high"}"#)
        XCTAssertEqual(
            NewSessionDefaults.choice(saved: saved, models: catalog),
            .init(model: "claude/claude-opus-4-7", effort: "xhigh"))
        // Older computers report no defaults: the same preselection.
        XCTAssertEqual(
            NewSessionDefaults.choice(saved: nil, models: catalog),
            .init(model: "claude/claude-opus-4-7", effort: "xhigh"))
        let noXhigh = [try model("claude/opus", ["low", "medium", "high"])]
        XCTAssertEqual(
            NewSessionDefaults.choice(saved: nil, models: noXhigh), .init(model: "claude/opus", effort: "high"))
    }

    func testWithoutClaudeModelsTheLocationsDefaultStays() throws {
        let codexOnly = [try model("gpt-5.6", provider: "codex", ["low", "high"])]
        let saved = try defaults(#"{"provider": "codex", "effort": "high"}"#)
        XCTAssertEqual(NewSessionDefaults.choice(saved: saved, models: codexOnly), .init(model: nil, effort: "high"))
        XCTAssertEqual(NewSessionDefaults.choice(saved: nil, models: []), .init(model: nil, effort: nil))
        // Models still loading (or failed): the default until they arrive.
        XCTAssertEqual(NewSessionDefaults.choice(saved: saved, models: nil), .init(model: nil, effort: "high"))
    }

    func testSavedDefaultsWin() throws {
        let catalog = [try model("claude/claude-opus-4-7", claudeEfforts)]
        let saved = try defaults(
            #"{"provider": "codex", "model": "gpt-5.6", "effort": "low", "repository": "/code/difu"}"#)
        XCTAssertEqual(NewSessionDefaults.choice(saved: saved, models: catalog), .init(model: "gpt-5.6", effort: "low"))
        XCTAssertEqual(NewSessionDefaults.choice(saved: saved, models: nil), .init(model: "gpt-5.6", effort: "low"))
        let sonnet = try defaults(#"{"model": "claude/sonnet"}"#)
        XCTAssertEqual(NewSessionDefaults.choice(saved: sonnet, models: catalog), .init(model: "claude/sonnet", effort: nil))

        let listed = [
            AgentRepository(path: "/code/app", name: "app"),
            AgentRepository(path: "/code/web", name: "web", isDefault: true),
        ]
        XCTAssertEqual(NewSessionDefaults.repository(saved: "/code/difu", in: listed), "/code/difu")
        XCTAssertEqual(NewSessionDefaults.repository(saved: nil, in: listed), "/code/web")
        XCTAssertEqual(NewSessionDefaults.repository(saved: nil, in: Array(listed.prefix(1))), "/code/app")
        XCTAssertNil(NewSessionDefaults.repository(saved: nil, in: []))
        let merged = NewSessionDefaults.repositories(listed, saved: "/code/difu")
        XCTAssertEqual(merged.map(\.path), ["/code/difu", "/code/app", "/code/web"])
        XCTAssertEqual(merged.first?.name, "difu")
        XCTAssertEqual(NewSessionDefaults.repositories(listed, saved: "/code/app"), listed)
    }

    func testDefaultsDecodeLenientlyWithEachProvidersStartingModes() throws {
        let saved = try defaults(
            """
            {"provider": "claude", "model": "claude/opus", "effort": "xhigh", "repository": "/code/difu",
             "startingMode": "plan", "isolated": true,
             "providers": [
               {"id": "codex", "label": "Codex", "startingModes": [{"id": "default", "label": "Regular", "description": "Native coding mode"}]},
               {"id": "claude", "startingModes": [{"id": "plan", "label": "Plan"}, {"label": "no id"}]},
               {"label": "nameless"}
             ],
             "future": 1}
            """)
        XCTAssertEqual(saved.provider, "claude")
        XCTAssertEqual(saved.model, "claude/opus")
        XCTAssertEqual(saved.startingMode, "plan")
        XCTAssertTrue(saved.isolated)
        XCTAssertEqual(saved.providers.map(\.id), ["codex", "claude"])
        XCTAssertEqual(saved.providers.last?.label, "Claude Code")
        XCTAssertEqual(saved.startingModes(for: "claude").map(\.id), ["plan"])
        XCTAssertEqual(saved.startingModes(for: "codex").first?.description, "Native coding mode")
        XCTAssertEqual(saved.startingModes(for: "other"), [])

        let sparse = try defaults(#"{"model": "claude/sonnet", "effort": "", "isolated": null}"#)
        XCTAssertEqual(sparse.provider, "claude", "derived from the model")
        XCTAssertNil(sparse.effort)
        XCTAssertTrue(sparse.isolated)
        XCTAssertEqual(try defaults("{}").provider, "codex")
    }

    func testAPIReadsAndSavesOnlyTheChangedDefaults() async throws {
        let recorder = Recorder()
        let api = DifuAPI { method, params in
            await recorder.record(method, params)
            return Data(#"{"id": 1, "result": {"provider": "claude", "model": "claude/opus", "effort": "xhigh"}}"#.utf8)
        }
        let read = try await api.agentDefaults()
        XCTAssertEqual(read.model, "claude/opus")
        try await api.setAgentDefaults(
            AgentDefaultsChange(provider: "claude", model: "claude/opus", effort: "xhigh"))
        try await api.setAgentDefaults(AgentDefaultsChange(model: .some(nil), startingMode: .some(nil)))
        let all = AgentDefaults(
            provider: "codex", model: "gpt-5.6", repository: "/code/difu", startingMode: "plan", isolated: true)
        try await api.setAgentDefaults(AgentDefaultsChange(all: all))
        let calls = await recorder.calls
        XCTAssertEqual(calls.map(\.0), ["agents.defaults", "agents.defaults.set", "agents.defaults.set", "agents.defaults.set"])
        XCTAssertEqual(calls[0].1, [:])
        XCTAssertEqual(
            calls[1].1, ["provider": "claude", "model": "claude/opus", "effort": "xhigh"])
        XCTAssertEqual(calls[2].1, ["model": .null, "startingMode": .null])
        XCTAssertEqual(
            calls[3].1,
            [
                "provider": "codex", "model": "gpt-5.6", "effort": .null, "repository": "/code/difu",
                "startingMode": "plan", "isolated": true,
            ])
    }

    func testSavingSendsOnlyWhatChanged() throws {
        let saved = try defaults(
            #"{"provider": "codex", "model": "gpt-5.6", "effort": "high", "repository": "/gone", "startingMode": "plan"}"#)
        var draft = saved
        XCTAssertTrue(AgentDefaultsChange(from: saved, to: draft).isEmpty)
        draft.effort = "low"
        XCTAssertEqual(AgentDefaultsChange(from: saved, to: draft).params, ["effort": "low"])
        draft.repository = nil
        XCTAssertEqual(AgentDefaultsChange(from: saved, to: draft).params, ["effort": "low", "repository": .null])
        // Another provider sends what belongs to it, even when it looks unchanged.
        var claude = saved
        claude.provider = "claude"
        claude.model = "claude/opus"
        claude.startingMode = "plan"
        XCTAssertEqual(
            AgentDefaultsChange(from: saved, to: claude).params,
            ["provider": "claude", "model": "claude/opus", "effort": "high", "startingMode": "plan"])
        XCTAssertEqual(saved.summary, "Codex · gpt-5.6 · high")
        XCTAssertEqual(claude.summary, "Claude Code · opus · high")
        XCTAssertEqual(try defaults("{}").summary, "Codex · Default model")
    }
}
