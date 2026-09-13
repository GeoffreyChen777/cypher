//! Engine v3 session storage/transport boundary. Local profiles own
//! their log; synced profiles must await server commits and never fall back
//! to local arbitration. All payload policies sit above this boundary.
use std::{
    path::Path,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use cypher_proto::sync3::{self as wire, Operation, Reply, Request};
use cypher_sync::sync3::{
    Error, Journal, Phase,
    execution::{DispatchPermit, Info, ObservationPermit, Plan, Progress},
    local::LocalAuthority,
    transport::{Client, RepairTransport, Status, Tuning},
};
use futures::future::BoxFuture;
use tokio::sync::watch;

use crate::EdgeConfig;

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn protocol(code: &str) -> Error {
    Error::Protocol(code.into())
}

enum Backend {
    Local {
        authority: Mutex<LocalAuthority>,
        status: watch::Sender<Status>,
    },
    Remote(Client),
}

pub struct SessionReplica {
    backend: Backend,
    retired: AtomicBool,
    active_claims: Mutex<std::collections::HashSet<String>>,
    active_observations: Mutex<std::collections::HashSet<String>>,
}

impl SessionReplica {
    pub fn open_local(path: &Path, account: &str, room: &str, actor: &str) -> Result<Self, Error> {
        let authority = LocalAuthority::open(path, account, room, actor)?;
        let (status, _) = watch::channel(Status {
            phase: Phase::Live,
            cursor: authority.journal().cursor()?,
            generation: 0,
            repairs: 0,
            error: None,
        });
        Ok(Self {
            active_claims: Mutex::new(Default::default()),
            active_observations: Mutex::new(Default::default()),
            retired: AtomicBool::new(false),
            backend: Backend::Local {
                authority: Mutex::new(authority),
                status,
            },
        })
    }

    /// Initialization is idempotent for the workspace-selected owner. It does
    /// not overwrite another owner's room or locally manufacture an ACK.
    #[allow(clippy::too_many_arguments)]
    pub async fn open_synced(
        path: &Path,
        account: &str,
        org: &str,
        user: &str,
        room: &str,
        actor: &str,
        owner: &str,
        edge: EdgeConfig,
    ) -> Result<Self, Error> {
        let replica = Self::open_synced_lazy(path, account, org, user, room, actor, owner, edge)?;
        let mut status = replica.watch();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let current = status.borrow_and_update().clone();
                if current.phase == Phase::Live {
                    return Ok::<_, Error>(());
                }
                if let Some(error) = current.error {
                    return Err(Error::Protocol(error));
                }
                status
                    .changed()
                    .await
                    .map_err(|_| protocol("replica_closed"))?;
            }
        })
        .await
        .map_err(|_| protocol("business_timeout"))??;
        Ok(replica)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open_synced_lazy(
        path: &Path,
        account: &str,
        org: &str,
        user: &str,
        room: &str,
        actor: &str,
        owner: &str,
        edge: EdgeConfig,
    ) -> Result<Self, Error> {
        let identity: (String, String) =
            serde_json::from_str(account).map_err(|_| protocol("account_scope_mismatch"))?;
        if identity.0 != org || identity.1 != user {
            return Err(protocol("account_scope_mismatch"));
        }
        for id in [org, room, actor, owner] {
            if !wire::valid_id(id) {
                return Err(protocol("invalid_identity"));
            }
        }
        let route = format!("/sync3/{org}/chats/{room}");
        let repair = Arc::new(EdgeTransport::new(
            edge.clone(),
            route.clone(),
            owner.into(),
            user.into(),
        )?);
        let journal = Journal::open(path, account, room, actor)?;
        if journal.is_local_authority()? {
            return Err(protocol("local_authority_cannot_join_cloud"));
        }
        let client = Client::spawn(
            journal,
            Arc::new(cypher_sync::AccountUrl::new(
                edge.room_url(format!("{route}/ws")),
                user,
            )),
            Some(repair),
            Tuning::default(),
        );
        Ok(Self {
            active_claims: Mutex::new(Default::default()),
            active_observations: Mutex::new(Default::default()),
            retired: AtomicBool::new(false),
            backend: Backend::Remote(client),
        })
    }

    pub fn watch(&self) -> watch::Receiver<Status> {
        match &self.backend {
            Backend::Local { status, .. } => status.subscribe(),
            Backend::Remote(client) => client.watch(),
        }
    }

    pub fn read<T>(&self, read: impl FnOnce(&Journal) -> Result<T, Error>) -> Result<T, Error> {
        match &self.backend {
            Backend::Local { authority, .. } => read(lock(authority).journal()),
            Backend::Remote(client) => read(&lock(&client.journal())),
        }
    }

    /// Durable write followed by transport wake. Only the local backend may
    /// commit its own outbox. A cloud write's result is never treated as a
    /// committed claim/run merely because it is now queued.
    pub fn write<T>(
        &self,
        write: impl FnOnce(&mut Journal) -> Result<T, Error>,
    ) -> Result<T, Error> {
        if self.retired.load(Ordering::Acquire) {
            return Err(protocol("replica_retired"));
        }
        match &self.backend {
            Backend::Local { authority, status } => {
                let mut authority = lock(authority);
                let result = write(authority.journal_mut())?;
                let commit = (|| {
                    while authority.commit_pending()? != 0 {}
                    authority.journal().cursor()
                })();
                match commit {
                    Ok(cursor) => {
                        status.send_if_modified(|s| {
                            if s.cursor == cursor {
                                return false;
                            }
                            s.cursor = cursor;
                            s.phase = Phase::Live;
                            s.error = None;
                            true
                        });
                        Ok(result)
                    }
                    Err(error) => {
                        status.send_modify(|s| {
                            s.phase = Phase::Suspect;
                            s.error = Some(error.to_string());
                        });
                        Err(error)
                    }
                }
            }
            Backend::Remote(client) => {
                let result = write(&mut lock(&client.journal()))?;
                client.wake();
                Ok(result)
            }
        }
    }

    pub fn enqueue(&self, operation: &Operation) -> Result<(), Error> {
        self.write(|journal| journal.enqueue(operation))
    }
    pub fn prepare(&self, command: &str, plan: Plan) -> Result<Info, Error> {
        self.write(|journal| journal.prepare_execution(command, plan))
    }
    pub fn advance(&self, command: &str) -> Result<Progress, Error> {
        let mut claims = lock(&self.active_claims);
        let progress = self.write(|journal| journal.advance_execution(command))?;
        if matches!(progress, Progress::Dispatch(_)) {
            claims.insert(command.into());
        }
        Ok(progress)
    }
    pub fn has_active_claim(&self, command: &str) -> bool {
        lock(&self.active_claims).contains(command)
    }
    pub fn reserve_command(&self, command: &str) {
        lock(&self.active_claims).insert(command.into());
    }
    pub fn finish_claim(&self, command: &str) {
        lock(&self.active_claims).remove(command);
    }
    pub fn has_active_observation(&self, run: &str) -> bool {
        lock(&self.active_observations).contains(run)
    }

    /// Record a persistent process separately from its first semantic run.
    /// Return only after the occupancy itself has committed. Losing this
    /// future/permit requires recovery; it is never permission to spawn again.
    pub async fn occupy(&self, permit: &DispatchPermit) -> Result<String, Error> {
        let execution_id = format!("execution-{}", permit.run_id());
        let actor = self.read(|j| Ok(j.actor().to_owned()))?;
        let (_, owner_epoch) = self.read(|j| j.owner())?;
        self.enqueue(&Operation {
            id: format!("occupy-{}", permit.run_id()),
            actor,
            owner_epoch,
            event: wire::Event::ExecutionStarted {
                execution_id: execution_id.clone(),
                command_id: permit.command().id.clone(),
            },
        })?;
        let mut status = self.watch();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let current = status.borrow_and_update().clone();
                if let Some(error) = current.error {
                    return Err(Error::Protocol(error));
                }
                let occupied =
                    self.read(|j| Ok(j.projection()?.executions.get(&execution_id).cloned()))?;
                if let Some(occupied) = occupied {
                    if occupied.closed || occupied.command_id != permit.command().id {
                        return Err(protocol("execution_occupancy_conflict"));
                    }
                    return Ok(execution_id);
                }
                status
                    .changed()
                    .await
                    .map_err(|_| protocol("replica_closed"))?;
            }
        })
        .await
        .map_err(|_| protocol("business_timeout"))?
    }

    /// Caller must have verified actual harness closure. The reducer also
    /// rejects closure while semantic runs or accepted work remain unsettled.
    pub fn release_occupancy(&self, execution_id: &str) -> Result<(), Error> {
        let actor = self.read(|j| Ok(j.actor().to_owned()))?;
        let (_, owner_epoch) = self.read(|j| j.owner())?;
        self.enqueue(&Operation {
            id: format!("close-{execution_id}"),
            actor,
            owner_epoch,
            event: wire::Event::ExecutionFinished {
                execution_id: execution_id.into(),
            },
        })
    }
    /// Do not poison the durable outbox with a premature close while a
    /// concurrent control command is still waiting to commit its outcome.
    pub fn release_if_reconciled(&self, execution_id: &str) -> Result<bool, Error> {
        let ready = self.read(|j| {
            let p = j.projection()?;
            if p.executions.get(execution_id).is_some_and(|e| e.closed) {
                return Ok(true);
            }
            Ok(!p.runs.values().any(|r| r.outcome.is_none())
                && !p.commands.values().any(|c| {
                    c.accepted_op_id.is_some()
                        && (c.command.status == cypher_proto::SessionCommandStatus::Pending
                            || (c.command.status == cypher_proto::SessionCommandStatus::Applied
                                && c.run_id.as_ref().is_some_and(|id| {
                                    p.runs.get(id).is_none_or(|r| r.outcome.is_none())
                                })))
                }))
        })?;
        if !ready {
            return Ok(false);
        }
        if self.read(|j| {
            Ok(j.projection()?
                .executions
                .get(execution_id)
                .is_some_and(|e| e.closed))
        })? {
            return Ok(true);
        }
        self.release_occupancy(execution_id)?;
        Ok(true)
    }
    /// Retaining private source observations must not wake network traffic.
    pub fn retain(
        &self,
        permit: &DispatchPermit,
        first: u64,
        events: &[cypher_proto::AgentEvent],
    ) -> Result<u64, Error> {
        match &self.backend {
            Backend::Local { authority, .. } => lock(authority)
                .journal_mut()
                .append_execution_events(permit, first, events),
            Backend::Remote(client) => client.append_execution_events(permit, first, events),
        }
    }
    pub async fn shutdown(self) {
        if let Backend::Remote(client) = self.backend {
            client.shutdown().await;
        }
    }
    pub async fn disconnect(&self) {
        self.retired.store(true, Ordering::Release);
        match &self.backend {
            Backend::Remote(client) => client.stop().await,
            Backend::Local { status, .. } => status.send_modify(|s| s.phase = Phase::Offline),
        }
    }
}

