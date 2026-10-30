//! Device trust: Settings → Devices pairing (see `crate::device_trust`).

use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;

use super::*;
use crate::device_trust::DeviceTrust;

fn trust(rpc: &EngineRpc) -> Result<&DeviceTrust, RpcError> {
    rpc.trust.as_ref().ok_or_else(|| {
        RpcError::Failed("Device pairing needs a signed-in, synced workspace.".into())
    })
}

/// The current status, then a fresh one on every change.
pub(super) fn watch(rpc: &EngineRpc) -> Result<RpcReply, RpcError> {
    let trust = trust(rpc)?.clone();
    let changes = trust.watch();
    let first = serde_json::to_value(trust.status()).map_err(failed)?;
    let rest = futures::stream::unfold((trust, changes), |(trust, mut changes)| async move {
        changes.changed().await.ok()?;
        let status = serde_json::to_value(trust.status()).ok()?;
        Some((status, (trust, changes)))
    });
    Ok(RpcReply::Stream(
        futures::stream::once(async move { first })
            .chain(rest)
            .boxed(),
    ))
}

#[derive(Deserialize)]
struct CodeParams {
    code: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceParams {
    device_id: String,
}

#[derive(Deserialize)]
struct EnabledParams {
    enabled: bool,
}

pub(super) async fn call(
    rpc: &EngineRpc,
    method: &str,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let trust = trust(rpc)?;
    let status = match method {
        methods::START_DEVICE_PAIRING => trust.start_invite().await,
        methods::APPROVE_DEVICE_PAIRING => trust.approve().await,
        methods::CANCEL_DEVICE_PAIRING => {
            trust.cancel_pairing().await;
            Ok(trust.status())
        }
        methods::JOIN_DEVICE_GROUP => {
            let p: CodeParams = parse_params(params)?;
            trust.join(&p.code).await
        }
        methods::REMOVE_TRUSTED_DEVICE => {
            let p: DeviceParams = parse_params(params)?;
            trust.remove(&p.device_id).await
        }
        methods::SET_REQUIRE_PAIRED_DEVICES => {
            let p: EnabledParams = parse_params(params)?;
            trust.set_require_paired(p.enabled).await
        }
        other => return Err(RpcError::UnknownMethod(other.to_string())),
    }
    .map_err(RpcError::Failed)?;
    RpcReply::value(&status)
}
