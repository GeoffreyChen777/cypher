//! Mock harness for engine/UI tests: replays a scripted event sequence.

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SteeringMode,
    UserInputQuestion,
};

use crate::{Harness, HarnessError, RunControls};

pub struct MockHarness {
    pub script: Vec<AgentEvent>,
}

/// The scripted question set for the `CYPHER_MOCK_QUESTION` variant (exercises
/// the QuestionPanel end-to-end: single-select page, multi-select page).
fn question_script() -> Vec<UserInputQuestion> {
    vec![
        UserInputQuestion {
            id: "q-sync".into(),
            header: "Question".into(),
            question: "Which sync strategy should the rewrite use?".into(),
            options: vec![
                "Poll the doc host every 120ms".into(),
                "Event-driven fold with coalesced commits".into(),
                "Hybrid: event-driven with a polling fallback".into(),
            ],
            multi_select: false,
        },
        UserInputQuestion {
            id: "q-gates".into(),
            header: "Question".into(),
            question: "Which suites should gate the merge?".into(),
            options: vec![
                "Unit tests".into(),
                "End-to-end (two-device)".into(),
                "Golden screenshots".into(),
            ],
            multi_select: true,
        },
    ]
}

/// The scripted work for the `CYPHER_MOCK_WORK` variant: thinking between
/// every few commands, the way real models work — eight thoughts around ten
/// commands, one of them failing. The transcript folds the whole run behind
/// one "Ran 10 commands · 8 thoughts · 1 failed" row instead of sixteen
/// alternating "Thought" / "Ran N commands" rows.
pub fn work_script() -> Vec<AgentEvent> {
    // A thought's title and body, then the commands it led to (and whether
    // each failed).
    type Step = (&'static str, &'static str, &'static [(&'static str, bool)]);
    let steps: [Step; 8] = [
        (
            "**Locating the fold**\n\n",
            "Events fold into parts before anything syncs. Find that entry point and the writer behind it.",
            &[
                ("rg -n \"fn fold_event_into_parts\" crates", false),
                ("rg -ln \"SegmentWriter\" crates", false),
            ],
        ),
        (
            "**Reading the writer**\n\n",
            "The writer appends into `LoroText`. Check how it batches commits.",
            &[("sed -n '1,80p' crates/doc/src/writer.rs", false)],
        ),
        (
            "**Checking the commit cadence**\n\n",
            "The 120ms coalescing is the claim to verify — look for the timer.",
            &[("rg -n \"120\" crates/doc/src", false)],
        ),
        (
            "**Running the fold tests**\n\n",
            "Before explaining the path, make sure it is green.",
            &[
                ("cargo test -p cypher-doc fold", false),
                ("cargo test -p cypher-doc writer", true),
            ],
        ),
        (
            "**A flaky writer test**\n\n",
            "One writer test failed on a timing assertion. Re-run it alone and see when it last changed.",
            &[
                (
                    "cargo test -p cypher-doc writer::coalesces -- --nocapture",
                    false,
                ),
                ("git log -3 --oneline -- crates/doc/src/writer.rs", false),
            ],
        ),
        (
            "**Tracing the relay**\n\n",
            "Commits leave through the session room; confirm the fan-out.",
            &[("rg -n \"SessionRoom\" edge/src", false)],
        ),
        (
            "**Confirming the device side**\n\n",
            "Each device folds the same commits back into transcript rows.",
            &[("rg -n \"fn rows_for_entry\" crates/ui/src", false)],
        ),
        (
            "**Writing it up**\n\n",
            "Enough to walk the pipeline in order: command, host, fold, relay.",
            &[],
        ),
    ];
    let mut events = Vec::new();
    let mut call = 0;
    for (title, body, commands) in steps {
        // Two deltas per thought, so `CYPHER_MOCK_DELAY_MS` shows it stream.
        for text in [title, body] {
            events.push(AgentEvent::ReasoningDelta { text: text.into() });
        }
        for &(command, is_error) in commands {
            call += 1;
            let id = format!("mock-work-{call}");
            events.push(AgentEvent::ToolCall {
                id: id.clone(),
                call: cypher_proto::ToolCall::Exec {
                    command: command.into(),
                },
            });
            events.push(AgentEvent::ToolResult {
                id,
                is_error,
                output: is_error.then(|| {
                    "test writer::coalesces ... FAILED\n\
                     assertion failed: commits <= 2 (got 3 within 120ms)"
                        .into()
                }),
                diff: None,
            });
        }
    }
    events
}

