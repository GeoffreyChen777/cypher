//! Permission control demo runs for the mock harness: what Cypher's Pi
//! permission extension and the Pi harness produce, scripted, so the
//! transcript rows and the permission card can be seen without Pi or a
//! sandbox.
//!
//! - [`STORY_MARKER`]: a settled run with every kind of row. A command the
//!   sandbox blocked folds into its run outside, approvals file under their
//!   calls, a refusal, a block left alone, an MCP tool a prompt gated, a
//!   prompt from an older Runtime, and an ordinary question for contrast.
//! - [`CARD_MARKER`]: a run that stops on a live "Run this command outside
//!   the sandbox?" card and finishes by the answer.
//! - [`TALL_CARD_MARKER`]: the same card for a command long enough to make
//!   it taller than a short chat column.
//! - [`MULTILINE_CARD_MARKER`]: the same card for a command and a reason
//!   that run over several lines.
//!
//! The prompts in the settled run are asked through the engine's input
//! bridge and withdrawn at once: the engine records them answered, as an
//! approval in the chat's history would be.

use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use cypher_proto::{
    AgentEvent, DoneStatus, PermissionOutcome, PermissionPrompt, PermissionPromptKind,
    PermissionScope, ToolCall, ToolPermission, UserInputAnswer, UserInputQuestion,
};

use crate::{HarnessError, RunControls};

/// Prompt marker: the settled permission story.
pub const STORY_MARKER: &str = "[mock:permissions]";
/// Prompt marker: stop on a live permission card.
pub const CARD_MARKER: &str = "[mock:permission-card]";
/// Prompt marker: stop on a live permission card for a very long command.
pub const TALL_CARD_MARKER: &str = "[mock:permission-card:tall]";
/// Prompt marker: stop on a live permission card for a multi-line command.
pub const MULTILINE_CARD_MARKER: &str = "[mock:permission-card:multiline]";

/// Which permission demo a prompt asks for, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Demo {
    Story,
    Card,
    TallCard,
    MultilineCard,
}

pub(super) fn demo_of(prompt: &str) -> Option<Demo> {
    if prompt.contains(STORY_MARKER) {
        Some(Demo::Story)
    } else if prompt.contains(TALL_CARD_MARKER) {
        Some(Demo::TallCard)
    } else if prompt.contains(MULTILINE_CARD_MARKER) {
        Some(Demo::MultilineCard)
    } else if prompt.contains(CARD_MARKER) {
        Some(Demo::Card)
    } else {
        None
    }
}

const SSH_STATUS: &str = "ssh deploy@web 'systemctl status app --no-pager'";
const SSH_RESTART: &str = "ssh deploy@web 'sudo systemctl restart app'";
const PUSH: &str = "git push --force origin release/0.3.49";
const PURPOSE: &str = "Check the app on the server web";
/// One command that checks a whole deploy at once: wraps to many lines.
const TALL_COMMAND: &str = "ssh -o ConnectTimeout=15 deploy@web 'date; echo ---ping; ping -c 2 -W 2 10.20.0.17 | tail -2; for p in 22 8080 9333 9334 18080; do timeout 3 bash -c \"echo > /dev/tcp/10.20.0.17/$p\" 2>/dev/null && echo \"port $p open\" || echo \"port $p closed\"; done; echo ---lookup; for v in 360 611 688 757 808 888 955; do echo -n \"vid $v: \"; curl -s -m 5 \"http://10.20.0.12:9333/dir/lookup?volumeId=$v\"; echo; done; echo ---app events since yesterday; journalctl -u app --since \"2026-10-14 07:30\" --no-pager | grep \"10.20.0.17\" | grep -vE \"crowded\" | cut -c1-220 | tail -15; echo ---worker errors by hour; zcat -f /var/log/app/worker.log.1 /var/log/app/worker.log 2>/dev/null | grep -E \"fail to read|<ERROR>\" | cut -c1-13 | uniq -c | tail -15; echo ---proxy misses last 2h; journalctl -u app-proxy --since \"2 hours ago\" --no-pager | grep -c \"urls not found\"' 2>&1";
const TALL_PURPOSE: &str = "Check whether the storage node and its volumes on web recovered";
/// A heredoc: a command of several lines, one of them shaped like a fact.
const MULTILINE_COMMAND: &str = "ssh deploy@web 'bash -s' <<'EOF'\nsystemctl status app --no-pager\nPath: /etc/app/config.toml\n\n  journalctl -u app -n 20 --no-pager\nEOF";
const MULTILINE_REASON: &str =
    "The sandbox hides the SSH key.\nThe task needs the server's status and its last log lines.";