/// One semantic turn's source and producer, independent of how many turns a
/// persistent harness process serves. The dispatch permit is owned here and
/// cannot be reconstructed after process death or copied to a second turn.
pub struct ExecutionPublication {
    replica: Arc<SessionReplica>,
    permit: DispatchPermit,
    source_seq: u64,
    writer: cypher_sync::sync3::writer::TranscriptWriter,
    parts: Vec<cypher_proto::MessagePart>,
    complete: bool,
    entry_id: String,
    observed: Option<ObservationPermit>,
}
impl ExecutionPublication {
    pub fn new(
        replica: Arc<SessionReplica>,
        permit: DispatchPermit,
        entry: &cypher_proto::SessionMessageEntry,
    ) -> Result<Self, Error> {
        let writer = replica.read(|journal| journal.new_execution_writer(&permit, entry))?;
        Ok(Self {
            replica,
            permit,
            source_seq: 0,
            writer,
            parts: Vec::new(),
            complete: false,
            entry_id: entry.id.clone(),
            observed: None,
        })
    }

    pub fn run_id(&self) -> &str {
        self.observed
            .as_ref()
            .map_or_else(|| self.permit.run_id(), |p| p.run_id())
    }
    pub fn entry_id(&self) -> &str {
        &self.entry_id
    }
    pub fn set_cwd(&self, cwd: &str) -> Result<(), Error> {
        self.replica
            .write(|j| j.set_execution_cwd(&self.permit, cwd))
    }
    pub async fn occupy(&self) -> Result<String, Error> {
        self.replica.occupy(&self.permit).await
    }
    pub fn command(&self) -> &cypher_proto::SessionCommandEntry {
        self.permit.command()
    }
    pub fn is_complete(&self) -> bool {
        self.complete
    }
    pub fn replica(&self) -> &Arc<SessionReplica> {
        &self.replica
    }

