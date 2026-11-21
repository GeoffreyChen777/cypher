//! Shared test builders for the proto/doc rows the UI tests construct. Each
//! returns an all-empty baseline (device `"dev"`, epoch timestamps, every
//! optional field unset); a test names only the fields it cares about and
//! fills the rest with struct-update syntax:
//! `Chat { id: "a".into(), ..test_fixtures::chat() }`.

use chrono::{DateTime, Utc};
use cypher_doc::{MessageRole, SessionMessageEntry};
use cypher_proto::{
    Bot, BotRoutine, BotWork, Chat, Session, SessionStatus, Space, StepState, WorkState, WorkStep,
};

pub fn chat() -> Chat {
    Chat {
        id: "chat".into(),
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
        created_at: DateTime::<Utc>::UNIX_EPOCH,
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

pub fn space() -> Space {
    Space {
        id: "space".into(),
        device_id: "dev".into(),
        path: "/".into(),
        name: None,
        git_detected: false,
        git_checked_at: None,
        checkout_id: None,
        created_at: DateTime::<Utc>::UNIX_EPOCH,
        pinned: false,
        icon: None,
        color: None,
    }
}

pub fn session() -> Session {
    Session {
        chat_id: "chat".into(),
        device_id: "dev".into(),
        status: SessionStatus::Idle,
        started_at: None,
        updated_at: DateTime::<Utc>::UNIX_EPOCH,
        subagents: Vec::new(),
        context_usage: None,
        throughput: None,
        recovery: None,
        progress: None,
    }
}

pub fn entry() -> SessionMessageEntry {
    SessionMessageEntry {
        id: "entry".into(),
        role: MessageRole::User,
        parts: Vec::new(),
        created_at: 0,
        device_id: "dev".into(),
        status: None,
        continuation_of: None,
        completed_at: None,
        comments: Vec::new(),
        models: Vec::new(),
        turn_changes: None,
        recovery: None,
    }
}

/// A workspace hosted on `"dev"` whose chat is `"bot-chat"`.
pub fn bot() -> Bot {
    Bot {
        id: "b".into(),
        name: "Bot".into(),
        icon: None,
        color: None,
        host_device_id: "dev".into(),
        chat_id: "bot-chat".into(),
        instructions: None,
        scope: Default::default(),
        capabilities: Default::default(),
        paused: false,
        created_at: DateTime::<Utc>::UNIX_EPOCH,
        plans: Default::default(),
        session_choice: Default::default(),
    }
}

/// A proposed one-step plan of `bot_id`, proposed in `"bot-chat"`.
pub fn bot_work(id: &str, bot_id: &str) -> BotWork {
    BotWork {
        id: id.into(),
        bot_id: bot_id.into(),
        title: "Plan".into(),
        instruction: String::new(),
        summary: None,
        steps: vec![WorkStep {
            id: "1".into(),
            title: "Step".into(),
            project: None,
            detail: None,
            state: StepState::Pending,
            sessions: vec![],
            note: None,
            started_at: None,
            ended_at: None,
            session_choice: Default::default(),
        }],
        state: WorkState::Proposed,
        chat_id: "bot-chat".into(),
        tool_call_id: None,
        thread_chat_id: None,
        created_at: 0,
        started_at: None,
        ended_at: None,
        result: None,
        decision: None,
        stop_requested_at: None,
        notify: false,
        time_limit_minutes: None,
        time_added_minutes: None,
        warned_at: None,
        coordinator_choice: Default::default(),
    }
}

/// A manual workspace routine of `bot_id` that has never run.
pub fn bot_routine(id: &str, bot_id: &str) -> BotRoutine {
    BotRoutine {
        id: id.into(),
        bot_id: bot_id.into(),
        name: "Routine".into(),
        prompt: String::new(),
        target: Default::default(),
        model: None,
        schedule: Default::default(),
        enabled: true,
        write_notes: false,
        work_directly: false,
        created_at: 0,
        updated_at: 0,
        run_requested_at: None,
        thread_chat_id: None,
        manual_chat_id: None,
        worktree_path: None,
        last_run: None,
        next_run_at: None,
        thinking: None,
        fast: None,
        run_note: None,
    }
}
