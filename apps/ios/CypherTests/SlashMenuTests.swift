import XCTest
@testable import Cypher

/// The composer's `/` menu: groups, what each command controls, and a
/// command's choices — the desktop's slash_menu.rs test vectors, plus the
/// phone's typed search.
@MainActor
final class SlashMenuTests: XCTestCase {
    private func catalog(_ names: [String]) -> [SlashCommand] {
        names.map { SlashCommand(name: $0) }
    }

    /// "# Heading" rows and command names, top to bottom.
    private func rows(_ sections: [SlashSection]) -> [String] {
        sections.flatMap { ["# \($0.group.title)"] + $0.commands.map(\.name) }
    }

    private let desktopCatalog = ["goal", "fast", "orchestrate", "subagents", "subagent-status",
                                  "skill:wiki", "compact", "mcp", "scripts", "export-html"]

    func testWithNothingTypedTheStatefulCommandsGroupInPageOrder() {
        XCTAssertEqual(rows(SlashMenu.sections(catalog(desktopCatalog), query: "")), [
            "# Conversation", "compact",
            "# Agent modes", "goal", "fast", "orchestrate", "scripts",
            "# Subagents", "subagent-status",
        ])
    }

    func testATypedNameAlsoFindsTheHostsOtherCommands() {
        let commands = catalog(desktopCatalog + ["review", "provider", "subagent-config", "compact-ui"])
        XCTAssertEqual(rows(SlashMenu.sections(commands, query: "exp")), ["# Conversation", "export-html"])
        XCTAssertEqual(rows(SlashMenu.sections(commands, query: "rev")), ["# Other", "review"])
        // Prefix matches come first within a group, then substrings.
        XCTAssertEqual(rows(SlashMenu.sections(commands, query: "COMP")),
                       ["# Conversation", "compact", "# Other", "compact-ui"])
        XCTAssertEqual(rows(SlashMenu.sections(commands, query: "s")), [
            "# Agent modes", "scripts", "fast", "orchestrate",
            "# Subagents", "subagents", "subagent-status",
            "# Skills", "skill:wiki",
        ])
        // What only configures desktop Settings never shows on the phone.
        for name in ["mcp", "provider", "subagent-config"] {
            XCTAssertEqual(SlashMenu.sections(commands, query: name).flatMap(\.commands).map(\.name)
                .filter { $0 == name }, [], name)
        }
        XCTAssertEqual(SlashMenu.sections(commands, query: "zzz"), [])
    }

    func testPlacementAndGlyphsFollowTheDesktop() {
        XCTAssertEqual(SlashMenu.placement("skill:x").group, .skills)
        XCTAssertEqual(SlashMenu.placement("subagent-retry").group, .subagents)
        XCTAssertTrue(SlashMenu.placement("llama-run").advanced)
        XCTAssertTrue(SlashMenu.placement("newapi-add").advanced)
        XCTAssertTrue(SlashMenu.placement("anything-config").advanced)
        XCTAssertEqual(SlashMenu.placement("anything").group, .other)
        XCTAssertFalse(SlashMenu.placement("anything").advanced)
        XCTAssertEqual(SlashMenu.icon(for: "compact"), .foldVertical)
        XCTAssertEqual(SlashMenu.icon(for: "fast"), .bolt)
        XCTAssertEqual(SlashMenu.icon(for: "scripts"), .code)
        XCTAssertEqual(SlashMenu.icon(for: "skill:wiki"), .book)
        XCTAssertEqual(SlashMenu.icon(for: "review"), .command)
    }

    func testChoiceTokensFollowTheFirstArgumentWord() throws {
        let token = try XCTUnwrap(SlashMenu.choiceToken(in: "/orchestrate of"))
        XCTAssertEqual(token.command, "orchestrate")
        XCTAssertEqual(token.query, "of")
        XCTAssertEqual(token.prefix, "/orchestrate ")
        // Right after the space: an empty query.
        XCTAssertEqual(SlashMenu.choiceToken(in: "/goal ")?.query, "")
        // Still in the command name, a second word, a line break: not a choice.
        XCTAssertNil(SlashMenu.choiceToken(in: "/goal"))
        XCTAssertNil(SlashMenu.choiceToken(in: "/goal pause now"))
        XCTAssertNil(SlashMenu.choiceToken(in: "/goal pause "))
        XCTAssertNil(SlashMenu.choiceToken(in: "/goal\npause"))
        XCTAssertNil(SlashMenu.choiceToken(in: "goal pause"))
    }

