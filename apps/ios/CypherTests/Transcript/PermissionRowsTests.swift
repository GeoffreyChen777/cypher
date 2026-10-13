import Loro
import XCTest

@testable import Cypher

/// Permission control in the transcript — transcript/tests.rs
/// `a_blocked_command_and_its_escalated_retry_*`, `a_permission_prompt_*`
/// and `held_calls_*`, ported: an answered prompt files under its call, a
/// blocked command folds into its retry, held calls are no failures.
@MainActor
final class PermissionRowsTests: XCTestCase {
    private func command(
        _ id: String, _ command: String, _ outcome: ToolPermission.Outcome?,
        scope: ToolPermission.Scope? = nil
    ) -> MessagePart {
        var call = RenderToolCall(tag: "exec", fields: ["command": command])
        call.permission = outcome.map { ToolPermission(outcome: $0, scope: scope, purpose: "Check the server") }
        return .tool(
            id: id, call: call, isError: outcome == .blocked || outcome == .denied, resolved: true)
    }

    private func prompt(_ request: String, call: String?, marked: Bool = true) -> MessagePart {
        .input(
            id: "in-\(request)", requestId: request,
            questions: [
                UserInputQuestion(
                    id: request,
                    header: "Run this command outside the sandbox?\n\nContext:\nCommand: ssh web uptime",
                    question: "Run this command outside the sandbox?\n\nContext:\nCommand: ssh web uptime",
                    options: ["Run once", "Allow for this chat", "Deny", "Deny and stop"],
                    permission: marked ? PermissionPrompt(kind: "escalate", toolCallId: call) : nil)
            ],
            resolved: true)
    }

    private func tools(_ rows: [TranscriptRow]) -> [ToolItem] {
        rows.flatMap { row -> [ToolItem] in
            if case .toolGroup(let tools, _) = row.kind { return tools }
            return []
        }
    }

    func testABlockedCommandAndItsEscalatedRetryReadAsOneRowWithTheApprovalOnIt() {
        let rows = buildRows([
            .fixture(
                "m1",
                parts: [
                    command("t1", "ssh web uptime", .blocked),
                    command("t2", "ssh web uptime", .outside, scope: .chat),
                    prompt("q1", call: "t2"),
                    .tool(
                        id: "t3", call: RenderToolCall(tag: "exec", fields: ["command": "ls"]), isError: false,
                        resolved: true),
                    .text(id: "x", text: "Done."),
                ])
        ])
        XCTAssertFalse(
            rows.contains { if case .inputChip = $0.kind { return true } else { return false } },
            "the answered prompt is filed under its call")
        let tools = tools(rows)
        XCTAssertEqual(tools.map(\.id), ["t2", "t3"], "one row for the attempt and its retry, then ls")
        XCTAssertTrue(tools[0].blockedFirst)
        XCTAssertEqual(tools[0].permission.map(PermissionBadge.text), "Outside the sandbox · this chat")
        XCTAssertEqual(toolGroupSummary(tools), "Ran 2 commands · 1 outside the sandbox", "no failure")
    }

    func testAPermissionPromptWithoutACallIsAPermissionRowNotAQuestion() {
        func chip(_ part: MessagePart) -> Bool? {
            buildRows([.fixture("m1", parts: [part, .text(id: "x", text: "ok")])]).compactMap { row -> Bool? in
                if case .inputChip(_, _, let permission) = row.kind { return permission }
                return nil
            }.first
        }
        XCTAssertEqual(chip(prompt("q", call: nil)), true)
        XCTAssertEqual(chip(prompt("q", call: nil, marked: false)), true, "an older Runtime's, by Deny and stop")
        let question = MessagePart.input(
            id: "in-a", requestId: "a",
            questions: [UserInputQuestion(id: "a", header: "Which branch?", question: "", options: ["main", "dev"])],
            resolved: true)
        XCTAssertEqual(chip(question), false, "an ordinary question stays a question")
    }

    func testHeldCallsCountApartFromFailuresAndAPromptedCallWithoutARecordShowsItsAnswer() {
        let mcp = MessagePart.tool(
            id: "m1", call: RenderToolCall(tag: "mcp", fields: ["server": "github", "tool": "create_issue"]),
            isError: false, resolved: true)
        let rows = buildRows([
            .fixture(
                "e",
                parts: [
                    command("t1", "rm -rf build", .denied),
                    command("t2", "cat ~/.ssh/id", .blocked),
                    mcp,
                    prompt("q", call: "m1"),
                    .text(id: "x", text: "ok"),
                ])
        ])
        let tools = tools(rows)
        XCTAssertEqual(tools.count, 3, "different commands are not folded together")
        XCTAssertEqual(toolGroupSummary(Array(tools.prefix(2))), "Ran 2 commands · 1 blocked · 1 denied")
        XCTAssertEqual(tools[0].status, .held)
        XCTAssertEqual(tools[2].permission?.outcome, .allowed)
    }

    func testEachStatusHasItsOwnShield() {
        let icons = [ToolPermission.Outcome.outside, .blocked, .denied, .allowed].map(PermissionBadge.icon)
        XCTAssertEqual(icons, [.shieldOut, .shieldKeyhole, .shieldCross, .shieldCheck])
    }

    func testTheDocsPermissionRecordDecodesOntoTheCall() throws {
        let doc = LoroDoc()
        let message = try doc.getList(id: "messages").pushContainer(child: LoroMap())
        try message.insert(key: "id", v: "m1")
        try message.insert(key: "role", v: "assistant")
        try message.insert(key: "status", v: "complete")
        let parts = try message.insertContainer(key: "parts", child: LoroList())
        let part = try parts.pushContainer(child: LoroMap())
        try part.insert(key: "id", v: "t1")
        try part.insert(key: "kind", v: "tool")
        try part.insert(key: "isError", v: false)
        let call = try part.insertContainer(key: "call", child: LoroMap())
        try call.insert(key: "kind", v: "exec")
        try call.insert(key: "command", v: "ssh web uptime")
        let permission = try part.insertContainer(key: "permission", child: LoroMap())
        try permission.insert(key: "outcome", v: "outside")
        try permission.insert(key: "scope", v: "rule")
        try permission.insert(key: "rule", v: "ssh web …")
        doc.commit()
        let entries = try XCTUnwrap(SessionStore.decodeEntries(from: doc))
        guard case .tool(_, let decoded, _, _) = entries[0].parts[0] else { return XCTFail("a tool part") }
        XCTAssertEqual(decoded.permission, ToolPermission(outcome: .outside, scope: .rule, rule: "ssh web …"))
    }
}
