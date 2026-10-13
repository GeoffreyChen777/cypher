//! The mock harness's turn changes demo (`[mock:changes:N]`): round N of a
//! scripted session on the demo project (`scripts/demo/tasks-app`). Each
//! round edits the run's checkout the way an agent's tools do — edits, new
//! files, a delete, a rename — so the engine records the round's changed
//! files exactly as it does for a real agent. The demo session
//! (`scripts/dev-demo.sh --changes`) is built from these rounds.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;

use cypher_proto::{AgentEvent, ToolCall};

use crate::HarnessError;

/// Prompt marker prefix: `[mock:changes:1]` … `[mock:changes:4]`.
pub const MARKER: &str = "[mock:changes:";

/// How many rounds the demo scripts.
pub const ROUNDS: usize = 4;

/// The engine snapshots the checkout as a round starts, in the background;
/// the first edit waits this long so it lands after that snapshot, the way
/// a real agent reads before it writes.
const FIRST_EDIT_PAUSE: Duration = Duration::from_millis(600);

/// The round a prompt asks for, when it carries the marker.
pub(super) fn round_of(prompt: &str) -> Option<usize> {
    let rest = &prompt[prompt.find(MARKER)? + MARKER.len()..];
    let digits = &rest[..rest.find(']')?];
    digits.parse().ok().filter(|n| (1..=ROUNDS).contains(n))
}

/// One tool call of a round, applied to the checkout.
enum Op {
    /// Replace the one occurrence of `old`.
    Edit {
        path: &'static str,
        old: &'static str,
        new: &'static str,
    },
    /// Replace every occurrence of `from`.
    ReplaceAll {
        path: &'static str,
        from: &'static str,
        to: &'static str,
    },
    Write {
        path: &'static str,
        content: &'static str,
    },
    Remove {
        path: &'static str,
    },
    Rename {
        from: &'static str,
        to: &'static str,
    },
}

impl Op {
    fn call(&self) -> ToolCall {
        match self {
            Op::Edit { path, old, new } => ToolCall::EditFile {
                path: (*path).into(),
                old_string: Some((*old).into()),
                new_string: Some((*new).into()),
            },
            Op::ReplaceAll { path, from, to } => ToolCall::EditFile {
                path: (*path).into(),
                old_string: Some((*from).into()),
                new_string: Some((*to).into()),
            },
            Op::Write { path, content } => ToolCall::WriteFile {
                path: (*path).into(),
                content: Some((*content).into()),
            },
            Op::Remove { path } => ToolCall::Exec {
                command: format!("rm {path}"),
            },
            Op::Rename { from, to } => ToolCall::Exec {
                command: format!("mv {from} {to}"),
            },
        }
    }

    /// Apply the call in `cwd`: its output, or what went wrong.
    async fn apply(&self, cwd: &Path) -> Result<String, String> {
        let at = |path: &str| cwd.join(path);
        let read = |path: PathBuf| async move {
            tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| format!("{}: {e}", path.display()))
        };
        let write = |path: PathBuf, text: String| async move {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            tokio::fs::write(&path, text)
                .await
                .map_err(|e| format!("{}: {e}", path.display()))
        };
        match self {
            Op::Edit { path, old, new } => {
                let text = read(at(path)).await?;
                if !text.contains(old) {
                    return Err(format!("{path}: the text to replace was not found"));
                }
                write(at(path), text.replacen(old, new, 1)).await?;
                Ok(format!("Edited {path}"))
            }
            Op::ReplaceAll { path, from, to } => {
                let text = read(at(path)).await?;
                let count = text.matches(from).count();
                if count == 0 {
                    return Err(format!("{path}: no occurrence of {from}"));
                }
                write(at(path), text.replace(from, to)).await?;
                Ok(format!("Replaced {count} occurrences in {path}"))
            }
            Op::Write { path, content } => {
                write(at(path), (*content).to_owned()).await?;
                Ok(format!("Wrote {path} ({} lines)", content.lines().count()))
            }
            Op::Remove { path } => tokio::fs::remove_file(at(path))
                .await
                .map(|()| String::new())
                .map_err(|e| format!("rm: {path}: {e}")),
            Op::Rename { from, to } => tokio::fs::rename(at(from), at(to))
                .await
                .map(|()| String::new())
                .map_err(|e| format!("mv: {from}: {e}")),
        }
    }
}

