//! Session setup before the first prompt: resume + model/thinking selection,
//! the context gauge, and the built-in slash commands (`/compact`,
//! `/export-html`) the harness dispatches over RPC itself.

use super::*;

/// Which synthesized built-in commands a run intercepts. A same-name
/// extension/prompt/skill command wins and is left to pi itself; an
/// UNPOPULATED cache still intercepts — the popup's selections come from
/// `commands()`, which already deduped the synthesized entries, so a
/// `/compact` prompt without a discovered `compact` command can only be the
/// built-in.
#[derive(Clone, Copy, Default)]
pub struct BuiltinIntercept {
    pub(super) compact: bool,
    export_html: bool,
}

impl BuiltinIntercept {
    pub fn all() -> Self {
        Self {
            compact: true,
            export_html: true,
        }
    }

    pub fn from_probe(discovered: &[SlashCommand]) -> Self {
        Self {
            compact: !discovered.iter().any(|c| c.name == "compact"),
            export_html: !discovered.iter().any(|c| c.name == "export-html"),
        }
    }
}

/// Whether an intercepted built-in ended the run or fell through.
pub(super) enum InterceptOutcome {
    /// Not a built-in command (or a discovered command owns the name): the
    /// normal prompt path runs.
    Passthrough,
    /// The built-in was dispatched and Done is already sent: the caller reaps
    /// the child and returns.
    Handled,
}

/// Does `prompt` invoke the built-in command `name` (exact `/name` or
/// `/name <rest>`)? Outer None = no match; inner Some = the non-empty
/// argument, None = no argument. Ordinary text is never matched.
pub fn builtin_match<'a>(prompt: &'a str, name: &str) -> Option<Option<&'a str>> {
    let slash = format!("/{name}");
    if prompt == slash {
        return Some(None);
    }
    prompt.strip_prefix(&format!("{slash} ")).map(|rest| {
        let rest = rest.trim();
        (!rest.is_empty()).then_some(rest)
    })
}

/// `compact` RPC params: the optional `/compact <instructions>` argument.
pub(super) fn compact_params(instructions: Option<&str>) -> Map<String, Value> {
    let mut params = Map::new();
    if let Some(instructions) = instructions {
        params.insert(
            "customInstructions".into(),
            Value::String(instructions.to_owned()),
        );
    }
    params
}

/// The transcript line a finished `compact` RPC reports.
pub(super) fn compact_summary(data: &Value) -> String {
    match (
        data.get("tokensBefore").and_then(Value::as_u64),
        data.get("estimatedTokensAfter").and_then(Value::as_u64),
    ) {
        (Some(before), Some(after)) => format!("Context compacted: {before} → {after} tokens"),
        _ => "Context compacted.".to_owned(),
    }
}

/// Response key a parked-turn built-in uses to hand its transcript line back
/// through the `idle_prompt` slot (a real `prompt` ACK never carries it).
pub(super) const BUILTIN_TEXT_KEY: &str = "cypherBuiltinText";

/// Pi's context gauge from a `get_session_stats` response. `tokens` is null
/// right after a compaction (pi cannot know until the next LLM response), so
/// the compaction's own estimate stands in when the caller has one.
fn context_usage_event(stats: &Value, fallback_used: Option<&Value>) -> Option<AgentEvent> {
    let usage = stats.get("contextUsage")?;
    let size = usage
        .get("contextWindow")
        .and_then(Value::as_f64)
        .filter(|size| *size > 0.0)?;
    let used = usage
        .get("tokens")
        .and_then(Value::as_f64)
        .or_else(|| fallback_used.and_then(Value::as_f64))?;
    Some(AgentEvent::ContextUsage {
        used: used.max(0.0).round() as u64,
        size: size.round() as u64,
    })
}

async fn read_context_usage(
    client: &PiClient,
    fallback_used: Option<&Value>,
) -> Option<AgentEvent> {
    let stats = client.request("get_session_stats", Map::new()).await.ok()?;
    context_usage_event(&stats, fallback_used)
}

/// Refresh the context gauge without stalling the event loop: the stats
/// request resolves on the client's reader task, and the reading lands as a
/// run-state event whenever it arrives (the engine never folds it).
pub(super) fn refresh_context_usage(
    client: &PiClient,
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    fallback_used: Option<Value>,
) {
    let client = client.clone();
    let event_tx = event_tx.clone();
    tokio::spawn(async move {
        if let Some(event) = read_context_usage(&client, fallback_used.as_ref()).await {
            let _ = send(&event_tx, event).await;
        }
    });
}

