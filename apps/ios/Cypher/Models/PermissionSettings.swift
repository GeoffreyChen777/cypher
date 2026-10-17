// A device's permission settings, as Settings → Permissions edits them
// (crates/engine/src/pi/permissions.rs `PiPermissionSnapshot` /
// `PiPermissionSettings`, read and written through the relay's
// GetPiPermissionSettings / SetPiPermissionSettings).

import Foundation

/// What a device's engine reports: its settings, whether it can sandbox,
/// and whether its `permissions.json` failed to parse.
struct PermissionSnapshot: Decodable, Equatable, Sendable {
    var settings: PermissionSettings
    var status: Status
    var unreadable: Bool

    struct Status: Decodable, Equatable, Sendable {
        /// The active Runtime ships the extension.
        var installed: Bool
        /// Landstrip works there; without it the extension stands down.
        var sandboxAvailable: Bool
        var implementation: String?
        var error: String?
    }
}

/// The settings, kept as the engine sent them: a save sends every field back
/// (the engine replaces the whole object), so the ones this version of the
/// app does not know must survive it. Typed views cover what the page edits.
struct PermissionSettings: Codable, Equatable, Sendable {
    var raw: [String: JSONValue]

    init(raw: [String: JSONValue]) {
        self.raw = raw
    }

    init(from decoder: Decoder) throws {
        raw = try [String: JSONValue](from: decoder)
    }

    func encode(to encoder: Encoder) throws {
        try raw.encode(to: encoder)
    }

    /// The approval keys the page shows, in its order (`PermissionApprovals`).
    static let approvalKeys = [
        "escalate", "externalWrite", "protectedWrite", "sensitiveRead", "sensitiveWrite", "network", "tools",
    ]

    /// The master switch: off unless turned on.
    var enabled: Bool {
        get { raw["enabled"].flatMap(\.boolValue) ?? false }
        set { raw["enabled"] = .bool(newValue) }
    }

    /// `deny`, `ask` or `assisted`; the model first unless set (the
    /// engine's `PermissionApprovals` default).
    func approval(_ key: String) -> String {
        raw["approval"]?.objectValue?[key]?.stringValue ?? "assisted"
    }

    mutating func setApproval(_ key: String, _ mode: String) {
        var approval = raw["approval"]?.objectValue ?? [:]
        approval[key] = .string(mode)
        raw["approval"] = .object(approval)
    }

    /// `provider/model` for the reviewing model; empty uses the chat's own.
    var judgeModel: String {
        get { raw["judgeModel"]?.stringValue ?? "" }
        set { raw["judgeModel"] = .string(newValue) }
    }

    /// `allow`, `elevate` or `deny`: the default for MCP and other extension tools.
    var toolsDefault: String {
        get { raw["toolsDefault"]?.stringValue ?? "allow" }
        set { raw["toolsDefault"] = .string(newValue) }
    }

    /// `open` or `filtered`.
    var networkMode: String {
        get { raw["networkMode"]?.stringValue ?? "open" }
        set { raw["networkMode"] = .string(newValue) }
    }

    /// Dev servers and tests on localhost.
    var localBinding: Bool {
        get { raw["localBinding"].flatMap(\.boolValue) ?? true }
        set { raw["localBinding"] = .bool(newValue) }
    }

    /// `sensitive`, `protected`, `writableSupport`, `unixSockets`, `allowDomains`.
    func list(_ key: String) -> [String] {
        raw[key]?.arrayValue?.compactMap(\.stringValue) ?? []
    }

    mutating func setList(_ key: String, _ items: [String]) {
        raw[key] = .array(items.map(JSONValue.string))
    }

    /// An "always allow" rule: a path (its subtree), `tool:<name>` or `net:<domain>`.
    struct Rule: Equatable {
        var capability: String
        var path: String
    }

    var rules: [Rule] {
        (raw["rules"]?.arrayValue ?? []).compactMap { value in
            guard let rule = value.objectValue, let path = rule["path"]?.stringValue else { return nil }
            return Rule(capability: rule["capability"]?.stringValue ?? "read", path: path)
        }
    }

    mutating func removeRule(at index: Int) {
        guard var list = raw["rules"]?.arrayValue, list.indices.contains(index) else { return }
        list.remove(at: index)
        raw["rules"] = .array(list)
    }

    /// A command rule (`cypher-permissions/commands.ts`): matching commands
    /// run outside the sandbox (`allow`), ask first, or never run.
    struct CommandRule: Equatable {
        var pattern: [String]
        var decision: String
        var label: String?
        var pinned: Bool

        /// The pattern as people read it: `**` reads as `…`.
        var text: String {
            pattern.map { word in
                if word == "**" { return "…" }
                return word.contains(where: { $0 == " " || $0 == "\t" || $0 == "'" || $0 == "\"" })
                    ? "\"\(word)\"" : word
            }
            .joined(separator: " ")
        }
    }

    var commands: [CommandRule] {
        (raw["commands"]?.arrayValue ?? []).compactMap { value in
            guard let rule = value.objectValue,
                let pattern = rule["match"]?.arrayValue?.compactMap(\.stringValue), !pattern.isEmpty
            else { return nil }
            return CommandRule(
                pattern: pattern, decision: rule["decision"]?.stringValue ?? "ask",
                label: rule["label"]?.stringValue, pinned: rule["sha256"]?.stringValue != nil)
        }
    }

    mutating func removeCommand(at index: Int) {
        guard var list = raw["commands"]?.arrayValue, list.indices.contains(index) else { return }
        list.remove(at: index)
        raw["commands"] = .array(list)
    }

    /// The settings as relay params.
    func params() throws -> [String: Any] {
        let data = try JSONEncoder().encode(self)
        return (try JSONSerialization.jsonObject(with: data) as? [String: Any]) ?? [:]
    }
}

extension JSONValue {
    var arrayValue: [JSONValue]? {
        if case .array(let value) = self { return value }
        return nil
    }
}
