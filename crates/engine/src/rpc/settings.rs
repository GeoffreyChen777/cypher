//! Device settings that pin a Pi model: web-search fallback, titles and
//! context recovery.

use serde_json::Value;

use super::*;

pub(super) async fn set_web_search_fallback(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    // Routing is consumed by the forwarder; strip it before the
    // strict parse (an explicit local target also carries it).
    let body = strip_target(params);
    let request: crate::pi::web_search::SetWebSearchFallback = parse_params(body)?;
    // Pinning points a real tool at one model: it must exist in
    // THIS device's catalog (the same rule as the title model).
    // Automatic ranks the catalog itself, and a disabled save only
    // remembers the preference.
    if let (true, Some(pinned)) = (request.enabled, request.model.as_deref()) {
        crate::pi::web_search::validate_model(pinned).map_err(RpcError::BadParams)?;
        rpc.require_pi_catalog_models(
            &[pinned],
            Duration::from_secs(20),
            "Model is not in this device's Pi catalog; refresh and choose again",
        )
        .await?;
    }
    let paths = rpc.pi_runtime()?.paths();
    let settings = crate::pi::web_search::save(paths, request).map_err(RpcError::Failed)?;
    // Enabling/disabling edits settings.json: parked Pi children
    // and cached discovery must pick the change up.
    rpc.reload_pi_runtime().await;
    RpcReply::value(&settings)
}

pub(super) fn get_title_model_settings(rpc: &EngineRpc) -> Result<RpcReply, RpcError> {
    let store = rpc.title_settings.as_ref().ok_or_else(|| {
        RpcError::Failed("Title settings unavailable; update this device's engine".into())
    })?;
    let settings = store.load().map_err(failed)?;
    RpcReply::value(&settings)
}

pub(super) async fn set_title_model_settings(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    // Require an explicit model field, including null for Auto.
    // An accidental {} must not clear the user's chosen model.
    if params.get("model").is_none() {
        return Err(RpcError::BadParams(
            "model is required (null selects Automatic)".into(),
        ));
    }
    let settings: cypher_proto::TitleModelSettings = parse_params(params)?;
    crate::session::title_settings::validate(&settings)
        .map_err(|e| RpcError::BadParams(e.to_string()))?;
    let store = rpc.title_settings.as_ref().ok_or_else(|| {
        RpcError::Failed("Title settings unavailable; update this device's engine".into())
    })?;
    if let Some(model) = &settings.model {
        rpc.require_pi_catalog_models(
            &[model.as_str()],
            Duration::from_secs(20),
            "Model is not in this device's Pi catalog; refresh and choose again",
        )
        .await?;
    }
    store.save(&settings).map_err(failed)?;
    RpcReply::value(&settings)
}

fn context_recovery_store(
    rpc: &EngineRpc,
) -> Result<&crate::session::context_recovery::ContextRecoverySettingsStore, RpcError> {
    rpc.context_recovery_settings.as_ref().ok_or_else(|| {
        RpcError::Failed("Context recovery is unavailable; update this device's engine".into())
    })
}

pub(super) fn get_context_recovery_settings(rpc: &EngineRpc) -> Result<RpcReply, RpcError> {
    let settings = context_recovery_store(rpc)?.load().map_err(failed)?;
    RpcReply::value(&settings)
}

pub(super) async fn set_context_recovery_settings(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let body = strip_target(params);
    let settings: cypher_proto::ContextRecoverySettings = parse_params(body)?;
    settings.validate().map_err(RpcError::BadParams)?;
    let store = context_recovery_store(rpc)?;
    // Only newly chosen models need the catalog: editing triggers or the
    // instruction, or deselecting, works while the catalog is unreachable.
    let previous = store.load().unwrap_or_default();
    let added = settings.new_model_selections(&previous);
    if !added.is_empty() {
        rpc.require_pi_catalog_models(
            &added,
            Duration::from_secs(10),
            "Selected model is not in this device's Pi catalog; refresh and choose again",
        )
        .await?;
    }
    store.save(&settings).map_err(failed)?;
    RpcReply::value(&settings)
}