    func testChoicesOpenForAnOfferedCommandAndCloseOnAnotherWord() {
        let commands = catalog(desktopCatalog)
        XCTAssertEqual(SlashMenu.level(in: "/or", commands: commands), .commands(query: "or"))
        XCTAssertEqual(SlashMenu.level(in: "/orchestrate ", commands: commands),
                       .choices(command: "orchestrate", query: ""))
        XCTAssertEqual(SlashMenu.choiceRows(command: "orchestrate", query: "o").map(\.value), ["on", "off"])
        XCTAssertEqual(SlashMenu.level(in: "/orchestrate o", commands: commands),
                       .choices(command: "orchestrate", query: "o"))
        // A goal's text matches no choice: the menu gets out of the way.
        XCTAssertNil(SlashMenu.level(in: "/goal ship", commands: commands))
        XCTAssertNil(SlashMenu.level(in: "/fast ", commands: commands), "no choices")
        XCTAssertNil(SlashMenu.level(in: "/orchestrate ", commands: catalog(["fast"])), "not offered")
        XCTAssertNil(SlashMenu.level(in: "hello", commands: commands))

        let on = SlashMenu.choices(for: "orchestrate")[0]
        XCTAssertEqual(SlashMenu.accept(on, in: "/orchestrate o"), "/orchestrate on ")
        XCTAssertNil(SlashMenu.level(in: "/orchestrate on ", commands: commands), "a picked choice closes it")
        let edit = SlashMenu.choices(for: "goal")[4]
        XCTAssertTrue(edit.takesText)
        XCTAssertEqual(SlashMenu.accept(edit, in: "/goal "), "/goal edit ")
    }

    func testBadgesSayWhatIsInEffect() {
        let facts = SlashFacts(
            modes: PiSessionModes(fast: true, codemode: false, orchestrate: false,
                                  goal: .init(status: "paused", text: "Ship the menu")),
            context: ContextUsage(used: 170_000, size: 200_000),
            runningSubagents: 2)
        XCTAssertEqual(SlashMenu.badge(for: "fast", facts: facts), SlashBadge(label: "On", tone: .on))
        XCTAssertEqual(SlashMenu.badge(for: "scripts", facts: facts), SlashBadge(label: "Off", tone: .off))
        XCTAssertEqual(SlashMenu.badge(for: "orchestrate", facts: facts), SlashBadge(label: "Off", tone: .off))
        XCTAssertEqual(SlashMenu.badge(for: "goal", facts: facts), SlashBadge(label: "Paused", tone: .neutral))
        XCTAssertEqual(SlashMenu.badge(for: "compact", facts: facts), SlashBadge(label: "85% used", tone: .warning))
        XCTAssertEqual(SlashMenu.badge(for: "subagent-status", facts: facts),
                       SlashBadge(label: "2 running", tone: .on))
        XCTAssertNil(SlashMenu.badge(for: "subagents", facts: facts))
        // A quieter context reads plainly; a running goal is on.
        var calm = facts
        calm.context = ContextUsage(used: 46_000, size: 200_000)
        calm.modes?.goal = .init(status: "active", text: "Ship")
        XCTAssertEqual(SlashMenu.badge(for: "compact", facts: calm), SlashBadge(label: "23% used", tone: .neutral))
        XCTAssertEqual(SlashMenu.badge(for: "goal", facts: calm), SlashBadge(label: "Running", tone: .on))
        // Nothing known yet: no badges rather than guesses.
        XCTAssertNil(SlashMenu.badge(for: "fast", facts: SlashFacts()))
        XCTAssertNil(SlashMenu.badge(for: "compact", facts: SlashFacts()))
        XCTAssertNil(SlashMenu.badge(for: "subagent-status", facts: SlashFacts()))

        let choices = SlashMenu.choices(for: "orchestrate")
        XCTAssertTrue(SlashMenu.choiceInEffect(choices[1], of: "orchestrate", facts: facts))
        XCTAssertFalse(SlashMenu.choiceInEffect(choices[0], of: "orchestrate", facts: facts))
        XCTAssertEqual(SlashMenu.choiceSummary(of: "goal", facts: facts), "Paused: Ship the menu")
        XCTAssertEqual(SlashMenu.choiceSummary(of: "orchestrate", facts: facts), "Adaptive orchestration is off")
        var noGoal = facts
        noGoal.modes?.goal = nil
        XCTAssertEqual(SlashMenu.choiceSummary(of: "goal", facts: noGoal), "No goal yet. Type one to start it.")
        XCTAssertNil(SlashMenu.choiceSummary(of: "goal", facts: SlashFacts()))
    }

    func testSessionModesDecodeTheEnginesShape() throws {
        let json = #"{"fast":true,"codemode":false,"orchestrate":true,"goal":{"status":"active","text":"Ship"}}"#
        let modes = try JSONDecoder().decode(PiSessionModes.self, from: Data(json.utf8))
        XCTAssertEqual(modes, PiSessionModes(fast: true, codemode: false, orchestrate: true,
                                             goal: .init(status: "active", text: "Ship")))
        // An engine from before the Scripts switch sends no codemode: on.
        let old = try JSONDecoder().decode(PiSessionModes.self, from: Data(#"{"fast":false,"orchestrate":false}"#.utf8))
        XCTAssertTrue(old.codemode)
        XCTAssertNil(old.goal)
    }

    func testModesKeepTheLastReadingForTheSameChatOnly() async {
        let catalog = SlashModesCatalog()
        await catalog.load(chatId: "a") { _ in PiSessionModes(fast: true) }
        XCTAssertEqual(catalog.modes?.fast, true)
        await catalog.load(chatId: "a") { _ in throw RelayError.hostOffline }
        XCTAssertEqual(catalog.modes?.fast, true, "a failed ask keeps the last reading")
        await catalog.load(chatId: "b") { _ in throw RelayError.rpc("unknown method: PiSessionModes") }
        XCTAssertNil(catalog.modes, "another chat starts with no badges")
        XCTAssertEqual(catalog.chatId, "b")
    }
}