/// A scripted round: what the agent says first, the calls it makes, and
/// its answer.
struct Round {
    intro: &'static str,
    ops: Vec<Op>,
    answer: &'static str,
}

const DATES_PY: &str = r#"import datetime as dt

WEEKDAYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]


def parse_due(text: str, today: dt.date | None = None) -> dt.date:
    """`today`, `tomorrow`, a weekday (the next one) or an ISO date."""
    today = today or dt.date.today()
    word = text.strip().lower()
    if word == "today":
        return today
    if word == "tomorrow":
        return today + dt.timedelta(days=1)
    if word[:3] in WEEKDAYS:
        ahead = (WEEKDAYS.index(word[:3]) - today.weekday()) % 7 or 7
        return today + dt.timedelta(days=ahead)
    return dt.date.fromisoformat(word)
"#;

const TEST_DATES_PY: &str = r#"import datetime as dt

import pytest

from tasks.dates import parse_due
from tasks.model import Task

MONDAY = dt.date(2026, 10, 12)


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        ("today", MONDAY),
        ("tomorrow", dt.date(2026, 10, 13)),
        ("fri", dt.date(2026, 10, 16)),
        ("Monday", dt.date(2026, 10, 19)),
        ("2026-12-24", dt.date(2026, 12, 24)),
    ],
)
def test_parse_due(text, expected):
    assert parse_due(text, today=MONDAY) == expected


def test_a_done_task_is_never_overdue():
    task = Task(id=1, title="File taxes", due=dt.date(2020, 4, 15))
    assert task.overdue
    task.complete()
    assert not task.overdue
"#;

