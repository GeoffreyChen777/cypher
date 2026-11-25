import XCTest

@testable import Cypher

/// The permission card, `/permissions` and a device's permission settings —
/// permission_card.rs, slash_menu.rs and settings/permissions.rs, ported.
@MainActor
final class PermissionTests: XCTestCase {
    private func question(_ prompt: String, _ options: [String], marked: Bool = false) -> UserInputQuestion {
        UserInputQuestion(
            id: "q", header: prompt, question: prompt, options: options,
            permission: marked ? PermissionPrompt(kind: "escalate", toolCallId: "c1") : nil)
    }

    func testARequestReadsAsFactsSentencesAndABlock() throws {
        let prompt =
            "Run this command outside the sandbox?\n\nContext:\nPurpose: Deploy to web\nCommand: scp a.tgz deploy@web:/srv/\nPattern: scp … deploy@web:*\nReason: the user asked\nNote: It runs as you, with your files, keys, keychain and network."
        let request = try XCTUnwrap(
            PermissionRequest(
                question(
                    prompt, ["Run once", "Allow for this chat", "Always allow", "Deny", "Deny and stop"], marked: true))
        )
        XCTAssertEqual(request.question, "Run this command outside the sandbox?")
        XCTAssertEqual(request.rows.map(\.kind), [.purpose, .command, .pattern, .reason])
        XCTAssertEqual(request.rows.map(\.kind.monospace), [false, true, true, false])
        XCTAssertEqual(request.sentences.count, 1)
        XCTAssertTrue(request.serious)

        let change = try XCTUnwrap(
            PermissionRequest(
                question(
                    "Allow the agent to write a protected file?\n\nContext:\nPath: ~/app/.git/config\nTool: edit\nChange:\n- [core]\n+ [core]",
                    ["Allow once", "Deny", "Deny and stop"])))
        XCTAssertEqual(change.block?.title, "Change")
        XCTAssertEqual(change.block?.text, "- [core]\n+ [core]")
    }

    func testOrdinaryQuestionsStayQuestions() {
        XCTAssertNil(PermissionRequest(question("Which branch?\n\nContext:\nmain or dev", ["main", "dev"])))
        XCTAssertNil(
            PermissionRequest(question("Allow it?\n\nContext:\nWho: me", ["Allow once", "Deny and stop"])),
            "an unknown fact reads as a question")
        XCTAssertNotNil(
            PermissionRequest(question("Allow it?\n\nContext:\nDomain: example.com", ["Allow", "Deny"], marked: true)),
            "a marked prompt needs no stop option")
    }

    func testAnIndentedLineContinuesItsValueWhateverItSays() throws {
        let prompt =
            "Run this command outside the sandbox?\n\nContext:\nPurpose: Write notes\nCommand: cat > /tmp/notes <<'EOF'\n  Path: /etc/passwd\n  \n    indented\n  EOF\nReason: First line.\n  Change:\nNote: It runs as you, with your files, keys, keychain and network."
        let request = try XCTUnwrap(
            PermissionRequest(question(prompt, ["Run once", "Deny", "Deny and stop"], marked: true)))
        XCTAssertEqual(request.rows.map(\.kind), [.purpose, .command, .reason])
        XCTAssertEqual(request.rows[1].value, "cat > /tmp/notes <<'EOF'\nPath: /etc/passwd\n\n  indented\nEOF")
        XCTAssertEqual(request.rows[2].value, "First line.\nChange:")
        XCTAssertEqual(request.sentences.count, 1)
        XCTAssertNil(request.block, "an indented Change: is no block")
    }

    func testAnUnindentedValueLineFromAnOlderRuntimeContinuesItToo() throws {
        let prompt =
            "Run this command outside the sandbox?\n\nContext:\nCommand: cd ~/app\nls -la\n\nmake\nReason: One.\nTwo.\nNote: It runs as you."
        for marked in [true, false] {
            let request = try XCTUnwrap(
                PermissionRequest(question(prompt, ["Run once", "Deny", "Deny and stop"], marked: marked)))
            XCTAssertEqual(request.rows.map(\.value), ["cd ~/app\nls -la\n\nmake", "One.\nTwo."])
            XCTAssertEqual(request.sentences.count, 1)
        }
    }

