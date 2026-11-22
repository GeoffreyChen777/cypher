// Code in prose showcase (`-demo -route chat:chat-code-typesetting`): one
// reply that mixes prose with short and long inline code, file references
// (one too long for its line), bold, a list, CJK text and a code block, so
// the one code scale, the shared box and the even line rhythm can be looked
// at in one screen; the prompt carries a mention chip.

import Foundation

extension DemoDataset {
    static let codeTypesettingChatId = "chat-code-typesetting"

    static func codeTypesettingChat(spaceId: String, cwd: String, config: ChatConfig, now: Int64) -> Chat {
        Chat(
            id: codeTypesettingChatId, deviceId: "dev-mac", title: "Code in prose",
            archived: false, cwd: cwd, branch: "main", checkoutId: nil,
            config: config, lastMessagePreview: "Run cargo test -p cypher-ui before you push.",
            lastMessageAt: now - 3_100_000, createdAt: now - 3_200_000,
            spaceId: spaceId, lastSeenAt: now - 3_100_000)
    }

    static func codeTypesettingTranscript(now: Int64) -> [MessageEntry] {
        let prompt =
            "Check how code reads in "
            + Mentions.fileLink(path: "crates/ui/src/markdown/render.rs", isDir: false) + " on a phone."
        return [
            MessageEntry(
                id: "u1", role: .user, parts: [.text(id: "t0", text: prompt)],
                createdAt: now - 3_200_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
            MessageEntry(
                id: "a1", role: .assistant, parts: [.text(id: "t0", text: codeTypesettingReply)],
                createdAt: now - 3_100_000, deviceId: "dev-mac", status: .complete, continuationOf: nil),
        ]
    }

    private static let codeTypesettingReply = #"""
        Run `cargo test -p cypher-ui` before you push; the long form is `cargo test -p cypher-ui --features gpui/test-support -- markdown::render::tests` and wraps. The box lives in `crates/ui/src/markdown/render.rs:934-960`, its tests in `render/tests.rs`, and the fixture in `crates/ui/src/markdown/fixtures/a_rather_long_generated_preview_fixture_name_that_goes_on.rs:120-148`.

        `crates/ui/src/kit/theme.rs`

        A line with no code at all keeps the rhythm of the lines around it, so the baselines stay the same distance apart from one line to the next, with code or without.

        **Bold with `inline code` and `theme.rs:292` inside.**

        - Build with `just ios`
        - `CODE_SCALE` sets the size of code in prose
        - `InlineCode.swift`

        运行 `cargo test -p cypher-ui` 然后查看 `render.rs`，有代码的行与没有代码的行行距一致，中文的基线也不变。

        ```rust
        pub const CODE_SCALE: f32 = 0.86;
        let size = font_size * CODE_SCALE;
        ```

        Inline code and chips share one box, `1.25em` of the code, centered on it.
        """#
}
