//! Session migration over RPC: `MigrateChats` on the chats' current host,
//! the destination's staging calls, and the relay peer the host drives a
//! destination on another device through.

use std::sync::Arc;

use cypher_proto::{
    ChatMigrationRequest, MigrationAbort, MigrationBegin, MigrationBegun, MigrationChunk,
    MigrationCommit, MigrationCommitted,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::EngineError;
use crate::session::migrations::MigrationPeer;

use super::*;

/// `MigrateChats`: plan here, then move in the background — the stream only
/// reports. A viewer that goes away mid-move does not stop it: every family
/// either commits or stays where it was.
pub(super) fn migrate_chats(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let request: ChatMigrationRequest = parse_params(strip_target(params))?;
    let migrations = rpc.migrations()?.clone();
    let plan = migrations.plan(&request).map_err(failed)?;
    let destination = plan.destination_device().to_string();
    let peer: Arc<dyn MigrationPeer> = if destination == rpc.doc_host.device_id() {
        Arc::new(migrations.clone())
    } else {
        let links = rpc.links.clone().ok_or_else(|| {
            RpcError::Failed(
                "The destination device can't be reached: this device isn't connected to sync."
                    .into(),
            )
        })?;
        Arc::new(RelayPeer {
            links,
            device_id: destination,
        })
    };
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move { migrations.run(plan, peer, tx).await });
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        let event = rx.recv().await?;
        Some((serde_json::to_value(event).unwrap_or(Value::Null), rx))
    });
    Ok(RpcReply::Stream(stream.boxed()))
}

/// The destination's side: staging, commit and abort.
pub(super) async fn receive(
    rpc: &EngineRpc,
    method: &str,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let migrations = rpc.migrations()?;
    let params = strip_target(params);
    match method {
        methods::BEGIN_CHAT_MIGRATION => {
            let request: MigrationBegin = parse_params(params)?;
            RpcReply::value(&migrations.receive_begin(request).map_err(failed)?)
        }
        methods::WRITE_CHAT_MIGRATION_CHUNK => {
            let chunk: MigrationChunk = parse_params(params)?;
            migrations.receive_chunk(chunk).map_err(failed)?;
            RpcReply::ok()
        }
        methods::COMMIT_CHAT_MIGRATION => {
            let commit: MigrationCommit = parse_params(params)?;
            RpcReply::value(&migrations.receive_commit(commit).await.map_err(failed)?)
        }
        _ => {
            let abort: MigrationAbort = parse_params(params)?;
            migrations.receive_abort(&abort.migration_id);
            RpcReply::ok()
        }
    }
}

/// A destination on another device, reached over its device relay.
struct RelayPeer {
    links: Arc<LinkCache>,
    device_id: String,
}

impl RelayPeer {
    async fn call<T: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        body: &T,
    ) -> Result<R, EngineError> {
        let mut params = serde_json::to_value(body)
            .map_err(|e| EngineError::Other(format!("serialize {method}: {e}")))?;
        if let Some(object) = params.as_object_mut() {
            object.insert("targetDeviceId".into(), Value::from(self.device_id.clone()));
        }
        let unreachable = |err: RpcError| {
            EngineError::Other(format!("The destination device can't be reached ({err})."))
        };
        let client = self
            .links
            .client(&self.device_id)
            .await
            .map_err(unreachable)?;
        match client.call(method, params).await {
            Ok(value) => serde_json::from_value(value)
                .map_err(|e| EngineError::Other(format!("{method} reply: {e}"))),
            Err(err @ (RpcError::Closed | RpcError::Transport(_))) => {
                self.links.invalidate(&self.device_id);
                Err(unreachable(err))
            }
            Err(RpcError::UnknownMethod(_)) => Err(EngineError::Other(
                "The destination device runs an older Cypher. Update it, then try again.".into(),
            )),
            Err(err) => Err(EngineError::Other(err.to_string())),
        }
    }
}

#[async_trait]
impl MigrationPeer for RelayPeer {
    fn is_local(&self) -> bool {
        false
    }

    async fn begin(&self, request: MigrationBegin) -> Result<MigrationBegun, EngineError> {
        self.call(methods::BEGIN_CHAT_MIGRATION, &request).await
    }

    async fn write_chunk(&self, chunk: MigrationChunk) -> Result<(), EngineError> {
        self.call::<_, Value>(methods::WRITE_CHAT_MIGRATION_CHUNK, &chunk)
            .await
            .map(drop)
    }

    async fn commit(&self, commit: MigrationCommit) -> Result<MigrationCommitted, EngineError> {
        self.call(methods::COMMIT_CHAT_MIGRATION, &commit).await
    }

    async fn abort(&self, migration_id: &str) {
        let abort = MigrationAbort {
            migration_id: migration_id.to_string(),
        };
        if let Err(err) = self
            .call::<_, Value>(methods::ABORT_CHAT_MIGRATION, &abort)
            .await
        {
            tracing::debug!(migration = %migration_id, error = %err, "migration abort not delivered");
        }
    }
}