    func testAMarkedPromptIsARequestHoweverItReads() throws {
        let prompt =
            "Allow the agent to read a sensitive path?\n\nContext:\n/Users/me/app/.env  (**/.env)\n\nTool: request_permission"
        let request = try XCTUnwrap(PermissionRequest(question(prompt, ["Allow once", "Deny"], marked: true)))
        XCTAssertEqual(request.sentences.map(\.value), ["/Users/me/app/.env  (**/.env)"])
        XCTAssertEqual(request.rows.map(\.kind), [.tool])
        XCTAssertNil(
            PermissionRequest(question(prompt, ["Allow once", "Deny", "Deny and stop"])),
            "unmarked, and its context starts with no fact")
        let bare = try XCTUnwrap(
            PermissionRequest(question("Allow the agent to use a tool?", ["Allow once", "Deny"], marked: true)))
        XCTAssertEqual(bare.question, "Allow the agent to use a tool?")
        XCTAssertTrue(bare.facts.isEmpty)
    }

    func testTheNarrowestApprovalIsPrimary() {
        XCTAssertEqual(
            PermissionRequest.roles(["Run once", "Allow for this chat", "Always allow", "Deny", "Deny and stop"]),
            [.primary, .secondary, .secondary, .deny, .stop])
    }

    func testACardShowsEveryOptionWithTheNarrowestApprovalLastAndProminent() {
        let options = ["Run once", "Allow for this chat", "Always allow", "Deny", "Deny and stop"]
        let shown = PermissionRequest.shownOptions(options)
        XCTAssertEqual(
            shown.map(\.label), ["Deny and stop", "Deny", "Always allow", "Allow for this chat", "Run once"])
        XCTAssertEqual(Set(shown.map(\.index)), Set(options.indices), "no option is dropped or folded away")
        XCTAssertEqual(shown.map(\.index), [4, 3, 2, 1, 0], "each keeps its permission-option index")
        XCTAssertEqual(shown.filter(\.prominent).map(\.label), ["Run once"])
        XCTAssertEqual(shown.last?.prominent, true)
    }

    func testEachOptionGetsTheSymbolAndToneOfItsKind() {
        let shown = PermissionRequest.shownOptions([
            "Allow once", "Allow for this command", "Allow for this task", "Allow for this chat", "Always allow",
            "Always in webapp", "Deny", "Deny and stop",
        ])
        let byLabel = Dictionary(uniqueKeysWithValues: shown.map { ($0.label, $0) })
        func check(_ label: String, _ symbol: String, _ caption: String, destructive: Bool, line: UInt = #line) {
            let option = byLabel[label]
            XCTAssertEqual(option?.symbol, symbol, label, line: line)
            XCTAssertEqual(option?.caption, caption, label, line: line)
            XCTAssertEqual(option?.destructive, destructive, label, line: line)
        }
        check("Deny", "xmark", "Deny", destructive: true)
        check("Deny and stop", "stop.fill", "Stop", destructive: true)
        check("Allow for this command", "checkmark.shield", "This command", destructive: false)
        check("Allow for this task", "checkmark.shield", "This task", destructive: false)
        check("Allow for this chat", "checkmark.shield", "This chat", destructive: false)
        check("Always allow", "checkmark.shield.fill", "Always", destructive: false)
        check("Always in webapp", "checkmark.shield.fill", "Always in webapp", destructive: false)
        XCTAssertEqual(byLabel["Allow once"]?.role, .primary)
        XCTAssertEqual(byLabel["Allow once"]?.prominent, true)
        XCTAssertEqual(shown.filter(\.prominent).count, 1, "one prominent option")
        XCTAssertEqual(shown.last?.label, "Allow once")
    }

    func testAPromptWithOnlyRefusalsShowsThemAllWithNothingProminent() {
        let shown = PermissionRequest.shownOptions(["Deny", "Deny and stop"])
        XCTAssertEqual(shown.map(\.label), ["Deny and stop", "Deny"])
        XCTAssertFalse(shown.contains(where: \.prominent))
        XCTAssertTrue(shown.allSatisfy(\.destructive))
    }

