//! Pi runtime: install, packages, subagent profiles, translation, providers.

use serde_json::Value;

use super::*;

pub(super) async fn install_pi(rpc: &EngineRpc) -> Result<RpcReply, RpcError> {
    rpc.pi_runtime()?
        .install_latest()
        .await
        .map_err(RpcError::Failed)?;
    rpc.registry.invalidate_discovery(HarnessId::Pi);
    RpcReply::value(&crate::pi_packages::list(rpc.pi_runtime()?.paths()))
}

pub(super) async fn install_pi_package(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let p: PiPackageParams = parse_params(params)?;
    crate::pi_packages::install_package(rpc.pi_runtime()?.paths(), &p.source)
        .await
        .map_err(RpcError::Failed)?;
    rpc.reload_pi_runtime().await;
    RpcReply::value(&crate::pi_packages::list(rpc.pi_runtime()?.paths()))
}

pub(super) async fn set_pi_package_enabled(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let p: crate::pi_packages::SetPackageEnabled = parse_params(params)?;
    crate::pi_packages::set_package_enabled(rpc.pi_runtime()?.paths(), p)
        .map_err(RpcError::Failed)?;
    rpc.reload_pi_runtime().await;
    RpcReply::value(&crate::pi_packages::list(rpc.pi_runtime()?.paths()))
}

pub(super) async fn list_pi_subagents(rpc: &EngineRpc) -> Result<RpcReply, RpcError> {
    let paths = rpc.pi_runtime()?.paths().clone();
    let agents = crate::off_runtime(move || crate::pi_subagents::list(&paths))
        .await
        .map_err(RpcError::Failed)?;
    RpcReply::value(&agents)
}

pub(super) async fn save_pi_subagent(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let body = strip_target(params);
    let request: SavePiSubagentParams = parse_params(body)?;
    request.agent.validate().map_err(RpcError::BadParams)?;
    let paths = rpc.pi_runtime()?.paths().clone();
    let list_paths = paths.clone();
    crate::off_runtime(move || {
        crate::pi_subagents::save(&paths, &request.agent, request.original_name.as_deref())
    })
    .await
    .map_err(RpcError::Failed)?
    .map_err(RpcError::BadParams)?;
    // The next child spawn has to read the new profile, and the
    // extension loads `agents/` once per process.
    rpc.reload_pi_runtime().await;
    let agents = crate::off_runtime(move || crate::pi_subagents::list(&list_paths))
        .await
        .map_err(RpcError::Failed)?;
    RpcReply::value(&agents)
}

pub(super) async fn delete_pi_subagent(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let body = strip_target(params);
    let request: DeletePiSubagentParams = parse_params(body)?;
    let paths = rpc.pi_runtime()?.paths().clone();
    let list_paths = paths.clone();
    crate::off_runtime(move || crate::pi_subagents::delete(&paths, &request.name))
        .await
        .map_err(RpcError::Failed)?
        .map_err(RpcError::BadParams)?;
    rpc.reload_pi_runtime().await;
    let agents = crate::off_runtime(move || crate::pi_subagents::list(&list_paths))
        .await
        .map_err(RpcError::Failed)?;
    RpcReply::value(&agents)
}

pub(super) async fn get_pi_translation_settings(rpc: &EngineRpc) -> Result<RpcReply, RpcError> {
    let paths = rpc.pi_runtime()?.paths().clone();
    let settings = crate::off_runtime(move || crate::pi_translation::load(&paths))
        .await
        .map_err(RpcError::Failed)?;
    RpcReply::value(&settings)
}

pub(super) async fn set_pi_translation_settings(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let body = strip_target(params);
    let settings: crate::pi_translation::PiTranslationSettings = parse_params(body)?;
    settings.validate().map_err(RpcError::BadParams)?;
    let paths = rpc.pi_runtime()?.paths().clone();
    let previous_paths = paths.clone();
    let previous = crate::off_runtime(move || crate::pi_translation::load(&previous_paths))
        .await
        .map_err(RpcError::Failed)?;
    // Language/display changes and deselection need no model
    // discovery. Only newly selected ids require catalog validation.
    let added = settings.new_model_selections(&previous);
    if !added.is_empty() {
        rpc.require_pi_catalog_models(
            &added,
            Duration::from_secs(10),
            "Selected model is not in this device's Pi catalog; refresh and choose again",
        )
        .await?;
    }
    let saved = crate::off_runtime(move || crate::pi_translation::save(&paths, settings))
        .await
        .and_then(|result| result)
        .map_err(RpcError::Failed)?;
    // The extension reads translation.json on every input/message
    // hook. Do not invalidate model discovery or recycle sessions.
    RpcReply::value(&saved)
}

