//! Edge-side payloads: lazy tool-output blobs and the development stream
//! preview of a live run.

use super::*;

impl DocHost {
    /// Fetch a sidecar blob by its doc-resident ref (`{chatId}/{partId}` or
    /// `…​.diff`) — the UI's lazy "Show full output" path, served over RPC
    /// because the UI crate has no HTTP client or edge bearer.
    pub async fn fetch_tool_blob(&self, blob_ref: &str) -> Result<String, EngineError> {
        // The `{chatId}/{partId}[.diff]` shape of doc-resident refs; anything
        // else is a forged ref.
        let valid = blob_ref.split_once('/').is_some_and(|(chat, part)| {
            !chat.is_empty()
                && chat.len() <= 128
                && chat
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                && !part.is_empty()
                && part.len() <= 200
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:#~-".contains(&b))
        });
        if !valid {
            return Err(EngineError::Other(format!("bad blob ref: {blob_ref}")));
        }
        let Some(edge) = self.inner.config.edge.clone() else {
            return Err(EngineError::Other("offline: no edge configured".into()));
        };
        let Some(bearer) = edge.bearer().await else {
            return Err(EngineError::Other("signed out".into()));
        };
        // `valid` above guarantees the split; re-split to encode the part
        // segment for transport (PART_RE allows `#`, which a raw URL would
        // truncate as a fragment and silently collide).
        let (chat, part) = blob_ref.split_once('/').expect("validated above");
        let url = format!(
            "{}/blob/{}/{}",
            edge.url.trim_end_matches('/'),
            chat,
            encode_part_segment(part)
        );
        let res = self
            .inner
            .http
            .get(&url)
            .bearer_auth(&bearer)
            .send()
            .await
            .map_err(|e| EngineError::Other(format!("sidecar fetch failed: {e}")))?;
        if !res.status().is_success() {
            return Err(EngineError::Other(format!(
                "sidecar fetch: HTTP {}",
                res.status().as_u16()
            )));
        }
        res.text()
            .await
            .map_err(|e| EngineError::Other(format!("sidecar body read failed: {e}")))
    }
}

impl DocHost {
    pub(crate) fn preview_run(&self, chat: &str, run: &str) {
        let Some(handle) = lock(&self.inner.handles).get(chat).cloned() else {
            return;
        };
        let Some(preview) = handle.preview.get().cloned() else {
            return;
        };
        preview.set_run(run);
        if !self.preview_is_host(chat) {
            if preview.options().publisher_token.is_some() {
                preview.set_publisher(None);
                if let Some(client) = lock(&handle.chat2).as_ref() {
                    client.redial();
                }
            }
            return;
        }
        // WatchDocMessages may open a newborn chat BEFORE CreateChat inserts
        // its registry row. Upgrade only once local ownership is known, keeping
        // the same ChatClient/outbox rather than discarding pending updates.
        if preview.options().publisher_token.is_none()
            && let Some(token) = self
                .inner
                .config
                .edge
                .as_ref()
                .and_then(|e| e.preview.as_ref())
                .and_then(|p| p.publisher_token.clone())
        {
            preview.set_publisher(Some(token));
            let hook = preview.clone();
            handle
                .doc
                .set_preview_hook(Arc::new(move |entry, parts, done| {
                    hook.stage(entry, parts, done)
                }));
            if let Some(client) = lock(&handle.chat2).as_ref() {
                client.redial();
            }
        }
    }
}

impl DocHost {
    pub(super) fn preview_is_host(&self, chat: &str) -> bool {
        // Unlike legacy command claim-on-first-use, preview publishing fails
        // closed for missing/unreadable registry rows in a workspace runtime.
        self.workspace().is_none_or(|ws| {
            ws.chat(chat)
                .ok()
                .flatten()
                .is_some_and(|row| row.device_id == self.inner.config.device_id)
        })
    }
}
