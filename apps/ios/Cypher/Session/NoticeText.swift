import Foundation

// Extension notices (proto notice.rs): an extension's `notify` during a model
// turn — a provider bridge's rate-limit warning — is never reply text. The
// host keeps the chat's latest one on its session row (`notice`); the status
// strip shows it, compact, for `SessionNotice.ttlMs`. Both readings below
// run protocol/vectors/notice-text-v1.json, as
// crates/proto/tests/notice_text_vectors.rs does.

/// proto notice.rs `NoticeLevel`.
enum NoticeLevel: String, Hashable {
    case info, warning
}

/// proto notice.rs `SessionNotice`: the chat's latest extension notice.
struct SessionNotice: Hashable {
    var level: NoticeLevel
    /// The extension's text as it sent it.
    var text: String
    /// When the host received it, Unix ms.
    var at: Int64

    /// `NOTICE_TTL_SECS`: how long it shows after it arrived.
    static let ttlMs: Int64 = 90 * 1000

    func isLive(now: Int64) -> Bool {
        now - at < Self.ttlMs
    }

    /// What the strip shows (`display_text`).
    var displayText: String { NoticeText.compact(text) }

    init(level: NoticeLevel, text: String, at: Int64) {
        self.level = level
        self.text = text
        self.at = at
    }

    /// rows.rs: lenient — a malformed value drops the notice, never the row.
    /// `at` is chrono's RFC 3339.
    init?(_ value: JSONValue?) {
        guard let object = value?.objectValue,
            let level = object["level"]?.stringValue.flatMap(NoticeLevel.init(rawValue:)),
            let text = object["text"]?.stringValue,
            let at = object["at"]?.stringValue.flatMap(RecoveryProgress.rfc3339Ms)
        else { return nil }
        self.init(level: level, text: text, at: at)
    }
}

/// proto notice.rs `compact_notice` / `strip_reply_notices`.
enum NoticeText {
    /// pi-claude-bridge's warning: `Claude rate limit warning: 58% used (seven_day)`.
    static let rateLimitPrefix = "Claude rate limit warning: "

    /// `RATE_LIMIT_WINDOWS`: the Claude SDK's window names → the strip's.
    /// An unknown window shows its raw name.
    static let rateLimitWindows: [String: String] = [
        "five_hour": "5-hour",
        "seven_day": "weekly",
        "seven_day_opus": "weekly Opus",
        "seven_day_sonnet": "weekly Sonnet",
    ]

    /// One warning at the start of `text` (`parse_rate_limit_prefix`): its
    /// percent, its window (nil: none; "": an empty one) and how many
    /// characters the warning spans.
    private static func leadingRateLimit(_ text: Substring) -> (
        percent: Substring, window: Substring?, end: Substring.Index
    )? {
        guard text.hasPrefix(rateLimitPrefix) else { return nil }
        var rest = text.dropFirst(rateLimitPrefix.count)
        let digits = rest.prefix { $0.isASCII && $0.isNumber }
        guard !digits.isEmpty else { return nil }
        rest = rest.dropFirst(digits.count)
        guard rest.hasPrefix("% used") else { return nil }
        rest = rest.dropFirst("% used".count)
        var window: Substring?
        if rest.hasPrefix(" (") {
            let inner = rest.dropFirst(2)
            guard let close = inner.firstIndex(of: ")") else { return nil }
            let name = inner[..<close]
            guard !name.contains("\n") else { return nil }
            window = name
            rest = inner[inner.index(after: close)...]
        }
        return (digits, window, rest.startIndex)
    }

    /// The whole pattern: one warning and nothing but whitespace after it.
    private static func rateLimit(_ text: Substring) -> (percent: Substring, window: Substring?)? {
        guard let parsed = leadingRateLimit(text),
            text[parsed.end...].allSatisfy(\.isWhitespace)
        else { return nil }
        return (parsed.percent, parsed.window)
    }

    /// `compact_notice`: `Claude: 58% of weekly limit`; text of no known
    /// shape keeps its first line, trimmed.
    static func compact(_ text: String) -> String {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if let warning = rateLimit(Substring(trimmed)) {
            let name = warning.window.map { $0.trimmingCharacters(in: .whitespaces) } ?? ""
            if name.isEmpty { return "Claude: \(warning.percent)% of limit" }
            return "Claude: \(warning.percent)% of \(rateLimitWindows[name] ?? name) limit"
        }
        let first = trimmed.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false).first ?? ""
        return first.trimmingCharacters(in: .whitespaces)
    }

    /// `is_reply_notice`: the whole text is one notice.
    static func isReplyNotice(_ text: String) -> Bool {
        let body = text.drop { $0.isWhitespace }
        return rateLimit(body) != nil
    }

    /// `strip_reply_notices`: `text` without the notices an older harness
    /// streamed into the reply — only ones at the very start or end that
    /// match the whole pattern. nil when nothing was stripped.
    static func strippingReplyNotices(_ text: String) -> String? {
        guard text.contains(rateLimitPrefix) else { return nil }
        var rest = Substring(text)
        while true {
            let body = rest.drop { $0.isWhitespace }
            guard let parsed = leadingRateLimit(body) else { break }
            rest = body[parsed.end...]
        }
        while let found = rest.range(of: rateLimitPrefix, options: .backwards) {
            guard rateLimit(rest[found.lowerBound...]) != nil else { break }
            rest = rest[..<found.lowerBound]
        }
        guard rest.count != text.count else { return nil }
        if rest.allSatisfy(\.isWhitespace) { return "" }
        return String(rest).trimmingCharacters(in: CharacterSet(charactersIn: "\r\n"))
    }

    /// rows.rs `without_reply_notices`: an assistant entry as it reads
    /// without such notices — a part that is only one empties.
    static func withoutReplyNotices(_ entry: MessageEntry) -> MessageEntry {
        guard entry.role != .user else { return entry }
        var cleaned = entry
        var changed = false
        cleaned.parts = entry.parts.map { part in
            guard case .text(let id, let text, let agentText) = part,
                let stripped = strippingReplyNotices(text)
            else { return part }
            changed = true
            return .text(id: id, text: stripped, agentText: agentText)
        }
        return changed ? cleaned : entry
    }
}