/// Emit a synthesized built-in command's terminal events: one TextDelta (also
/// pushed into Done's `result`) then Done{Completed}. The run ends
/// immediately — there is no agent stream for these commands, so the
/// no-activity grace never applies.
async fn finish_builtin(
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    session_file: &str,
    text: String,
    last_assistant_text: &mut String,
) {
    last_assistant_text.push_str(&text);
    let _ = send(event_tx, AgentEvent::TextDelta { text: text.clone() }).await;
    let _ = send(
        event_tx,
        AgentEvent::Done {
            status: DoneStatus::Completed,
            result: Some(text),
            error: None,
            session_id: Some(session_file.to_owned()),
        },
    )
    .await;
}

/// Dispatch a `/compact` or `/export-html` prompt over RPC (pi's built-in TUI
/// commands have RPC equivalents; the harness synthesizes and intercepts
/// them). On success one TextDelta + Done{Completed} end the run; a rejected
/// command is Done{Errored} whose error names the command and failure. Any
/// other prompt — or a same-name discovered command (extension wins) — is a
/// Passthrough.
pub(super) async fn intercept_builtin(
    client: &PiClient,
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    prompt: &str,
    session_file: &str,
    intercept: BuiltinIntercept,
    last_assistant_text: &mut String,
) -> InterceptOutcome {
    if intercept.compact
        && let Some(instructions) = builtin_match(prompt, "compact")
    {
        match client
            .request("compact", compact_params(instructions))
            .await
        {
            Ok(data) => {
                // The child is reaped right after this Done, so the gauge is
                // read inline rather than via the spawned refresh.
                if let Some(event) =
                    read_context_usage(client, data.get("estimatedTokensAfter")).await
                {
                    let _ = send(event_tx, event).await;
                }
                finish_builtin(
                    event_tx,
                    session_file,
                    compact_summary(&data),
                    last_assistant_text,
                )
                .await;
            }
            Err(e) => {
                // The request error already names the command (`compact: …`).
                let _ = send(
                    event_tx,
                    AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some(e.to_string()),
                        session_id: Some(session_file.to_owned()),
                    },
                )
                .await;
            }
        }
        return InterceptOutcome::Handled;
    }

    if intercept.export_html
        && let Some(path) = builtin_match(prompt, "export-html")
    {
        let mut params = Map::new();
        if let Some(path) = path {
            params.insert("outputPath".into(), Value::String(path.to_owned()));
        }
        match client.request("export_html", params).await {
            Ok(data) => {
                let text = data
                    .get("path")
                    .and_then(Value::as_str)
                    .map(|p| format!("Exported to {p}"))
                    .unwrap_or_else(|| "Exported.".to_owned());
                finish_builtin(event_tx, session_file, text, last_assistant_text).await;
            }
            Err(e) => {
                let _ = send(
                    event_tx,
                    AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some(e.to_string()),
                        session_id: Some(session_file.to_owned()),
                    },
                )
                .await;
            }
        }
        return InterceptOutcome::Handled;
    }

    InterceptOutcome::Passthrough
}

fn requested_model_parts(requested: &str) -> Result<(&str, &str), HarnessError> {
    let Some((provider, model_id)) = requested.split_once('/') else {
        return Err(HarnessError::Protocol(format!(
            "pi model must use provider/id form: {requested}"
        )));
    };
    if provider.is_empty() || model_id.is_empty() {
        return Err(HarnessError::Protocol(format!(
            "pi model must use provider/id form: {requested}"
        )));
    }
    Ok((provider, model_id))
}

fn state_model_key(state: &Value) -> Option<String> {
    let model = state.get("model")?;
    let provider = model.get("provider")?.as_str()?;
    let model_id = model.get("id")?.as_str()?;
    Some(format!("{provider}/{model_id}"))
}

fn state_uses_model(state: &Value, requested: &str) -> bool {
    state_model_key(state).as_deref() == Some(requested)
}

fn catalog_contains_model(catalog: &Value, provider: &str, model_id: &str) -> bool {
    catalog
        .get("models")
        .and_then(Value::as_array)
        .is_some_and(|models| {
            models.iter().any(|model| {
                model.get("provider").and_then(Value::as_str) == Some(provider)
                    && model.get("id").and_then(Value::as_str) == Some(model_id)
            })
        })
}