    pub async fn begin_observation(
        &mut self,
        execution_id: &str,
        entry_id: String,
    ) -> Result<(), Error> {
        if !self.complete {
            return Err(protocol("source_turn_not_settled"));
        }
        let run_id = uuid::Uuid::new_v4().to_string();
        lock(&self.replica.active_observations).insert(run_id.clone());
        let result = async {
            let permit = self.replica.write(|j| {
                j.prepare_observation(&self.permit, execution_id, &run_id, self.source_seq)
            })?;
            let mut status = self.replica.watch();
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let current = status.borrow_and_update().clone();
                    if self.replica.read(|j| j.observation_ready(&permit))? {
                        return Ok::<_, Error>(());
                    }
                    if let Some(error) = current.error {
                        return Err(Error::Protocol(error));
                    }
                    status
                        .changed()
                        .await
                        .map_err(|_| protocol("replica_closed"))?;
                }
            })
            .await
            .map_err(|_| protocol("business_timeout"))??;
            let entry = cypher_proto::SessionMessageEntry {
                id: entry_id.clone(),
                role: cypher_proto::MessageRole::Assistant,
                parts: vec![],
                device_id: self.replica.read(|j| Ok(j.actor().to_owned()))?,
                created_at: chrono::Utc::now().timestamp_millis(),
                status: Some(cypher_proto::MessageStatus::Streaming),
                continuation_of: None,
            };
            self.writer = self
                .replica
                .read(|j| j.new_observation_writer(&permit, &entry))?;
            self.observed = Some(permit);
            self.parts.clear();
            self.entry_id = entry_id;
            self.complete = false;
            Ok::<_, Error>(())
        }
        .await;
        if result.is_err() {
            lock(&self.replica.active_observations).remove(&run_id);
        }
        result
    }

    /// Retain without choosing a public fold. Native runtime filters (e.g.
    /// repeated SessionStarted or late observations) must not discard source.
    pub fn retain(&mut self, event: &cypher_proto::AgentEvent) -> Result<(), Error> {
        let next = self
            .source_seq
            .checked_add(1)
            .ok_or_else(|| protocol("sequence_exhausted"))?;
        self.replica
            .retain(&self.permit, next, std::slice::from_ref(event))?;
        self.source_seq = next;
        Ok(())
    }

    pub fn sync_parts(&mut self, parts: &[cypher_proto::MessagePart]) -> Result<(), Error> {
        if self.complete {
            return Err(protocol("execution_already_complete"));
        }
        self.parts = parts.to_vec();
        self.flush()
    }

    /// Source retention is a separate private transaction and precedes any
    /// public frame. After fence loss it still captures late observations,
    /// while enqueue_execution_frame fails closed and publishes nothing.
    pub fn observe(&mut self, event: &cypher_proto::AgentEvent) -> Result<(), Error> {
        self.retain(event)?;
        if self.complete {
            return Ok(());
        }
        cypher_proto::parts::fold_event_into_parts(&mut self.parts, event);
        self.flush()
    }

    pub fn flush(&mut self) -> Result<(), Error> {
        if self.complete {
            return Err(protocol("execution_already_complete"));
        }
        while self
            .writer
            .sync(&self.parts, |frame| {
                self.replica.write(|j| {
                    enqueue_output(
                        j,
                        &self.permit,
                        self.observed.as_ref(),
                        self.source_seq,
                        frame,
                    )
                })
            })?
            .more
        {}
        Ok(())
    }

    /// Does not close persistent-process occupancy. The runtime must only
    /// publish executionFinished after the actual process and all work settle.
    pub fn finish(&mut self, outcome: wire::Outcome) -> Result<(), Error> {
        self.finish_with(outcome, cypher_proto::SessionCommandStatus::Applied, None)
    }

    pub fn finish_with(
        &mut self,
        outcome: wire::Outcome,
        command_status: cypher_proto::SessionCommandStatus,
        resolution: Option<String>,
    ) -> Result<(), Error> {
        if self.complete {
            return Err(protocol("execution_already_complete"));
        }
        let status = if outcome == wire::Outcome::Completed {
            cypher_proto::MessageStatus::Complete
        } else {
            cypher_proto::MessageStatus::Aborted
        };
        while self
            .writer
            .finish(&self.parts, Some(status), |frame| {
                self.replica.write(|j| {
                    enqueue_output(
                        j,
                        &self.permit,
                        self.observed.as_ref(),
                        self.source_seq,
                        frame,
                    )
                })
            })?
            .more
        {}
        self.replica.write(|j| {
            if let Some(observed) = &self.observed {
                j.complete_observation(observed, self.source_seq, outcome)
            } else {
                j.complete_execution(&self.permit, Some(outcome), command_status, resolution)
            }
        })?;
        self.complete = true;
        if let Some(observed) = &self.observed {
            lock(&self.replica.active_observations).remove(observed.run_id());
        }
        self.replica.finish_claim(&self.permit.command().id);
        Ok(())
    }

    /// A display segment may settle on quiescence without asserting that the
    /// semantic run or persistent process ended. Later observations continue
    /// through a new writer under the SAME run and source journal.
    pub fn rotate(&mut self, entry_id: String) -> Result<(), Error> {
        if self.complete {
            return Ok(());
        }
        while self
            .writer
            .finish(
                &self.parts,
                Some(cypher_proto::MessageStatus::Complete),
                |frame| {
                    self.replica.write(|j| {
                        enqueue_output(
                            j,
                            &self.permit,
                            self.observed.as_ref(),
                            self.source_seq,
                            frame,
                        )
                    })
                },
            )?
            .more
        {}
        let entry = cypher_proto::SessionMessageEntry {
            id: entry_id.clone(),
            role: cypher_proto::MessageRole::Assistant,
            parts: vec![],
            device_id: self.replica.read(|j| Ok(j.actor().to_owned()))?,
            created_at: chrono::Utc::now().timestamp_millis(),
            status: Some(cypher_proto::MessageStatus::Streaming),
            continuation_of: None,
        };
        self.writer = self.replica.read(|j| {
            if let Some(observed) = &self.observed {
                j.new_observation_writer(observed, &entry)
            } else {
                j.new_execution_writer(&self.permit, &entry)
            }
        })?;
        self.entry_id = entry_id;
        self.parts.clear();
        Ok(())
    }
}

