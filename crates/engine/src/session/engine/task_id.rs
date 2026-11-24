//! The task a chat's run belongs to, for the Runtime's permission check
//! (`CYPHER_TASK_ID`; docs/design/permission-control.md §7.3.13).
//!
//! The permission extension's other-task floor holds a destroy of what
//! another chat made, unless that chat belongs to the same task. A task id is
//! a `/`-separated lineage, widest first:
//!
//! - `workspace:<bot>` — a workspace's own chat and its threads (routines,
//!   peers' questions, a project routine's session);
//! - `workspace:<bot>/plan:<work>` — every session one of its plans started,
//!   and that plan's background thread;
//! - `chat:<id>` — an owner's plain chat (no workspace, no plan).
//!
//! A subagent's child chat takes its root chat's id, so a child is never
//! another task to the chat that started it. Two ids are the same task when
//! one is the other or a prefix of it by whole segments (a workspace's chat
//! and its plans' sessions; two sessions of one plan); two different plans of
//! one workspace, or two workspaces, are different tasks. A plain chat's id
//! names a lineage, not a task the engine knows the bounds of — the owner may
//! split one piece of work over two chats by hand — so the extension reads a
//! `chat:` id against any other unrelated id as "lineage unknown" (the judge
//! decides inside the chat's working tree), never as a known other task.
//! Memory passes get no id (they touch only memory files).

use cypher_proto::{BotRecords, BotThread, Chat};

/// How deep a chain of subagents may go before the lineage is given up as
/// unknown (a cycle in corrupt rows must not hang a spawn).
const MAX_CHILD_DEPTH: usize = 16;

