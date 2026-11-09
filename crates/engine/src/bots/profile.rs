//! What a bot thread's run may do, derived from (chat, bot row, origin) when
//! the run spawns (docs/design/agent-bots.md §4.4).
//!
//! Spawn time fixes what a run *can* call: its tool allowlist, its system
//! prompt and its workspace roots. Whether a call is *allowed* is decided per
//! call by the bot tools, which re-read the bot row, so neither scope nor
//! capabilities live in the profile.

use cypher_harness::BotRunEnv;
use cypher_proto::{
    Bot, BotOrigin, BotThread, Chat, PermissionPromptKind, SandboxLevel, UserInputQuestion,
};

use super::{memory, network, prompt, work};
use crate::host::workspace_host::WorkspaceHost;

/// The run env for a bot thread; `None` for every other chat, a project
/// routine's session included. A thread whose bot row is gone still runs
/// locked down, seeing nothing.
pub fn bot_env(workspace: &WorkspaceHost, chat: &Chat) -> Option<BotRunEnv> {
    let marker = chat.bot.as_ref().filter(|m| m.is_bot_thread())?;
    let mut bot = workspace.bot(&marker.bot_id).ok().flatten();
    // A peer thread describes the scope it reads (bot ∩ connection) and
    // who is asking.
    if let Some(bot) = bot.as_mut() {
        let chats = workspace.read_chats().unwrap_or_default();
        if let Some(scope) = network::peer_scope(workspace, bot, marker, &chats) {
            bot.scope = scope;
        }
    }
    let asker = network::asker(workspace, marker);
    let spaces = workspace.read_spaces().unwrap_or_default();
    let devices = workspace.read_devices().unwrap_or_default();
    let mut system_prompt = match &bot {
        Some(bot) => prompt::system_prompt(
            bot,
            marker.origin,
            asker.as_ref().map(network::as_asker).as_ref(),
            &spaces,
            &devices,
        ),
        None => prompt::orphan_prompt(),
    };
    let mut workspace_roots = Vec::new();
    if let Some(bot) = &bot {
        for section in [
            work::work_prompt(workspace, bot, marker),
            memory::memory_prompt(workspace, bot, marker),
            network::network_prompt(workspace, bot, marker),
        ]
        .into_iter()
        .flatten()
        {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&section);
        }
        workspace_roots = work::workspace_roots(workspace, bot, marker);
    }
    let mut tools = tools_for(marker, bot.as_ref());
    // A routine granted Work directly gets Pi's own tools too; the bot row
    // and the routine are re-read on every call (the direct-tool gate).
    if let Some(bot) = &bot
        && work::works_directly(workspace, bot, marker)
    {
        for tool in cypher_proto::BOT_DIRECT_TOOLS {
            if !tools.iter().any(|t| t == tool) {
                tools.push(tool.to_string());
            }
        }
    }
    Some(BotRunEnv {
        bot_id: marker.bot_id.clone(),
        origin: marker.origin,
        tools,
        system_prompt,
        workspace_roots,
    })
}

/// The tool allowlist of a thread: the read tools for every origin, then
/// what each track adds for this thread and the bot's capabilities (Pi's
/// tools with *Work directly*, coordination, notes, the network). A deleted
/// bot's thread keeps only the read tools, which see nothing.
pub fn tools_for(marker: &cypher_proto::BotChat, bot: Option<&Bot>) -> Vec<String> {
    let mut tools: Vec<String> = cypher_proto::BOT_TOOLS
        .iter()
        .map(|tool| tool.to_string())
        .collect();
    if bot.is_some() {
        for tool in work::work_tools(marker, bot)
            .into_iter()
            .chain(memory::memory_tools(marker, bot))
            .chain(network::network_tools(marker, bot))
        {
            if !tools.iter().any(|t| t == tool) {
                tools.push(tool.to_string());
            }
        }
    }
    tools
}