/// The run's events, in order; `lead` are the script's opening events
/// (`SessionStarted`) and `done` its closing ones.
pub(super) fn run(
    lead: Vec<AgentEvent>,
    done: Vec<AgentEvent>,
    controls: RunControls,
    delay: Duration,
    demo: Demo,
) -> BoxStream<'static, Result<AgentEvent, HarnessError>> {
    let (tx, rx) = mpsc::unbounded_channel();
    let RunControls {
        request_input,
        interrupt,
        ..
    } = controls;
    let script = Script {
        tx,
        delay,
        request_input,
        interrupt,
    };
    tokio::spawn(async move {
        for event in lead {
            script.emit(event).await;
        }
        let finished = match demo {
            Demo::Story => {
                script.story().await;
                true
            }
            Demo::Card | Demo::TallCard | Demo::MultilineCard => script.card(demo).await,
        };
        if finished {
            for event in done {
                script.emit(event).await;
            }
        } else {
            script.emit(interrupted()).await;
        }
    });
    futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|event| (Ok(event), rx))
    })
    .boxed()
}

fn interrupted() -> AgentEvent {
    AgentEvent::Done {
        status: DoneStatus::Interrupted,
        result: None,
        error: None,
        session_id: None,
    }
}

fn record(
    outcome: PermissionOutcome,
    scope: Option<PermissionScope>,
    rule: Option<&str>,
    purpose: Option<&str>,
) -> Option<ToolPermission> {
    Some(ToolPermission {
        outcome,
        scope,
        rule: rule.map(str::to_owned),
        purpose: purpose.map(str::to_owned),
    })
}

/// A dialog as the harness hands it on: the title in both fields, the
/// `cypher-permission:` line already turned into `permission`.
fn prompt(
    id: &str,
    title: &str,
    options: &[&str],
    permission: Option<(PermissionPromptKind, Option<&str>)>,
) -> UserInputQuestion {
    UserInputQuestion {
        id: id.into(),
        header: title.into(),
        question: title.into(),
        options: options.iter().map(|option| option.to_string()).collect(),
        multi_select: false,
        permission: permission.map(|(kind, call)| PermissionPrompt {
            kind,
            tool_call_id: call.map(str::to_owned),
        }),
        asked_by: None,
    }
}

const ESCALATE_OPTIONS: [&str; 5] = [
    "Run once",
    "Allow for this chat",
    "Always allow",
    "Deny",
    "Deny and stop",
];
const ACCESS_OPTIONS: [&str; 5] = [
    "Allow once",
    "Allow for this chat",
    "Always allow",
    "Deny",
    "Deny and stop",
];

/// A value as the extension writes it into a dialog (`core.ts` `fact`): its
/// lines after the first indented.
fn fact_value(value: &str) -> String {
    value.replace('\n', "\n  ")
}

fn escalate_prompt(
    call: &str,
    command: &str,
    purpose: &str,
    pattern: &str,
    reason: &str,
) -> UserInputQuestion {
    let (command, reason) = (fact_value(command), fact_value(reason));
    prompt(
        &format!("prompt-{call}"),
        &format!(
            "Run this command outside the sandbox?\n\nContext:\nPurpose: {purpose}\nCommand: {command}\nPattern: {pattern}\nReason: {reason}\nNote: It runs as you, with your files, keys, keychain and network."
        ),
        &ESCALATE_OPTIONS,
        Some((PermissionPromptKind::Escalate, Some(call))),
    )
}

struct Script {
    tx: mpsc::UnboundedSender<AgentEvent>,
    delay: Duration,
    request_input: Box<
        dyn Fn(Vec<UserInputQuestion>) -> tokio::sync::oneshot::Receiver<Vec<UserInputAnswer>>
            + Send
            + Sync,
    >,
    interrupt: CancellationToken,
}

impl Script {
    async fn emit(&self, event: AgentEvent) {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let _ = self.tx.send(event);
    }

    async fn text(&self, text: &str) {
        self.emit(AgentEvent::TextDelta { text: text.into() }).await;
    }

    async fn call(&self, id: &str, call: ToolCall) {
        self.emit(AgentEvent::ToolCall {
            id: id.into(),
            call,
        })
        .await;
    }

