//! A running process's permission level, changed in place.
//!
//! A chat's level is its launch config's (`/permissions` writes it, a plan's
//! host writes its sessions', docs/design/agent-bots.md §6.6) and, for a
//! workspace's own thread, the engine's ([`crate::bots::profile::run_sandbox`]).
//! Before, a change reached only the next process: a parked one launched at
//! another level was ended before its next turn, and a busy one kept its
//! level to the end of its turn. Now this device watches the rows, and the
//! harness of each live run it hosts is told the new level
//! ([`cypher_harness::Harness::apply_level`]): Pi's permission extension
//! re-reads it before its next decision, mid-turn. A harness that can't
//! apply a level live keeps the old behaviour.

use std::sync::Arc;
use std::time::Duration;

use cypher_proto::SandboxLevel;

use super::SessionsEngine;
use crate::util::lock;

/// Rows change in bursts (a config, then its signed record); one pass after
/// a short pause covers them.
const SETTLE: Duration = Duration::from_millis(200);
/// A signed launch record can land after its row: the levels are checked
/// again this often.
const TICK: Duration = Duration::from_secs(5);

impl SessionsEngine {
    /// The level `chat_id`'s runs launch with: its launch config's (the
    /// row's, or under "Only paired devices" its signed record's), and for
    /// a workspace's own thread the engine's, whatever its row says.
    pub fn run_level(&self, chat_id: &str) -> Option<SandboxLevel> {
        let ws = self.inner.workspace()?;
        let mut level = ws.launch_config(chat_id).map(|config| config.sandbox);
        if let Ok(Some(chat)) = ws.chat(chat_id)
            && let Some(bot) = crate::bots::profile::run_sandbox(&ws, &chat)
        {
            level = Some(bot);
        }
        level
    }

    /// The level `chat_id`'s run is held to now: what its live process was
    /// launched with or last given, else what its next run launches with.
    pub fn live_level(&self, chat_id: &str) -> Option<SandboxLevel> {
        let live = lock(&self.inner.runs)
            .get(chat_id)
            .map(|run| run.launch.sandbox);
        live.or_else(|| self.run_level(chat_id))
    }

    /// Give every live run on this device whose level changed its new one,
    /// where its harness applies a level live. Returns the chats changed.
    pub fn apply_live_levels(&self) -> Vec<(String, SandboxLevel)> {
        let live: Vec<_> = lock(&self.inner.runs)
            .iter()
            .map(|(chat, run)| {
                (
                    chat.clone(),
                    run.run_id.clone(),
                    run.launch.harness,
                    run.launch.sandbox,
                )
            })
            .collect();
        let mut changed = Vec::new();
        for (chat_id, run_id, harness, launched) in live {
            let Some(level) = self.run_level(&chat_id) else {
                continue;
            };
            if level == launched {
                continue;
            }
            let Ok(harness) = self.inner.registry.resolve(harness) else {
                continue;
            };
            // Held to it before the process hears of it: Pi's extension asks
            // the engine (`PermissionLevel`) before it takes a higher level,
            // and must be told the new one. A parked process isn't ended
            // over it either.
            let set = |to: SandboxLevel| match lock(&self.inner.runs).get_mut(&chat_id) {
                Some(run) if run.run_id == run_id => {
                    run.launch.sandbox = to;
                    true
                }
                _ => false,
            };
            if !set(level) {
                continue;
            }
            if !harness.apply_level(&chat_id, level) {
                set(launched);
                continue;
            }
            // A turn rebuilt from the last request launches with it too.
            if let Some(request) = lock(&self.inner.last_requests).get_mut(&chat_id) {
                request.sandbox = level;
            }
            tracing::info!(chat = %chat_id, from = ?launched, to = ?level, "permission level applied to the live run");
            changed.push((chat_id, level));
        }
        changed
    }

    /// Watch the rows a level comes from and apply changes to live runs
    /// ([`Self::apply_live_levels`]). Ends with the engine.
    pub fn start_live_levels(&self) {
        let Some(ws) = self.inner.workspace() else {
            return;
        };
        let mut chats = ws.watch_chats();
        let mut records = ws.watch_bot_records();
        let mut bots = ws.watch_bots();
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(TICK);
            loop {
                tokio::select! {
                    changed = chats.changed() => if changed.is_err() { return },
                    changed = records.changed() => if changed.is_err() { return },
                    changed = bots.changed() => if changed.is_err() { return },
                    _ = tick.tick() => {}
                }
                tokio::time::sleep(SETTLE).await;
                chats.borrow_and_update();
                records.borrow_and_update();
                bots.borrow_and_update();
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                SessionsEngine { inner }.apply_live_levels();
            }
        });
    }
}
