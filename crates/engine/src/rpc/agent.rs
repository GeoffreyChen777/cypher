//! The agent socket's service: what an agent process may ask of its engine.
//!
//! Agents are spawned with the engine's keyless `agent.sock` (see
//! `cypher_rpc`'s `local` module) rather than the keyed `engine.sock`. Here
//! they reach only the calls the bundled extensions make — subagents and
//! language detection — and only on this device: an agent that follows
//! injected instructions cannot open terminals, change settings, or queue
//! work in chats that run elsewhere.

use std::sync::Arc;

use async_trait::async_trait;
use cypher_rpc::{RpcError, RpcReply, RpcService, methods};
use serde_json::Value;

use super::{EngineRpc, failed};

pub struct AgentRpc {
    inner: Arc<EngineRpc>,
}

impl AgentRpc {
    pub fn new(inner: Arc<EngineRpc>) -> Self {
        Self { inner }
    }

    /// `QueueCommand` and `WatchAgentEvents` address a subagent chat hosted on
    /// this device. `StartSubagent` needs no check here: its handler already
    /// refuses a parent hosted elsewhere.
    fn require_local_child(&self, params: &Value) -> Result<(), RpcError> {
        let chat_id = params
            .get("chatId")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::BadParams("chatId required".into()))?;
        match self.inner.workspace.chat(chat_id).map_err(failed)? {
            Some(chat) if chat.is_child() && chat.device_id == self.inner.doc_host.device_id() => {
                Ok(())
            }
            _ => Err(RpcError::Failed(
                "Agents can only reach subagent chats on this device.".into(),
            )),
        }
    }
}

#[async_trait]
impl RpcService for AgentRpc {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        if params.get("targetDeviceId").is_some() {
            return Err(RpcError::Failed(
                "Agents cannot address other devices.".into(),
            ));
        }
        match method {
            methods::DETECT_PI_LANGUAGE | methods::START_SUBAGENT => {}
            methods::QUEUE_COMMAND | methods::WATCH_AGENT_EVENTS => {
                self.require_local_child(&params)?
            }
            other => return Err(RpcError::UnknownMethod(other.to_string())),
        }
        self.inner.handle(method, params).await
    }
}
