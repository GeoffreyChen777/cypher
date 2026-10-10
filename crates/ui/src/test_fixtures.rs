//! Shared test builders for the proto/doc rows the UI tests construct. Each
//! returns an all-empty baseline (device `"dev"`, epoch timestamps, every
//! optional field unset); a test names only the fields it cares about and
//! fills the rest with struct-update syntax:
//! `Chat { id: "a".into(), ..test_fixtures::chat() }`.

use chrono::{DateTime, Utc};
use cypher_doc::{MessageRole, SessionMessageEntry};
use cypher_proto::{Chat, Session, SessionStatus, Space};

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
    }
}