    func testWrappedIconOptionsFillAnEvenGrid() {
        // Four 60 pt options with 8 pt between: one line from 264 pt.
        XCTAssertEqual(IconOptionGrid.columns(fitting: 311, item: 60, spacing: 8), 4)
        XCTAssertEqual(IconOptionGrid.columns(fitting: 264, item: 60, spacing: 8), 4)
        XCTAssertEqual(IconOptionGrid.columns(fitting: 263, item: 60, spacing: 8), 3)
        XCTAssertEqual(IconOptionGrid.columns(fitting: 130, item: 60, spacing: 8), 2)
        // Never none, however narrow.
        XCTAssertEqual(IconOptionGrid.columns(fitting: 30, item: 60, spacing: 8), 1)
        XCTAssertEqual(IconOptionGrid.columns(fitting: .infinity, item: 60, spacing: 8), 1)
        // The options are shared out evenly between the lines they need.
        XCTAssertEqual(IconOptionGrid.balanced(count: 4, columns: 4), 4)
        XCTAssertEqual(IconOptionGrid.balanced(count: 4, columns: 3), 2, "two and two, not three and one")
        XCTAssertEqual(IconOptionGrid.balanced(count: 5, columns: 4), 3)
        XCTAssertEqual(IconOptionGrid.balanced(count: 5, columns: 2), 2)
        XCTAssertEqual(IconOptionGrid.balanced(count: 3, columns: 1), 1)
        XCTAssertEqual(IconOptionGrid.balanced(count: 2, columns: 6), 2)
    }

    func testPermissionsLeadAgentModesAndPickALevel() {
        let commands = SlashMenu.withComposerCommands(
            [SlashCommand(name: "goal"), SlashCommand(name: "compact")], permissions: true)
        let sections = SlashMenu.sections(commands, query: "")
        XCTAssertEqual(sections.first { $0.group == .agentModes }?.commands.map(\.name), ["permissions", "goal"])
        XCTAssertEqual(
            SlashMenu.level(in: "/permissions ", commands: commands), .choices(command: "permissions", query: ""))
        XCTAssertEqual(SlashMenu.choiceRows(command: "permissions", query: "fu").map(\.value), ["full"])
        XCTAssertEqual(SlashMenu.choiceRows(command: "permissions", query: "edit").map(\.value), ["project"])

        let facts = SlashFacts(permission: "danger-full-access")
        XCTAssertEqual(
            SlashMenu.badge(for: "permissions", facts: facts), SlashBadge(label: "Full access", tone: .warning))
        XCTAssertEqual(
            SlashMenu.badge(for: "permissions", facts: SlashFacts(permission: "workspace-write")),
            SlashBadge(label: "Edit project", tone: .neutral))
        XCTAssertEqual(
            SlashMenu.choices(for: "permissions").map { SlashMenu.choiceInEffect($0, of: "permissions", facts: facts) },
            [false, false, true])
        XCTAssertEqual(SlashMenu.choiceSummary(of: "permissions", facts: facts), "Now: Full access")

        XCTAssertEqual(SlashMenu.typedPermission("/permissions full")??.sandbox, "danger-full-access")
        XCTAssertEqual(SlashMenu.typedPermission("/permissions Ask before edits")??.sandbox, "read-only")
        XCTAssertNotNil(SlashMenu.typedPermission("/permissions everything").map { $0 == nil }, "names no level")
        XCTAssertNil(SlashMenu.typedPermission("/goal ship it"))
        XCTAssertNil(SlashMenu.typedPermission("/permissionsfoo"))
    }

    func testSettingsKeepWhatThePageDoesNotShow() throws {
        let json = """
            {"enabled":false,"approval":{"escalate":"ask","externalWrite":"assisted"},"futureField":{"x":1},
             "commands":[{"match":["ssh","web","**"],"decision":"allow","label":"Web","sha256":"\(String(repeating: "a", count: 64))"},
                         {"match":["kubectl","delete","**"],"decision":"deny"}],
             "rules":[{"capability":"read","path":"tool:mcp__github__create_issue"}]}
            """
        var settings = try JSONDecoder().decode(PermissionSettings.self, from: Data(json.utf8))
        XCTAssertFalse(settings.enabled)
        XCTAssertEqual(settings.commands.map(\.text), ["ssh web …", "kubectl delete …"])
        XCTAssertTrue(settings.commands[0].pinned)
        XCTAssertEqual(settings.rules.first?.path, "tool:mcp__github__create_issue")
        settings.enabled = true
        settings.setApproval("escalate", "deny")
        settings.removeCommand(at: 1)
        let params = try settings.params()
        XCTAssertEqual(params["enabled"] as? Bool, true)
        XCTAssertNotNil(params["futureField"], "a field this app does not know survives a save")
        XCTAssertEqual((params["approval"] as? [String: Any])?["escalate"] as? String, "deny")
        XCTAssertEqual((params["approval"] as? [String: Any])?["externalWrite"] as? String, "assisted")
        XCTAssertEqual(settings.approval("sensitiveRead"), "assisted", "a mode not set is the model's")
        XCTAssertEqual((params["commands"] as? [Any])?.count, 1)
    }
}
