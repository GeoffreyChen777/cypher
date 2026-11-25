//! The engine's integration tests, in one binary: each area below is a
//! folder of modules, and `support` holds the fixtures they share.
//!
//! One binary links the engine once and runs every test side by side; a
//! binary per file linked it forty times and ran the files one after another.
//! Nothing here may change the process's environment or working directory:
//! every test shares them.
//!
//!   cargo test -p cypher-engine --test it             # all of them
//!   cargo test -p cypher-engine --test it bots::      # one area
//!   cargo test -p cypher-engine --test it routines    # one file

mod support;

/// Agent bots: creation, coordination, routines, the bot network, memory.
mod bots {
    mod agents;
    mod file_roots;
    mod memory;
    mod network;
    mod plan_body_watch;
    mod routines;
    mod work;
}

/// The mock harness's scripted demos, run through a real engine.
mod demos {
    mod permissions;
    mod translation;
    mod turn_changes;
}

/// Repositories, worktrees, diffs and terminals.
mod repos {
    mod diff_sync_churn;
    mod repos_diffs_terminals;
    mod turn_changes;
    mod worktree_on_run;
}

/// What becomes of a whole chat: forks, moves between projects, Side Chats.
mod sessions {
    mod session_forks;
    mod session_migration;
    mod side_chats;
}

/// Child chats a parent agent starts, and their live status.
mod subagents {
    mod child_bridge;
    mod subagent_status;
}

/// Accounts, profiles, devices and the workspace registry between them.
mod sync {
    mod auth;
    mod device_routing;
    mod local_first;
    mod local_import;
    mod local_profiles;
    mod workspace_sync;
}

/// One turn from the queued command to the transcript: sending, steering,
/// attachments, restarts and recovery.
mod turns {
    mod annotated_prompt;
    mod born_chat2_race;
    mod chat_permission_level;
    mod command_dismissal;
    mod command_signatures;
    mod context_recovery;
    mod crash_outbox_recovery;
    mod e2e;
    mod extension_notices;
    mod follow_up_attachments;
    mod input_translation;
    mod model_switch;
    mod projectless;
    mod queue_first_attachments;
    mod restart_resume;
    mod steer_ledger;
    mod turn_quiesce;
    mod unarchive_on_send;
    mod uploads_and_titles;
}
