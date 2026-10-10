// Tool chip content (transcript.rs tool_chip_content_raw): the verb, target
// and detail a tool call renders as.

import Foundation

// MARK: - Tool chip content (transcript.rs tool_chip_content_raw)

extension RenderToolCall {
    /// Pi's `codemode` tool: instead of calling tools one at a time, the
    /// model writes a JavaScript script that calls them (`await
    /// tools.read({…})`), and Pi reports each call the script makes as a tool
    /// call of its own (view.rs CODEMODE_TOOL).
    static let codemodeTool = "codemode"
    /// Pi's `tool_search`: finds and loads tools the model wasn't shown up
    /// front (view.rs TOOL_SEARCH_TOOL).
    static let toolSearchTool = "tool_search"

    /// The input field the doc keeps for an extension tool, if any: a
    /// codemode call's script and a tool_search's query.
    static func keptInputField(_ name: String) -> String? {
        switch name {
        case codemodeTool: return "code"
        case toolSearchTool: return "query"
        default: return nil
        }
    }

    var isScript: Bool { tag == "unknown" && string("name") == Self.codemodeTool }
    var isToolSearch: Bool { tag == "unknown" && string("name") == Self.toolSearchTool }

    /// The script of a codemode call, when the transcript kept it.
    var script: String? { isScript ? string("code") : nil }

    var chipLabel: String {
        if isScript { return "Script" }
        if isToolSearch { return "Find tools" }
        switch tag {
        case "exec": return "Run"
        case "readFile": return "Read"
        case "writeFile": return "Write"
        case "editFile": return "Edit"
        case "applyPatch": return "Patch"
        case "search": return "Search"
        case "glob": return "Glob"
        case "webFetch": return "Fetch"
        case "webSearch": return "Web"
        case "todo": return "Todo"
        case "mcp": return "MCP"
        default: return "Tool"
        }
    }

    var chipDetail: String {
        if isScript { return script.map(Self.scriptSummary) ?? "" }
        if isToolSearch { return string("query") ?? "" }
        switch tag {
        case "exec": return string("command") ?? ""
        case "readFile", "writeFile", "editFile": return shortPath(string("path") ?? "")
        case "applyPatch":
            let changes = (fields["changes"] as? [String])?.count ?? 0
            return changes == 1 ? "1 file" : "\(changes) files"
        case "search": return string("pattern") ?? ""
        case "glob": return string("pattern") ?? ""
        case "webFetch": return string("url") ?? ""
        case "webSearch": return string("query") ?? ""
        case "todo":
            return string("summary") ?? "task list"
        case "mcp":
            let server = string("server").map { "\($0) · " } ?? ""
            return server + (string("tool") ?? "")
        default: return string("name") ?? ""
        }
    }

    var chipSymbol: String {
        if isScript { return "chevron.left.forwardslash.chevron.right" }
        if isToolSearch { return "magnifyingglass" }
        switch tag {
        case "exec": return "terminal"
        case "readFile", "applyPatch": return "doc.text"
        case "writeFile": return "doc.badge.plus"
        case "editFile": return "pencil"
        case "search": return "magnifyingglass"
        case "glob": return "folder"
        case "webFetch", "webSearch": return "globe"
        case "todo": return "checklist"
        default: return "square.grid.2x2"
        }
    }

    private func shortPath(_ path: String) -> String {
        let comps = path.split(separator: "/")
        guard comps.count > 2 else { return path }
        return comps.suffix(2).joined(separator: "/")
    }

    /// view.rs `script_tools`: the tools a script calls, in order of first
    /// use — `tools.read(…)` and `tools["read"](…)`, an MCP tool by its tool
    /// name. Read off the source, so this names what the script is about;
    /// the nested chips under it say what actually ran.
    static func scriptTools(_ code: String) -> [String] {
        func ident(_ b: UInt8) -> Bool {
            (b >= 0x30 && b <= 0x39) || (b >= 0x41 && b <= 0x5A) || (b >= 0x61 && b <= 0x7A)
                || b == UInt8(ascii: "_") || b == UInt8(ascii: "$")
        }
        let bytes = Array(code.utf8)
        let global = Array("tools".utf8)
        let quotes: [UInt8] = [UInt8(ascii: "\""), UInt8(ascii: "'"), UInt8(ascii: "`")]
        var names: [String] = []
        var at = 0
        while at + global.count <= bytes.count {
            guard bytes[at..<at + global.count].elementsEqual(global) else {
                at += 1
                continue
            }
            let start = at
            let end = at + global.count
            at = end
            // `ALL_TOOLS`, `myTools.x` and `x.tools.y` are not the global.
            if start > 0, ident(bytes[start - 1]) || bytes[start - 1] == UInt8(ascii: ".") { continue }
            guard end < bytes.count else { continue }
            var name = ""
            if bytes[end] == UInt8(ascii: ".") {
                var stop = end + 1
                while stop < bytes.count, ident(bytes[stop]) { stop += 1 }
                name = String(decoding: bytes[(end + 1)..<stop], as: UTF8.self)
            } else if bytes[end] == UInt8(ascii: "["), end + 1 < bytes.count, quotes.contains(bytes[end + 1]),
                      let close = bytes[(end + 2)...].firstIndex(of: bytes[end + 1]) {
                name = String(decoding: bytes[(end + 2)..<close], as: UTF8.self)
            }
            guard !name.isEmpty else { continue }
            let shown = mcpToolLabel(name) ?? name
            if !names.contains(shown) { names.append(shown) }
        }
        return names
    }

    /// The tool part of a Pi MCP tool name (`mcp__{server}__{tool}`). A
    /// script only carries Pi's sanitized server name; the nested MCP chips
    /// name the server as configured, so the summary leaves it to them.
    private static func mcpToolLabel(_ name: String) -> String? {
        guard name.hasPrefix("mcp__") else { return nil }
        let rest = name.dropFirst("mcp__".count)
        guard let split = rest.range(of: "__") else { return nil }
        let server = rest[..<split.lowerBound]
        let tool = rest[split.upperBound...]
        return server.isEmpty || tool.isEmpty ? nil : String(tool)
    }

    /// view.rs `script_summary`: a script chip's one-line detail — the tools
    /// it calls, else its first line of code (the `// @options:` header is
    /// configuration, not content).
    static func scriptSummary(_ code: String) -> String {
        let tools = scriptTools(code)
        if !tools.isEmpty { return tools.joined(separator: ", ") }
        return code.split(omittingEmptySubsequences: false, whereSeparator: \.isNewline)
            .lazy
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .first { !$0.isEmpty && !$0.hasPrefix("// @options:") } ?? ""
    }

    /// transcript.rs `SCRIPT_DETAIL_MAX_LINES`: a script gets more room than a
    /// command's echo before the counted tail.
    static let scriptBodyMaxLines = 80

    /// transcript.rs `call_block` for a script: the code itself, without the
    /// blank lines around it (a model's script routinely opens with a
    /// newline), capped at `scriptBodyMaxLines` with the rest counted.
    static func scriptBody(_ code: String) -> (code: String, truncatedBy: Int)? {
        var lines = code.split(omittingEmptySubsequences: false, whereSeparator: \.isNewline)
            .drop(while: { $0.trimmingCharacters(in: .whitespaces).isEmpty })
            .map(String.init)
        while lines.last?.trimmingCharacters(in: .whitespaces).isEmpty == true { lines.removeLast() }
        guard !lines.isEmpty else { return nil }
        let truncatedBy = max(lines.count - scriptBodyMaxLines, 0)
        return (lines.prefix(scriptBodyMaxLines).joined(separator: "\n"), truncatedBy)
    }
}
