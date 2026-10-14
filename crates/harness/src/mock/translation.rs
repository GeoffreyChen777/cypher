//! Translation demo reply for the mock harness: what Cypher's Pi translation
//! extension and the Pi harness produce for a translated session, scripted,
//! so the transcript can be seen translating without Pi or a translation
//! model.
//!
//! The user writes Chinese and the agent works in English. The prompt arrives
//! translated ([`AgentEvent::InputTranslation`]); each statement the agent
//! makes alongside a tool call is translated in the background, its frames
//! (`Translation` with a `source`) landing while the next tools run; and the
//! final answer is translated once it has finished streaming (`Translation`
//! without one). Frames carry the whole rendering, as the extension's do.

use cypher_proto::{AgentEvent, ToolCall};

/// Prompt marker: reply with the translation showcase, translations appended
/// below the originals (the extension's default).
pub const MARKER: &str = "[mock:translation]";
/// Prompt marker: the same reply with translations replacing the originals.
pub const REPLACE_MARKER: &str = "[mock:translation:replace]";

/// The prompt the showcase answers, as the user typed it.
pub const PROMPT: &str = "服务器限流的时候，同步客户端重试得太快了。帮我查一下原因并修好。";
const PROMPT_TRANSLATED: &str =
    "The sync client retries too quickly when the server rate-limits it. Find out why and fix it.";

/// How a translation is rendered (`renderTranslation` in
/// `pi-runtime/extensions/cypher-translation.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Append,
    Replace,
}

/// The mode a prompt asks for, if it asks for the showcase at all.
pub fn mode_of(prompt: &str) -> Option<Mode> {
    if prompt.contains(REPLACE_MARKER) {
        Some(Mode::Replace)
    } else if prompt.contains(MARKER) {
        Some(Mode::Append)
    } else {
        None
    }
}

fn render(mode: Mode, original: &str, translated: &str) -> String {
    match mode {
        Mode::Append => format!("{original}\n\n---\n\n{translated}"),
        Mode::Replace => translated.to_owned(),
    }
}

/// A statement made alongside tool calls: what the agent says, its
/// translation, and the calls (with their output) that follow it.
struct Step {
    text: &'static str,
    translated: &'static str,
    tools: &'static [(ToolKind, &'static str, &'static str)],
}

#[derive(Clone, Copy)]
enum ToolKind {
    Search,
    Read,
    Edit,
    Exec,
}

const STEPS: &[Step] = &[
    Step {
        text: "I'll start by finding where the sync client decides how long to wait before a retry.",
        translated: "我先找一下同步客户端在哪里决定重试前要等多久。",
        tools: &[(
            ToolKind::Search,
            "retry_after",
            "crates/sync/src/client.rs:142:    let wait = retry_after(attempt);\ncrates/sync/src/backoff.rs:18:pub fn retry_after(attempt: u32) -> Duration {",
        )],
    },
    Step {
        text: "Found it: `retry_after` only looks at the attempt number. A 429's `Retry-After` header is never read, so the client waits one second however long the server asked for. Let me check what the tests cover before changing it.",
        translated: "找到了：`retry_after` 只看重试次数，从不读取 429 响应里的 `Retry-After` 头，所以无论服务器要求等多久，客户端都只等一秒。改之前我先看看测试覆盖了什么。",
        tools: &[
            (
                ToolKind::Read,
                "crates/sync/src/backoff.rs",
                "pub fn retry_after(attempt: u32) -> Duration {\n    Duration::from_secs(1).min(Duration::from_millis(250 << attempt))\n}",
            ),
            (
                ToolKind::Exec,
                "cargo test -p sync backoff",
                "running 3 tests\ntest backoff::grows_per_attempt ... ok\ntest backoff::caps_at_one_second ... ok\ntest backoff::first_retry_is_quick ... ok\n\ntest result: ok. 3 passed; 0 failed",
            ),
        ],
    },
    Step {
        text: "The tests only pin the attempt-based curve. I'll make the client honour `Retry-After` when the server sends one, capped at 30 seconds, and add a test for it.",
        translated: "测试只约束了按重试次数增长的曲线。我会让客户端在服务器给出 `Retry-After` 时遵守它（上限 30 秒），并为此补一个测试。",
        tools: &[
            (ToolKind::Edit, "crates/sync/src/backoff.rs", ""),
            (
                ToolKind::Exec,
                "cargo test -p sync backoff",
                "running 4 tests\ntest backoff::grows_per_attempt ... ok\ntest backoff::caps_at_one_second ... ok\ntest backoff::first_retry_is_quick ... ok\ntest backoff::honours_retry_after ... ok\n\ntest result: ok. 4 passed; 0 failed",
            ),
        ],
    },
];

const ANSWER: &str = "The client was ignoring the server.\n\n\
**Cause.** `retry_after` in `crates/sync/src/backoff.rs` computed the wait from the attempt number alone and capped it at one second. A 429 response's `Retry-After` header was never read, so a rate-limited client came back after a second no matter how long the server asked it to wait — and was limited again.\n\n\
**Fix.**\n\
- `retry_after` now takes the response's `Retry-After` value and waits that long when it is present, capped at 30 seconds so a bad header cannot stall a sync.\n\
- Without the header, the existing attempt-based backoff is unchanged.\n\
- New test `honours_retry_after` pins both cases; all 4 backoff tests pass.";