/// The task id of `chat` (see the module docs), or `None` for a memory pass
/// or a child chat whose parent row can't be read. `parent` reads another
/// chat's row by id.
pub(crate) fn task_id(
    chat: &Chat,
    records: &BotRecords,
    parent: impl Fn(&str) -> Option<Chat>,
) -> Option<String> {
    let mut root = chat.clone();
    let mut depth = 0;
    while let Some(child) = &root.child {
        depth += 1;
        if depth > MAX_CHILD_DEPTH {
            return None;
        }
        root = parent(&child.parent_chat_id)?;
    }
    if root.memory.is_some() {
        return None;
    }
    let plan = |bot: &str, work: &str| format!("workspace:{bot}/plan:{work}");
    if let Some(by) = &root.started_by {
        return Some(match &by.work_id {
            Some(work) => plan(&by.bot_id, work),
            None => format!("workspace:{}", by.bot_id),
        });
    }
    if let Some((work, _)) = records.started_by(&root.id) {
        return Some(plan(&work.bot_id, &work.id));
    }
    if let Some(marker) = &root.bot {
        if let BotThread::Work { work_id } = &marker.thread {
            return Some(plan(&marker.bot_id, work_id));
        }
        if let Some(work) = records
            .work
            .iter()
            .find(|work| work.thread_chat_id.as_deref() == Some(root.id.as_str()))
        {
            return Some(plan(&work.bot_id, &work.id));
        }
        return Some(format!("workspace:{}", marker.bot_id));
    }
    Some(format!("chat:{}", root.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::{BotChat, BotOrigin, ChatStartedBy};
    use std::collections::HashMap;

    fn chat(id: &str) -> Chat {
        Chat {
            id: id.into(),
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
            created_at: chrono::DateTime::from_timestamp(0, 0).expect("epoch"),
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: None,
            last_seen_at: None,
            room_gen: None,
            child: None,
            bot: None,
            memory: None,
            started_by: None,
        }
    }

    fn child_of(id: &str, parent: &str) -> Chat {
        let mut c = chat(id);
        c.child = Some(cypher_proto::ChildChat {
            parent_chat_id: parent.into(),
            parent_run_id: "run".into(),
            agent: "actor".into(),
            task: "t".into(),
            mode: cypher_proto::SubagentRunMode::Async,
            tool_call_id: None,
            profile: cypher_proto::ChildAgentProfile {
                system_prompt: String::new(),
                tools: Vec::new(),
                model: None,
                thinking: None,
            },
        });
        c
    }

    fn records() -> BotRecords {
        BotRecords::default()
    }

    fn lookup(rows: &[Chat]) -> impl Fn(&str) -> Option<Chat> + '_ {
        let map: HashMap<String, Chat> = rows.iter().map(|c| (c.id.clone(), c.clone())).collect();
        move |id| map.get(id).cloned()
    }

    #[test]
    fn plain_chat_is_its_own_lineage_and_children_follow_it() {
        let root = chat("c1");
        let kid = child_of("k1", "c1");
        let grandkid = child_of("k2", "k1");
        let rows = [root.clone(), kid.clone(), grandkid.clone()];
        assert_eq!(
            task_id(&root, &records(), lookup(&rows)).as_deref(),
            Some("chat:c1")
        );
        assert_eq!(
            task_id(&kid, &records(), lookup(&rows)).as_deref(),
            Some("chat:c1")
        );
        assert_eq!(
            task_id(&grandkid, &records(), lookup(&rows)).as_deref(),
            Some("chat:c1")
        );
    }

    #[test]
    fn workspace_chat_and_threads_share_the_workspace_id() {
        let mut main = chat("w-main");
        main.bot = Some(BotChat::owner("atlas"));
        let mut routine = chat("w-routine");
        routine.bot = Some(BotChat::thread(
            "atlas",
            BotOrigin::Routine,
            BotThread::Routine {
                routine_id: "r1".into(),
            },
        ));
        let mut peer = chat("w-peer");
        peer.bot = Some(BotChat::thread(
            "atlas",
            BotOrigin::Peer,
            BotThread::Peer {
                connection_id: "p".into(),
            },
        ));
        for c in [&main, &routine, &peer] {
            assert_eq!(
                task_id(c, &records(), |_| None).as_deref(),
                Some("workspace:atlas")
            );
        }
    }

    #[test]
    fn a_plans_sessions_thread_and_their_subagents_share_the_plan_id() {
        let mut session = chat("s1");
        session.started_by = Some(ChatStartedBy {
            bot_id: "atlas".into(),
            work_id: Some("plan-7".into()),
        });
        let mut thread = chat("t1");
        thread.bot = Some(BotChat::thread(
            "atlas",
            BotOrigin::Owner,
            BotThread::Work {
                work_id: "plan-7".into(),
            },
        ));
        let kid = child_of("k1", "s1");
        let rows = [session.clone(), thread.clone(), kid.clone()];
        for c in [&session, &thread, &kid] {
            assert_eq!(
                task_id(c, &records(), lookup(&rows)).as_deref(),
                Some("workspace:atlas/plan:plan-7")
            );
        }
        // Another plan of the same workspace is another task.
        let mut other = chat("s2");
        other.started_by = Some(ChatStartedBy {
            bot_id: "atlas".into(),
            work_id: Some("plan-8".into()),
        });
        assert_eq!(
            task_id(&other, &records(), |_| None).as_deref(),
            Some("workspace:atlas/plan:plan-8")
        );
    }

    #[test]
    fn a_session_known_only_from_the_plan_record_gets_the_plan_id() {
        let mut recs = records();
        let work: cypher_proto::BotWork = serde_json::from_value(serde_json::json!({
            "id": "plan-9", "botId": "scout", "title": "t",
            "instruction": "go", "chatId": "w", "createdAt": 0,
            "steps": [{ "id": "st", "title": "s", "sessions": ["old-session"] }],
            "threadChatId": "bg-thread"
        }))
        .expect("work record");
        recs.work.push(work);
        assert_eq!(
            task_id(&chat("old-session"), &recs, |_| None).as_deref(),
            Some("workspace:scout/plan:plan-9")
        );
        let mut bg = chat("bg-thread");
        bg.bot = Some(BotChat::new("scout", BotOrigin::Owner));
        assert_eq!(
            task_id(&bg, &recs, |_| None).as_deref(),
            Some("workspace:scout/plan:plan-9")
        );
    }

    #[test]
    fn unknown_lineage_and_memory_passes_get_none() {
        // The parent row is gone: unknown, not the child's own id.
        assert_eq!(task_id(&child_of("k", "gone"), &records(), |_| None), None);
        // A cycle in corrupt rows ends.
        let a = child_of("a", "b");
        let b = child_of("b", "a");
        let rows = [a.clone(), b];
        assert_eq!(task_id(&a, &records(), lookup(&rows)), None);
        let mut pass = chat("m");
        pass.memory = Some(cypher_proto::MemoryChat {
            store_id: "s".into(),
            scope: cypher_proto::MemoryScope::You,
            pass_id: "p".into(),
        });
        assert_eq!(task_id(&pass, &records(), |_| None), None);
    }
}
