import XCTest
import Loro
@testable import Cypher

/// Pi codemode scripts in the transcript: the Script chip, the calls nested
/// under it and the group summary — the desktop's view.rs / transcript.rs
/// test vectors — and the status every chip shows.
@MainActor
final class ScriptChipTests: XCTestCase {
    private func script(_ code: String?) -> RenderToolCall {
        var fields: [String: AnyHashable] = ["name": "codemode"]
        if let code { fields["code"] = code }
        return RenderToolCall(tag: "unknown", fields: fields)
    }

    private func exec(_ command: String) -> RenderToolCall {
        RenderToolCall(tag: "exec", fields: ["command": command])
    }

    private func item(_ id: String, _ call: RenderToolCall, failed: Bool = false) -> ToolItem {
        ToolItem(id: id, call: call, isError: failed, resolved: true)
    }

    private func groups(_ parts: [MessagePart]) -> [(tools: [ToolItem], version: UInt64)] {
        buildRows([.fixture(parts: parts)])
            .compactMap { row in
                guard case .toolGroup(let tools, _) = row.kind else { return nil }
                return (tools, row.version)
            }
    }

    private func tool(_ id: String, _ call: RenderToolCall) -> MessagePart {
        .tool(id: id, call: call, isError: false, resolved: true)
    }

    func testScriptChipsNameTheToolsTheScriptCalls() {
        let call = script(
            "const [a, b] = await Promise.all([\n  tools.read({ path: 'x' }),\n  tools.bash({ command: 'ls' }),\n]);\nawait tools.read({ path: 'y' });"
        )
        XCTAssertEqual(call.chipLabel, "Script")
        XCTAssertEqual(call.chipDetail, "read, bash")
        // Bracket access, and MCP tools by their tool name.
        XCTAssertEqual(
            script("await tools[\"my-tool\"]({});\nawait tools.mcp__mvp_lab_discord__search({ q: 1 });").chipDetail,
            "my-tool, search")
        // Lookalikes are not the `tools` global.
        XCTAssertEqual(RenderToolCall.scriptTools("ALL_TOOLS.map(t => t.name); myTools.x(); a.tools.y(); tools."), [])
        // No tool calls: the first line of code that is not the options header.
        XCTAssertEqual(script("// @options: {\"timeout_ms\": 1000}\n\nreturn 6 * 7;").chipDetail, "return 6 * 7;")
        // A script the doc did not keep still labels as a script, with nothing to open.
        let bare = script(nil)
        XCTAssertEqual(bare.chipLabel, "Script")
        XCTAssertEqual(bare.chipDetail, "")
        XCTAssertNil(bare.script)
        // Other extension tools keep the generic chip.
        let other = RenderToolCall(tag: "unknown", fields: ["name": "codemode_helper"])
        XCTAssertEqual(other.chipLabel, "Tool")
        XCTAssertEqual(other.chipDetail, "codemode_helper")
    }

    func testToolSearchChipsShowTheirQuery() {
        let call = RenderToolCall(tag: "unknown", fields: ["name": "tool_search", "query": "discord messages"])
        XCTAssertEqual(call.chipLabel, "Find tools")
        XCTAssertEqual(call.chipDetail, "discord messages")
    }

    func testGroupSummariesCountScriptsWithCommands() {
        let read = RenderToolCall(tag: "readFile", fields: ["path": "a"])
        XCTAssertEqual(
            toolGroupSummary([item("s", script("")), item("r", read)]),
            "Ran 1 script · read 1 file")
        XCTAssertEqual(
            toolGroupSummary([
                item("s", script("")), item("a", exec("ls")),
                item("b", exec("ls"), failed: true), item("r", read),
            ]),
            "Ran 2 commands and 1 script · read 1 file · 1 failed")
        let search = RenderToolCall(tag: "unknown", fields: ["name": "tool_search"])
        XCTAssertEqual(toolGroupSummary([item("q", search)]), "1 search")
    }