    async fn result(
        &self,
        id: &str,
        is_error: bool,
        output: &str,
        permission: Option<ToolPermission>,
    ) {
        self.emit(AgentEvent::ToolResult {
            id: id.into(),
            is_error,
            output: Some(output.into()),
            diff: None,
            permission,
        })
        .await;
    }

    async fn exec(
        &self,
        id: &str,
        command: &str,
        is_error: bool,
        output: &str,
        permission: Option<ToolPermission>,
    ) {
        self.call(
            id,
            ToolCall::Exec {
                command: command.into(),
            },
        )
        .await;
        self.result(id, is_error, output, permission).await;
    }

    /// Ask through the engine's bridge, then withdraw: the engine records
    /// the prompt answered. It stays open a moment first, or the engine
    /// forgets the request before its run task sees the question and drops
    /// it as one it never asked. The pauses keep it between the call and its
    /// result, as a real prompt is.
    async fn settled(&self, question: UserInputQuestion) {
        tokio::time::sleep(Duration::from_millis(80)).await;
        let answer = (self.request_input)(vec![question]);
        tokio::time::sleep(Duration::from_millis(150)).await;
        drop(answer);
        tokio::time::sleep(Duration::from_millis(80)).await;
    }

    /// The settled story: every kind of permission row.
    async fn story(&self) {
        self.text("I'll check the app on **web** before restarting it.\n\n")
            .await;
        // Blocked in the sandbox, then run outside it: one row.
        self.exec(
            "perm-1",
            SSH_STATUS,
            true,
            "Warning: Identity file /Users/you/.ssh/id_ed25519 not accessible: Operation not permitted.\ndeploy@web: Permission denied (publickey).\nCommand exited with code 255",
            record(PermissionOutcome::Blocked, None, None, None),
        )
        .await;
        self.call(
            "perm-2",
            ToolCall::Exec {
                command: SSH_STATUS.into(),
            },
        )
        .await;
        self.settled(escalate_prompt(
            "perm-2",
            SSH_STATUS,
            PURPOSE,
            "ssh deploy@web …",
            "The sandbox hides the SSH key; the task needs the server.",
        ))
        .await;
        self.result(
            "perm-2",
            false,
            "● app.service - Web app\n     Loaded: loaded (/etc/systemd/system/app.service; enabled)\n     Active: active (running) since Mon 2026-10-12 09:14:02 UTC; 1 day ago",
            record(
                PermissionOutcome::Outside,
                Some(PermissionScope::Chat),
                None,
                Some(PURPOSE),
            ),
        )
        .await;
        // Allowed by the chat's pattern: no prompt.
        self.exec(
            "perm-3",
            SSH_RESTART,
            false,
            "",
            record(
                PermissionOutcome::Outside,
                Some(PermissionScope::Rule),
                Some("ssh deploy@web …"),
                Some(PURPOSE),
            ),
        )
        .await;
        // A file tool reaching outside the workspace, approved for the chat.
        self.call(
            "perm-4",
            ToolCall::EditFile {
                path: "../shared/deploy-notes.md".into(),
                old_string: None,
                new_string: None,
            },
        )
        .await;
        self.settled(prompt(
            "prompt-perm-4",
            "Allow the agent to write outside the workspace?\n\nContext:\nPath: ~/Developer/shared/deploy-notes.md\nTool: edit\nReason: Record today's restart in the team's deploy notes.",
            &ACCESS_OPTIONS,
            Some((PermissionPromptKind::Access, Some("perm-4"))),
        ))
        .await;
        self.result(
            "perm-4",
            false,
            "Edited ../shared/deploy-notes.md",
            record(
                PermissionOutcome::Allowed,
                Some(PermissionScope::Chat),
                None,
                None,
            ),
        )
        .await;
        self.text("The app is back up. Then the release steps:\n\n")
            .await;
        // Blocked, and left at that.
        self.exec(
            "perm-5",
            "cat .env",
            true,
            "cat: .env: Operation not permitted\nCommand exited with code 1",
            record(PermissionOutcome::Blocked, None, None, None),
        )
        .await;
        // Blocked, escalated, refused: one row.
        self.exec(
            "perm-6",
            PUSH,
            true,
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled\nCommand exited with code 128",
            record(PermissionOutcome::Blocked, None, None, None),
        )
        .await;
        self.call(
            "perm-7",
            ToolCall::Exec {
                command: PUSH.into(),
            },
        )
        .await;
        self.settled(escalate_prompt(
            "perm-7",
            PUSH,
            "Publish the release branch",
            "git push …",
            "Pushing needs the keychain's GitHub credentials.",
        ))
        .await;
        self.result(
            "perm-7",
            true,
            "The user denied running this outside the sandbox. Continue in the sandbox or ask the user.",
            record(
                PermissionOutcome::Denied,
                None,
                None,
                Some("Publish the release branch"),
            ),
        )
        .await;
        // An MCP tool a prompt gated: no record, it ran.
        self.call(
            "perm-8",
            ToolCall::Mcp {
                server: "github".into(),
                tool: "create_issue".into(),
                input: None,
            },
        )
        .await;
        self.settled(prompt(
            "prompt-perm-8",
            "Allow the agent to use mcp__github__create_issue?\n\nContext:\nTool: mcp__github__create_issue\nInput:\n{\n  \"title\": \"Publish 0.3.49\"\n}",
            &ACCESS_OPTIONS,
            Some((PermissionPromptKind::Tool, Some("perm-8"))),
        ))
        .await;
        self.result("perm-8", false, "Created issue #42: Publish 0.3.49", None)
            .await;
        // A prompt from a Runtime that does not mark its prompts: a
        // Permission row, not a Question.
        self.settled(prompt(
            "prompt-old",
            "Allow the agent to reach registry.example.com?\n\nContext:\nDomain: registry.example.com\nNote: Commands reach the network only through the filter; other domains still ask.",
            &[
                "Allow for this command",
                "Allow for this chat",
                "Always allow",
                "Deny",
                "Deny and stop",
            ],
            None,
        ))
        .await;
        // An ordinary question, for contrast.
        self.settled(prompt(
            "question",
            "Which environment should the next deploy go to?",
            &["staging", "production"],
            None,
        ))
        .await;
        self.text("Done: the app on **web** is running again and the notes are updated. You declined the force-push, so `release/0.3.49` is still local, and I left `.env` alone; I opened issue #42 to track publishing.")
            .await;
    }