impl Drop for ExecutionPublication {
    fn drop(&mut self) {
        if let Some(observed) = &self.observed {
            lock(&self.replica.active_observations).remove(observed.run_id());
        }
    }
}
fn enqueue_output(
    journal: &mut Journal,
    dispatch: &DispatchPermit,
    observed: Option<&ObservationPermit>,
    seq: u64,
    frame: &cypher_sync::sync3::writer::Frame,
) -> Result<(), Error> {
    match observed {
        Some(permit) => journal.enqueue_observation_frame(permit, seq, frame),
        None => journal.enqueue_execution_frame(dispatch, seq, frame),
    }
}

/// No redirects, bounded bodies and an aggregate timeout. The v3 Client is
/// the single repair/retry owner; this transport executes one exchange only.
#[derive(Clone)]
struct EdgeTransport {
    edge: EdgeConfig,
    route: String,
    http: reqwest::Client,
    owner: String,
    user: String,
    initialized: Arc<AtomicBool>,
}
impl EdgeTransport {
    fn new(edge: EdgeConfig, route: String, owner: String, user: String) -> Result<Self, Error> {
        if user.is_empty() || user.len() > 256 || user.chars().any(char::is_control) {
            return Err(protocol("invalid_expected_user"));
        }
        let url = reqwest::Url::parse(&edge.url).map_err(|_| protocol("invalid_endpoint"))?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(protocol("invalid_endpoint"));
        }
        if !(url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))))
        {
            return Err(protocol("insecure_endpoint"));
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| protocol("transport_unavailable"))?;
        Ok(Self {
            edge,
            route,
            http,
            owner,
            user,
            initialized: Arc::new(AtomicBool::new(false)),
        })
    }

    async fn post(&self, method: &str, body: &impl serde::Serialize) -> Result<Reply, Error> {
        let bytes = serde_json::to_vec(body)?;
        if bytes.len() > wire::MAX_FRAME_BYTES {
            return Err(protocol("frame_too_large"));
        }
        let reply = tokio::time::timeout(Duration::from_secs(15), async {
            let bearer = self
                .edge
                .bearer()
                .await
                .ok_or_else(|| protocol("reauth_required"))?;
            let mut response = self
                .http
                .post(format!(
                    "{}{}/{method}",
                    self.edge.url.trim_end_matches('/'),
                    self.route
                ))
                .bearer_auth(bearer)
                .header(cypher_sync::EXPECTED_USER_HEADER, &self.user)
                .header("content-type", "application/json")
                .body(bytes)
                .send()
                .await
                .map_err(|_| protocol("transport_unavailable"))?;
            match response.status().as_u16() {
                200 | 400 | 409 => {}
                401 => return Err(protocol("reauth_required")),
                403 => return Err(protocol("not_authorized")),
                429 | 500..=599 => return Err(protocol("transport_unavailable")),
                _ => return Err(protocol("unexpected_http_status")),
            }
            if response
                .content_length()
                .is_some_and(|n| n > wire::MAX_FRAME_BYTES as u64)
            {
                return Err(protocol("frame_too_large"));
            }
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| protocol("transport_unavailable"))?
            {
                if body.len() + chunk.len() > wire::MAX_FRAME_BYTES {
                    return Err(protocol("frame_too_large"));
                }
                body.extend_from_slice(&chunk);
            }
            let reply: Reply = serde_json::from_slice(&body)?;
            if let Reply::Error { code, .. } = reply {
                return Err(protocol(&code));
            }
            Ok(reply)
        })
        .await
        .map_err(|_| protocol("business_timeout"))??;
        Ok(reply)
    }
}
impl RepairTransport for EdgeTransport {
    fn initialize(&self) -> BoxFuture<'static, Result<(), Error>> {
        let this = self.clone();
        Box::pin(async move {
            if !this.initialized.load(Ordering::Acquire) {
                this.post("init", &serde_json::json!({ "owner": this.owner }))
                    .await?;
                this.initialized.store(true, Ordering::Release);
            }
            Ok(())
        })
    }
    fn exchange(&self, request: Request) -> BoxFuture<'static, Result<Reply, Error>> {
        let this = self.clone();
        Box::pin(async move {
            this.initialize().await?;
            this.post("exchange", &request).await
        })
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    #[test]
    fn captured_user_must_match_durable_account_before_open_or_network() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("must-not-exist.sqlite");
        let result = SessionReplica::open_synced_lazy(
            &path,
            r#"["org","first"]"#,
            "org",
            "second",
            "chat",
            "host",
            "host",
            EdgeConfig::with_static_token("http://127.0.0.1:1", "second@org"),
        );
        assert!(matches!(result, Err(Error::Protocol(code)) if code == "account_scope_mismatch"));
        assert!(!path.exists());
    }
}