fn round(n: usize) -> Round {
    match n {
        1 => Round {
            intro: "I'll add an optional `due` date to `Task`, store it, parse friendly \
                    dates on `tasks add --due`, and flag overdue tasks in the list.",
            ops: vec![
                Op::Edit {
                    path: "tasks/model.py",
                    old: "from dataclasses import dataclass\n",
                    new: "from dataclasses import dataclass\nfrom datetime import date\n",
                },
                Op::Edit {
                    path: "tasks/model.py",
                    old: "    done: bool = False\n",
                    new: "    done: bool = False\n    due: date | None = None\n\n    \
                          @property\n    def overdue(self) -> bool:\n        \
                          return self.due is not None and not self.done and self.due < date.today()\n",
                },
                Op::Write {
                    path: "tasks/dates.py",
                    content: DATES_PY,
                },
                Op::Edit {
                    path: "tasks/store.py",
                    old: "import json\n",
                    new: "import json\nfrom datetime import date\n",
                },
                Op::Edit {
                    path: "tasks/store.py",
                    old: "    return [Task(id=row[\"id\"], title=row[\"title\"], done=row[\"done\"]) for row in rows]\n",
                    new: "    return [\n        Task(\n            id=row[\"id\"],\n            \
                          title=row[\"title\"],\n            done=row[\"done\"],\n            \
                          due=date.fromisoformat(row[\"due\"]) if row.get(\"due\") else None,\n        \
                          )\n        for row in rows\n    ]\n",
                },
                Op::Edit {
                    path: "tasks/store.py",
                    old: "    rows = [{\"id\": t.id, \"title\": t.title, \"done\": t.done} for t in tasks]\n",
                    new: "    rows = [\n        {\"id\": t.id, \"title\": t.title, \"done\": t.done, \
                          \"due\": t.due.isoformat() if t.due else None}\n        for t in tasks\n    ]\n",
                },
                Op::Edit {
                    path: "tasks/cli.py",
                    old: "from tasks import render, store\n",
                    new: "from tasks import render, store\nfrom tasks.dates import parse_due\n",
                },
                Op::Edit {
                    path: "tasks/cli.py",
                    old: "    add.add_argument(\"title\")\n",
                    new: "    add.add_argument(\"title\")\n    add.add_argument(\n        \"--due\", \
                          type=parse_due, help=\"today, tomorrow, a weekday or YYYY-MM-DD\"\n    )\n",
                },
                Op::Edit {
                    path: "tasks/cli.py",
                    old: "title=args.title)",
                    new: "title=args.title, due=args.due)",
                },
                Op::Edit {
                    path: "tasks/render.py",
                    old: "    return f\"[{mark}] {task.id:>3}  {task.title}\"\n",
                    new: "    due = f\"  (due {task.due:%b %d})\" if task.due else \"\"\n    \
                          flag = \"  OVERDUE\" if task.overdue else \"\"\n    \
                          return f\"[{mark}] {task.id:>3}  {task.title}{due}{flag}\"\n",
                },
                Op::Edit {
                    path: "README.md",
                    old: "tasks add \"Write the release notes\"\n",
                    new: "tasks add \"Write the release notes\"\ntasks add \"Renew the domain\" --due fri\n",
                },
            ],
            answer: "Tasks now take an optional due date:\n\n\
                     - `tasks add \"Renew the domain\" --due fri` — `today`, `tomorrow`, a weekday \
                     or `YYYY-MM-DD` (new `tasks/dates.py`).\n\
                     - The date is stored in `~/.tasks.json`; lists saved before this load with no \
                     due date.\n\
                     - `tasks list` shows `(due Oct 16)` and flags `OVERDUE` tasks that are not done.",
        },
        2 => Round {
            intro: "Removing the 0.3 importer and its command, then covering the date parser \
                    and preparing 0.5.0.",
            ops: vec![
                Op::Remove {
                    path: "tasks/legacy_import.py",
                },
                Op::Edit {
                    path: "tasks/cli.py",
                    old: "import argparse\nfrom pathlib import Path\n",
                    new: "import argparse\n",
                },
                Op::Edit {
                    path: "tasks/cli.py",
                    old: "from tasks.legacy_import import read_csv\n",
                    new: "",
                },
                Op::Edit {
                    path: "tasks/cli.py",
                    old: "    legacy = commands.add_parser(\"import\", help=\"import a tasks 0.3 CSV export\")\n    \
                          legacy.add_argument(\"path\", type=Path)\n",
                    new: "",
                },
                Op::Edit {
                    path: "tasks/cli.py",
                    old: "    elif args.command == \"import\":\n        tasks.extend(read_csv(args.path))\n",
                    new: "",
                },
                Op::Write {
                    path: "tests/test_dates.py",
                    content: TEST_DATES_PY,
                },
                Op::Rename {
                    from: "CHANGES.txt",
                    to: "CHANGELOG.md",
                },
                Op::Edit {
                    path: "CHANGELOG.md",
                    old: "0.4.0\n",
                    new: "0.5.0\n- Due dates: `tasks add --due fri`; overdue tasks are flagged.\n\
                          - Removed `tasks import` for tasks 0.3 CSV exports.\n\n0.4.0\n",
                },
                Op::Edit {
                    path: "pyproject.toml",
                    old: "version = \"0.4.0\"",
                    new: "version = \"0.5.0\"",
                },
            ],
            answer: "Done:\n\n\
                     - Deleted `tasks/legacy_import.py` and the `tasks import` command.\n\
                     - `tests/test_dates.py` covers every form `--due` accepts, plus overdue \
                     tasks once completed.\n\
                     - Renamed `CHANGES.txt` to `CHANGELOG.md` with a 0.5.0 entry, and bumped \
                     the version.",
        },
        3 => Round {
            intro: "Renaming the class and every use of it.",
            ops: [
                "tasks/model.py",
                "tasks/__init__.py",
                "tasks/store.py",
                "tasks/render.py",
                "tasks/cli.py",
                "tests/test_store.py",
                "tests/test_dates.py",
                "README.md",
            ]
            .into_iter()
            .map(|path| Op::ReplaceAll {
                path,
                from: "Task",
                to: "Todo",
            })
            .collect(),
            answer: "`Task` is now `Todo` in the package, its tests and the README. The \
                     command is still `tasks`, and the storage format is unchanged, so \
                     existing lists keep loading.",
        },
        _ => Round {
            intro: "",
            ops: Vec::new(),
            answer: "Two things before tagging 0.5.0:\n\n\
                     1. **Time zones** — `overdue` compares against the local date, so a task \
                     due today turns overdue at local midnight. That's probably what people \
                     expect, but worth a line in the README.\n\
                     2. **Old exports** — anyone still on 0.3 loses `tasks import`. The \
                     changelog says so; a short migration note would help.\n\n\
                     Nothing else needs changing in the code.",
        },
    }
}

