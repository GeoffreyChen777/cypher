//! Coordination (docs/design/agent-bots.md §6, P2): *Work directly*,
//! plans, orchestration threads, worker sessions, Stop, limits, approvals
//! and the activity log.
//!
//! Which tools a thread gets is decided at spawn ([`work_tools`]); whether a
//! call may run is decided per call, against the bot row as it is now. Pi's
//! own tools run inside Pi, so the bot tools extension asks the engine
//! before each of them ([`Bots::direct_tool`]): that is where *Work
//! directly* switched off mid-run, permission control being off, and the
//! taint rule apply to them.
//!
//! The coordination tools are in [`coordinate`], the host loop that acts on
//! plan decisions and Stop in [`host`], and the prompt sections and the
//! orchestration brief in [`text`].

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use cypher_proto::{
    self as p, ActivityKind, ApprovalState, Bot, BotActivity, BotApproval, BotChat, BotOrigin,
    BotThread, BotWork, Chat, SandboxLevel,
};

use super::Bots;
use crate::EngineError;
use crate::host::workspace_host::{Fields, WorkspaceHost};
use crate::pi::runtime::PiRuntimePaths;
use crate::util::{lock, new_id, off_runtime};

mod coordinate;
mod host;
mod text;

/// The engine-only "tool" the bot tools extension calls before each of Pi's
/// own tools runs: the answer is `allow` or why not.
pub(super) const DIRECT_GATE: &str = "direct_tool";
const ALLOW: &str = "allow";

/// The coordination tools of the bot chat (the rest need an orchestration
/// thread).
const MAIN_TOOLS: [&str; 3] = ["propose_plan", "list_work", "get_work"];

/// How long an action waits for the owner's approval (§4.3).
const APPROVAL_WAIT: Duration = Duration::from_secs(10 * 60);
const APPROVAL_POLL: Duration = Duration::from_millis(400);
/// How long Landstrip's verdict on this device is trusted.
const SANDBOX_CHECK_TTL: Duration = Duration::from_secs(10 * 60);

/// What a thread is to this track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Role {
    /// The bot chat.
    Main,
    /// The orchestration thread of this work item.
    Work(String),
    /// Routine, peer and unknown threads: nothing from this track.
    Other,
}

pub(super) fn role(marker: &BotChat) -> Role {
    if marker.origin != BotOrigin::Owner {
        return Role::Other;
    }
    match &marker.thread {
        BotThread::Main => Role::Main,
        BotThread::Work { work_id } => Role::Work(work_id.clone()),
        _ => Role::Other,
    }
}

/// This track's state shared by the tools and the host loop.
pub(super) struct WorkShared {
    /// The Pi runtime, for permission control's settings and status.
    pi_paths: Mutex<Option<PiRuntimePaths>>,
    /// Landstrip's verdict on this device (`doctor` spawns a process).
    sandbox: Mutex<Option<(Instant, Result<(), String>)>>,
    /// Held across every read-modify-write of a work row, so a tool and the
    /// host loop never overwrite each other's change.
    rows: Mutex<()>,
    approval_wait: Mutex<Duration>,
}

impl Default for WorkShared {
    fn default() -> Self {
        Self {
            pi_paths: Default::default(),
            sandbox: Default::default(),
            rows: Default::default(),
            approval_wait: Mutex::new(APPROVAL_WAIT),
        }
    }
}

/// The tools of this track a thread may call, beyond the read tools: Pi's
/// own with *Work directly*; with *Coordinate*, plans and following up in
/// the bot chat and everything else in an orchestration thread. Routine and
/// peer threads get none.
pub(super) fn work_tools(marker: &BotChat, bot: Option<&Bot>) -> Vec<&'static str> {
    let Some(bot) = bot else {
        return Vec::new();
    };
    let caps = bot.capabilities;
    let mut tools = Vec::new();
    let role = role(marker);
    if role == Role::Other {
        return tools;
    }
    if caps.work_directly {
        tools.extend(p::BOT_DIRECT_TOOLS);
    }
    if caps.coordinate {
        match role {
            Role::Main => tools.extend(MAIN_TOOLS),
            Role::Work(_) => tools.extend(
                p::BOT_WORK_TOOLS
                    .into_iter()
                    .filter(|tool| *tool != "propose_plan"),
            ),
            Role::Other => {}
        }
    }
    tools
}

