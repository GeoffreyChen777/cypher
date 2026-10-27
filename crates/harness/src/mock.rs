//! Mock harness for engine/UI tests: replays a scripted event sequence.

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SteeringMode,
    UserInputQuestion,
};

use crate::{Harness, HarnessError, RunControls};

mod artifacts;
mod changes;
mod diffs;
mod html;
mod math;
mod permissions;
mod progress;
mod translation;

pub use artifacts::{MARKER as ARTIFACTS_MARKER, REVISE_MARKER as ARTIFACTS_REVISE_MARKER};
pub use changes::{MARKER as CHANGES_MARKER, ROUNDS as CHANGES_ROUNDS};
pub use diffs::MARKER as DIFFS_MARKER;
pub use html::MARKER as HTML_MARKER;
pub use math::MARKER as MATH_MARKER;
pub use permissions::{
    CARD_MARKER as PERMISSION_CARD_MARKER,
    MULTILINE_CARD_MARKER as MULTILINE_PERMISSION_CARD_MARKER, STORY_MARKER as PERMISSIONS_MARKER,
    TALL_CARD_MARKER as TALL_PERMISSION_CARD_MARKER,
};
pub use progress::MARKER as PROGRESS_MARKER;
pub use translation::{
    MARKER as TRANSLATION_MARKER, Mode as TranslationMode, PROMPT as TRANSLATION_PROMPT,
    REPLACE_MARKER as TRANSLATION_REPLACE_MARKER, settled as translation_settled,
};

/// Prompt marker: the run asks one question through the engine's input bridge
/// (the session sits in awaiting-input) and replays its script once answered.
pub const ASK_MARKER: &str = "[mock:ask]";
/// Prompt marker: as [`ASK_MARKER`], with a question whose context and
/// options make its card taller than a short chat column.
pub const TALL_ASK_MARKER: &str = "[mock:ask:tall]";
/// Prompt marker: the run streams its opening, starts a command that never
/// finishes and keeps working until interrupted (Stop), ending
/// `Done { status: Interrupted }`. The open command is what keeps it live:
/// the engine parks a turn that goes silent after finished output, but
/// never one with a tool still running.
pub const HOLD_MARKER: &str = "[mock:hold]";

/// The id of the command a `[mock:hold]` run leaves running.
pub const HOLD_TOOL_ID: &str = "mock-hold-tool";

/// One step of a marked run: a scripted event, or a pause on the controls.
enum Step {
    Event(AgentEvent),
    /// Wait on a question: the tall one when true.
    Ask(bool),
    Hold,
}

fn interrupted() -> AgentEvent {
    AgentEvent::Done {
        status: DoneStatus::Interrupted,
        result: None,
        error: None,
        session_id: None,
    }
}