pub(super) fn detect_pi_language(params: Value) -> Result<RpcReply, RpcError> {
    let p: DetectPiLanguageParams = parse_params(params)?;
    if p.text.chars().count() > 24_000 {
        return Err(RpcError::BadParams(
            "Text is too long for offline language detection".into(),
        ));
    }
    RpcReply::value(&crate::pi_translation::detect_language(&p.text))
}

pub(super) async fn pi_provider(
    rpc: &EngineRpc,
    method: &str,
    params: Value,
) -> Result<RpcReply, RpcError> {
    // Routing is consumed here, never passed to the Runtime helper.
    // Non-local requests have already been forwarded above.
    let params = strip_target(params);
    let (action, args) = match method {
        methods::LIST_PI_PROVIDERS => ("list", serde_json::json!({})),
        methods::SAVE_PI_PROVIDER => {
            let p = serde_json::from_value::<crate::pi_providers::SaveProvider>(params)
                .map_err(|_| RpcError::BadParams("Invalid provider settings.".into()))?;
            (
                "save",
                serde_json::to_value(p)
                    .map_err(|_| RpcError::BadParams("Invalid provider.".into()))?,
            )
        }
        _ => {
            let id = params
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError::BadParams("Missing provider id.".into()))?;
            let action = match method {
                methods::REFRESH_PI_PROVIDER => "refresh",
                methods::LOGOUT_PI_PROVIDER => "logout",
                _ => "remove",
            };
            (action, serde_json::json!({ "id": id }))
        }
    };
    let result = crate::pi_providers::request(rpc.pi_runtime()?.paths(), action, args).await;
    // Even a partially completed disk operation needs cache invalidation.
    if action != "list" {
        rpc.reload_pi_runtime().await;
    }
    RpcReply::value(&result.map_err(RpcError::Failed)?)
}

pub(super) fn begin_pi_provider_login(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let id = params
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::BadParams("Missing provider id.".into()))?;
    let status = rpc
        .provider_logins
        .begin(rpc.pi_runtime()?.paths(), id)
        .map_err(RpcError::Failed)?;
    RpcReply::value(&status)
}

pub(super) async fn pi_provider_login(
    rpc: &EngineRpc,
    method: &str,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let id = params
        .get("attemptId")
        .and_then(serde_json::Value::as_str)
        .filter(|id| id.len() <= 64)
        .ok_or_else(|| RpcError::BadParams("Provider sign-in attempt ID required.".into()))?;
    let status = match method {
        methods::COMPLETE_PI_PROVIDER_LOGIN => {
            let callback = params
                .get("callbackUrl")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| RpcError::BadParams("Callback URL required.".into()))?;
            rpc.provider_logins.respond(id, callback)
        }
        methods::CANCEL_PI_PROVIDER_LOGIN => rpc.provider_logins.cancel(id),
        _ => rpc.provider_logins.status(id),
    }
    .map_err(RpcError::Failed)?;
    if status.phase == "succeeded" {
        rpc.reload_pi_runtime().await;
    }
    RpcReply::value(&status)
}

pub(super) async fn pi_session_modes(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: PiSessionModesParams = parse_params(params)?;
    let agent_dir = rpc.pi_runtime()?.paths().agent_dir.clone();
    let sessions = rpc.sessions.clone();
    let modes = crate::off_runtime(move || {
        let session = p
            .chat_id
            .as_deref()
            .and_then(|chat_id| sessions.pi_session_file(chat_id));
        crate::pi_session_modes::read(session.as_deref(), &agent_dir)
    })
    .await
    .map_err(RpcError::Failed)?;
    RpcReply::value(&modes)
}