#[async_trait]
impl Harness for MockHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Mock"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![
            Model {
                id: "mock-1".into(),
                label: "Mock 1".into(),
                description: None,
                reasoning_levels: vec![ReasoningLevel::Medium],
                options: vec![],
            },
            // Claude-mirroring demo model: lets scripted runs carry the same
            // chip labels ("Fable 5 · High") as a real Claude session.
            Model {
                id: "mock-fable-5".into(),
                label: "Fable 5".into(),
                description: None,
                reasoning_levels: vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High,
                    ReasoningLevel::XHigh,
                ],
                options: vec![],
            },
        ])
    }
    async fn run(
        &self,
        _request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        // Optional pacing knob for demos/manual testing: `CYPHER_MOCK_DELAY_MS`
        // spaces the scripted events out so live-run UI states (working
        // indicator, streaming fade, trailing tool-group auto-open) are
        // observable. Unset (the default, and in tests) streams instantly.
        let delay_ms = cypher_env::var("MOCK_DELAY_MS")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let delay = std::time::Duration::from_millis(delay_ms);

        // Dev/testing knob: `CYPHER_MOCK_QUESTION=1` swaps in a run that asks
        // the user questions mid-stream via `controls.request_input` (the
        // engine mints the request id, emits `InputRequested`, and resolves it
        // from the `RespondInput` doc command) — the only data-side way to put
        // the QuestionPanel on screen.
        let question_mode =
            cypher_env::var("MOCK_QUESTION").is_some_and(|v| !v.is_empty() && v != "0");
        if question_mode {
            let request_input = controls.request_input;
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
            tokio::spawn(async move {
                let pause = if delay_ms == 0 {
                    std::time::Duration::from_millis(50)
                } else {
                    delay
                };
                tokio::time::sleep(pause).await;
                let _ = tx.send(AgentEvent::TextDelta {
                    text:
                        "Before I wire the reconciliation path I need two decisions from you.\n\n"
                            .into(),
                });
                tokio::time::sleep(pause).await;
                let answers = request_input(question_script()).await.unwrap_or_default();
                let picked: Vec<String> = answers
                    .iter()
                    .flat_map(|a| a.labels.iter().cloned())
                    .collect();
                tokio::time::sleep(pause).await;
                let _ = tx.send(AgentEvent::TextDelta {
                    text: format!(
                        "Locked in: **{}**. Proceeding with the plan.",
                        if picked.is_empty() {
                            "your defaults".to_string()
                        } else {
                            picked.join("**, **")
                        }
                    ),
                });
                let _ = tx.send(AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                });
            });
            let stream = futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|event| (Ok(event), rx))
            });
            return Ok(stream.boxed());
        }

        // Dev/testing knob: `CYPHER_MOCK_REPEAT=N` loops the script body N times
        // before the final Done — long single-reply streams for frame-cost /
        // smoothness measurement (the terminal `Done` is emitted exactly once,
        // at the very end).
        let repeat = cypher_env::var("MOCK_REPEAT")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        // Dev/testing knob: `CYPHER_MOCK_ERROR=1` appends a scripted error
        // before the terminal Done — the only data-side way to put the
        // transcript ErrorChip on screen with the mock harness.
        let mock_error = cypher_env::var("MOCK_ERROR").is_some_and(|v| !v.is_empty() && v != "0");
        // Dev/testing knob: `CYPHER_MOCK_TABLE=1` appends scripted GFM tables
        // before the terminal Done — a plain 3-column grid plus a wide/uneven
        // one (long prose cell beside short cells, mixed alignment) for
        // table-styling checks against the reference app.
        let mock_table = cypher_env::var("MOCK_TABLE").is_some_and(|v| !v.is_empty() && v != "0");
        let done_ix = self
            .script
            .iter()
            .position(|e| matches!(e, AgentEvent::Done { .. }))
            .unwrap_or(self.script.len());
        let (body, tail) = self.script.split_at(done_ix);
        let error_event = mock_error.then(|| AgentEvent::Error {
            message: "Claude usage limit reached — try again after the limit resets.".into(),
        });
        // Dev/testing knob: `CYPHER_MOCK_CODE=1` appends rust + ts code blocks
        // (keywords, strings, numbers, comments) plus inline code — for
        // syntax-palette and inline-code styling checks against the reference.
        let mock_code = cypher_env::var("MOCK_CODE").is_some_and(|v| !v.is_empty() && v != "0");
        let code_event = mock_code.then(|| AgentEvent::TextDelta {
            text: concat!(
                "\n### Code check\n\n",
                "The `fold_event_into_parts` helper feeds `writer.sync` on a `120ms` cadence:\n\n",
                "```rust\n",
                "// Fold one event into the accumulated parts.\n",
                "pub fn fold(mut acc: Vec<Part>, event: &AgentEvent) -> Vec<Part> {\n",
                "    let label = \"delta\";\n",
                "    if acc.len() > 128 {\n",
                "        acc.truncate(64); // keep the tail hot\n",
                "    }\n",
                "    acc\n",
                "}\n",
                "```\n\n",
                "```ts\n",
                "// Subscribe and fold on the client.\n",
                "const room = await connect(\"wss://mesh.local\", { retries: 3 });\n",
                "export function fold(parts: Part[], event: AgentEvent): Part[] {\n",
                "    return event.kind === \"delta\" ? [...parts, event] : parts;\n",
                "}\n",
                "```\n\n",
            )
            .into(),
        });
        let table_event = mock_table.then(|| AgentEvent::TextDelta {
            text: "\n### Table check\n\n\
                | Column A | Column B | Column C |\n\
                |---|---|---|\n\
                | a1 | b1 | c1 |\n\
                | a2 | b2 | c2 |\n\n\
                And a wide, uneven one:\n\n\
                | Stage | What happens | p95 |\n\
                |:--|:--|--:|\n\
                | Fold | Events fold into parts and diff into the Loro doc on a 120ms coalesced commit cadence, keeping the oplog RLE-merged across devices | 4.2ms |\n\
                | Sync | Session-room fan-out | 18ms |\n\n"
                .into(),
        });
        // Dev/testing knob: `CYPHER_MOCK_MEND=1` appends a link/list-heavy
        // passage — bold-led list items, inline links, emphasis, strikethrough
        // — the shapes whose half-streamed markers the display mend
        // (crates/ui markdown/mend.rs) must hold steady while streaming.
        let mock_mend = cypher_env::var("MOCK_MEND").is_some_and(|v| !v.is_empty() && v != "0");
        let mend_event = mock_mend.then(|| AgentEvent::TextDelta {
            text: concat!(
                "\n### Streaming mend check\n\n",
                "Inline styles hold while text arrives: **bold stays bold**, ",
                "*italic stays italic*, `code stays code`, and ~~this stays struck~~.\n\n",
                "- **Fold** — parts diff into the [Loro doc](https://loro.dev) on a 120ms cadence\n",
                "- **Relay** — commits fan out through the [session room](https://developers.cloudflare.com/durable-objects/) to every device\n",
                "- **Paint** — the [display tree](https://github.com/pulldown-cmark/pulldown-cmark) mends hanging markers in the last block only\n\n",
                "Links above never flash their URLs, and closing markers never reflow the paragraph.\n",
            )
            .into(),
        });
        // With the code knob, also exercise a MULTILINE Exec command — the
        // round-9 chip breaker shape ("set -e\nfixture_in_original=0"): the
        // Run chip must stay one 30px line.
        let code_tool_events = mock_code
            .then(|| {
                [
                    AgentEvent::ToolCall {
                        id: "mock-code-tool".into(),
                        call: cypher_proto::ToolCall::Exec {
                            command: "set -e\nfixture_in_original=0\ngrep -rn \"veil\" crates/ui/src | wc -l".into(),
                        },
                    },
                    AgentEvent::ToolResult {
                        id: "mock-code-tool".into(),
                        is_error: false,
                        output: None,
                        diff: None,
                    },
                ]
            })
            .into_iter()
            .flatten();
        // Dev/testing knob: `CYPHER_MOCK_THINK=1` opens the reply with scripted
        // thinking (`ReasoningDelta`, a few chunks so `CYPHER_MOCK_DELAY_MS`
        // shows it stream) — the data-side way to put the collapsed thought
        // toggle on screen with the mock harness.
        let mock_think = cypher_env::var("MOCK_THINK").is_some_and(|v| !v.is_empty() && v != "0");
        let think_events = mock_think
            .then(|| {
                [
                    "**Planning the answer**\n\n",
                    "The user wants the streaming path explained. ",
                    "Walk it in order — command, host, fold — ",
                    "then run the tests to confirm nothing regressed.",
                ]
                .map(|text| AgentEvent::ReasoningDelta { text: text.into() })
            })
            .into_iter()
            .flatten();
        // Dev/testing knob: `CYPHER_MOCK_WORK=1` opens the reply (after any
        // `CYPHER_MOCK_THINK` thinking) with a run of thoughts between tool
        // calls ([`work_script`]) — the data-side way to put the merged work
        // row on screen. The scripted answer that follows closes the run.
        let mock_work = cypher_env::var("MOCK_WORK").is_some_and(|v| !v.is_empty() && v != "0");
        let work_events = mock_work.then(work_script).into_iter().flatten();
        // Thinking follows a leading SessionStarted, which resets the fold.
        let lead = body
            .iter()
            .take_while(|e| matches!(e, AgentEvent::SessionStarted { .. }))
            .count();
        let events: Vec<Result<AgentEvent, HarnessError>> = body[..lead]
            .iter()
            .cloned()
            .chain(think_events)
            .chain(work_events)
            .chain(body[lead..].iter().cloned())
            .chain(body.iter().cycle().take(body.len() * (repeat - 1)).cloned())
            .chain(code_tool_events)
            .chain(code_event)
            .chain(table_event)
            .chain(mend_event)
            .chain(error_event)
            .chain(tail.iter().cloned())
            .map(Ok)
            .collect();
        // Dev/testing knob: `CYPHER_MOCK_CHARS=N` re-chunks every TextDelta
        // into N-char deltas, so `CYPHER_MOCK_DELAY_MS` paces *characters*
        // instead of whole scripted blocks — delta boundaries then land inside
        // inline markers and links, which is the streaming shape real
        // harnesses produce and the display mend exists for.
        let chunk_chars = cypher_env::var("MOCK_CHARS")
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0);
        let events: Vec<Result<AgentEvent, HarnessError>> = match chunk_chars {
            None => events,
            Some(n) => events
                .into_iter()
                .flat_map(|event| match event {
                    Ok(AgentEvent::TextDelta { text }) => {
                        let chars: Vec<char> = text.chars().collect();
                        chars
                            .chunks(n)
                            .map(|c| {
                                Ok(AgentEvent::TextDelta {
                                    text: c.iter().collect(),
                                })
                            })
                            .collect::<Vec<_>>()
                    }
                    other => vec![other],
                })
                .collect(),
        };
        if delay_ms == 0 {
            return Ok(futures::stream::iter(events).boxed());
        }
        Ok(futures::stream::iter(events)
            .then(move |event| async move {
                tokio::time::sleep(delay).await;
                event
            })
            .boxed())
    }
}