/// One step of the round's stream.
enum Step {
    Event(AgentEvent),
    Apply { id: String, op: Op },
}

/// Round `n`: `lead` (the script's opening), the round in `cwd`, then
/// `tail` (its Done). Events are spaced by `delay`.
pub(super) fn run(
    lead: Vec<AgentEvent>,
    tail: Vec<AgentEvent>,
    cwd: PathBuf,
    n: usize,
    delay: Duration,
) -> BoxStream<'static, Result<AgentEvent, HarnessError>> {
    let Round { intro, ops, answer } = round(n);
    let text = |text: &str| Step::Event(AgentEvent::TextDelta { text: text.into() });
    let mut steps: Vec<Step> = lead.into_iter().map(Step::Event).collect();
    if !intro.is_empty() {
        steps.push(text(intro));
    }
    for (ix, op) in ops.into_iter().enumerate() {
        let id = format!("mock-change-{n}-{ix}");
        steps.push(Step::Event(AgentEvent::ToolCall {
            id: id.clone(),
            call: op.call(),
        }));
        steps.push(Step::Apply { id, op });
    }
    steps.push(text(answer));
    steps.extend(tail.into_iter().map(Step::Event));
    futures::stream::unfold(
        (steps.into_iter(), cwd, true),
        move |(mut steps, cwd, first_edit)| async move {
            let step = steps.next()?;
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let (event, first_edit) = match step {
                Step::Event(event) => (event, first_edit),
                Step::Apply { id, op } => {
                    if first_edit {
                        tokio::time::sleep(FIRST_EDIT_PAUSE).await;
                    }
                    let result = op.apply(&cwd).await;
                    let is_error = result.is_err();
                    let output = result.unwrap_or_else(|e| e);
                    let event = AgentEvent::ToolResult {
                        id,
                        is_error,
                        output: (!output.is_empty()).then_some(output),
                        diff: None,
                        permission: None,
                    };
                    (event, false)
                }
            };
            Some((Ok(event), (steps, cwd, first_edit)))
        },
    )
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_name_their_round() {
        assert_eq!(round_of("Add due dates [mock:changes:1]"), Some(1));
        assert_eq!(round_of("[mock:changes:4] anything"), Some(4));
        assert_eq!(round_of("[mock:changes:9]"), None);
        assert_eq!(round_of("[mock:changes:x]"), None);
        assert_eq!(round_of("no marker"), None);
    }

    /// Every scripted edit applies to the demo project as the rounds leave
    /// it, in order.
    #[tokio::test]
    async fn every_round_applies_to_the_demo_project() {
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/demo/tasks-app");
        let tmp = tempfile::tempdir().unwrap();
        copy_dir(&project, tmp.path());
        for n in 1..=ROUNDS {
            for op in round(n).ops {
                if let Err(err) = op.apply(tmp.path()).await {
                    panic!("round {n}: {err}");
                }
            }
        }
        let model = std::fs::read_to_string(tmp.path().join("tasks/model.py")).unwrap();
        assert!(model.contains("class Todo:") && model.contains("due: date | None"));
        assert!(!tmp.path().join("tasks/legacy_import.py").exists());
        assert!(tmp.path().join("CHANGELOG.md").exists());
    }

    fn copy_dir(from: &Path, to: &Path) {
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                if entry.file_name() == "__pycache__" {
                    continue;
                }
                std::fs::create_dir_all(&target).unwrap();
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }
}
