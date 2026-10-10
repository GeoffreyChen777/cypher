//! Attachment staging (use-attachments.ts).

use super::*;

impl Composer {
    /// Staged attachments for the chat the composer is showing.
    pub(super) fn staged(&self) -> &[StagedAttachment] {
        self.attachments
            .get(&self.current_key)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub(super) fn add_staged(&mut self, staged: Vec<StagedAttachment>, cx: &mut Context<Self>) {
        if staged.is_empty() {
            return;
        }
        self.attachments
            .entry(self.current_key.clone())
            .or_default()
            .extend(staged);
        cx.notify();
    }

    /// Stage files (picker / drop / pasted paths): images preview as
    /// thumbnails, anything else as a file tile. Folders, read failures, and
    /// oversize files surface in the failure notice.
    pub fn add_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let mut staged = Vec::new();
        for path in &paths {
            match attachments::stage_file(path) {
                Ok(att) => staged.push(att),
                Err(message) => {
                    self.failure = Some(message.into());
                    cx.notify();
                }
            }
        }
        self.add_staged(staged, cx);
    }

    pub(super) fn remove_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(list) = self.attachments.get_mut(&self.current_key) {
            list.retain(|a| a.id != id);
            if list.is_empty() {
                self.attachments.remove(&self.current_key);
            }
        }
        cx.notify();
    }

    /// Drop a deleted chat's per-chat composer state — staged attachments hold
    /// raw image bytes, and a deleted chat's stage could never be sent again.
    pub fn purge_chat(&mut self, chat_id: &str) {
        self.attachments.remove(chat_id);
    }

    /// The current draft text (promotion handoff): the live input text, or the
    /// stashed draft when the input is empty.
    pub fn current_draft(&self, cx: &App) -> String {
        let text = self.input.read(cx).text().to_string();
        if text.is_empty() {
            self.drafts
                .get(&self.current_key)
                .cloned()
                .unwrap_or_default()
        } else {
            text
        }
    }

    /// The staged-but-unsent attachments for the current chat (promotion
    /// handoff).
    pub fn staged_attachments(&self) -> Vec<StagedAttachment> {
        self.attachments
            .get(&self.current_key)
            .cloned()
            .unwrap_or_default()
    }

    /// Seed a draft for `chat_id` (promotion handoff). Handles both the
    /// already-selected case (set the live input) and the not-yet-swapped case
    /// (stash under the chat key — the swap picks it up).
    pub fn seed_draft(&mut self, chat_id: &str, text: String, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        if self.current_key == chat_id {
            self.input.update(cx, |input, cx| input.set_text(text, cx));
        } else {
            self.drafts.insert(chat_id.to_string(), text);
        }
        cx.notify();
    }

    /// Seed staged attachments for `chat_id` (promotion handoff).
    pub fn seed_attachments(
        &mut self,
        chat_id: &str,
        staged: Vec<StagedAttachment>,
        cx: &mut Context<Self>,
    ) {
        if staged.is_empty() {
            return;
        }
        self.attachments
            .entry(chat_id.to_string())
            .or_default()
            .extend(staged);
        cx.notify();
    }
}