/// The permission level a bot thread's run spawns with, decided by the
/// engine whatever the chat row says: workspace-write for an owner thread
/// with *Work directly* and checkouts on this device, read-only for every
/// other bot thread (§4.4). `None` for other chats.
pub fn run_sandbox(workspace: &WorkspaceHost, chat: &Chat) -> Option<SandboxLevel> {
    work::run_sandbox(workspace, chat)
}

/// Whether nobody watches this chat's runs, so they must never wait for an
/// answer: the engine denies every question and approval such a run asks
/// and settles the run as failed (§8). Routine threads; orchestration
/// threads, which are hidden: a permission prompt there would stall the
/// work where nobody sees it, so it fails the work instead (§6.5). A project
/// routine's session is a routine's too (origin routine).
pub fn is_unattended(_workspace: &WorkspaceHost, chat: &Chat) -> bool {
    chat.bot.as_ref().is_some_and(|marker| {
        marker.origin == BotOrigin::Routine || matches!(marker.thread, BotThread::Work { .. })
    })
}

/// An unattended run the engine stopped because it asked for input. The
/// engine hands it to its unattended-stop listeners; the bot scheduler's
/// settles the routine or task run as failed with this reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnattendedStop {
    pub chat_id: String,
    /// The error the transcript shows, naming what the run tried to ask.
    pub reason: String,
}

