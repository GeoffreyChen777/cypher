//! The service this device serves through its device-room relay: the full
//! engine surface, minus
//! - the methods only this device's own viewports and CLI may call
//!   (`MethodSpec::local` — device trust), so no other device can approve
//!   itself here or change the group's policy through this device's keys;
//! - forwarding to a third device. A relayed caller is answered here, never
//!   passed on under this device's identity — which would let an unpaired
//!   device borrow a paired one's trust.

use std::sync::Arc;

use async_trait::async_trait;
use cypher_rpc::{RpcError, RpcReply, RpcService, methods};
use serde_json::Value;

pub struct RelayRpc {
    inner: Arc<dyn RpcService>,
    device_id: String,
}

impl RelayRpc {
    pub fn new(inner: Arc<dyn RpcService>, device_id: impl Into<String>) -> Self {
        Self {
            inner,
            device_id: device_id.into(),
        }
    }
}

#[async_trait]
impl RpcService for RelayRpc {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        if methods::spec(method).is_some_and(|spec| spec.local) {
            return Err(RpcError::Failed(format!(
                "{method} can only be used on the device itself."
            )));
        }
        if params
            .get("targetDeviceId")
            .and_then(Value::as_str)
            .is_some_and(|target| target != self.device_id)
        {
            return Err(RpcError::Failed(
                "Calls through this device can't be passed on to another device.".into(),
            ));
        }
        self.inner.handle(method, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    #[async_trait]
    impl RpcService for Echo {
        async fn handle(&self, method: &str, _: Value) -> Result<RpcReply, RpcError> {
            Ok(RpcReply::Value(serde_json::json!(method)))
        }
    }

    async fn call(method: &str, params: Value) -> Result<RpcReply, RpcError> {
        RelayRpc::new(Arc::new(Echo), "dev-a")
            .handle(method, params)
            .await
    }

    #[tokio::test]
    async fn relayed_calls_cannot_reach_local_methods_or_other_devices() {
        for method in [
            methods::SET_REQUIRE_PAIRED_DEVICES,
            methods::APPROVE_DEVICE_PAIRING,
            methods::JOIN_DEVICE_GROUP,
            methods::REMOVE_TRUSTED_DEVICE,
            methods::WATCH_DEVICE_TRUST,
        ] {
            let err = call(method, serde_json::json!({})).await.err().unwrap();
            assert!(
                err.to_string()
                    .contains("only be used on the device itself"),
                "{method}"
            );
        }
        let err = call(
            methods::OPEN_TERMINAL,
            serde_json::json!({ "targetDeviceId": "dev-c" }),
        )
        .await
        .err()
        .unwrap();
        assert!(err.to_string().contains("can't be passed on"), "{err}");

        assert!(
            call(
                methods::OPEN_TERMINAL,
                serde_json::json!({ "targetDeviceId": "dev-a" })
            )
            .await
            .is_ok()
        );
        assert!(
            call(methods::LIST_REPOS, serde_json::json!({}))
                .await
                .is_ok()
        );
    }
}
