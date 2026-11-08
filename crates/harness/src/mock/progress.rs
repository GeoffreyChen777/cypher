//! Narrated progress demo for the mock harness: a long scripted run (about
//! four minutes) working through a plan, so the working trailer's progress
//! bar can be seen appearing after the turn's first minute and moving every
//! half minute after that. Midway the agent finds a step its plan lacked,
//! so the bar's upcoming steps can be seen refreshing.
//!
//! The bar itself comes from the engine's progress narrator, which asks the
//! run's harness for a reading. The mock answers that request
//! ([`is_reading`]) from the plan the request quotes — the first unchecked
//! item is what the agent is doing now — so the whole reading path runs
//! without a model.

use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;

use cypher_proto::{AgentEvent, LIVE_PLAN_TOOL_ID, TodoItem, ToolCall};

use crate::{CancellationToken, HarnessError};

/// Prompt marker: reply with the long, planned run.
pub const MARKER: &str = "[mock:progress]";

/// How the engine's progress request is told apart: it quotes the turn in an
/// `<agent_activity>` block (`session::progress::prompt` in cypher-engine).
const READING_TAG: &str = "<agent_activity>";

/// A quick call: what it does and to what.
#[derive(Clone, Copy)]
enum Quick {
    Search(&'static str),
    Read(&'static str),
    Edit(&'static str),
}

impl Quick {
    fn call(self) -> ToolCall {
        match self {
            Quick::Search(pattern) => ToolCall::Search {
                pattern: pattern.into(),
                path: None,
            },
            Quick::Read(path) => ToolCall::ReadFile { path: path.into() },
            Quick::Edit(path) => ToolCall::EditFile {
                path: path.into(),
                old_string: None,
                new_string: None,
            },
        }
    }
}

/// One phase of the run: the plan item it works on, what the agent says, the
/// quick calls it makes, and the slow command it then waits on.
struct Phase {
    item: &'static str,
    text: &'static str,
    quick: &'static [Quick],
    slow: &'static str,
    slow_output: &'static str,
    /// How long the slow command runs.
    slow_secs: u64,
    /// The phase whose start puts this one on the plan: a step the agent
    /// only finds once it gets there (0 = planned from the start).
    found_at: usize,
}

const PHASES: &[Phase] = &[
    Phase {
        item: "Read the list endpoint and its tests",
        text: "I'll start with how `GET /tasks` lists today and what its tests pin.",
        quick: &[
            Quick::Search("fn list_tasks"),
            Quick::Read("crates/api/src/tasks.rs"),
        ],
        slow: "cargo test -p api tasks::list",
        slow_output: "running 6 tests\ntest result: ok. 6 passed; 0 failed",
        slow_secs: 28,
        found_at: 0,
    },
    Phase {
        item: "Add a cursor to the store query",
        text: "The store loads every row and the handler slices them. I'll page in the query instead: order by `(created_at, id)` and start after a cursor.",
        quick: &[
            Quick::Read("crates/store/src/tasks.rs"),
            Quick::Edit("crates/store/src/tasks.rs"),
        ],
        slow: "cargo build -p store",
        slow_output: "Finished `dev` profile [unoptimized + debuginfo] target(s) in 24.81s",
        slow_secs: 30,
        found_at: 0,
    },
    Phase {
        item: "Expose cursor and limit on GET /tasks",
        text: "Now the endpoint: `?cursor=` and `?limit=` (default 50, max 200), and a `next_cursor` in the response when there is more.",
        quick: &[
            Quick::Edit("crates/api/src/tasks.rs"),
            Quick::Edit("crates/api/src/schema.rs"),
        ],
        slow: "cargo build -p api",
        slow_output: "Finished `dev` profile [unoptimized + debuginfo] target(s) in 27.02s",
        slow_secs: 32,
        found_at: 0,
    },
    Phase {
        item: "Update the API client",
        text: "The client's `list_tasks` follows `next_cursor` until it runs out, so callers keep getting every task.",
        quick: &[
            Quick::Read("crates/client/src/tasks.rs"),
            Quick::Edit("crates/client/src/tasks.rs"),
        ],
        slow: "cargo check -p client",
        slow_output: "Finished `dev` profile [unoptimized + debuginfo] target(s) in 18.40s",
        slow_secs: 30,
        found_at: 0,
    },
    Phase {
        item: "Keep the client's cache in step",
        text: "The client caches whole lists by URL, so a paged list would cache page by page and go stale between them. I'm adding a step for that: cache by cursor and drop the pages when a task changes.",
        quick: &[
            Quick::Search("ListCache"),
            Quick::Edit("crates/client/src/cache.rs"),
        ],
        slow: "cargo test -p client cache",
        slow_output: "running 4 tests\ntest result: ok. 4 passed; 0 failed",
        slow_secs: 30,
        found_at: 3,
    },
    Phase {
        item: "Write pagination tests",
        text: "Tests next: page boundaries, an empty page, a bad cursor, and a task added between pages.",
        quick: &[Quick::Edit("crates/api/tests/pagination.rs")],
        slow: "cargo test -p api tasks::pagination",
        slow_output: "running 5 tests\ntest result: ok. 5 passed; 0 failed",
        slow_secs: 30,
        found_at: 0,
    },
    Phase {
        item: "Run the full test suite",
        text: "Everything is in place; running the whole suite before I wrap up.",
        quick: &[],
        slow: "cargo test --workspace",
        slow_output: "test result: ok. 214 passed; 0 failed",
        slow_secs: 34,
        found_at: 0,
    },
];

const ANSWER: &str = "`GET /tasks` now pages with a cursor.\n\n\
- **Store**: the list query orders by `(created_at, id)` and starts after the cursor, so a page never loads the whole table.\n\
- **API**: `?cursor=` and `?limit=` (default 50, max 200); responses carry `next_cursor` while there is more.\n\
- **Client**: `list_tasks` follows `next_cursor`, so existing callers still get every task.\n\
- **Tests**: five new pagination tests; the full suite passes.";

/// Whether `prompt` is the engine asking for a progress reading.
pub(super) fn is_reading(prompt: &str) -> bool {
    prompt.contains(READING_TAG)
}

/// The reading for a progress request: the quoted plan's first unchecked
/// item is under way, every one after it is still to come.
pub(super) fn reading(prompt: &str) -> String {
    let mut done = 0usize;
    let mut pending = Vec::new();
    for line in prompt.lines() {
        if line.starts_with("- [x] ") {
            done += 1;
        } else if let Some(item) = line.strip_prefix("- [ ] ") {
            pending.push(item.trim().to_owned());
        }
    }
    let total = done + pending.len();
    let (percent, current, next) = match pending.split_first() {
        _ if total == 0 => (5, "Getting started".to_owned(), Vec::new()),
        Some((current, rest)) => (
            ((done as f64 + 0.5) / total as f64 * 100.0).round() as u8,
            current.clone(),
            rest.to_vec(),
        ),
        None => (95, "Wrapping up".to_owned(), Vec::new()),
    };
    let steps_left = next.len();
    serde_json::json!({
        "percent": percent.min(99),
        "current": current,
        "next": next,
        "steps_left": steps_left,
    })
    .to_string()
}

/// One beat of the scripted run.
enum Beat {
    Event(AgentEvent),
    Pause(Duration),
}

fn plan(done: usize) -> Vec<AgentEvent> {
    let items = PHASES
        .iter()
        .enumerate()
        .filter(|(_, phase)| phase.found_at <= done)
        .map(|(ix, phase)| TodoItem {
            text: phase.item.into(),
            done: ix < done,
        })
        .collect();
    vec![
        AgentEvent::ToolCall {
            id: LIVE_PLAN_TOOL_ID.into(),
            call: ToolCall::Todo { items },
        },
        AgentEvent::ToolResult {
            id: LIVE_PLAN_TOOL_ID.into(),
            is_error: false,
            output: None,
            diff: None,
            permission: None,
        },
    ]
}

fn beats(lead: Vec<AgentEvent>, tail: Vec<AgentEvent>) -> Vec<Beat> {
    let short = Duration::from_millis(1_200);
    let mut beats: Vec<Beat> = lead.into_iter().map(Beat::Event).collect();
    for (ix, phase) in PHASES.iter().enumerate() {
        beats.extend(plan(ix).into_iter().map(Beat::Event));
        beats.push(Beat::Pause(short));
        beats.push(Beat::Event(AgentEvent::TextDelta {
            text: phase.text.into(),
        }));
        for (n, call) in phase.quick.iter().enumerate() {
            let id = format!("mock-progress-{ix}-{n}");
            beats.push(Beat::Pause(short));
            beats.push(Beat::Event(AgentEvent::ToolCall {
                id: id.clone(),
                call: call.call(),
            }));
            beats.push(Beat::Event(result(id, None)));
        }
        // The slow command stays open while it runs, which keeps the turn
        // live through the pause (the engine never parks a running tool).
        let id = format!("mock-progress-{ix}");
        beats.push(Beat::Pause(short));
        beats.push(Beat::Event(AgentEvent::ToolCall {
            id: id.clone(),
            call: ToolCall::Exec {
                command: phase.slow.into(),
            },
        }));
        beats.push(Beat::Pause(Duration::from_secs(phase.slow_secs)));
        beats.push(Beat::Event(result(id, Some(phase.slow_output))));
    }
    beats.extend(plan(PHASES.len()).into_iter().map(Beat::Event));
    beats.push(Beat::Pause(short));
    beats.push(Beat::Event(AgentEvent::TextDelta {
        text: ANSWER.into(),
    }));
    beats.extend(tail.into_iter().map(Beat::Event));
    beats
}

fn result(id: String, output: Option<&str>) -> AgentEvent {
    AgentEvent::ToolResult {
        id,
        is_error: false,
        output: output.map(str::to_owned),
        diff: None,
        permission: None,
    }
}

/// The planned run: `lead` (the script's opening), the phases, the answer,
/// then `tail` (its Done). A Stop during a pause ends it `Interrupted`.
pub(super) fn run(
    lead: Vec<AgentEvent>,
    tail: Vec<AgentEvent>,
    interrupt: CancellationToken,
) -> BoxStream<'static, Result<AgentEvent, HarnessError>> {
    futures::stream::unfold(
        Some((beats(lead, tail).into_iter(), interrupt)),
        |state| async move {
            let (mut beats, interrupt) = state?;
            loop {
                match beats.next()? {
                    Beat::Event(event) => return Some((Ok(event), Some((beats, interrupt)))),
                    Beat::Pause(pause) => {
                        tokio::select! {
                            _ = tokio::time::sleep(pause) => {}
                            _ = interrupt.cancelled() => {
                                return Some((Ok(super::interrupted()), None));
                            }
                        }
                    }
                }
            }
        },
    )
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_lasts_long_enough_for_several_readings() {
        let total: Duration = beats(Vec::new(), Vec::new())
            .iter()
            .map(|beat| match beat {
                Beat::Pause(pause) => *pause,
                Beat::Event(_) => Duration::ZERO,
            })
            .sum();
        // First reading at 1:00, then every 0:30: at least five readings.
        assert!(total >= Duration::from_secs(190), "{total:?}");
    }

    #[test]
    fn the_plan_gains_a_step_midway() {
        let items = |done| match plan(done).remove(0) {
            AgentEvent::ToolCall {
                call: ToolCall::Todo { items },
                ..
            } => items.len(),
            _ => unreachable!(),
        };
        assert_eq!(items(0), PHASES.len() - 1);
        assert_eq!(items(3), PHASES.len());
    }

    #[test]
    fn a_reading_follows_the_quoted_plan() {
        let prompt = "<agent_activity>\nAgent's current plan:\n- [x] Read\n- [ ] Store\n- [ ] API\n- [ ] Client\n- [ ] Tests\n- [ ] Suite\n</agent_activity>";
        assert!(is_reading(prompt));
        let value: serde_json::Value = serde_json::from_str(&reading(prompt)).unwrap();
        assert_eq!(value["percent"], 25);
        assert_eq!(value["current"], "Store");
        assert_eq!(
            value["next"],
            serde_json::json!(["API", "Client", "Tests", "Suite"])
        );
        assert_eq!(value["steps_left"], 4);
        let value: serde_json::Value =
            serde_json::from_str(&reading("<agent_activity>\n(nothing yet)")).unwrap();
        assert_eq!(value["current"], "Getting started");
        assert!(!is_reading("Add pagination [mock:progress]"));
    }
}
