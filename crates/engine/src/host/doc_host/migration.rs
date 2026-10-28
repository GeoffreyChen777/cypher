//! Session-migration hooks: holding a chat's command drain while it moves
//! off this device, and re-homing Runs queued before it moved here.

use super::*;

/// Folders a moved chat's queued Runs may still name, and where it runs now.
pub(crate) struct MovedCwd {
    previous: Vec<String>,
    cwd: String,
}

impl DocHost {
    /// Hold `chat_id`'s command drain for a migration. False when another
    /// migration already holds it.
    pub fn hold_commands(&self, chat_id: &str) -> bool {
        lock(&self.inner.held).insert(chat_id.to_string())
    }

    /// Release a hold. A chat this device still hosts (the move failed)
    /// drains what queued meanwhile; a chat that moved away no longer
    /// passes the host gate.
    pub fn release_commands(&self, chat_id: &str) {
        if !lock(&self.inner.held).remove(chat_id) {
            return;
        }
        let handle = lock(&self.inner.handles).get(chat_id).cloned();
        if let Some(handle) = handle {
            let host = self.clone();
            self.spawn_worker(async move { host.drain_commands(&handle).await });
        }
    }

    pub(super) fn commands_held(&self, chat_id: &str) -> bool {
        lock(&self.inner.held).contains(chat_id)
    }

    /// Commands of `chat_id` still pending: queued, executing, or claimed by
    /// a process that died before resolving them (the next drain rejects
    /// those). The processed ledger is this device's alone, so a migration
    /// refuses until they settle — the new host would run them again.
    pub fn unsettled_commands(&self, chat_id: &str) -> Result<usize, EngineError> {
        let handle = self.open(chat_id)?;
        Ok(handle
            .doc
            .read_commands()?
            .iter()
            .filter(|command| command.status == SessionCommandStatus::Pending)
            .count())
    }

    /// A chat moved here: Runs queued before the move name one of
    /// `previous` and run in `cwd` instead.
    pub fn note_moved_cwd(&self, chat_id: &str, previous: Vec<String>, cwd: &str) {
        let previous: Vec<String> = previous.into_iter().filter(|p| p != cwd).collect();
        let mut moved = lock(&self.inner.moved_cwds);
        if previous.is_empty() {
            moved.remove(chat_id);
            return;
        }
        moved.insert(
            chat_id.to_string(),
            MovedCwd {
                previous,
                cwd: cwd.to_string(),
            },
        );
    }

    /// The folder a Run queued with `cwd` runs in, when the chat moved here
    /// since it was queued.
    pub(super) fn moved_cwd(&self, chat_id: &str, cwd: &str) -> Option<String> {
        let moved = lock(&self.inner.moved_cwds);
        let entry = moved.get(chat_id)?;
        entry
            .previous
            .iter()
            .any(|p| p == cwd)
            .then(|| entry.cwd.clone())
    }

    /// Open the chat so its drain runs (on open and on every change). The
    /// new host calls this once it owns a moved chat: commands queued during
    /// the move then execute here.
    pub fn kick_drain(&self, chat_id: &str) {
        match self.open(chat_id) {
            Ok(handle) => {
                let host = self.clone();
                self.spawn_worker(async move { host.drain_commands(&handle).await });
            }
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "moved chat: open failed");
            }
        }
    }
}
