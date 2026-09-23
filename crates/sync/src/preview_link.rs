//! Bounded, disposable native preview state. No disk, command, or HTTP access.
use crate::stream_preview as wire;
use cypher_doc::{MessagePart, PreviewCoverage};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Notify;

#[derive(Clone)]
pub struct PreviewOptions {
    pub publisher_token: Option<String>,
}
impl std::fmt::Debug for PreviewOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewOptions")
            .field("publisher", &self.publisher_token.is_some())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewView {
    pub coverage: PreviewCoverage,
    pub text: String,
    pub interrupted: bool,
}
#[derive(Clone)]
struct Source {
    segment: String,
    text: String,
    complete: bool,
}
#[derive(Default)]
struct State {
    run: String,
    source: Option<Source>,
    ready: bool,
    starting: bool,
    starting_at: Option<std::time::Instant>,
    grant: Option<PreviewCoverage>,
    revision: u64,
    sent: Option<(u64, String)>,
    snapshot: bool,
    view: Option<PreviewView>,
    view_received_at: Option<std::time::Instant>,
    authorized: bool,
    awaiting_snapshot: bool,
    resume_requested: bool,
    controls: VecDeque<Vec<u8>>,
}

pub struct PreviewLink {
    chat: String,
    options: Mutex<PreviewOptions>,
    state: Mutex<State>,
    pub notify: Arc<Notify>,
    changed: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
impl PreviewLink {
    pub fn new(chat: impl Into<String>, options: PreviewOptions) -> Arc<Self> {
        Arc::new(Self {
            chat: chat.into(),
            options: Mutex::new(options),
            state: Mutex::new(State::default()),
            notify: Arc::new(Notify::new()),
            changed: Mutex::new(None),
        })
    }
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn options(&self) -> PreviewOptions {
        self.options
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn is_publisher(&self) -> bool {
        self.options
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .publisher_token
            .is_some()
    }
    pub fn set_publisher(&self, token: Option<String>) {
        self.options
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .publisher_token = token;
        self.disconnected(); // Caller redials the same ChatClient; pending durable batches survive.
    }
    pub fn on_change(&self, callback: Arc<dyn Fn() + Send + Sync>) {
        *self.changed.lock().unwrap_or_else(|e| e.into_inner()) = Some(callback);
    }
    fn changed(&self) {
        let callback = self
            .changed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(callback) = callback {
            callback();
        }
    }
    pub fn set_run(&self, run: &str) {
        let mut s = self.lock();
        if s.run != run {
            s.run = run.into();
            s.source = None;
            s.starting = false;
        }
    }
    /// Called before the writer's commit. Only pure text gets a preview in v1;
    /// a tool/input boundary durably retires the preceding text preview.
    pub fn stage(
        &self,
        segment: &str,
        parts: &[MessagePart],
        complete: bool,
    ) -> Option<PreviewCoverage> {
        if !self.is_publisher() {
            return None;
        }
        let text = if parts.len() == 1 {
            match &parts[0] {
                MessagePart::Text { text, .. } if text.len() <= wire::MAX_TEXT_BYTES => {
                    Some(text.clone())
                }
                _ => None,
            }
        } else {
            None
        };
        let mut s = self.lock();
        if s.run.is_empty() {
            return None;
        }
        let compatible = s
            .grant
            .as_ref()
            .is_some_and(|g| g.run_id == s.run && g.segment_id == segment);
        let Some(text) = text else {
            if compatible {
                if let Some(source) = s.source.as_mut() {
                    source.complete = true;
                }
                let mut coverage = s.grant.clone()?;
                coverage.revision = s.revision;
                coverage.complete = true;
                drop(s);
                self.notify.notify_one();
                return Some(coverage);
            }
            s.source = None;
            return None;
        };
        let changed = s
            .source
            .as_ref()
            .is_none_or(|old| old.segment != segment || old.text != text);
        if changed && compatible {
            s.revision = s
                .revision
                .checked_add(1)
                .filter(|n| *n <= 9_007_199_254_740_991)?;
        }
        if !compatible && complete {
            s.source = None;
            return None;
        }
        if s.source.as_ref().is_some_and(|old| old.segment != segment) {
            s.starting = false;
        }
        s.source = Some(Source {
            segment: segment.into(),
            text,
            complete,
        });
        let coverage = compatible.then(|| {
            let mut g = s.grant.clone().unwrap();
            g.revision = s.revision;
            g.complete = complete;
            g
        });
        drop(s);
        self.notify.notify_one();
        coverage
    }
    pub fn disconnected(&self) {
        let mut s = self.lock();
        s.ready = false;
        s.starting = false;
        s.grant = None;
        s.authorized = false;
        s.controls.clear();
        s.sent = None;
        if let Some(v) = s.view.as_mut() {
            v.interrupted = true;
        }
        drop(s);
        self.changed();
    }
    pub fn send_failed(&self) {
        let mut s = self.lock();
        s.snapshot = true;
        s.sent = None;
    }
    /// One-second actor tick: retry disposable traffic independently of durable
    /// ACK/probe clocks. It never runs an HTTP sync or writes the document.
    pub fn tick(&self, cursor: u64) {
        let mut s = self.lock();
        let mut changed = false;
        if let Some(age) = s.view_received_at.map(|t| t.elapsed().as_secs()) {
            if age >= 300 && s.view.is_some() {
                s.view = None;
                changed = true;
            } else if age >= 60
                && let Some(view) = s.view.as_mut()
                && !view.interrupted
            {
                view.interrupted = true;
                changed = true;
            }
        }
        if s.starting_at.is_some_and(|at| at.elapsed().as_secs() >= 5) {
            s.starting = false;
            s.starting_at = None;
        }
        if !self.is_publisher()
            && s.authorized
            && s.awaiting_snapshot
            && let Some(g) = s.grant.clone()
        {
            let revision = s.view.as_ref().map_or(0, |v| v.coverage.revision);
            if !s.controls.iter().any(|b| b[0] == wire::RESUME) {
                let bytes = wire::encode(wire::RESUME, &json!({"chatId":self.chat,"runId":g.run_id,"segmentId":g.segment_id,"epoch":g.epoch,"revision":revision,"baseSeq":cursor}), "").unwrap();
                Self::queue(&mut s, bytes);
            }
        }
        drop(s);
        self.notify.notify_one();
        if changed {
            self.changed();
        }
    }
    pub fn view(&self) -> Option<PreviewView> {
        self.lock().view.clone()
    }
    pub fn rejected(&self, code: &str) {
        if !self.is_publisher() {
            return;
        } // Late receipt/Resume errors cannot revoke a newer grant.
        let mut s = self.lock();
        match code {
            "preview_snapshot_required" => s.snapshot = true,
            "preview_stale_grant" | "preview_stale_revision" => {
                s.grant = None;
                s.starting = false;
            }
            "bad_preview_receipt" => return,
            _ => {
                s.ready = false;
                s.grant = None;
                s.starting = false;
            }
        }
        drop(s);
        self.notify.notify_one();
    }

    pub fn receive(&self, bytes: &[u8]) {
        let Some(frame) = wire::decode(bytes) else {
            return;
        };
        if frame.header["chatId"] != self.chat {
            return;
        }
        let mut s = self.lock();
        if frame.kind == wire::STATE {
            s.controls.retain(|b| b[0] == wire::RECEIPT);
            let mode = frame.header["mode"].as_str().unwrap_or("");
            s.ready = mode != "legacy";
            s.starting = false;
            s.sent = None;
            if mode == "preview" {
                let grant = PreviewCoverage {
                    run_id: frame.header["runId"].as_str().unwrap().into(),
                    segment_id: frame.header["segmentId"].as_str().unwrap().into(),
                    epoch: frame.header["epoch"].as_str().unwrap().into(),
                    revision: 0,
                    complete: false,
                };
                s.grant = Some(grant);
                s.revision = 0;
                s.snapshot = true;
                s.authorized = true;
                s.awaiting_snapshot = true;
                s.resume_requested = false;
                s.view = None;
            } else {
                s.grant = None;
                s.authorized = false;
                if let Some(v) = s.view.as_mut() {
                    v.interrupted = true;
                }
            }
        } else {
            let Some(grant) = s.grant.clone() else { return };
            if frame.header["epoch"] != grant.epoch
                || frame.header["runId"] != grant.run_id
                || frame.header["segmentId"] != grant.segment_id
                || !s.authorized
            {
                return;
            }
            if self.is_publisher() {
                if frame.kind == wire::RESUME {
                    s.snapshot = true;
                }
            } else if matches!(frame.kind, wire::SNAPSHOT | wire::DELTA | wire::FINISHED) {
                let revision = frame.header["revision"].as_f64().unwrap() as u64;
                let old = s.view.as_ref().map(|v| v.coverage.revision);
                if frame.kind == wire::FINISHED {
                    // A finish hint never clears text or declares durable completion.
                } else if old.is_some_and(|old| revision < old)
                    || (frame.kind == wire::DELTA && old == Some(revision))
                {
                    // Duplicate/late: receipt still releases the sender's flow credit.
                } else if frame.kind == wire::DELTA
                    && (s.awaiting_snapshot
                        || old != frame.header["prevRevision"].as_f64().map(|n| n as u64))
                {
                    s.awaiting_snapshot = true;
                    let mut header = frame.header.clone();
                    header.as_object_mut().unwrap().remove("prevRevision");
                    if !s.resume_requested {
                        Self::queue(&mut s, wire::encode(wire::RESUME, &header, "").unwrap());
                        s.resume_requested = true;
                    }
                } else {
                    let text = std::str::from_utf8(&frame.payload).unwrap();
                    let text = if frame.kind == wire::DELTA {
                        format!("{}{text}", s.view.as_ref().map_or("", |v| v.text.as_str()))
                    } else {
                        text.to_owned()
                    };
                    if text.len() > wire::MAX_TEXT_BYTES {
                        s.authorized = false;
                        return;
                    }
                    let mut coverage = grant;
                    coverage.revision = revision;
                    s.view = Some(PreviewView {
                        coverage,
                        text,
                        interrupted: false,
                    });
                    s.view_received_at = Some(std::time::Instant::now());
                    s.awaiting_snapshot = false;
                    s.resume_requested = false;
                }
                let mut header = frame.header;
                header.as_object_mut().unwrap().remove("prevRevision");
                header.as_object_mut().unwrap().remove("batchId");
                Self::queue(&mut s, wire::encode(wire::RECEIPT, &header, "").unwrap());
            }
        }
        drop(s);
        self.notify.notify_one();
        self.changed();
    }
    fn queue(s: &mut State, bytes: Vec<u8>) {
        // Highest receipt subsumes previous receipts for this epoch. Preserve Resume.
        if bytes[0] == wire::RECEIPT {
            let frame = wire::decode(&bytes).unwrap();
            let epoch = &frame.header["epoch"];
            let revision = frame.header["revision"].as_f64().unwrap();
            if s.controls.iter().any(|b| {
                b[0] == wire::RECEIPT
                    && wire::decode(b).is_some_and(|old| {
                        old.header["epoch"] == *epoch
                            && old.header["revision"].as_f64().unwrap() >= revision
                    })
            }) {
                return;
            }
            s.controls.retain(|b| {
                b[0] != wire::RECEIPT
                    || wire::decode(b).is_none_or(|old| old.header["epoch"] != *epoch)
            });
        }
        if s.controls.len() < 4 {
            s.controls.push_back(bytes);
        }
    }
    pub fn next_frame(&self, cursor: u64) -> Option<Vec<u8>> {
        let mut s = self.lock();
        if let Some(bytes) = s.controls.pop_front() {
            if !s.controls.is_empty() {
                self.notify.notify_one();
            }
            return Some(bytes);
        }
        if !self.is_publisher() || !s.ready {
            return None;
        }
        let source = s.source.clone()?;
        let compatible = s
            .grant
            .as_ref()
            .is_some_and(|g| g.run_id == s.run && g.segment_id == source.segment);
        if !compatible {
            if s.starting || source.complete {
                return None;
            }
            s.starting = true;
            s.starting_at = Some(std::time::Instant::now());
            return wire::encode(
                wire::START,
                &json!({"chatId":self.chat,"runId":s.run,"segmentId":source.segment}),
                "",
            );
        }
        let g = s.grant.as_ref().unwrap();
        let mut header = json!({"chatId":self.chat,"runId":g.run_id,"segmentId":g.segment_id,"epoch":g.epoch,"revision":s.revision,"baseSeq":cursor});
        if s.snapshot || s.sent.as_ref().is_none_or(|(r, _)| *r != s.revision) {
            let (kind, text) = if !s.snapshot
                && s.sent
                    .as_ref()
                    .is_some_and(|(r, t)| *r + 1 == s.revision && source.text.starts_with(t))
            {
                let (revision, old) = s.sent.as_ref().unwrap();
                header["prevRevision"] = (*revision).into();
                (wire::DELTA, source.text[old.len()..].to_owned())
            } else {
                (wire::SNAPSHOT, source.text.clone())
            };
            let bytes = wire::encode(kind, &header, &text)?;
            s.snapshot = false;
            s.sent = Some((s.revision, source.text));
            return Some(bytes);
        }
        // Completion is proved by imported durable coverage. Do not attach a
        // speculative batch ID to Finished: the current outbox has no atomic
        // commit-to-batch mapping (P2 prerequisite).
        None
    }
}

/// Projection only: never call this to prepare a checkpoint, notification or command.
pub fn overlay(
    entries: &mut Vec<cypher_doc::SessionMessageEntry>,
    view: Option<PreviewView>,
    durable: Option<PreviewCoverage>,
) {
    use cypher_doc::{MessageRole, MessageStatus, SessionMessageEntry};
    let Some(view) = view else { return };
    if view.text.is_empty() {
        return;
    }
    let c = &view.coverage;
    let materialized = entries
        .iter()
        .any(|e| e.id == c.segment_id && e.role == MessageRole::Assistant);
    if materialized
        && durable.as_ref().is_some_and(|d| {
            d.run_id == c.run_id && d.segment_id == c.segment_id && d.epoch != c.epoch
        })
    {
        return; // Never cover another durable epoch with this cached preview.
    }
    let covered = materialized
        && durable.as_ref().is_some_and(|d| {
            d.epoch == c.epoch
                && d.run_id == c.run_id
                && d.segment_id == c.segment_id
                && d.revision >= c.revision
        });
    if covered && (view.interrupted || durable.as_ref().is_some_and(|d| d.complete)) {
        return;
    }
    let notice = if view.interrupted {
        "> 暂存预览 · 尚未确认同步\n\n"
    } else {
        "> 实时预览 · 本段结果待确认\n\n"
    };
    let parts = vec![MessagePart::Text {
        id: "preview-text".into(),
        text: format!("{notice}{}", view.text),
        agent_text: None,
    }];
    if let Some(entry) = entries.iter_mut().find(|e| e.id == c.segment_id) {
        if entry.role != MessageRole::Assistant
            || !entry
                .parts
                .iter()
                .all(|p| matches!(p, MessagePart::Text { .. }))
        {
            return;
        }
        if view.interrupted {
            entry.parts.push(MessagePart::Text {
                id: "preview-status".into(),
                text: "\n\n> 暂存预览 · 正在显示已同步内容，等待预览确认".into(),
                agent_text: None,
            });
        } else if covered {
            let text = entry
                .parts
                .iter()
                .filter_map(|p| match p {
                    MessagePart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            entry.parts = vec![MessagePart::Text {
                id: "preview-text".into(),
                text: format!("{notice}{text}"),
                agent_text: None,
            }];
        } else {
            entry.parts = parts;
            entry.status = Some(MessageStatus::Streaming);
        }
    } else {
        entries.push(SessionMessageEntry {
            id: c.segment_id.clone(),
            role: MessageRole::Assistant,
            parts,
            created_at: entries.last().map_or(0, |e| e.created_at + 1),
            device_id: String::new(),
            status: Some(MessageStatus::Streaming),
            continuation_of: None,
            completed_at: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_doc::{MessageStatus, SegmentWriter, SessionDoc};
    fn publisher() -> Arc<PreviewLink> {
        PreviewLink::new(
            "c",
            PreviewOptions {
                publisher_token: Some("a".repeat(64)),
            },
        )
    }
    fn state(link: &PreviewLink, mode: &str) {
        let header = if mode == "preview" {
            json!({"chatId":"c","mode":mode,"runId":"r","segmentId":"s","epoch":"e"})
        } else {
            json!({"chatId":"c","mode":mode})
        };
        link.receive(&wire::encode(wire::STATE, &header, "").unwrap());
    }
    fn text(value: &str) -> Vec<MessagePart> {
        vec![MessagePart::Text {
            id: "t".into(),
            text: value.into(),
            agent_text: None,
        }]
    }
    #[test]
    fn shared_reducer_vectors() {
        let link = PreviewLink::new(
            "c",
            PreviewOptions {
                publisher_token: None,
            },
        );
        let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../../edge/src/fixtures/preview-reducer-v1.json"
        ))
        .unwrap();
        for c in cases {
            link.receive(
                &wire::encode(
                    c["kind"].as_u64().unwrap() as u8,
                    &c["header"],
                    c["text"].as_str().unwrap(),
                )
                .unwrap(),
            );
            let view = link.view();
            assert_eq!(
                view.as_ref().map_or("", |v| &v.text),
                c["display"].as_str().unwrap()
            );
            assert_eq!(
                view.is_some_and(|v| v.interrupted),
                c["interrupted"].as_bool().unwrap()
            );
            let mut replies = vec![];
            while let Some(frame) = link.next_frame(0) {
                replies.push(frame[0]);
                assert!(replies.len() <= 4);
            }
            assert_eq!(serde_json::to_value(replies).unwrap(), c["replies"]);
        }
    }
    #[test]
    fn coalesced_updates_use_snapshot_not_a_delta_with_missing_revisions() {
        let p = publisher();
        p.set_run("r");
        state(&p, "ready");
        p.stage("s", &text("a"), false);
        assert_eq!(p.next_frame(0).unwrap()[0], wire::START);
        state(&p, "preview");
        assert_eq!(p.next_frame(0).unwrap()[0], wire::SNAPSHOT);
        p.stage("s", &text("ab"), false);
        assert_eq!(p.next_frame(0).unwrap()[0], wire::DELTA);
        p.stage("s", &text("abc"), false);
        p.stage("s", &text("abcd"), false);
        let frame = wire::decode(&p.next_frame(0).unwrap()).unwrap();
        assert_eq!(frame.kind, wire::SNAPSHOT);
        assert_eq!(frame.payload, b"abcd");
    }
    #[test]
    fn role_change_requires_a_fresh_grant_and_redacts_credentials() {
        let p = PreviewLink::new(
            "c",
            PreviewOptions {
                publisher_token: None,
            },
        );
        state(&p, "preview");
        let secret = "b".repeat(64);
        p.set_publisher(Some(secret.clone()));
        assert!(p.lock().grant.is_none());
        assert!(!p.lock().authorized);
        assert!(!format!("{:?}", p.options()).contains(&secret));
        p.set_run("r");
        p.stage("s", &text("hello"), false);
        assert!(p.next_frame(0).is_none());
        state(&p, "ready");
        assert_eq!(p.next_frame(0).unwrap()[0], wire::START);
        p.set_publisher(None);
        assert!(p.next_frame(0).is_none());
    }
    #[test]
    fn coverage_is_atomic_with_text_and_survives_checkpoint_rebuild() {
        let doc = SessionDoc::init("c").unwrap();
        let p = publisher();
        p.set_run("r");
        state(&p, "ready");
        let hook = p.clone();
        doc.set_preview_hook(Arc::new(move |id, parts, done| hook.stage(id, parts, done)));
        let mut writer = SegmentWriter::begin(&doc, "s", "host", 1).unwrap();
        writer.sync(&text("a")).unwrap();
        p.next_frame(0);
        state(&p, "preview");
        p.next_frame(0);
        let updates = Arc::new(Mutex::new(vec![]));
        let captured = updates.clone();
        let _sub = doc.doc().subscribe_local_update(Box::new(move |b| {
            captured.lock().unwrap().push(b.clone());
            true
        }));
        let reader = SessionDoc::from_doc(loro::LoroDoc::new());
        reader
            .doc()
            .import(&doc.export_snapshot().unwrap())
            .unwrap();
        writer.sync(&text("ab")).unwrap();
        writer
            .finish(&text("abc"), MessageStatus::Complete)
            .unwrap();
        for bytes in updates.lock().unwrap().iter() {
            reader.doc().import(bytes).unwrap();
            if let Some(c) = reader.preview_coverage() {
                let value = if c.revision == 1 { "ab" } else { "abc" };
                assert_eq!(reader.read_entries().unwrap()[0].parts, text(value));
                if c.complete {
                    assert_eq!(
                        reader.read_entries().unwrap()[0].status,
                        Some(MessageStatus::Complete)
                    );
                }
            }
        }
        let rebuilt = cypher_doc::rebuild_thin_doc(&reader).unwrap();
        assert_eq!(rebuilt.doc.preview_coverage(), reader.preview_coverage());
        p.next_frame(0);
        assert!(p.next_frame(0).is_none());
    }
    #[test]
    fn only_matching_imported_coverage_retires_preview_and_never_mutates_doc() {
        let link = PreviewLink::new(
            "c",
            PreviewOptions {
                publisher_token: None,
            },
        );
        state(&link, "preview");
        link.receive(&wire::encode(wire::SNAPSHOT, &json!({"chatId":"c","runId":"r","segmentId":"s","epoch":"e","revision":2,"baseSeq":99}), "preview").unwrap());
        let doc = SessionDoc::init("c").unwrap();
        let mut entries = doc.read_entries().unwrap();
        let before = doc.export_snapshot().unwrap();
        let view = link.view().unwrap();
        let mut wrong = view.coverage.clone();
        wrong.epoch = "old".into();
        overlay(&mut entries, Some(view.clone()), Some(wrong));
        assert_eq!(entries.len(), 1);
        entries.clear();
        overlay(
            &mut entries,
            Some(view.clone()),
            Some(view.coverage.clone()),
        );
        assert_eq!(
            entries.len(),
            1,
            "a marker without a materialized entry is not coverage"
        );
        entries[0].parts = text("durable");
        let expected = entries.clone();
        let mut complete = view.coverage.clone();
        complete.complete = true;
        overlay(&mut entries, Some(view), Some(complete));
        assert_eq!(entries, expected);
        assert_eq!(doc.export_snapshot().unwrap(), before);
        assert!(doc.read_commands().unwrap().is_empty());
    }
}