/// This track's section of the thread's system prompt.
pub(super) fn work_prompt(
    workspace: &WorkspaceHost,
    bot: &Bot,
    marker: &BotChat,
) -> Option<String> {
    let role = role(marker);
    if role == Role::Other {
        let roots = workspace_roots(workspace, bot, marker);
        if roots.is_empty() {
            return None;
        }
        // A routine granted Work directly (§8).
        return Some(format!(
            "This routine may run commands and edit files with bash, read, write, edit, grep, \
             find and ls, under your owner's permission rules, in these local checkouts:\n{}\n\
             Nobody is watching: a command that would need approval fails the run, so stay \
             inside these folders and the sandbox. Change files only when the routine asks you \
             to.",
            roots
                .iter()
                .map(|root| format!("- {root}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    let roots = workspace_roots(workspace, bot, marker);
    text::prompt(bot, &role, &roots)
}

/// The workspace roots an owner thread with *Work directly* gets: the
/// bot's scoped checkouts on this device (§4.4), canonical, as permission
/// control compares them. Projects on other devices are reached through
/// sessions.
pub(super) fn workspace_roots(
    workspace: &WorkspaceHost,
    bot: &Bot,
    marker: &BotChat,
) -> Vec<String> {
    if !works_directly(workspace, bot, marker) {
        return Vec::new();
    }
    let mut roots: Vec<String> = Vec::new();
    for space in workspace.read_spaces().unwrap_or_default() {
        if space.device_id != workspace.device_id() || !bot.scope.sees_space(&space.id) {
            continue;
        }
        let path = std::fs::canonicalize(&space.path)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or(space.path);
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
    roots
}

/// Whether a thread gets Pi's own tools: an owner thread while the bot has
/// *Work directly*, and a routine's thread when its routine was granted it
/// too (never more than an owner thread, §4.1).
pub(super) fn works_directly(workspace: &WorkspaceHost, bot: &Bot, marker: &BotChat) -> bool {
    if !bot.capabilities.work_directly {
        return false;
    }
    match role(marker) {
        Role::Main | Role::Work(_) => true,
        Role::Other => routine_works_directly(workspace, marker),
    }
}

/// A routine thread whose routine may work directly.
fn routine_works_directly(workspace: &WorkspaceHost, marker: &BotChat) -> bool {
    let (BotOrigin::Routine, BotThread::Routine { routine_id }) = (marker.origin, &marker.thread)
    else {
        return false;
    };
    workspace
        .record::<p::BotRoutine>(p::KIND_BOT_ROUTINES, routine_id)
        .is_some_and(|routine| routine.bot_id == marker.bot_id && routine.work_directly)
}

/// The permission level a bot thread's run spawns with, whatever its chat
/// row says (any owner device may write that): workspace-write for an
/// owner thread working directly in local checkouts, read-only for every
/// other bot thread. `None` for other chats.
pub(super) fn run_sandbox(workspace: &WorkspaceHost, chat: &Chat) -> Option<SandboxLevel> {
    let marker = chat.bot.as_ref().filter(|m| m.is_bot_thread())?;
    let writes = workspace
        .bot(&marker.bot_id)
        .ok()
        .flatten()
        .is_some_and(|bot| !workspace_roots(workspace, &bot, marker).is_empty());
    Some(if writes {
        SandboxLevel::WorkspaceWrite
    } else {
        SandboxLevel::ReadOnly
    })
}

/// The `direct_tool` gate's arguments: the Pi tool and what it touches.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct DirectCall {
    tool: String,
    command: Option<String>,
    path: Option<String>,
}

impl DirectCall {
    /// The call in plain words, for an approval.
    fn describe(&self) -> String {
        match (&self.command, &self.path) {
            (Some(command), _) => format!("Run `{}`", p::clip(command, 300)),
            (None, Some(path)) => match self.tool.as_str() {
                "write" => format!("Write {path}"),
                "edit" => format!("Edit {path}"),
                _ => format!("Use {} on {path}", self.tool),
            },
            (None, None) => format!("Use the {} tool", self.tool),
        }
    }

    /// Only these act; reading under permission control needs no approval.
    fn acts(&self) -> bool {
        matches!(
            self.tool.as_str(),
            "bash" | "write" | "edit" | "request_permission"
        )
    }
}

impl Bots {
    /// Start this track's host loop (called once at engine start).
    pub(super) fn start_work_host(&self) {
        let task = tokio::spawn(host::run(self.clone(), self.hosts_cancelled()));
        self.keep_host_task(task);
    }

    /// Hand the work track the Pi runtime, whose permission control guards
    /// a bot's shell and file tools.
    pub fn set_pi_runtime(&self, paths: PiRuntimePaths) {
        *lock(&self.inner.work.pi_paths) = Some(paths);
        *lock(&self.inner.work.sandbox) = None;
    }

    /// Tests: how long an action waits for the owner's approval.
    #[doc(hidden)]
    pub fn set_approval_wait(&self, wait: Duration) {
        *lock(&self.inner.work.approval_wait) = wait;
    }

    /// Tests: taint a thread the way `ask_bot` does (§4.3).
    #[doc(hidden)]
    pub fn taint_thread(&self, chat_id: &str, reason: &str) {
        self.inner.taint.mark(chat_id, reason);
    }

    /// Answer `tool` when it is one of this track's; `None` otherwise.
    pub(super) async fn work_tool(
        &self,
        chat: &Chat,
        marker: &BotChat,
        tool: &str,
        args: &serde_json::Value,
    ) -> Option<Result<String, EngineError>> {
        if tool == DIRECT_GATE {
            return Some(Ok(self.direct_tool(chat, marker, args).await));
        }
        if !p::BOT_WORK_TOOLS.contains(&tool) {
            return None;
        }
        Some(self.coordination_tool(chat, marker, tool, args).await)
    }

    /// Whether one of Pi's own tools may run now: `allow`, or why not in
    /// words the model reads as the tool's result.
    async fn direct_tool(&self, chat: &Chat, marker: &BotChat, args: &serde_json::Value) -> String {
        let call: DirectCall = serde_json::from_value(args.clone()).unwrap_or_default();
        let tool = if call.tool.is_empty() {
            "this tool"
        } else {
            call.tool.as_str()
        };
        let bot = match self.inner.workspace.bot(&marker.bot_id) {
            Ok(Some(bot)) => bot,
            _ => return "This workspace has been deleted, so it can't use any tools.".into(),
        };
        if role(marker) == Role::Other && !routine_works_directly(&self.inner.workspace, marker) {
            return format!("{tool} isn't available in this session.");
        }
        if !bot.capabilities.work_directly {
            return format!(
                "Your owner turned off Work directly, so you can't use {tool} any more. Say \
                 what you would have done instead."
            );
        }
        if let Err(why) = self.permission_control().await {
            return why;
        }
        if call.acts()
            && let Some(reason) = self.inner.taint.reason(&chat.id)
        {
            let work_id = match role(marker) {
                Role::Work(id) => Some(id),
                _ => None,
            };
            if let Err(err) = self
                .ask_owner(&bot, chat, work_id.as_deref(), call.describe(), reason)
                .await
            {
                return err.to_string();
            }
        }
        ALLOW.into()
    }

    /// Permission control guards a bot's shell and file tools; while it is
    /// off, or cannot enforce on this device, those tools don't run.
    async fn permission_control(&self) -> Result<(), String> {
        let paths = lock(&self.inner.work.pi_paths).clone().ok_or_else(|| {
            "Pi isn't set up on this device, so shell and file tools are off for workspaces."
                .to_string()
        })?;
        let settings_path = crate::pi::permissions::settings_path(&paths);
        let enabled = off_runtime(move || {
            std::fs::read(&settings_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .is_some_and(|root| crate::pi::permissions::from_json(&root).enabled)
        })
        .await
        .unwrap_or(false);
        if !enabled {
            return Err(
                "Permission control is off on this device, so workspaces can't use shell and \
                 file tools. Your owner can turn it on in Settings → Permissions."
                    .into(),
            );
        }
        let cached = lock(&self.inner.work.sandbox)
            .as_ref()
            .filter(|(at, _)| at.elapsed() < SANDBOX_CHECK_TTL)
            .map(|(_, verdict)| verdict.clone());
        let verdict = match cached {
            Some(verdict) => verdict,
            None => {
                let verdict = off_runtime(move || {
                    let status = crate::pi::permissions::status(&paths);
                    if status.installed && status.sandbox_available {
                        Ok(())
                    } else {
                        Err(status
                            .error
                            .unwrap_or_else(|| "the OS sandbox does not work here".into()))
                    }
                })
                .await
                .unwrap_or_else(Err);
                *lock(&self.inner.work.sandbox) = Some((Instant::now(), verdict.clone()));
                verdict
            }
        };
        verdict.map_err(|error| {
            format!(
                "Permission control can't enforce anything on this device ({error}), so workspaces \
                 can't use shell and file tools."
            )
        })
    }

    /// Ask the owner to approve `action` and wait for the answer (§4.3):
    /// `Ok` once approved; declined, unanswered in time or stopped work is
    /// an error the model reads.
    pub(super) async fn ask_owner(
        &self,
        bot: &Bot,
        chat: &Chat,
        work_id: Option<&str>,
        action: String,
        reason: String,
    ) -> Result<(), EngineError> {
        let ws = &self.inner.workspace;
        let mut approval = BotApproval {
            id: new_id(),
            bot_id: bot.id.clone(),
            thread_chat_id: chat.id.clone(),
            work_id: work_id.map(String::from),
            action,
            reason,
            created_at: p::now_ms(),
            state: ApprovalState::Pending,
            decision: None,
        };
        approval.clamp();
        ws.put_record(
            p::KIND_BOT_APPROVALS,
            &approval.id,
            &approval,
            Fields::Except(BotApproval::OWNER_FIELDS),
        )?;
        self.log(
            &bot.id,
            ActivityKind::ApprovalAsked,
            format!("Asked for approval: {}", approval.action),
            Some(&chat.id),
            work_id,
            None,
        );
        let wait = *lock(&self.inner.work.approval_wait);
        let deadline = tokio::time::Instant::now() + wait;
        let cancelled = self.hosts_cancelled();
        let outcome = loop {
            let row: Option<BotApproval> = ws.record(p::KIND_BOT_APPROVALS, &approval.id);
            match row.as_ref().and_then(|row| row.decision.as_ref()) {
                Some(decision) if decision.approve => break ApprovalState::Approved,
                Some(_) => break ApprovalState::Declined,
                None if row.is_none() => break ApprovalState::Declined,
                None => {}
            }
            let stopped = work_id.is_some_and(|id| {
                ws.record::<BotWork>(p::KIND_BOT_WORK, id)
                    .is_none_or(|work| work.stop_requested_at.is_some() || !work.is_live())
            });
            if stopped || tokio::time::Instant::now() >= deadline {
                break ApprovalState::Expired;
            }
            tokio::select! {
                () = cancelled.cancelled() => break ApprovalState::Expired,
                () = tokio::time::sleep(APPROVAL_POLL) => {}
            }
        };
        approval.state = outcome;
        if let Err(err) = ws.put_record(
            p::KIND_BOT_APPROVALS,
            &approval.id,
            &approval,
            Fields::Only(&["state"]),
        ) {
            tracing::warn!(error = %err, "could not record an approval's outcome");
        }
        match outcome {
            ApprovalState::Approved => Ok(()),
            ApprovalState::Declined => Err(EngineError::Other(format!(
                "Your owner declined this, so it didn't happen: {}.",
                approval.action
            ))),
            _ => Err(EngineError::Other(format!(
                "Your owner didn't approve this in time, so it didn't happen: {}.",
                approval.action
            ))),
        }
    }

    /// Append one line to the bot's activity log (the audit trail).
    pub(super) fn log(
        &self,
        bot_id: &str,
        kind: ActivityKind,
        text: String,
        chat_id: Option<&str>,
        work_id: Option<&str>,
        space_id: Option<&str>,
    ) {
        let mut activity = BotActivity {
            id: new_id(),
            bot_id: bot_id.to_string(),
            at: p::now_ms(),
            kind,
            text,
            chat_id: chat_id.map(String::from),
            work_id: work_id.map(String::from),
            space_id: space_id.map(String::from),
        };
        activity.clamp();
        if let Err(err) = self.inner.workspace.put_record(
            p::KIND_BOT_ACTIVITY,
            &activity.id,
            &activity,
            Fields::All,
        ) {
            tracing::warn!(bot = %bot_id, error = %err, "could not log bot activity");
        }
    }

    /// Change one work row under the row lock and write the host's fields
    /// back; `None` when the row is gone. The owner's fields are never
    /// written here.
    pub(super) fn edit_work<T>(
        &self,
        work_id: &str,
        edit: impl FnOnce(&mut BotWork) -> T,
    ) -> Result<Option<T>, EngineError> {
        let _guard = lock(&self.inner.work.rows);
        let ws = &self.inner.workspace;
        let Some(mut work) = ws.record::<BotWork>(p::KIND_BOT_WORK, work_id) else {
            return Ok(None);
        };
        let out = edit(&mut work);
        work.clamp();
        ws.put_record(
            p::KIND_BOT_WORK,
            work_id,
            &work,
            Fields::Except(BotWork::OWNER_FIELDS),
        )?;
        Ok(Some(out))
    }

    /// The bot's private folder (every thread's cwd).
    pub(super) fn folder(&self, bot_id: &str) -> std::path::PathBuf {
        self.inner.root.join(bot_id)
    }
}

/// Whether `path` is `root` or inside it (by components, so `/a/bc` is not
/// inside `/a/b`).
pub(super) fn is_within(path: &str, root: &str) -> bool {
    Path::new(path).starts_with(Path::new(root))
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use cypher_proto::{BotCapabilities, BotScope};

    use super::*;

    fn bot(caps: BotCapabilities) -> Bot {
        Bot {
            id: "b".into(),
            name: "Bob".into(),
            icon: None,
            color: None,
            host_device_id: "dev".into(),
            chat_id: "bot-chat".into(),
            instructions: None,
            scope: BotScope::default(),
            capabilities: caps,
            paused: false,
            created_at: DateTime::from_timestamp_millis(0).unwrap(),
        }
    }

    fn caps(work_directly: bool, coordinate: bool) -> BotCapabilities {
        BotCapabilities {
            work_directly,
            coordinate,
            steer: false,
        }
    }

    fn work_thread() -> BotChat {
        BotChat::thread(
            "b",
            BotOrigin::Owner,
            BotThread::Work {
                work_id: "w1".into(),
            },
        )
    }

    #[test]
    fn tools_follow_the_thread_and_the_capabilities() {
        let main = BotChat::owner("b");
        let thread = work_thread();
        assert!(work_tools(&main, Some(&bot(caps(false, false)))).is_empty());
        assert_eq!(
            work_tools(&main, Some(&bot(caps(true, false)))),
            p::BOT_DIRECT_TOOLS
        );
        assert_eq!(
            work_tools(&main, Some(&bot(caps(false, true)))),
            ["propose_plan", "list_work", "get_work"]
        );
        let worker_tools = work_tools(&thread, Some(&bot(caps(false, true))));
        assert!(!worker_tools.contains(&"propose_plan"));
        assert_eq!(worker_tools.len(), p::BOT_WORK_TOOLS.len() - 1);
        let both = work_tools(&thread, Some(&bot(caps(true, true))));
        assert!(both.contains(&"bash") && both.contains(&"start_session"));
        for origin in [BotOrigin::Routine, BotOrigin::Peer] {
            for thread in [
                BotThread::Main,
                BotThread::Routine {
                    routine_id: "r".into(),
                },
                BotThread::Peer {
                    connection_id: "c".into(),
                },
            ] {
                let marker = BotChat::thread("b", origin, thread);
                assert!(work_tools(&marker, Some(&bot(caps(true, true)))).is_empty());
            }
        }
        // An owner thread of a kind this engine doesn't know gets nothing.
        let unknown = BotChat::thread("b", BotOrigin::Owner, BotThread::Unknown);
        assert!(work_tools(&unknown, Some(&bot(caps(true, true)))).is_empty());
        assert!(work_tools(&main, None).is_empty());
    }

    #[test]
    fn direct_calls_read_as_actions() {
        let call = |tool: &str, command: Option<&str>, path: Option<&str>| DirectCall {
            tool: tool.into(),
            command: command.map(Into::into),
            path: path.map(Into::into),
        };
        assert_eq!(
            call("bash", Some("cargo test"), None).describe(),
            "Run `cargo test`"
        );
        assert_eq!(
            call("write", None, Some("/w/a.rs")).describe(),
            "Write /w/a.rs"
        );
        assert!(call("bash", None, None).acts());
        assert!(call("edit", None, None).acts());
        assert!(!call("read", None, Some("/w/a.rs")).acts());
        assert!(!call("grep", None, None).acts());
    }

    #[test]
    fn paths_are_within_roots_by_component() {
        assert!(is_within("/w/billing", "/w/billing"));
        assert!(is_within("/w/billing/src", "/w/billing"));
        assert!(!is_within("/w/billing-old", "/w/billing"));
    }
}
