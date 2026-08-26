//! Durable command ledger — port of `packages/session-doc/src/commands.ts`.
//!
//! Rules (verbatim from zeron's design):
//! 1. Each device inserts only its own entries; entries are append-only and immutable.
//! 2. The chat's HOST is the sole writer of command outcomes; a composer may only set
//!    `cancelled` on its own still-pending entries.
//! 3. Evaluation (`evaluate_command`, pure): processed-id dedupe → Skip; expired TTL → Expired;
//!    a newer command of the same kind supersedes steer/interrupt; an interrupt whose
//!    `based_on.turn_id` is already past → Superseded; otherwise Execute.

use serde::{Deserialize, Serialize};

use cypher_proto::{PendingAttachment, RunRequest, UserInputAnswer};

use crate::constants::COMMAND_DEFAULT_TTL_MS;

/// How long a Run carrying pending attachments may wait for its uploads to be
/// sealed before the host expires it (aligned with the engine's staging-dir
/// TTL, so a wedged upload can't leave the command Pending forever). Bounded
/// wait is a product requirement: the durable queue must eventually resolve.
pub const ATTACHMENT_SEAL_GRACE_MS: i64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionCommandKind {
    Run,
    Steer,
    Interrupt,
    RespondInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionCommandStatus {
    Pending,
    Applied,
    Rejected,
    Expired,
    Superseded,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SessionCommandPayload {
    #[serde(rename_all = "camelCase")]
    Run {
        request: RunRequest,
        /// Client-minted message id for the optimistic user entry (dedup key).
        message_id: String,
        /// Optional EFFECTIVE harness prompt override (the Comment feature):
        /// the doc user entry keeps `request.prompt` (the visible truth)
        /// while the agent receives this augmented prompt when present.
        /// Additive + serde-defaulted for wire compat (old payloads stay
        /// byte-identical).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_prompt: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Steer {
        prompt: String,
        message_id: Option<String>,
        /// See [`SessionCommandPayload::Run::agent_prompt`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_prompt: Option<String>,
    },
    Interrupt {},
    #[serde(rename_all = "camelCase")]
    RespondInput {
        request_id: String,
        answers: Vec<UserInputAnswer>,
    },
}

impl SessionCommandPayload {
    pub fn kind(&self) -> SessionCommandKind {
        match self {
            SessionCommandPayload::Run { .. } => SessionCommandKind::Run,
            SessionCommandPayload::Steer { .. } => SessionCommandKind::Steer,
            SessionCommandPayload::Interrupt {} => SessionCommandKind::Interrupt,
            SessionCommandPayload::RespondInput { .. } => SessionCommandKind::RespondInput,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandBasedOn {
    pub turn_id: Option<String>,
    pub frontier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCommandEntry {
    pub id: String,
    pub payload: SessionCommandPayload,
    pub issued_by: String,
    /// Epoch millis.
    pub issued_at: i64,
    #[serde(default)]
    pub based_on: Option<CommandBasedOn>,
    /// Epoch millis; defaults to issued_at + COMMAND_DEFAULT_TTL_MS when absent.
    #[serde(default)]
    pub expires_at: Option<i64>,
    pub status: SessionCommandStatus,
    #[serde(default)]
    pub resolution: Option<String>,
    /// Epoch millis of the ORIGINAL user send — the first attempt's
    /// `issued_at`. Preserved across retries (`retry_command` copies it from
    /// the failed attempt) so the UI can show the true send time/order even
    /// after a reissue. Additive + serde-defaulted: legacy entries written
    /// before this field existed stay byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<i64>,
}

impl SessionCommandEntry {
    pub fn kind(&self) -> SessionCommandKind {
        self.payload.kind()
    }

    pub fn effective_expiry(&self) -> i64 {
        self.expires_at
            .unwrap_or(self.issued_at + COMMAND_DEFAULT_TTL_MS)
    }
}

/// Rule 2: only the composer that issued a still-pending command may cancel it.
pub fn can_composer_cancel(entry: &SessionCommandEntry, device_id: &str) -> bool {
    entry.status == SessionCommandStatus::Pending && entry.issued_by == device_id
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandDisposition {
    /// Already in the processed ledger — do nothing (idempotence).
    Skip,
    /// Mark expired.
    Expired,
    /// Mark superseded.
    Superseded,
    /// A Run whose pending attachments are not all sealed yet: hold WITHOUT
    /// marking processed; the next seal commit re-triggers the drain. Never
    /// permanent — past the seal grace window it degrades to [`Expired`].
    WaitForAttachments,
    /// Mark processed BEFORE executing, then execute.
    Execute,
}

/// Context the host evaluates a pending command against.
pub struct EvaluationContext<'a> {
    /// Processed-command ledger membership test.
    pub is_processed: &'a dyn Fn(&str) -> bool,
    /// Current wall clock, epoch millis.
    pub now_ms: i64,
    /// All command entries in doc order (used to find newer same-kind entries).
    pub entries: &'a [SessionCommandEntry],
    /// The id of the turn currently (or most recently) running, if any.
    pub current_turn_id: Option<&'a str>,
    /// True when the given turn id has already completed.
    pub turn_is_past: &'a dyn Fn(&str) -> bool,
    /// Resolve a pending-upload id to its sealed final path (the doc's
    /// `sealedAttachments` map), or `None` when not sealed yet. `None` for
    /// any pending id is what gates [`CommandDisposition::WaitForAttachments`].
    pub sealed_attachment_path: &'a dyn Fn(&str) -> Option<String>,
}

/// The pending-attachment descriptors of a Run command, if it carries any.
pub fn run_pending_attachments(entry: &SessionCommandEntry) -> Option<&[PendingAttachment]> {
    match &entry.payload {
        SessionCommandPayload::Run { request, .. } => {
            let pending = &request.pending_attachments;
            (!pending.is_empty()).then_some(pending.as_slice())
        }
        _ => None,
    }
}

/// Rule 3 — pure evaluation of a single pending command.
pub fn evaluate_command(
    entry: &SessionCommandEntry,
    cx: &EvaluationContext<'_>,
) -> CommandDisposition {
    if (cx.is_processed)(&entry.id) {
        return CommandDisposition::Skip;
    }
    if cx.now_ms >= entry.effective_expiry() {
        return CommandDisposition::Expired;
    }
    // A Run whose pending attachments are not all sealed yet holds WITHOUT
    // being marked processed (the seal commit re-triggers the drain), but
    // only within the attachment grace window — past it the command Expires
    // instead of sitting Pending forever. Pending ids never execute: the
    // request only dispatches with sealed final paths.
    if let Some(pending) = run_pending_attachments(entry) {
        let all_sealed = pending
            .iter()
            .all(|p| (cx.sealed_attachment_path)(&p.upload_id).is_some());
        if !all_sealed {
            if cx.now_ms >= entry.issued_at + ATTACHMENT_SEAL_GRACE_MS {
                return CommandDisposition::Expired;
            }
            return CommandDisposition::WaitForAttachments;
        }
    }
    // A newer pending command of the same kind supersedes steer/interrupt.
    let kind = entry.kind();
    if matches!(
        kind,
        SessionCommandKind::Steer | SessionCommandKind::Interrupt
    ) {
        let has_newer_same_kind = cx.entries.iter().any(|other| {
            other.id != entry.id
                && other.kind() == kind
                && other.status == SessionCommandStatus::Pending
                && other.issued_at > entry.issued_at
        });
        if has_newer_same_kind {
            return CommandDisposition::Superseded;
        }
    }
    // An interrupt aimed at a turn that already finished is moot.
    if kind == SessionCommandKind::Interrupt
        && let Some(based_on) = &entry.based_on
        && let Some(turn_id) = &based_on.turn_id
    {
        let is_current = cx.current_turn_id == Some(turn_id.as_str());
        if !is_current && (cx.turn_is_past)(turn_id) {
            return CommandDisposition::Superseded;
        }
    }
    CommandDisposition::Execute
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, payload: SessionCommandPayload, issued_at: i64) -> SessionCommandEntry {
        SessionCommandEntry {
            id: id.into(),
            payload,
            issued_by: "device-a".into(),
            issued_at,
            based_on: None,
            expires_at: None,
            status: SessionCommandStatus::Pending,
            resolution: None,
            sent_at: None,
        }
    }

    fn steer(id: &str, issued_at: i64) -> SessionCommandEntry {
        entry(
            id,
            SessionCommandPayload::Steer {
                prompt: "go".into(),
                message_id: None,
                agent_prompt: None,
            },
            issued_at,
        )
    }

    fn eval_cx<'a>(
        entries: &'a [SessionCommandEntry],
        processed: &'a dyn Fn(&str) -> bool,
        turn_is_past: &'a dyn Fn(&str) -> bool,
        now_ms: i64,
        current_turn_id: Option<&'a str>,
    ) -> EvaluationContext<'a> {
        EvaluationContext {
            is_processed: processed,
            now_ms,
            entries,
            current_turn_id,
            turn_is_past,
            sealed_attachment_path: &|_| None,
        }
    }

    const NEVER: fn(&str) -> bool = |_| false;

    #[test]
    fn processed_commands_are_skipped() {
        let e = steer("c1", 1_000);
        let entries = vec![e.clone()];
        let processed = |id: &str| id == "c1";
        let cx = eval_cx(&entries, &processed, &NEVER, 2_000, None);
        assert_eq!(evaluate_command(&e, &cx), CommandDisposition::Skip);
    }

    #[test]
    fn expired_commands_are_expired() {
        let e = steer("c1", 0);
        let entries = vec![e.clone()];
        let cx = eval_cx(&entries, &NEVER, &NEVER, COMMAND_DEFAULT_TTL_MS + 1, None);
        assert_eq!(evaluate_command(&e, &cx), CommandDisposition::Expired);
    }

    #[test]
    fn newer_steer_supersedes_older_pending_steer() {
        let older = steer("c1", 1_000);
        let newer = steer("c2", 2_000);
        let entries = vec![older.clone(), newer.clone()];
        let cx1 = eval_cx(&entries, &NEVER, &NEVER, 3_000, None);
        assert_eq!(
            evaluate_command(&older, &cx1),
            CommandDisposition::Superseded
        );
        assert_eq!(evaluate_command(&newer, &cx1), CommandDisposition::Execute);
    }

    #[test]
    fn interrupt_for_past_turn_is_superseded() {
        let mut e = entry("c1", SessionCommandPayload::Interrupt {}, 1_000);
        e.based_on = Some(CommandBasedOn {
            turn_id: Some("turn-1".into()),
            frontier: None,
        });
        let entries = vec![e.clone()];
        let past = |id: &str| id == "turn-1";
        let cx1 = eval_cx(&entries, &NEVER, &past, 2_000, Some("turn-2"));
        assert_eq!(evaluate_command(&e, &cx1), CommandDisposition::Superseded);
        // …but if that turn is still the current one, execute.
        let cx2 = eval_cx(&entries, &NEVER, &past, 2_000, Some("turn-1"));
        assert_eq!(evaluate_command(&e, &cx2), CommandDisposition::Execute);
    }

    #[test]
    fn runs_are_not_superseded_by_newer_runs() {
        // Two queued runs both execute (in order); supersession applies to steer/interrupt only.
        let r1 = entry(
            "r1",
            SessionCommandPayload::Run {
                request: run_request(),
                message_id: "m1".into(),
                agent_prompt: None,
            },
            1_000,
        );
        let r2 = entry(
            "r2",
            SessionCommandPayload::Run {
                request: run_request(),
                message_id: "m2".into(),
                agent_prompt: None,
            },
            2_000,
        );
        let entries = vec![r1.clone(), r2.clone()];
        let cx1 = eval_cx(&entries, &NEVER, &NEVER, 3_000, None);
        assert_eq!(evaluate_command(&r1, &cx1), CommandDisposition::Execute);
        assert_eq!(evaluate_command(&r2, &cx1), CommandDisposition::Execute);
    }

    fn pending_run(upload_id: &str, issued_at: i64) -> SessionCommandEntry {
        entry(
            "r-pending",
            SessionCommandPayload::Run {
                request: RunRequest {
                    prompt: "look at this".into(),
                    harness: None,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    cwd: "/tmp".into(),
                    sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                    attachments: Vec::new(),
                    pending_attachments: vec![cypher_proto::PendingAttachment {
                        upload_id: upload_id.into(),
                        file_name: "photo.png".into(),
                    }],
                    resume: None,
                    worktree: None,
                },
                message_id: "m-p".into(),
                agent_prompt: None,
            },
            issued_at,
        )
    }

    #[test]
    fn run_with_unsealed_pending_attachments_waits_not_executes() {
        let e = pending_run("up-1", 1_000);
        let entries = vec![e.clone()];
        let cx = eval_cx(&entries, &NEVER, &NEVER, 2_000, None);
        assert_eq!(
            evaluate_command(&e, &cx),
            CommandDisposition::WaitForAttachments
        );
        // …but does NOT expire while inside the grace window.
        let inside = eval_cx(
            &entries,
            &NEVER,
            &NEVER,
            1_000 + ATTACHMENT_SEAL_GRACE_MS - 1,
            None,
        );
        assert_eq!(
            evaluate_command(&e, &inside),
            CommandDisposition::WaitForAttachments
        );
    }

    #[test]
    fn run_with_sealed_pending_attachments_executes() {
        let e = pending_run("up-1", 1_000);
        let entries = vec![e.clone()];
        // All pending ids sealed → Execute (the drain resolves them to paths).
        let cx = EvaluationContext {
            is_processed: &NEVER,
            now_ms: 2_000,
            entries: &entries,
            current_turn_id: None,
            turn_is_past: &NEVER,
            sealed_attachment_path: &|id| (id == "up-1").then(|| "/up/1.png".to_string()),
        };
        assert_eq!(evaluate_command(&e, &cx), CommandDisposition::Execute);
        // Partial seal (one of two pending) still waits.
        let mut both = pending_run("up-1", 1_000);
        if let SessionCommandPayload::Run { request, .. } = &mut both.payload {
            request
                .pending_attachments
                .push(cypher_proto::PendingAttachment {
                    upload_id: "up-2".into(),
                    file_name: "b.png".into(),
                });
        }
        let cx = EvaluationContext {
            is_processed: &NEVER,
            now_ms: 2_000,
            entries: &entries,
            current_turn_id: None,
            turn_is_past: &NEVER,
            sealed_attachment_path: &|id| (id == "up-1").then(|| "/up/1.png".to_string()),
        };
        assert_eq!(
            evaluate_command(&both, &cx),
            CommandDisposition::WaitForAttachments
        );
    }

    #[test]
    fn run_past_attachment_grace_expires_instead_of_pending_forever() {
        let e = pending_run("up-1", 1_000);
        let entries = vec![e.clone()];
        let cx = eval_cx(
            &entries,
            &NEVER,
            &NEVER,
            1_000 + ATTACHMENT_SEAL_GRACE_MS,
            None,
        );
        assert_eq!(evaluate_command(&e, &cx), CommandDisposition::Expired);
        // A plain run (no pending attachments) is unaffected by the grace.
        let r = entry(
            "r-plain",
            SessionCommandPayload::Run {
                request: run_request(),
                message_id: "m1".into(),
                agent_prompt: None,
            },
            1_000,
        );
        let entries = vec![r.clone()];
        let cx = eval_cx(
            &entries,
            &NEVER,
            &NEVER,
            1_000 + ATTACHMENT_SEAL_GRACE_MS,
            None,
        );
        assert_eq!(evaluate_command(&r, &cx), CommandDisposition::Execute);
    }

    #[test]
    fn composer_cancel_rules() {
        let e = steer("c1", 1_000);
        assert!(can_composer_cancel(&e, "device-a"));
        assert!(!can_composer_cancel(&e, "device-b"));
        let mut applied = e.clone();
        applied.status = SessionCommandStatus::Applied;
        assert!(!can_composer_cancel(&applied, "device-a"));
    }

    #[test]
    fn agent_prompt_absent_serializes_unchanged() {
        // Old payloads with no `agentPrompt` deserialize to None and re-
        // serialize byte-identically (no field leaks into the JSON).
        let json = r#"{"kind":"run","request":{"prompt":"hello","harness":null,"model":null,"reasoning":null,"modelOptions":{},"cwd":"/tmp","sandbox":"workspace-write","autoApprove":false,"resume":null,"attachments":[]},"messageId":"m1"}"#;
        let run: SessionCommandPayload = serde_json::from_str(json).unwrap();
        assert!(matches!(
            &run,
            SessionCommandPayload::Run {
                agent_prompt: None,
                ..
            }
        ));
        // Canonical re-serialization: no agentPrompt key, and the additive
        // defaults (`harness`, `attachments`) stay omitted.
        let out = serde_json::to_string(&run).unwrap();
        assert!(!out.contains("agentPrompt"));
        assert!(!out.contains("\"harness\""));
        assert!(!out.contains("\"attachments\""));
        // Round-trip is stable: serialize→deserialize→serialize is a fixpoint.
        let again: SessionCommandPayload = serde_json::from_str(&out).unwrap();
        assert_eq!(serde_json::to_string(&again).unwrap(), out);

        let steer_json = r#"{"kind":"steer","prompt":"focus","messageId":null}"#;
        let steer: SessionCommandPayload = serde_json::from_str(steer_json).unwrap();
        assert!(matches!(
            &steer,
            SessionCommandPayload::Steer {
                agent_prompt: None,
                ..
            }
        ));
        let out = serde_json::to_string(&steer).unwrap();
        assert_eq!(out, steer_json);
    }

    #[test]
    fn agent_prompt_some_round_trips() {
        let payload = SessionCommandPayload::Run {
            request: run_request(),
            message_id: "m1".into(),
            agent_prompt: Some("Conversation annotations (JSON): {}".into()),
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains(r#""agentPrompt":"Conversation annotations (JSON): {}""#));
        let back: SessionCommandPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, payload);
        assert!(matches!(
            &back,
            SessionCommandPayload::Run {
                agent_prompt: Some(p),
                ..
            } if p.starts_with("Conversation annotations")
        ));

        let steer = SessionCommandPayload::Steer {
            prompt: "focus".into(),
            message_id: Some("m2".into()),
            agent_prompt: Some("annotated".into()),
        };
        let json = serde_json::to_string(&steer).unwrap();
        assert!(json.contains(r#""agentPrompt":"annotated""#));
        let back: SessionCommandPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, steer);
    }

    #[test]
    fn sent_at_absent_serializes_unchanged() {
        // Legacy entry JSON (no `sentAt`) deserializes to None and
        // re-serializes byte-identically — the additive field never leaks
        // into old payloads.
        let json = r#"{"id":"c1","payload":{"kind":"run","request":{"prompt":"hello","model":null,"reasoning":null,"modelOptions":{},"cwd":"/tmp","sandbox":"workspace-write","autoApprove":false,"resume":null},"messageId":"m1"},"issuedBy":"device-a","issuedAt":1000,"basedOn":null,"expiresAt":null,"status":"pending","resolution":null}"#;
        let entry: SessionCommandEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.sent_at, None);
        let out = serde_json::to_string(&entry).unwrap();
        assert_eq!(
            out, json,
            "legacy command JSON must round-trip byte-identically"
        );

        // A present sentAt round-trips.
        let mut e = entry.clone();
        e.sent_at = Some(1000);
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""sentAt":1000"#));
        let back: SessionCommandEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sent_at, Some(1000));
        assert_eq!(serde_json::to_string(&back).unwrap(), json);
    }

    #[test]
    fn sent_at_round_trips_through_doc_ledger() {
        // queue_command persists sentAt; read_commands returns it.
        let doc = crate::SessionDoc::init("chat-1").unwrap();
        let mut e = entry(
            "c1",
            SessionCommandPayload::Run {
                request: run_request(),
                message_id: "m1".into(),
                agent_prompt: None,
            },
            1_000,
        );
        e.sent_at = Some(1_000);
        doc.queue_command(&e).unwrap();
        let commands = doc.read_commands().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].sent_at, Some(1_000));
        assert_eq!(commands[0].issued_at, 1_000);
        // A legacy-style entry without sentAt round-trips with None.
        let mut legacy = e.clone();
        legacy.id = "c2".into();
        legacy.sent_at = None;
        doc.queue_command(&legacy).unwrap();
        let commands = doc.read_commands().unwrap();
        assert_eq!(commands[1].sent_at, None);
    }

    fn run_request() -> RunRequest {
        RunRequest {
            prompt: "hello".into(),
            harness: None,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: "/tmp".into(),
            sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
            auto_approve: false,
            attachments: Vec::new(),
            pending_attachments: Vec::new(),
            resume: None,
            worktree: None,
        }
    }
}
