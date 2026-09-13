//! Runtime-scoped v3 session replicas and command admission for direct host
//! execution. Normal command-drain callers can pass an existing committed
//! command; direct/local calls create a durable command first.
use crate::{
    EdgeConfig, EngineProfile,
    session_replica::{ExecutionPublication, SessionReplica},
};
use cypher_proto::{
    MessageRole, MessageStatus, SessionCommandEntry, SessionCommandPayload, SessionCommandStatus,
    SessionMessageEntry,
    sync3::{Event, Operation},
};
use cypher_sync::sync3::{
    Error, Phase,
    execution::{Plan, Progress},
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub struct SessionReplicas {
    root: PathBuf,
    account: String,
    org: String,
    user: String,
    actor: String,
    edge: Option<EdgeConfig>,
    replicas: Mutex<HashMap<String, Arc<SessionReplica>>>,
    retired: AtomicBool,
}
impl SessionReplicas {
    pub fn new(
        profile: &EngineProfile,
        actor: String,
        edge: Option<EdgeConfig>,
    ) -> Result<Self, Error> {
        let root = profile.store_root().join("v3-sessions");
        std::fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            root,
            account: serde_json::to_string(&(profile.org_id(), profile.user_id()))?,
            org: profile.org_id().into(),
            user: profile.user_id().into(),
            actor,
            edge,
            replicas: Mutex::new(HashMap::new()),
            retired: AtomicBool::new(false),
        })
    }

    pub async fn get(&self, chat: &str) -> Result<Arc<SessionReplica>, Error> {
        self.get_for_owner(chat, &self.actor)
    }

    pub fn get_for_owner(&self, chat: &str, owner: &str) -> Result<Arc<SessionReplica>, Error> {
        if self.retired.load(Ordering::Acquire) {
            return Err(Error::Protocol("runtime_retired".into()));
        }
        if !cypher_proto::sync3::valid_id(chat) {
            return Err(Error::Protocol("invalid_chat_id".into()));
        }
        let mut replicas = self
            .replicas
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(replica) = replicas.get(chat) {
            return Ok(replica.clone());
        }
        let digest = Sha256::digest(serde_json::to_vec(&(
            self.edge.as_ref().map(|e| &e.url),
            &self.account,
            &self.actor,
            chat,
        ))?);
        let path = self.root.join(format!("{digest:x}.sqlite"));
        let replica = Arc::new(match &self.edge {
            Some(edge) => SessionReplica::open_synced_lazy(
                &path,
                &self.account,
                &self.org,
                &self.user,
                chat,
                &self.actor,
                owner,
                edge.clone(),
            )?,
            None => SessionReplica::open_local(&path, &self.account, chat, &self.actor)?,
        });
        replicas.insert(chat.into(), replica.clone());
        Ok(replica)
    }

    pub async fn shutdown(&self) {
        self.retired.store(true, Ordering::Release);
        let replicas = std::mem::take(
            &mut *self
                .replicas
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for replica in replicas.into_values() {
            replica.disconnect().await;
        }
    }

    pub async fn prepare(
        &self,
        chat: &str,
        payload: SessionCommandPayload,
        reply_id: &str,
    ) -> Result<ExecutionPublication, Error> {
        let replica = self.get(chat).await?;
        let now = chrono::Utc::now().timestamp_millis();
        let command = SessionCommandEntry {
            id: uuid::Uuid::new_v4().to_string(),
            payload,
            issued_by: self.actor.clone(),
            issued_at: now,
            sent_at: Some(now),
            based_on: None,
            expires_at: None,
            status: SessionCommandStatus::Pending,
            resolution: None,
        };
        let mut status = replica.watch();
        let owner_epoch = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let s = status.borrow_and_update().clone();
                let (_, epoch) = replica.read(|j| j.owner())?;
                if epoch > 0 && s.phase == Phase::Live {
                    return Ok::<_, Error>(epoch);
                }
                if let Some(error) = s.error {
                    return Err(Error::Protocol(error));
                }
                status
                    .changed()
                    .await
                    .map_err(|_| Error::Protocol("replica_closed".into()))?;
            }
        })
        .await
        .map_err(|_| Error::Protocol("business_timeout".into()))??;
        replica.reserve_command(&command.id);
        if let Err(error) = replica.enqueue(&Operation {
            id: uuid::Uuid::new_v4().to_string(),
            actor: self.actor.clone(),
            owner_epoch,
            event: Event::CommandQueued {
                command_id: command.id.clone(),
                command: command.clone(),
            },
        }) {
            replica.finish_claim(&command.id);
            return Err(error);
        }
        let result = self.acquire(replica.clone(), &command.id, reply_id).await;
        if result.is_err() {
            replica.finish_claim(&command.id);
        }
        result
    }

    pub async fn acquire(
        &self,
        replica: Arc<SessionReplica>,
        command: &str,
        reply_id: &str,
    ) -> Result<ExecutionPublication, Error> {
        let mut status = replica.watch();
        let permit = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let s = status.borrow_and_update().clone();
                if let Some(error) = s.error {
                    return Err(Error::Protocol(error));
                }
                let exists =
                    replica.read(|j| Ok(j.projection()?.commands.contains_key(command)))?;
                if exists {
                    replica.prepare(command, Plan::Run)?;
                    match replica.advance(command)? {
                        Progress::Dispatch(permit) => return Ok(permit),
                        Progress::WaitingForRun | Progress::WaitingForClaim => {}
                        _ => {
                            return Err(Error::Protocol(
                                "execution_requires_reconciliation".into(),
                            ));
                        }
                    }
                }
                status
                    .changed()
                    .await
                    .map_err(|_| Error::Protocol("replica_closed".into()))?;
            }
        })
        .await
        .map_err(|_| Error::Protocol("business_timeout".into()))??;
        ExecutionPublication::new(
            replica,
            permit,
            &SessionMessageEntry {
                id: reply_id.into(),
                role: MessageRole::Assistant,
                parts: vec![],
                device_id: self.actor.clone(),
                created_at: chrono::Utc::now().timestamp_millis(),
                status: Some(MessageStatus::Streaming),
                continuation_of: None,
            },
        )
    }
}