const ANSWER_TRANSLATED: &str = "客户端一直在无视服务器的要求。\n\n\
**原因。** `crates/sync/src/backoff.rs` 里的 `retry_after` 只根据重试次数计算等待时间，并且上限是一秒。429 响应里的 `Retry-After` 头从来没被读取，所以被限流的客户端无论服务器要求等多久，一秒后就会回来——然后再次被限流。\n\n\
**修复。**\n\
- `retry_after` 现在会读取响应的 `Retry-After` 值，有这个值时就按它等待，上限 30 秒，避免异常的头让同步卡住。\n\
- 没有这个头时，原有的按次数退避保持不变。\n\
- 新测试 `honours_retry_after` 覆盖了这两种情况；4 个退避测试全部通过。";

/// A translation as its frames arrive: the original restated (the frame that
/// starts the extension's keepalive), then the translation growing in a few
/// chunks to the whole of it.
fn frames(mode: Mode, original: &str, translated: &str, chunks: usize) -> Vec<String> {
    let chars: Vec<char> = translated.chars().collect();
    let mut out = vec![original.to_owned()];
    for n in 1..=chunks {
        let end = chars.len() * n / chunks;
        let partial: String = chars[..end].iter().collect();
        out.push(render(mode, original, &partial));
    }
    out
}

fn call(kind: ToolKind, subject: &str) -> ToolCall {
    match kind {
        ToolKind::Search => ToolCall::Search {
            pattern: subject.into(),
            path: Some("crates/sync".into()),
        },
        ToolKind::Read => ToolCall::ReadFile {
            path: subject.into(),
        },
        ToolKind::Edit => ToolCall::EditFile {
            path: subject.into(),
            old_string: Some("pub fn retry_after(attempt: u32) -> Duration {".into()),
            new_string: Some(
                "pub fn retry_after(attempt: u32, header: Option<Duration>) -> Duration {\n    if let Some(wait) = header {\n        return wait.min(MAX_RETRY_AFTER);\n    }"
                    .into(),
            ),
        },
        ToolKind::Exec => ToolCall::Exec {
            command: subject.into(),
        },
    }
}

/// The showcase reply's events, between the script's opening and its Done.
pub(super) fn reply(mode: Mode) -> Vec<AgentEvent> {
    let mut events = vec![AgentEvent::InputTranslation {
        source: PROMPT.into(),
        text: PROMPT_TRANSLATED.into(),
    }];
    let mut call_ix = 0;
    // A step's last frame, held back until the agent has moved on.
    let mut late: Option<AgentEvent> = None;
    for step in STEPS {
        events.push(AgentEvent::TextDelta {
            text: step.text.into(),
        });
        // The previous statement's translation finishes only now, after the
        // next one has started: by then it is not the last text part, so its
        // frame finds it by `source`.
        events.extend(late.take());
        let mut pending = frames(mode, step.text, step.translated, 3)
            .into_iter()
            .map(|text| AgentEvent::Translation {
                text,
                source: Some(step.text.into()),
            })
            .collect::<std::collections::VecDeque<_>>();
        late = pending.pop_back();
        // The rest are spread across the step's tools: the translation runs
        // in the background while they do.
        let per_tool = pending.len().div_ceil(step.tools.len());
        for &(kind, subject, output) in step.tools {
            call_ix += 1;
            let id = format!("mock-translation-{call_ix}");
            events.push(AgentEvent::ToolCall {
                id: id.clone(),
                call: call(kind, subject),
            });
            events.extend(pending.drain(..per_tool.min(pending.len())));
            events.push(AgentEvent::ToolResult {
                id,
                is_error: false,
                output: (!output.is_empty()).then(|| output.into()),
                diff: None,
                permission: None,
            });
        }
    }
    for (ix, chunk) in ANSWER.split_inclusive("\n\n").enumerate() {
        events.push(AgentEvent::TextDelta { text: chunk.into() });
        if ix == 0 {
            events.extend(late.take());
        }
    }
    events.extend(
        frames(mode, ANSWER, ANSWER_TRANSLATED, 6)
            .into_iter()
            .map(|text| AgentEvent::Translation { text, source: None }),
    );
    events
}

/// The transcript the showcase settles on: each statement and the answer,
/// as the model wrote them and as they render once translated.
pub fn settled(mode: Mode) -> Vec<(String, String)> {
    STEPS
        .iter()
        .map(|step| (step.text, step.translated))
        .chain([(ANSWER, ANSWER_TRANSLATED)])
        .map(|(original, translated)| (original.to_owned(), render(mode, original, translated)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_pick_the_mode() {
        assert_eq!(mode_of("x [mock:translation]"), Some(Mode::Append));
        assert_eq!(mode_of("x [mock:translation:replace]"), Some(Mode::Replace));
        assert_eq!(mode_of("x"), None);
    }

    /// The point of the showcase: some step frame lands after a later
    /// statement has begun, so only its `source` can place it.
    #[test]
    fn a_step_finishes_translating_after_the_next_one_starts() {
        let events = reply(Mode::Append);
        let second = events
            .iter()
            .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == STEPS[1].text))
            .unwrap();
        assert!(events[second..].iter().any(|e| matches!(
            e,
            AgentEvent::Translation { source: Some(source), .. } if source == STEPS[0].text
        )));
    }
}