/// The transcript error for an unattended run stopped by `questions`.
pub fn unattended_stop_reason(questions: &[UserInputQuestion]) -> String {
    let asked = questions
        .first()
        .map(|question| match &question.permission {
            Some(prompt) => format!(
                "ask for approval to {}",
                match prompt.kind {
                    PermissionPromptKind::Escalate => "run a command outside the sandbox",
                    PermissionPromptKind::Access => "reach files beyond its defaults",
                    PermissionPromptKind::Network => "reach a domain the network filter blocks",
                    PermissionPromptKind::Tool => "use a tool that asks first",
                }
            ),
            None => {
                let line = question.question.lines().next().unwrap_or_default().trim();
                if line.is_empty() {
                    "ask a question".to_string()
                } else {
                    format!("ask \"{line}\"")
                }
            }
        });
    format!(
        "This run is unattended, so it couldn't {}. It stopped.",
        asked.as_deref().unwrap_or("ask a question")
    )
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use cypher_proto::BotChat;

    use super::*;
    use crate::bots::refuse_widening_command;

    fn chat(bot: Option<BotChat>) -> Chat {
        Chat {
            id: "c1".into(),
            device_id: "dev".into(),
            title: None,
            archived: false,
            pinned: false,
            cwd: None,
            branch: None,
            checkout_id: None,
            config: None,
            last_message_preview: None,
            last_message_at: None,
            created_at: DateTime::from_timestamp_millis(0).unwrap(),
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: None,
            last_seen_at: None,
            room_gen: None,
            child: None,
            bot,
            memory: None,
        }
    }

    #[test]
    fn network_tools_reach_owner_threads_only() {
        let bot = Bot {
            id: "b1".into(),
            name: "Bot".into(),
            icon: None,
            color: None,
            host_device_id: "dev".into(),
            chat_id: "c1".into(),
            instructions: None,
            scope: Default::default(),
            capabilities: Default::default(),
            paused: false,
            created_at: DateTime::from_timestamp_millis(0).unwrap(),
        };
        let owner = tools_for(&BotChat::owner("b1"), Some(&bot));
        for tool in cypher_proto::BOT_NETWORK_TOOLS {
            assert!(owner.iter().any(|t| t == tool), "{tool}");
        }
        for origin in [BotOrigin::Routine, BotOrigin::Peer] {
            let thread = BotChat::thread(
                "b1",
                origin,
                cypher_proto::BotThread::Peer {
                    connection_id: "cn".into(),
                },
            );
            let tools = tools_for(&thread, Some(&bot));
            assert!(
                cypher_proto::BOT_NETWORK_TOOLS
                    .iter()
                    .all(|tool| !tools.iter().any(|t| t == tool)),
                "{origin:?}: {tools:?}"
            );
        }
    }

    #[test]
    fn every_origin_gets_the_read_tools() {
        for origin in [BotOrigin::Owner, BotOrigin::Routine, BotOrigin::Peer] {
            assert_eq!(
                tools_for(&BotChat::new("b1", origin), None),
                cypher_proto::BOT_TOOLS
            );
        }
    }

    #[tokio::test]
    async fn routine_threads_and_routine_sessions_are_unattended() {
        let dir = tempfile::tempdir().unwrap();
        let ws = WorkspaceHost::open(
            std::sync::Arc::new(cypher_sync::DocsStore::open(dir.path()).unwrap()),
            crate::host::workspace_host::WorkspaceHostConfig {
                device_id: "dev".into(),
                device_name: "Local".into(),
                platform: "linux".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
                allow_device_rejoin: false,
            },
        )
        .unwrap();
        assert!(!is_unattended(&ws, &chat(None)));
        assert!(!is_unattended(&ws, &chat(Some(BotChat::owner("b1")))));
        assert!(!is_unattended(
            &ws,
            &chat(Some(BotChat::new("b1", BotOrigin::Peer)))
        ));
        assert!(is_unattended(
            &ws,
            &chat(Some(BotChat::new("b1", BotOrigin::Routine)))
        ));
        assert!(is_unattended(
            &ws,
            &chat(Some(BotChat::thread(
                "b1",
                BotOrigin::Owner,
                BotThread::Work {
                    work_id: "w".into()
                }
            )))
        ));

        // A project routine's session: unattended, but a plain session.
        let session = chat(Some(BotChat::thread(
            "b1",
            BotOrigin::Routine,
            BotThread::RoutineSession {
                routine_id: "r".into(),
            },
        )));
        assert!(is_unattended(&ws, &session));
        assert!(bot_env(&ws, &session).is_none());
        assert!(run_sandbox(&ws, &session).is_none());
    }

    #[test]
    fn unattended_stops_name_what_the_run_asked() {
        let question = |text: &str, kind: Option<PermissionPromptKind>| UserInputQuestion {
            id: "q".into(),
            header: "h".into(),
            question: text.into(),
            options: Vec::new(),
            multi_select: false,
            permission: kind.map(|kind| cypher_proto::PermissionPrompt {
                kind,
                tool_call_id: None,
            }),
            asked_by: None,
        };
        assert_eq!(
            unattended_stop_reason(&[question(
                "Run this command outside the sandbox?\n\nContext:",
                Some(PermissionPromptKind::Escalate)
            )]),
            "This run is unattended, so it couldn't ask for approval to run a command \
             outside the sandbox. It stopped."
        );
        assert_eq!(
            unattended_stop_reason(&[question("Which approach?\nmore", None)]),
            "This run is unattended, so it couldn't ask \"Which approach?\". It stopped."
        );
        assert_eq!(
            unattended_stop_reason(&[]),
            "This run is unattended, so it couldn't ask a question. It stopped."
        );
    }

    #[test]
    fn slash_commands_follow_the_origin() {
        let plain = chat(None);
        let owner = chat(Some(BotChat::owner("b1")));
        for prompt in [
            "/scripts on",
            "/goal x",
            "/orchestrate",
            "/compact",
            "hi /goal",
        ] {
            refuse_widening_command(&plain, prompt).expect("ordinary chats run every command");
        }
        for prompt in ["/scripts on", "  /goal ship it", "/orchestrate"] {
            let err = refuse_widening_command(&owner, prompt).unwrap_err();
            assert!(
                err.to_string()
                    .contains("isn't available in a workspace chat")
            );
        }
        for prompt in ["/compact", "what changed?", "see /goal"] {
            refuse_widening_command(&owner, prompt).expect("owner threads keep the rest");
        }
        for origin in [BotOrigin::Routine, BotOrigin::Peer] {
            let thread = chat(Some(BotChat::new("b1", origin)));
            for prompt in ["/compact", " /model x", "/", "/scripts"] {
                let err = refuse_widening_command(&thread, prompt).unwrap_err();
                assert!(err.to_string().contains("Slash commands aren't available"));
            }
            refuse_widening_command(&thread, "what changed? a/b").expect("plain prompts run");
        }
    }
}