    /// Stop on a live card for the demo's command; finish by the answer.
    /// False when the turn stopped there.
    async fn card(&self, demo: Demo) -> bool {
        const REASON: &str = "The sandbox hides the SSH key; the task needs the server.";
        let (command, purpose, pattern, reason) = match demo {
            Demo::TallCard => (
                TALL_COMMAND,
                TALL_PURPOSE,
                "ssh -o ConnectTimeout=* deploy@web …",
                REASON,
            ),
            Demo::MultilineCard => (
                MULTILINE_COMMAND,
                PURPOSE,
                "ssh deploy@web …",
                MULTILINE_REASON,
            ),
            _ => (SSH_STATUS, PURPOSE, "ssh deploy@web …", REASON),
        };
        self.text("Let me check the app on **web**.\n\n").await;
        self.exec(
            "card-1",
            command,
            true,
            "Warning: Identity file /Users/you/.ssh/id_ed25519 not accessible: Operation not permitted.\ndeploy@web: Permission denied (publickey).\nCommand exited with code 255",
            record(PermissionOutcome::Blocked, None, None, None),
        )
        .await;
        self.call(
            "card-2",
            ToolCall::Exec {
                command: command.into(),
            },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        let answer = (self.request_input)(vec![escalate_prompt(
            "card-2", command, purpose, pattern, reason,
        )]);
        let answers = tokio::select! {
            answers = answer => answers.unwrap_or_default(),
            () = self.interrupt.cancelled() => return false,
        };
        let picked = answers
            .first()
            .and_then(|answer| answer.labels.first())
            .map(String::as_str);
        let scope = match picked {
            Some("Run once") => Some(PermissionScope::Once),
            Some("Allow for this chat") => Some(PermissionScope::Chat),
            Some("Always allow") => Some(PermissionScope::Always),
            _ => None,
        };
        match scope {
            Some(scope) => {
                self.result(
                    "card-2",
                    false,
                    "● app.service - Web app\n     Active: active (running) since Mon 2026-10-12 09:14:02 UTC; 1 day ago",
                    record(PermissionOutcome::Outside, Some(scope), None, Some(purpose)),
                )
                .await;
                self.text("The app is running on **web**.").await;
                true
            }
            None => {
                self.result(
                    "card-2",
                    true,
                    "The user denied running this outside the sandbox. Continue in the sandbox or ask the user.",
                    record(PermissionOutcome::Denied, None, None, Some(purpose)),
                )
                .await;
                if picked == Some("Deny and stop") {
                    return false;
                }
                self.text("Without the server I can't check the app; the sandbox keeps the SSH key from commands.")
                    .await;
                true
            }
        }
    }
}