pub struct MockHarness {
    pub script: Vec<AgentEvent>,
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
            &[(
                "sed -n '1,80p' crates/doc/src/schema/segment_writer.rs",
                false,
            )],
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
                (
                    "git log -3 --oneline -- crates/doc/src/schema/segment_writer.rs",
                    false,
                ),
            ],
        ),
        (
            "**Tracing the relay**\n\n",
            "Commits leave through the chat's relay room; confirm the fan-out.",
            &[("rg -n \"ChatRoom\" apps/edge/src", false)],
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
                permission: None,
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
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        // The engine's progress narrator asking for a reading of a long turn
        // ([`PROGRESS_MARKER`] runs one): answered at once from the plan it
        // quotes.
        if progress::is_reading(&request.prompt) {
            return Ok(futures::stream::iter(
                [
                    AgentEvent::TextDelta {
                        text: progress::reading(&request.prompt),
                    },
                    AgentEvent::Done {
                        status: DoneStatus::Completed,
                        result: None,
                        error: None,
                        session_id: None,
                    },
                ]
                .map(Ok),
            )
            .boxed());
        }
        // Optional pacing knob for demos/manual testing: `CYPHER_MOCK_DELAY_MS`
        // spaces the scripted events out so live-run UI states (working
        // indicator, streaming fade, trailing tool-group auto-open) are
        // observable. Unset (the default, and in tests) streams instantly.
        let delay_ms = cypher_env::var("MOCK_DELAY_MS")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let delay = std::time::Duration::from_millis(delay_ms);

        let done_ix = self
            .script
            .iter()
            .position(|e| matches!(e, AgentEvent::Done { .. }))
            .unwrap_or(self.script.len());
        let (body, tail) = self.script.split_at(done_ix);
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
        // Permission control demos ([`PERMISSIONS_MARKER`],
        // [`PERMISSION_CARD_MARKER`], [`TALL_PERMISSION_CARD_MARKER`],
        // [`MULTILINE_PERMISSION_CARD_MARKER`]): the
        // transcript rows and the permission card, scripted, between the
        // script's opening and its Done.
        if let Some(demo) = permissions::demo_of(&request.prompt) {
            return Ok(permissions::run(
                body[..lead].to_vec(),
                tail.to_vec(),
                controls,
                delay,
                demo,
            ));
        }
        // Turn changes demo ([`CHANGES_MARKER`]): round N edits the run's
        // checkout between the script's opening and its Done.
        if let Some(round) = changes::round_of(&request.prompt) {
            return Ok(changes::run(
                body[..lead].to_vec(),
                tail.to_vec(),
                std::path::PathBuf::from(&request.cwd),
                round,
                delay,
            ));
        }
        // Narrated progress demo ([`PROGRESS_MARKER`]): a run of about four
        // minutes through a plan that gains a step midway, between the
        // script's opening and its Done. `CYPHER_MOCK_PROGRESS=1` plays it for
        // every prompt, so a message typed into the demo runs long too.
        if request.prompt.contains(PROGRESS_MARKER)
            || cypher_env::var("MOCK_PROGRESS").is_some_and(|v| !v.is_empty() && v != "0")
        {
            return Ok(progress::run(
                body[..lead].to_vec(),
                tail.to_vec(),
                controls.interrupt,
            ));
        }
        // Diff fence, math, HTML preview, artifact and translation showcases
        // ([`DIFFS_MARKER`], [`MATH_MARKER`], [`HTML_MARKER`],
        // [`ARTIFACTS_MARKER`], [`ARTIFACTS_REVISE_MARKER`],
        // [`TRANSLATION_MARKER`]): their reply replaces the
        // script's text and tools. `CYPHER_MOCK_TRANSLATION=1` (or `replace`)
        // answers every prompt with the translation showcase, so a message
        // typed into the demo translates too.
        let translation_mode = translation::mode_of(&request.prompt).or_else(|| {
            cypher_env::var("MOCK_TRANSLATION")
                .filter(|v| !v.is_empty() && v != "0")
                .map(|v| {
                    if v == "replace" {
                        TranslationMode::Replace
                    } else {
                        TranslationMode::Append
                    }
                })
        });
        let showcase = if request.prompt.contains(DIFFS_MARKER) {
            Some(diffs::reply())
        } else if request.prompt.contains(MATH_MARKER) {
            Some(math::reply())
        } else if request.prompt.contains(HTML_MARKER) {
            Some(html::reply())
        } else if request.prompt.contains(ARTIFACTS_REVISE_MARKER) {
            Some(artifacts::revision())
        } else if request.prompt.contains(ARTIFACTS_MARKER) {
            Some(artifacts::reply())
        } else {
            translation_mode.map(translation::reply)
        };
        let showcase_reply;
        let body = if let Some(reply) = showcase {
            showcase_reply = body[..lead]
                .iter()
                .cloned()
                .chain(reply)
                .collect::<Vec<_>>();
            &showcase_reply[..]
        } else {
            body
        };
        // Dev/demo prompt markers ([`ASK_MARKER`], [`TALL_ASK_MARKER`],
        // [`HOLD_MARKER`]): the
        // data-side way to put live awaiting-input and working sessions on
        // screen, e.g. when seeding a demo sidebar.
        let tall_ask = request.prompt.contains(TALL_ASK_MARKER);
        let ask = tall_ask || request.prompt.contains(ASK_MARKER);
        let hold = request.prompt.contains(HOLD_MARKER);
        if ask || hold {
            let rest = think_events
                .chain(work_events)
                .chain(body[lead..].iter().cloned());
            let mut steps: Vec<Step> = body[..lead].iter().cloned().map(Step::Event).collect();
            if ask {
                steps.push(Step::Ask(tall_ask));
            }
            if hold {
                steps.extend(rest.take(1).map(Step::Event));
                steps.push(Step::Event(AgentEvent::ToolCall {
                    id: HOLD_TOOL_ID.into(),
                    call: cypher_proto::ToolCall::Exec {
                        command: "cargo test --workspace -- --include-ignored".into(),
                    },
                }));
                steps.push(Step::Hold);
            } else {
                steps.extend(rest.chain(tail.iter().cloned()).map(Step::Event));
            }
            return Ok(marked_run(steps, controls, delay));
        }
        let events: Vec<Result<AgentEvent, HarnessError>> = body[..lead]
            .iter()
            .cloned()
            .chain(think_events)
            .chain(work_events)
            .chain(body[lead..].iter().cloned())
            .chain(tail.iter().cloned())
            .map(Ok)
            .collect();
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

/// The [`TALL_ASK_MARKER`] question: a long context and many options.
fn tall_question() -> UserInputQuestion {
    let context = [
        "The settings store has three writers today, and they disagree on the file format:",
        "- The app writes settings.json with a version field and atomic renames.",
        "- The CLI writes the same file in place, without a version, so a crash mid-write truncates it.",
        "- The sync worker writes settings.remote.json and merges it on the next launch, last write wins.",
        "",
        "What I found while reading the code:",
        "- crates/settings/src/store.rs: load() silently falls back to defaults on a parse error, which is how two users lost their model choice last week.",
        "- crates/cli/src/config.rs: set() rewrites the whole file and drops keys it doesn't know (theme, wide, translation).",
        "- crates/sync/src/merge.rs: merge() compares wall-clock timestamps across devices, so a device with a fast clock always wins.",
        "- There are no tests that run two writers against one file.",
        "",
        "Constraints you mentioned:",
        "- Existing settings files must keep working without a manual step.",
        "- The CLI must keep working when the app isn't running.",
        "- Sync must not lose a change made offline on one device.",
        "",
        "Each option below is a complete plan; I'll write it up as a checklist before touching code.",
    ]
    .join("\n");
    UserInputQuestion {
        id: "mock-question".into(),
        header: "Migration plan".into(),
        question: format!(
            "Which migration plan should I follow for the settings store?\n\nContext:\n{context}"
        ),
        options: vec![
            "One writer: the app owns the file; the CLI and sync send it changes over IPC".into(),
            "Shared library: every writer goes through one store crate with atomic renames and versioning".into(),
            "Per-key records: replace the JSON file with a small SQLite table keyed by setting".into(),
            "Minimal fix: atomic renames in the CLI and a parse-error backup, nothing else".into(),
            "Logical clocks for sync, on top of whichever plan you pick".into(),
            "Stop here: write the findings up as an issue first".into(),
        ],
        multi_select: false,
        permission: None,
        asked_by: None,
    }
}

/// Replay a marked run's steps: scripted events (paced by `delay`), the
/// question an `Ask` step waits on, and the interrupt a `Hold` step waits
/// for. An interrupt during either pause ends the run `Interrupted`.
fn marked_run(
    steps: Vec<Step>,
    controls: RunControls,
    delay: std::time::Duration,
) -> BoxStream<'static, Result<AgentEvent, HarnessError>> {
    let RunControls {
        request_input,
        interrupt,
        ..
    } = controls;
    futures::stream::unfold(
        (steps.into_iter(), Some((request_input, interrupt))),
        move |(mut steps, controls)| async move {
            let (request_input, interrupt) = controls?;
            loop {
                match steps.next()? {
                    Step::Event(event) => {
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                        return Some((Ok(event), (steps, Some((request_input, interrupt)))));
                    }
                    Step::Ask(tall) => {
                        let answer = (request_input)(vec![if tall {
                            tall_question()
                        } else {
                            UserInputQuestion {
                                id: "mock-question".into(),
                                header: "Approach".into(),
                                question: "Which approach should I take?".into(),
                                options: vec![
                                    "Patch it in place".into(),
                                    "Rewrite the module".into(),
                                ],
                                multi_select: false,
                                permission: None,
                                asked_by: None,
                            }
                        }]);
                        tokio::select! {
                            _ = answer => {}
                            _ = interrupt.cancelled() => {
                                return Some((Ok(interrupted()), (steps, None)));
                            }
                        }
                    }
                    Step::Hold => {
                        interrupt.cancelled().await;
                        return Some((Ok(interrupted()), (steps, None)));
                    }
                }
            }
        },
    )
    .boxed()
}