/// Re-select after `switch_session` only when the loaded session overrode the
/// launch model. Pi's RPC `set_model` consults an asynchronously populated
/// snapshot and can report "Model not found" during a cold start, so wait for
/// the exact catalog row before retrying. A user-selected model is never a
/// best-effort hint: exhaustion is a loud setup failure, not a silent fallback.
async fn select_requested_model(
    client: &PiClient,
    requested: &str,
    catalog_wait: Duration,
) -> Result<(), HarnessError> {
    let (provider, model_id) = requested_model_parts(requested)?;
    let set_params = || {
        let mut params = Map::new();
        params.insert("provider".into(), Value::String(provider.into()));
        params.insert("modelId".into(), Value::String(model_id.into()));
        params
    };
    let first_error = match client.request("set_model", set_params()).await {
        Ok(_) => return Ok(()),
        Err(err) => err,
    };
    if !first_error.to_string().contains("Model not found") {
        return Err(first_error);
    }

    let deadline = Instant::now() + catalog_wait;
    loop {
        let catalog = client.request("get_available_models", Map::new()).await?;
        if catalog_contains_model(&catalog, provider, model_id) {
            return client.request("set_model", set_params()).await.map(|_| ());
        }
        if Instant::now() >= deadline {
            return Err(HarnessError::Protocol(format!(
                "requested pi model {requested} was unavailable after {}s; \
                 initial set_model failed: {first_error}",
                catalog_wait.as_secs_f32()
            )));
        }
        tokio::time::sleep(MODEL_CATALOG_POLL).await;
    }
}

/// Handshake + session setup: resume the engine-provided session file, then
/// make the live model and thinking level match the request. Returns the
/// session file and the model's display name.
pub(super) async fn setup_session(
    client: &PiClient,
    request: &RunRequest,
    model_catalog_wait: Duration,
) -> Result<(String, String), HarnessError> {
    // Resume: switch to the engine-provided session file. Loud failure:
    // a stale/missing path must never silently start fresh.
    if let Some(path) = &request.resume {
        let mut params = Map::new();
        params.insert("sessionPath".into(), Value::String(path.clone()));
        if let Err(e) = client.request("switch_session", params).await {
            return Err(HarnessError::Protocol(format!(
                "pi session resume failed: {e} ({path})"
            )));
        }
    }
    // Fresh runs already launched with --model/--thinking, so there is no
    // default-model initialization followed by an unconditional switch.
    // A resumed session may restore its historical model; detect that
    // exact case and re-select, waiting out Pi's cold catalog snapshot.
    let mut state = client.request("get_state", Map::new()).await?;
    if let Some(requested) = request
        .model
        .as_deref()
        .filter(|model| concrete_model(model))
        && !state_uses_model(&state, requested)
    {
        select_requested_model(client, requested, model_catalog_wait).await?;
        state = client.request("get_state", Map::new()).await?;
        if !state_uses_model(&state, requested) {
            let actual = state_model_key(&state).unwrap_or_else(|| "<none>".into());
            return Err(HarnessError::Protocol(format!(
                "pi selected {actual} instead of requested model {requested}"
            )));
        }
    }
    if let Some(level) = request.reasoning
        && state_model_key(&state)
            .as_deref()
            .is_some_and(concrete_model)
    {
        let requested = thinking_level(level);
        if state.get("thinkingLevel").and_then(Value::as_str) != Some(requested) {
            let mut params = Map::new();
            params.insert("level".into(), Value::String(requested.into()));
            client.request("set_thinking_level", params).await?;
            state = client.request("get_state", Map::new()).await?;
            let actual_level = state.get("thinkingLevel").and_then(Value::as_str);
            // Older pi runtimes (and some provider adapters) normalize the
            // lowest setting from `minimal` to `low`.  That is a compatible
            // downgrade, not a protocol failure: rejecting it makes
            // best-effort jobs such as automatic chat titling fail even
            // though the model is ready to run.
            let compatible = actual_level == Some(requested)
                || (level == ReasoningLevel::Minimal && actual_level == Some("low"));
            if !compatible {
                let actual = actual_level.unwrap_or("<none>");
                return Err(HarnessError::Protocol(format!(
                    "pi selected thinking level {actual} instead of requested {requested}"
                )));
            }
        }
    }
    let session_file = state
        .get("sessionFile")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let model_name = state
        .get("model")
        .and_then(|m| m.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Ok((session_file, model_name))
}