    func testScriptCallsNestUnderTheirScript() {
        // Pi ran a script (`s`) next to a direct call (`d`); the script's own
        // calls (`s/1`, `s/2`) arrived after `d` and still list under `s`.
        let nested = groups([
            tool("s", script("await tools.bash({ command: 'ls' })")),
            tool("d", exec("pwd")),
            tool("s/1", exec("ls")),
            tool("s/2", exec("cat release.json")),
        ])
        XCTAssertEqual(nested.count, 1)
        XCTAssertEqual(nested[0].tools.map(\.call.chipDetail), ["bash", "ls", "cat release.json", "pwd"])
        XCTAssertEqual(nested[0].tools.map(\.depth), [0, 1, 1, 0])
        XCTAssertEqual(toolGroupSummary(nested[0].tools), "Ran 3 commands and 1 script")

        // Nesting only follows ids: a slash with no caller in the group keeps
        // arrival order at depth 0.
        let flat = groups([tool("a", exec("one")), tool("x/1", exec("two"))])
        XCTAssertEqual(flat[0].tools.map(\.call.chipDetail), ["one", "two"])
        XCTAssertEqual(flat[0].tools.map(\.depth), [0, 0])

        // Depth is part of the row's version: nesting re-renders the group.
        let calls = [tool("a", exec("one")), tool("a/1", exec("two"))]
        XCTAssertNotEqual(groups(calls)[0].version, flat[0].version)
    }

    func testNestedCallsKeepSiblingOrderAndGoDeeper() {
        let nested = TranscriptRowBuilder.nestToolCalls(["a", "a/1", "b", "a/1/1", "a/2"].map { item($0, exec($0)) })
        XCTAssertEqual(nested.map(\.id), ["a", "a/1", "a/1/1", "a/2", "b"])
        XCTAssertEqual(nested.map(\.depth), [0, 1, 2, 1, 0])
    }

    func testScriptBodyIsTheCodeItself() throws {
        let body = try XCTUnwrap(
            RenderToolCall.scriptBody(
                "\n\nconst a = await tools.read({ path: \"x\" });\nreturn a.length;\n\n"))
        XCTAssertEqual(body.code, "const a = await tools.read({ path: \"x\" });\nreturn a.length;")
        XCTAssertEqual(body.truncatedBy, 0)
        // Scripts get 80 lines before the counted tail.
        let long = (0..<100).map { "text(\($0));" }.joined(separator: "\n")
        let capped = try XCTUnwrap(RenderToolCall.scriptBody(long))
        XCTAssertEqual(capped.code.split(separator: "\n").count, RenderToolCall.scriptBodyMaxLines)
        XCTAssertEqual(capped.truncatedBy, 100 - RenderToolCall.scriptBodyMaxLines)
        XCTAssertNil(RenderToolCall.scriptBody("\n  \n"))
    }

    func testChipStatusFollowsTheCallsLifecycle() {
        let read = RenderToolCall(tag: "readFile", fields: ["path": "README.md"])
        XCTAssertEqual(ToolItem(id: "a", call: read, isError: false, resolved: true).status, .completed)
        XCTAssertEqual(ToolItem(id: "a", call: read, isError: false, resolved: false).status, .running)
        XCTAssertEqual(ToolItem(id: "a", call: read, isError: true, resolved: true).status, .failed)
        // A failed call that also never resolved still reads as failed.
        XCTAssertEqual(ToolItem(id: "a", call: read, isError: true, resolved: false).status, .failed)

        // A call resolving re-renders its group.
        let running = groups([.tool(id: "a", call: read, isError: false, resolved: false)])
        let done = groups([tool("a", read)])
        XCTAssertEqual(running[0].tools.map(\.status), [.running])
        XCTAssertNotEqual(running[0].version, done[0].version)
    }

    func testTheDocsScriptAndQueryReachTheChip() throws {
        func part(_ id: String, _ name: String, _ input: [String: Any]) -> [String: Any] {
            [
                "kind": "tool", "id": id, "isError": false,
                "call": ["kind": "unknown", "name": name, "input": input],
            ]
        }
        let entry = try XCTUnwrap(
            SessionStore.entryFrom(
                LoroValue.fromJSON(
                    [
                        "id": "m", "role": "assistant", "createdAt": 1, "deviceId": "d",
                        "parts": [
                            part("s1", "codemode", ["code": "return await tools.read({ path: 'a' });"]),
                            part("q", "tool_search", ["query": "discord"]),
                            part("o", "other", ["code": "not kept"]),
                        ],
                    ] as [String: Any])))
        let calls = entry.parts.compactMap { part -> RenderToolCall? in
            if case .tool(_, let call, _, _) = part { return call }
            return nil
        }
        XCTAssertEqual(calls.map(\.chipLabel), ["Script", "Find tools", "Tool"])
        XCTAssertEqual(calls[0].script, "return await tools.read({ path: 'a' });")
        XCTAssertEqual(calls[0].chipDetail, "read")
        XCTAssertEqual(calls[1].chipDetail, "discord")
        XCTAssertNil(calls[2].fields["code"], "other extension inputs stay out")
    }
}
