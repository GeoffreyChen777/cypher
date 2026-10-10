//! One live pi session: builtin slash-command intercepts, the UI-request bridge,
//! steer routing, model selection and the `run_session` event loop.

use super::*;

pub(super) struct Session {
    pub(super) child: Child,
    pub(super) client: PiClient,
    pub(super) incoming: mpsc::Receiver<Incoming>,
    pub(super) event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    pub(super) controls: RunControls,
    pub(super) request: RunRequest,
    pub(super) interrupt_grace: Duration,
    pub(super) kill_grace: Duration,
    pub(super) handshake_timeout: Duration,
    pub(super) no_activity_grace: Duration,
    pub(super) model_catalog_wait: Duration,
    pub(super) stderr_tail: crate::process::StderrTail,
    /// Which synthesized built-in commands this run intercepts (computed from
    /// the discovery cache at run start).
    pub(super) intercept: BuiltinIntercept,
    /// Temp file holding the child agent's persisted system prompt
    /// (`--append-system-prompt`), removed when the run ends.
    pub(super) temp_prompt: Option<PathBuf>,
    /// Configured MCP server names, read at run start, so MCP tool calls
    /// show the server under the name Settings uses ([`mcp_tool_parts`]).
    pub(super) mcp_servers: Vec<String>,
}

/// Which synthesized built-in commands a run intercepts. A same-name
/// extension/prompt/skill command wins and is left to pi itself; an
/// UNPOPULATED cache still intercepts — the popup's selections come from
/// `commands()`, which already deduped the synthesized entries, so a
/// `/compact` prompt without a discovered `compact` command can only be the
/// built-in.
#[derive(Clone, Copy, Default)]
pub(super) struct BuiltinIntercept {
    compact: bool,
    export_html: bool,
}

impl BuiltinIntercept {
    pub(super) fn all() -> Self {
        Self {
            compact: true,
            export_html: true,
        }
    }

    pub(super) fn from_probe(discovered: &[SlashCommand]) -> Self {
        Self {
            compact: !discovered.iter().any(|c| c.name == "compact"),
            export_html: !discovered.iter().any(|c| c.name == "export-html"),
        }
    }
}

/// Whether an intercepted built-in ended the run or fell through.
enum InterceptOutcome {
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
pub(super) fn builtin_match<'a>(prompt: &'a str, name: &str) -> Option<Option<&'a str>> {
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
fn compact_params(instructions: Option<&str>) -> Map<String, Value> {
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
fn compact_summary(data: &Value) -> String {
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
const BUILTIN_TEXT_KEY: &str = "cypherBuiltinText";

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
fn refresh_context_usage(
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
async fn intercept_builtin(
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

type RequestInputFn = Box<
    dyn Fn(
            Vec<UserInputQuestion>,
        ) -> tokio::sync::oneshot::Receiver<Vec<cypher_proto::UserInputAnswer>>
        + Send
        + Sync,
>;

/// The role of a `message_start` / `message_end` payload (assistant only —
/// toolResult/user messages are internal to the turn).
fn message_is_assistant(message: Option<&Value>) -> bool {
    message
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        .map(|r| r == "assistant")
        .unwrap_or(false)
}

/// The model that answered an assistant `message_end`: the provider's
/// `responseModel` (pi-ai sets it only when the response named a different
/// model than the request), else the requested `model`. `None` for a message
/// no model produced — an empty error/abort stand-in for a failed request.
fn answered_model(message: &Value) -> Option<AnsweredModel> {
    let text = |key: &str| {
        message
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let requested = text("model");
    if let Some(served) = text("responseModel") {
        return Some(AnsweredModel {
            model: served.to_owned(),
            requested: requested.filter(|r| *r != served).map(str::to_owned),
        });
    }
    let produced = message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|content| !content.is_empty());
    if !produced {
        return None;
    }
    Some(AnsweredModel {
        model: requested?.to_owned(),
        requested: None,
    })
}

/// One `extension_ui_request` dialog → the engine's input bridge. The bridge
/// answers with option labels; the response maps them back per method:
/// select/input/editor take a `value` (or `cancelled`), confirm a boolean.
/// A dropped resolver degrades to cancelled/`confirmed: false` — never a
/// silent pick. Fire-and-forget methods (notify/setStatus/setWidget/setTitle/
/// set_editor_text) never reach this — the caller maps notify itself and
/// ignores the transient TUI methods.
fn bridge_ui_request(
    client: &PiClient,
    request_input: std::sync::Arc<RequestInputFn>,
    id: &str,
    method: &str,
    payload: &Value,
) {
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Agent question")
        .to_owned();
    // The RPC fallback used by pi-ask-user has two dialog stages for the
    // TUI's in-place "optional comment" mode: select first, then input with
    // the original prompt plus a `Selected option(s):` section. Preserve that
    // semantic label in the Cypher question model so the second stage reads
    // like the TUI's comment editor instead of looking like a duplicate
    // question.
    let input_header = if matches!(method, "input" | "editor") {
        if title.contains("Selected option:") || title.contains("Selected options:") {
            "Optional comment"
        } else if method == "editor" {
            "Custom answer"
        } else {
            "Your answer"
        }
    } else {
        &title
    };
    let question = UserInputQuestion {
        // The extension request's own id: the answer comes back keyed on it.
        id: id.to_owned(),
        header: input_header.to_owned(),
        question: title.clone(),
        options: match method {
            "select" => payload
                .get("options")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            "confirm" => vec!["Confirm".into(), "Cancel".into()],
            // input/editor: free text. `prefill` is ignored — cypher's input
            // bridge has no prefilled-text slot.
            _ => Vec::new(),
        },
        multi_select: false,
    };
    let client = client.clone();
    let request_input = std::sync::Arc::clone(&request_input);
    // Owned copies for the spawned task (the caller's refs are not 'static).
    let id = id.to_owned();
    let method = method.to_owned();
    tokio::spawn(async move {
        let answers = (request_input)(vec![question.clone()])
            .await
            .unwrap_or_default();
        let picked = answers
            .iter()
            .find(|a| a.question_id == question.id)
            .and_then(|a| a.labels.first());
        client.respond_ui(
            &id,
            ui_response_payload(&method, picked.map(String::as_str)),
        );
    });
}

/// The `extension_ui_response` body for one answered dialog. No label at all
/// is the cancel signal. An EMPTY label is a real answer for input/editor —
/// pi-ask-user's optional comment submitted blank ("press Enter to skip")
/// must reach it as `""`, or pi resolves the input as cancelled and the
/// option the user picked in the stage before is thrown away. A select has
/// no empty option, so there an empty label still cancels.
pub(super) fn ui_response_payload(method: &str, picked: Option<&str>) -> Value {
    match method {
        "confirm" => json!({ "confirmed": picked == Some("Confirm") }),
        "input" | "editor" => match picked {
            Some(value) => json!({ "value": value }),
            None => json!({ "cancelled": true }),
        },
        _ => match picked.filter(|l| !l.is_empty()) {
            Some(value) => json!({ "value": value }),
            None => json!({ "cancelled": true }),
        },
    }
}

/// What pi did with an accepted `prompt` / `steer`. pi ≥ 0.99 reports it as
/// the response's `data.disposition`; older runtimes report nothing, and the
/// loop falls back to inferring it from its own view of the turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Disposition {
    /// pi was idle: the message started a fresh run.
    Started,
    /// pi was running: the message is queued as a steer and delivered at the
    /// run's next step (pi re-runs for queued input before it settles).
    Queued,
    /// An extension command or input handler consumed it: nothing reaches
    /// the model for it.
    Handled,
}

pub(super) fn disposition(data: &Value) -> Option<Disposition> {
    match data.get("disposition")?.as_str()? {
        "started" => Some(Disposition::Started),
        "queued" => Some(Disposition::Queued),
        "handled" => Some(Disposition::Handled),
        _ => None,
    }
}

/// A mailbox message routed into the live turn, awaiting its response —
/// [`Incoming::Response`] with this id, in order with the event stream.
struct RoutedSteer {
    id: String,
    text: String,
}

/// The next turn a parked run opens (the main loop's top branch).
enum NextTurn {
    /// A mailbox message to dispatch as a parked `prompt`.
    Prompt(String),
    /// A routed message pi already took after the turn it was meant to steer
    /// had ended: it started a fresh run, or an extension consumed it. The
    /// turn opens with nothing left to dispatch.
    Accepted,
}

/// Route a mailbox message into the live turn. A pi that reports
/// dispositions gets `prompt` with `streamingBehavior:"steer"` — atomic
/// across its real state, and the response says which way it went. Older
/// runtimes get a raw `steer`, whose fate the loop infers from the turn
/// state. Either response arrives in stdout order, so one that followed a
/// settle is always seen after it.
fn route_steer(client: &PiClient, text: String, atomic: bool) -> Option<RoutedSteer> {
    let mut params = Map::new();
    params.insert("message".into(), Value::String(text.clone()));
    let command = if atomic {
        params.insert("streamingBehavior".into(), Value::String("steer".into()));
        "prompt"
    } else {
        "steer"
    };
    match client.send_ordered(command, params) {
        Ok(id) => Some(RoutedSteer { id, text }),
        Err(e) => {
            // The child is gone; its EOF ends the run.
            tracing::debug!(target: "cypher_harness::pi", "steer not sent (dropped): {e}");
            None
        }
    }
}

/// One `Steered` boundary per routed message. The engine retires one
/// accepted mailbox message per boundary, so a message an extension consumed
/// must confirm too — otherwise the run's exit re-dispatches (re-runs) it.
async fn emit_boundaries(
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    assistant_message_id: &mut String,
    count: usize,
) -> bool {
    for _ in 0..count {
        let (prev, next) = rotate(assistant_message_id);
        let boundary = AgentEvent::Steered {
            assistant_message_id: Some(prev),
            next_assistant_message_id: Some(next),
        };
        if !send(event_tx, boundary).await {
            return false;
        }
    }
    true
}

/// Steers pi accepted but never delivered before the turn closed sit in its
/// queue, and pi drains that queue at the start of its next run — a retry
/// alone would deliver each one twice. Clear the queue first (pi applies
/// commands in order, so the clear lands before the retry), then retry each
/// as a parked prompt.
fn requeue_stranded(
    client: &PiClient,
    stranded: &mut VecDeque<String>,
    backlog: &mut VecDeque<NextTurn>,
) {
    if stranded.is_empty() {
        return;
    }
    client.send("clear_queue", Map::new());
    backlog.extend(stranded.drain(..).map(NextTurn::Prompt));
}

/// Owns the child prompt temp file for the run's lifetime: removing it on
/// drop covers every `run_session` return path (early errors included).
struct TempPromptGuard(Option<PathBuf>);

impl Drop for TempPromptGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
            let _ = path.parent().map(std::fs::remove_dir);
        }
    }
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
async fn setup_session(
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

/// The structured live projection a cypher `setStatus` key carries. Strictly
/// validated: any other key — or an invalid snapshot — stays TUI furniture
/// (ignored).
fn status_event(key: &str, text: &str) -> Option<AgentEvent> {
    match key {
        SUBAGENTS_STATUS_KEY => {
            parse_subagent_status(text).map(|runs| AgentEvent::SubagentStatus { runs })
        }
        TRANSLATION_STATUS_KEY => {
            parse_translation_status(text).map(|text| AgentEvent::Translation { text })
        }
        INPUT_TRANSLATION_STATUS_KEY => parse_input_translation_status(text)
            .map(|(source, text)| AgentEvent::InputTranslation { source, text }),
        _ => None,
    }
}

/// Close the current turn: confirm the routed messages an extension
/// consumed, requeue steers pi only queued (retried after the park), then
/// send the turn's Done. False once the consumer has hung up.
#[allow(clippy::too_many_arguments)]
async fn park_turn(
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    client: &PiClient,
    assistant_message_id: &mut String,
    handled_steers: &mut usize,
    steers_queued: &mut VecDeque<String>,
    prompt_backlog: &mut VecDeque<NextTurn>,
    last_assistant_text: &str,
    status: DoneStatus,
    error: Option<String>,
    session_file: &str,
) -> bool {
    if !emit_boundaries(
        event_tx,
        assistant_message_id,
        std::mem::take(handled_steers),
    )
    .await
    {
        return false;
    }
    requeue_stranded(client, steers_queued, prompt_backlog);
    let result = (!last_assistant_text.is_empty()).then(|| last_assistant_text.to_owned());
    send(
        event_tx,
        AgentEvent::Done {
            status,
            result,
            error,
            session_id: Some(session_file.to_owned()),
        },
    )
    .await
}

/// What the main loop does after an arm.
enum Flow {
    Continue,
    Break,
}

/// The main loop's state: the run's fixed context plus everything the
/// `select!` arms mutate. One method per arm, each returning whether the run
/// goes on.
struct PiRun {
    client: PiClient,
    event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    session_file: String,
    mcp_servers: Vec<String>,
    request_input: std::sync::Arc<RequestInputFn>,
    intercept: BuiltinIntercept,
    no_activity_grace: Duration,
    interrupt_grace: Duration,
    kill_grace: Duration,
    assistant_message_id: String,
    /// The current assistant message's streamed text (Done's `result` and
    /// the error text for an `error` stopReason).
    last_assistant_text: String,
    /// The last assistant message's stopReason ("stop"/"length"/"error"/
    /// "aborted"); Completed for anything but error/aborted.
    last_stop_reason: String,
    last_error_message: Option<String>,
    interrupted: bool,
    interrupt_sent: bool,
    done_sent: bool,
    /// Live-progress throttle: toolCallId → last FORWARDED ToolProgress. A
    /// tool_execution_update is forwarded only when ≥[`PROGRESS_THROTTLE`]
    /// elapsed since the last forward for that id (first always forwards).
    /// `progress_ended` marks tools whose end we've seen — late/duplicate
    /// updates after end are dropped (the doc fold would ignore them anyway
    /// once resolved; this stops the harness from even emitting them).
    progress_last: HashMap<String, Instant>,
    progress_ended: HashSet<String>,
    /// The working trailer's tok/s, estimated from the streamed deltas.
    throughput: throughput::ThroughputMeter,
    /// False until the FIRST agent event of any kind arrives. While it stays
    /// false the run is proven inert (no agent activity, e.g. an extension
    /// command whose handler only notifies) and the `no_activity` timer ends it.
    agent_started: bool,
    in_turn: bool,
    steering_open: bool,
    /// Steers pi has QUEUED but not yet delivered (one per assistant message;
    /// pi's default steering mode is one-at-a-time). Texts are kept so a steer
    /// the turn settles before delivering can be retried as an idle prompt
    /// (an idle pi only QUEUES steers).
    steers_queued: VecDeque<String>,
    /// Routed messages an extension consumed mid-turn (`handled`). Each still
    /// owes the engine its Steered boundary: at the next assistant message (the
    /// next step's output belongs below the message), or before the turn's Done.
    handled_steers: usize,
    /// The in-flight routed steer (its response arrives in order on
    /// `incoming`), plus followers awaiting their turn.
    steer_call: Option<RoutedSteer>,
    steer_backlog: VecDeque<String>,
    /// Whether this pi reports dispositions (≥ 0.99), learned from the first
    /// prompt's response — which always resolves before any mid-turn routing.
    /// Mid-turn messages then ride an atomic `prompt` instead of a raw `steer`.
    atomic_steer: bool,
    /// In-flight `prompt` RPC: the first turn starts here, and parked-turn
    /// restarts reuse the same slot. Serialized with steer calls (never both
    /// in flight); followers queue in `prompt_backlog` until the turn settles.
    idle_prompt: Option<BoxFuture<'static, Result<Value, HarnessError>>>,
    /// True if a blocking dialog (select/input/editor/confirm) or a notify
    /// landed while the prompt RPC was in flight. Real pi only ACKs extension
    /// commands after the handler returns, so an ACK with that UI and no agent
    /// lifecycle means the command is done — do not wait the 2s no-activity
    /// grace (that spin after closing a picker). Transient TUI furniture
    /// (`setStatus`/`setWidget`/`setTitle`/`set_editor_text`) never counts:
    /// the goal, MCP and subagents extensions push status updates at startup
    /// and mid-turn, and treating those as "UI happened" collapsed the grace
    /// to zero on ordinary prompts, so the harness Done'd the turn before the
    /// agent's first event.
    had_ui: bool,
    /// The zero-grace shortcut is for extension slash commands only: a plain
    /// prompt always starts an agent turn, so it keeps the full grace even if
    /// a real dialog fires during its preflight.
    /// Runtimes without dispositions only leave the slash prefix to go on.
    prompt_is_command: bool,
    prompt_backlog: VecDeque<NextTurn>,
    /// Interrupt escalation: abort, then SIGTERM → SIGKILL if the agent
    /// doesn't wind down.
    escalation: Option<tokio::task::JoinHandle<()>>,
    /// Started when the main loop begins (right after the prompt was accepted):
    /// if pi never emits a single agent event, the run terminates with
    /// Done{Completed} rather than sit "Working" forever. Any agent-lifecycle
    /// event disarms it (informational events do not). Late sendMessage work
    /// arriving after this fires is dropped — a documented degradation. A
    /// parked-turn restart re-arms it (a fresh sleep) only once its prompt is
    /// ACCEPTED.
    no_activity: std::pin::Pin<Box<tokio::time::Sleep>>,
}

impl PiRun {
    /// Parked with a queued mailbox message: open the next turn. `None`
    /// proceeds to the loop's `select!`.
    async fn open_parked_turn(&mut self) -> Option<Flow> {
        // Parked with a queued mailbox message: start the NEXT turn via RPC
        // `prompt` with `streamingBehavior:"steer"` — atomic across pi's
        // real state: a truly idle pi starts a fresh turn; a pi still (or
        // newly) active queues the message as a steer instead of rejecting
        // the prompt (the confirmed parked-session wedge). A raw `steer` is
        // never sent idle — a parked pi only QUEUES steers, so it would
        // strand forever. The Steered boundary fires BEFORE the prompt is
        // dispatched: an extension notify can land before the prompt response
        // and must fold into the new turn's segment. If the boundary cannot
        // be sent, the run is over — do not dispatch. A routed message pi
        // already took (`NextTurn::Accepted`) opens its turn the same way,
        // minus the dispatch.
        if !self.in_turn
            && self.idle_prompt.is_none()
            && self.steer_call.is_none()
            && self.steers_queued.is_empty()
            && let Some(turn) = self.prompt_backlog.pop_front()
        {
            let (prev, next) = rotate(&mut self.assistant_message_id);
            if !send(
                &self.event_tx,
                AgentEvent::Steered {
                    assistant_message_id: Some(prev),
                    next_assistant_message_id: Some(next),
                },
            )
            .await
            {
                return Some(Flow::Break);
            }
            // Per-turn reset: Done's result/status, the progress throttle,
            // and activity tracking all belong to the new turn.
            self.last_assistant_text.clear();
            self.last_stop_reason = "stop".to_owned();
            self.last_error_message = None;
            self.agent_started = false;
            self.done_sent = false;
            self.in_turn = true;
            self.progress_last.clear();
            self.progress_ended.clear();
            self.throughput.start_turn();
            self.had_ui = false;
            let text = match turn {
                NextTurn::Prompt(text) => text,
                NextTurn::Accepted => {
                    // Already accepted: arm the grace as the prompt arm
                    // would. A fresh run's first event disarms it; a
                    // consumed message settles through it.
                    self.prompt_is_command = false;
                    self.no_activity = Box::pin(tokio::time::sleep(self.no_activity_grace));
                    return Some(Flow::Continue);
                }
            };
            // The no-activity timer is NOT armed here: the previous turn's
            // sleep may already have elapsed and must not fire during this
            // prompt's preflight. The `idle_prompt.is_none()` guard keeps the
            // branch disabled while the request is in flight; the resolution
            // re-arms it once accepted (lifecycle events that landed first
            // disarm it via agent_started).
            self.prompt_is_command = text.trim_start().starts_with('/');
            let prompt_client = self.client.clone();
            // A parked `/compact` is the built-in too: pi's `prompt` never
            // runs TUI built-ins and would hand the text to the model. The
            // RPC's summary rides back through the prompt slot; the accept
            // arm streams it and the zero grace settles the turn.
            let compact = self
                .intercept
                .compact
                .then(|| builtin_match(&text, "compact"))
                .flatten()
                .map(|instructions| compact_params(instructions));
            self.idle_prompt = Some(match compact {
                Some(params) => Box::pin(async move {
                    let data = prompt_client.request("compact", params).await?;
                    Ok(json!({ BUILTIN_TEXT_KEY: compact_summary(&data) }))
                }),
                None => {
                    let mut params = Map::new();
                    params.insert("message".into(), Value::String(text));
                    // `streamingBehavior:"steer"` makes the parked restart
                    // atomic: an idle pi starts a fresh turn, a
                    // still-streaming pi queues the message as a steer — a
                    // plain prompt would be REJECTED while pi streams (the
                    // confirmed parked-session wedge).
                    params.insert("streamingBehavior".into(), Value::String("steer".into()));
                    Box::pin(async move { prompt_client.request("prompt", params).await })
                }
            });
        }
        None
    }

    /// The in-flight `prompt` RPC resolved.
    async fn on_prompt_response(&mut self, res: Result<Value, HarnessError>) -> Flow {
        let _ = self.idle_prompt.take();
        // The response only means the prompt was ACCEPTED — the turn
        // streams from here. A rejected prompt is the one case
        // nothing will ever stream for it: one Done Errored ends the
        // run (and the terminal bookkeeping reaps the child).
        match res {
            Ok(data) if data.get(BUILTIN_TEXT_KEY).is_some() => {
                // A parked built-in finished: its line is the turn's
                // whole output, and the zero grace settles it through
                // the no-activity arm (Done + park, like any inert
                // command).
                let text = data[BUILTIN_TEXT_KEY]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                self.last_assistant_text.push_str(&text);
                if !send(&self.event_tx, AgentEvent::TextDelta { text }).await {
                    return Flow::Break;
                }
                self.no_activity = Box::pin(tokio::time::sleep(Duration::ZERO));
            }
            Ok(data) => {
                let disposition = disposition(&data);
                self.atomic_steer |= disposition.is_some();
                // Arm the no-activity grace NOW that the prompt is
                // accepted. Lifecycle events that landed during the
                // preflight already set agent_started (disarming the
                // branch); a genuinely inert (notify-only) turn gets
                // a fresh grace window from here — never a stale
                // timer from the previous turn.
                // Extension commands ACK only after the handler
                // returns. If a command already showed a dialog or
                // notified and no agent started, skip the 2s wait
                // (close-picker spin). Zero-sleep still yields to
                // `incoming` first (biased select) so a
                // ui-select-then-ACK-then-text burst is not cut off.
                // pi names a consumed prompt `handled`; a `started`
                // one (a skill or prompt template, despite its slash)
                // always gets the full grace.
                let is_command =
                    disposition.map_or(self.prompt_is_command, |d| d == Disposition::Handled);
                let grace = if self.had_ui && is_command && !self.agent_started {
                    Duration::ZERO
                } else {
                    self.no_activity_grace
                };
                self.no_activity = Box::pin(tokio::time::sleep(grace));
            }
            Err(e) => {
                self.done_sent = true;
                let _ = self
                    .event_tx
                    .send(Ok(AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some(e.to_string()),
                        session_id: Some(self.session_file.clone()),
                    }))
                    .await;
                return Flow::Break;
            }
        }
        Flow::Continue
    }

    async fn on_event(&mut self, ev: Value) -> Flow {
        // Only AGENT-LIFECYCLE events prove a turn is running and
        // disarm the no-activity grace. Informational events fire
        // outside any turn — `thinking_level_changed` rides the
        // set_model/set_thinking_level setup commands (live-verified:
        // it is what hung /subagents runs), `extension_error` can
        // arrive from extension activity — counting either would
        // leave a no-LLM run parked "Working" forever.
        let kind = ev.get("type").and_then(Value::as_str).unwrap_or("");
        if matches!(
            kind,
            "agent_start"
                | "turn_start"
                | "message_start"
                | "message_update"
                | "message_end"
                | "tool_execution_start"
                | "tool_execution_update"
                | "tool_execution_end"
                | "agent_end"
                | "agent_settled"
        ) {
            self.agent_started = true;
        }
        match kind {
            "message_update" => self.on_message_update(&ev).await,
            "message_start" => self.on_message_start(&ev).await,
            "message_end" => self.on_message_end(&ev).await,
            "tool_execution_start" => self.on_tool_start(&ev).await,
            "tool_execution_update" => self.on_tool_update(&ev).await,
            "tool_execution_end" => self.on_tool_end(&ev).await,
            "extension_error" => self.on_extension_error(&ev).await,
            "agent_settled" => self.on_agent_settled().await,
            // Manual (parked `/compact`) and automatic compactions
            // both end here: re-read the gauge, falling back to the
            // compaction's own estimate while pi reports none.
            "compaction_end" => {
                let estimate = ev
                    .get("result")
                    .and_then(|r| r.get("estimatedTokensAfter"))
                    .cloned();
                refresh_context_usage(&self.client, &self.event_tx, estimate);
                Flow::Continue
            }
            // agent_end/turn_*/queue_update/compaction_start/auto_retry_*/
            // summarization_*/bash_execution_update: nothing cypher
            // renders — ignored.
            _ => Flow::Continue,
        }
    }

    /// A streamed delta: text, thinking or tool-call arguments.
    async fn on_message_update(&mut self, ev: &Value) -> Flow {
        let ame = ev.get("assistantMessageEvent");
        match ame.and_then(|a| a.get("type")).and_then(Value::as_str) {
            Some("text_delta") => {
                if let Some(text) = ame.and_then(|a| a.get("delta")).and_then(Value::as_str) {
                    self.last_assistant_text.push_str(text);
                    if !text.is_empty()
                        && !send(
                            &self.event_tx,
                            AgentEvent::TextDelta {
                                text: text.to_owned(),
                            },
                        )
                        .await
                    {
                        return Flow::Break;
                    }
                }
            }
            Some("thinking_delta") => {
                if let Some(text) = ame.and_then(|a| a.get("delta")).and_then(Value::as_str)
                    && !text.is_empty()
                    && !send(
                        &self.event_tx,
                        AgentEvent::ReasoningDelta {
                            text: text.to_owned(),
                        },
                    )
                    .await
                {
                    return Flow::Break;
                }
            }
            // *_start/*_end/toolcall_*: internal state only.
            _ => {}
        }
        // Every streamed delta is output — text,
        // thinking, and tool-call arguments alike.
        let kind = ame.and_then(|a| a.get("type")).and_then(Value::as_str);
        let delta = ame.and_then(|a| a.get("delta")).and_then(Value::as_str);
        if let (Some(kind @ ("text_delta" | "thinking_delta" | "toolcall_delta")), Some(delta)) =
            (kind, delta)
            && let Some(reading) =
                self.throughput
                    .delta(delta, kind == "thinking_delta", Instant::now())
            && !send(
                &self.event_tx,
                AgentEvent::Throughput {
                    throughput: Some(reading),
                },
            )
            .await
        {
            return Flow::Break;
        }
        Flow::Continue
    }

    /// A message opened; an assistant one starts a fresh text accumulator.
    async fn on_message_start(&mut self, ev: &Value) -> Flow {
        // A new assistant message starts a fresh text
        // accumulator — Done's `result` is the LAST
        // assistant message's text, not the whole turn's.
        // (toolResult/user messages are internal.)
        if message_is_assistant(ev.get("message")) {
            self.last_assistant_text.clear();
            self.throughput.start_message(Instant::now());
            // The NEXT assistant message after a queued
            // steer is the steer's reply: split the doc entry
            // here (before its content streams). Messages an
            // extension consumed split here too.
            let delivered = self.steers_queued.pop_front().is_some();
            if delivered {
                // A steer delivery opens a turn: even if
                // an agent_settled raced ahead of it, the
                // steer reply's own settle must Done.
                self.in_turn = true;
            }
            let boundaries = std::mem::take(&mut self.handled_steers) + usize::from(delivered);
            if !emit_boundaries(&self.event_tx, &mut self.assistant_message_id, boundaries).await {
                return Flow::Break;
            }
        }
        Flow::Continue
    }

    /// A message closed; an assistant one records its outcome and rotates the id.
    async fn on_message_end(&mut self, ev: &Value) -> Flow {
        if message_is_assistant(ev.get("message")) {
            refresh_context_usage(&self.client, &self.event_tx, None);
            // The reported count replaces the estimate;
            // the rate drops until the next message.
            let usage = ev.get("message").and_then(|m| m.get("usage"));
            let count = |key: &str| usage.and_then(|u| u.get(key)).and_then(Value::as_u64);
            let reading = self
                .throughput
                .end_message(count("output"), count("reasoning"));
            if !send(
                &self.event_tx,
                AgentEvent::Throughput {
                    throughput: Some(reading),
                },
            )
            .await
            {
                return Flow::Break;
            }
            if let Some(message) = ev.get("message") {
                if let Some(stop) = message.get("stopReason").and_then(Value::as_str) {
                    self.last_stop_reason = stop.to_owned();
                }
                if let Some(err) = message.get("errorMessage").and_then(Value::as_str) {
                    self.last_error_message = Some(err.to_owned());
                }
            }
            // Journal boundary marker: the doc fold treats
            // this as a no-op (one segment per turn until
            // a Steered/Done), but the journal records it
            // per assistant message. Rotate the id for the
            // next one.
            let completed = self.assistant_message_id.clone();
            rotate(&mut self.assistant_message_id);
            if !send(
                &self.event_tx,
                AgentEvent::AssistantMessageCompleted {
                    assistant_message_id: completed,
                    model: ev.get("message").and_then(answered_model),
                },
            )
            .await
            {
                return Flow::Break;
            }
        }
        Flow::Continue
    }

    async fn on_tool_start(&mut self, ev: &Value) -> Flow {
        let id = ev
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let name = ev
            .get("toolName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let args = ev.get("args").cloned().unwrap_or(Value::Null);
        if !send(
            &self.event_tx,
            AgentEvent::ToolCall {
                id,
                call: pi_typed_call(&name, &args, &self.mcp_servers),
            },
        )
        .await
        {
            return Flow::Break;
        }
        Flow::Continue
    }

    async fn on_tool_update(&mut self, ev: &Value) -> Flow {
        // Live progress for an unresolved tool: same
        // `{content:[{type:"text",...}]}` shape as the
        // end result, so `tool_output_text` (which caps
        // at OUTPUT_CAP) extracts it. Throttled per
        // toolCallId (first always forwards) — the doc
        // fold only ever keeps the last 8 lines, so
        // forwarding every partial chunk is pure churn.
        let id = ev
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let output = ev.get("partialResult").and_then(tool_output_text);
        if let (Some(output), false) = (output, id.is_empty())
            && !self.progress_ended.contains(&id)
        {
            let now = Instant::now();
            let due = self
                .progress_last
                .get(&id)
                .is_none_or(|last| now.duration_since(*last) >= PROGRESS_THROTTLE);
            if due {
                self.progress_last.insert(id.clone(), now);
                if !send(&self.event_tx, AgentEvent::ToolProgress { id, output }).await {
                    return Flow::Break;
                }
            }
        }
        Flow::Continue
    }

    async fn on_tool_end(&mut self, ev: &Value) -> Flow {
        let id = ev
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let is_error = ev.get("isError").and_then(Value::as_bool).unwrap_or(false);
        let output = ev.get("result").and_then(|result| {
            if ev.get("toolName").and_then(Value::as_str) == Some(cypher_proto::view::CODEMODE_TOOL)
            {
                tool_output_text(&without_codemode_header(result))
            } else {
                tool_output_text(result)
            }
        });
        // The tool settled: retire its throttle entry (no
        // more progress) and stop forwarding late updates.
        self.progress_last.remove(&id);
        self.progress_ended.insert(id.clone());
        if !send(
            &self.event_tx,
            AgentEvent::ToolResult {
                id,
                is_error,
                output,
                diff: None,
            },
        )
        .await
        {
            return Flow::Break;
        }
        Flow::Continue
    }

    async fn on_extension_error(&mut self, ev: &Value) -> Flow {
        let message = ev
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("extension error")
            .to_owned();
        if !send(&self.event_tx, AgentEvent::Error { message }).await {
            return Flow::Break;
        }
        Flow::Continue
    }

    /// The turn settled: send its Done, then park or end the run.
    async fn on_agent_settled(&mut self) -> Flow {
        // A stale duplicate (or an abort racing a settled
        // turn) must not double-Done.
        if !self.in_turn {
            return Flow::Continue;
        }
        self.in_turn = false;
        // The settled branch is final: its gauge reading
        // supersedes any per-message one that raced a
        // session write.
        refresh_context_usage(&self.client, &self.event_tx, None);
        self.done_sent = true;
        let (status, error) = if self.interrupted {
            (DoneStatus::Interrupted, None)
        } else {
            match self.last_stop_reason.as_str() {
                "error" => (
                    DoneStatus::Errored,
                    Some(
                        self.last_error_message
                            .clone()
                            .filter(|m| !m.trim().is_empty())
                            .or_else(|| {
                                (!self.last_assistant_text.is_empty())
                                    .then(|| self.last_assistant_text.clone())
                            })
                            .unwrap_or_else(|| "The agent reported an error.".into()),
                    ),
                ),
                "aborted" => (DoneStatus::Interrupted, None),
                _ => (DoneStatus::Completed, None),
            }
        };
        // Messages an extension consumed confirm before
        // the Done (the last segment then ends empty).
        // Steers pi queued but never delivered (the turn
        // settled before the steer reply streamed) are
        // stranded — an idle pi only QUEUES steers. They
        // retry as idle prompts after the park, never
        // dropped.
        if !park_turn(
            &self.event_tx,
            &self.client,
            &mut self.assistant_message_id,
            &mut self.handled_steers,
            &mut self.steers_queued,
            &mut self.prompt_backlog,
            &self.last_assistant_text,
            status,
            error,
            &self.session_file,
        )
        .await
        {
            return Flow::Break;
        }
        // An interrupt or an errored turn ends the run; a
        // clean turn parks the child + mailbox for the
        // next routed send. With the mailbox closed AND
        // nothing left to dispatch, the run ends here.
        if self.interrupted
            || status == DoneStatus::Errored
            || (!self.steering_open
                && self.idle_prompt.is_none()
                && self.steer_call.is_none()
                && self.steers_queued.is_empty()
                && self.prompt_backlog.is_empty())
        {
            return Flow::Break;
        }
        Flow::Continue
    }

    /// A routed steer's response, in order with the events: any settle pi
    /// wrote before it has already been handled.
    async fn on_steer_response(&mut self, id: String, result: Result<Value, String>) -> Flow {
        let Some(call) = self.steer_call.take_if(|call| call.id == id) else {
            return Flow::Continue;
        };
        if !self.interrupted {
            match result.map(|data| disposition(&data)) {
                // pi was running: delivered at its next step,
                // where the Steered boundary fires.
                Ok(Some(Disposition::Queued)) => self.steers_queued.push_back(call.text),
                Ok(Some(Disposition::Handled)) if self.in_turn => self.handled_steers += 1,
                // pi was idle — the turn this message meant to
                // steer had ended — so it started a fresh run, or
                // an extension took it. Either way it opens the
                // next turn; nothing is retried.
                Ok(Some(Disposition::Started | Disposition::Handled)) => {
                    if self.in_turn {
                        // pi is idle, yet the turn is open: only an
                        // inert one (command output, no agent run)
                        // — any run's settle would have preceded
                        // this response. Close it as its grace would.
                        self.in_turn = false;
                        self.done_sent = true;
                        if !park_turn(
                            &self.event_tx,
                            &self.client,
                            &mut self.assistant_message_id,
                            &mut self.handled_steers,
                            &mut self.steers_queued,
                            &mut self.prompt_backlog,
                            &self.last_assistant_text,
                            DoneStatus::Completed,
                            None,
                            &self.session_file,
                        )
                        .await
                        {
                            return Flow::Break;
                        }
                    }
                    self.prompt_backlog.push_front(NextTurn::Accepted);
                }
                // A runtime without dispositions: a steer accepted
                // in a live turn is delivered at its next step...
                Ok(None) if self.in_turn => self.steers_queued.push_back(call.text),
                // ...but one the turn settled ahead of is stranded
                // (an idle pi only QUEUES steers).
                Ok(None) => requeue_stranded(
                    &self.client,
                    &mut VecDeque::from([call.text]),
                    &mut self.prompt_backlog,
                ),
                Err(e) if self.in_turn => {
                    tracing::debug!(
                        target: "cypher_harness::pi",
                        "steer rejected (dropped): {e}"
                    );
                }
                // Rejected once the turn had ended: restart with
                // it like any parked message — a real failure
                // surfaces there as that turn's error.
                Err(_) => self.prompt_backlog.push_back(NextTurn::Prompt(call.text)),
            }
        }
        if !self.in_turn {
            // Parked: queued followers can't be delivered as
            // steers — they restart the next turn via prompt.
            self.prompt_backlog
                .extend(self.steer_backlog.drain(..).map(NextTurn::Prompt));
        } else if let Some(text) = self.steer_backlog.pop_front() {
            self.steer_call = route_steer(&self.client, text, self.atomic_steer);
        }
        if !self.steering_open
            && !self.in_turn
            && self.steers_queued.is_empty()
            && self.steer_call.is_none()
            && self.prompt_backlog.is_empty()
        {
            return Flow::Break;
        }
        Flow::Continue
    }

    async fn on_ui_request(&mut self, id: String, method: String, payload: Value) -> Flow {
        match method.as_str() {
            // Dialog methods block the agent until answered.
            "select" | "input" | "editor" | "confirm" => {
                self.had_ui = true;
                bridge_ui_request(
                    &self.client,
                    std::sync::Arc::clone(&self.request_input),
                    &id,
                    &method,
                    &payload,
                );
            }
            // notify is the extension command's output channel:
            // info/warning maps to a TextDelta (and feeds Done's
            // result, so a notify-only run still carries its
            // output), error to an Error event. The message lives
            // at the request top level and may carry escaped
            // multi-line text — passed through as-is.
            "notify" => {
                self.had_ui = true;
                let message = payload
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let is_error = payload
                    .get("notifyType")
                    .and_then(Value::as_str)
                    .map(|t| t == "error")
                    .unwrap_or(false);
                if is_error {
                    if !send(&self.event_tx, AgentEvent::Error { message }).await {
                        return Flow::Break;
                    }
                } else if !message.is_empty() {
                    self.last_assistant_text.push_str(&message);
                    if !send(&self.event_tx, AgentEvent::TextDelta { text: message }).await {
                        return Flow::Break;
                    }
                }
            }
            // setStatus with the cypher subagent status key is the
            // one exception to the transient-TUI-furniture rule:
            // a STRUCTURED live projection (`cypher.subagents.v1`
            // snapshot JSON in `statusText`) that the engine
            // consumes. Strictly validated; any other key — or an
            // invalid snapshot — stays ignored and can never
            // interrupt the run.
            "setStatus" => {
                let key = payload
                    .get("statusKey")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let text = payload
                    .get("statusText")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(event) = status_event(key, text)
                    && !send(&self.event_tx, event).await
                {
                    return Flow::Break;
                }
            }
            // Deliberate: setWidget/setTitle/set_editor_text (and
            // any non-cypher setStatus) are transient TUI furniture
            // — cypher has its own state surface (see the
            // classification table in docs/research/pi-rpc.md).
            _ => {}
        }
        Flow::Continue
    }

    async fn on_eof(
        &mut self,
        child: &mut Child,
        stderr_tail: &crate::process::StderrTail,
        agent_name: &str,
    ) -> Flow {
        // A child death while PARKED (turn already settled) ends
        // the run cleanly — the engine treats a parked stream end
        // as such. Mid-turn, it is a crash.
        if self.done_sent && !self.in_turn {
            return Flow::Break;
        }
        if self.interrupted {
            self.done_sent = true;
            let _ = self
                .event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: Some(self.session_file.clone()),
                }))
                .await;
        } else {
            self.done_sent = true;
            let status = child.try_wait().ok().flatten();
            let _ = self
                .event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(crash_message(agent_name, status, stderr_tail)),
                    session_id: Some(self.session_file.clone()),
                }))
                .await;
        }
        Flow::Break
    }

    /// A mailbox message, or the mailbox closing.
    fn on_steer(&mut self, steer: Option<crate::SteerMessage>) -> Flow {
        match steer {
            Some(msg) => {
                if !self.in_turn || self.idle_prompt.is_some() {
                    // Parked (or a parked-turn prompt still in preflight):
                    // this mailbox message starts a NEW turn via RPC
                    // prompt — a parked pi only QUEUES steers, so sending
                    // one idle would strand it forever. Followers queue
                    // for after the turn settles (never concurrent with
                    // the in-flight prompt).
                    self.prompt_backlog.push_back(NextTurn::Prompt(msg.prompt));
                } else {
                    // Active turn: pi-native mid-run steer — delivered
                    // after the current assistant message's tool calls,
                    // before the next LLM call. One in flight at a time.
                    if self.steer_call.is_some() {
                        self.steer_backlog.push_back(msg.prompt);
                    } else {
                        self.steer_call = route_steer(&self.client, msg.prompt, self.atomic_steer);
                    }
                }
            }
            None => {
                self.steering_open = false;
                if !self.in_turn
                    && self.steers_queued.is_empty()
                    && self.steer_call.is_none()
                    && self.prompt_backlog.is_empty()
                {
                    return Flow::Break;
                }
            }
        }
        Flow::Continue
    }

    fn on_interrupt(&mut self, child: &Child) -> Flow {
        self.interrupt_sent = true;
        self.interrupted = true;
        if self.in_turn {
            self.client.send("abort", Map::new());
            // Escalate if pi doesn't wind down (agent_settled) within
            // the grace periods.
            if let Some(pid) = child.id() {
                let (interrupt_grace, kill_grace) = (self.interrupt_grace, self.kill_grace);
                self.escalation = Some(tokio::spawn(async move {
                    tokio::time::sleep(interrupt_grace).await;
                    send_signal(pid, Signal::Term);
                    tokio::time::sleep(kill_grace).await;
                    send_signal(pid, Signal::Kill);
                }));
            }
        } else {
            // Idle between turns: nothing to abort — the terminal
            // bookkeeping below still guarantees Done { Interrupted }.
            return Flow::Break;
        }
        Flow::Continue
    }

    async fn on_no_activity(&mut self) -> Flow {
        // The prompt was accepted but no agent event ever arrived
        // (an extension command whose handler only notifies, say).
        // Settle this TURN with the notify output, but keep a
        // steerable child parked exactly like `agent_settled`.
        // Stateful extension commands can keep toggles in process
        // memory; reaping here made every invocation start from the
        // configured default instead of observing the previous turn.
        self.in_turn = false;
        self.done_sent = true;
        // The turn's routed messages settle exactly as at
        // `agent_settled`: consumed ones confirm, and a steer an idle
        // pi only queued retries after the park.
        if !park_turn(
            &self.event_tx,
            &self.client,
            &mut self.assistant_message_id,
            &mut self.handled_steers,
            &mut self.steers_queued,
            &mut self.prompt_backlog,
            &self.last_assistant_text,
            DoneStatus::Completed,
            None,
            &self.session_file,
        )
        .await
        {
            return Flow::Break;
        }
        // If nobody can route another turn, there is no reason to
        // retain the child. Otherwise remain parked until a mailbox
        // prompt, interrupt, child exit, or sender close arrives.
        if !self.steering_open
            && self.idle_prompt.is_none()
            && self.steer_call.is_none()
            && self.steers_queued.is_empty()
            && self.prompt_backlog.is_empty()
        {
            return Flow::Break;
        }
        Flow::Continue
    }
}

/// The in-flight `prompt` RPC's response (the branch is guarded on `Some`).
async fn prompt_response(
    slot: &mut Option<BoxFuture<'static, Result<Value, HarnessError>>>,
) -> Result<Value, HarnessError> {
    slot.as_mut().expect("guarded by if").await
}

/// The per-run event loop: one task multiplexing agent events, the steering
/// mailbox, the interrupt token, and consumer liveness.
pub(super) async fn run_session(session: Session) {
    let Session {
        mut child,
        client,
        mut incoming,
        event_tx,
        controls,
        request,
        interrupt_grace,
        kill_grace,
        handshake_timeout,
        no_activity_grace,
        model_catalog_wait,
        stderr_tail,
        intercept,
        temp_prompt,
        mcp_servers,
    } = session;
    // Dropped at the end of every path — the temp prompt file never leaks.
    let _temp_prompt = TempPromptGuard(temp_prompt);
    let RunControls {
        request_input,
        mut steering,
        interrupt,
        host,
    } = controls;
    let _host = host; // child env was already applied at spawn; kept for clarity
    let request_input = std::sync::Arc::new(request_input);
    let agent_name = "pi";

    // ---- handshake + session setup (interruptible) -------------------------
    let setup = setup_session(&client, &request, model_catalog_wait);
    let (session_file, model_name) = tokio::select! {
        res = tokio::time::timeout(handshake_timeout, setup) => {
            let res = res.unwrap_or_else(|_| Err(HarnessError::Protocol(format!(
                "pi did not complete the RPC handshake within {}s (the agent \
                 may be waiting for a login — try running it once in a terminal)",
                handshake_timeout.as_secs()
            ))));
            match res {
                Ok(v) => v,
                Err(e) => {
                    let error = match child.try_wait() {
                        Ok(Some(status)) => {
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            format!("{e}; {}", crash_message(agent_name, Some(status), &stderr_tail))
                        }
                        _ => match stderr_tail.snapshot() {
                            Some(tail) => format!("{e}; stderr: {tail}"),
                            None => e.to_string(),
                        },
                    };
                    tracing::warn!(target: "cypher_harness::pi", %error, "pi setup failed");
                    let _ = event_tx
                        .send(Ok(AgentEvent::Done {
                            status: DoneStatus::Errored,
                            result: None,
                            error: Some(error),
                            session_id: None,
                        }))
                        .await;
                    shutdown_child(&mut child, kill_grace).await;
                    return;
                }
            }
        },
        _ = interrupt.cancelled() => {
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: None,
                }))
                .await;
            shutdown_child(&mut child, kill_grace).await;
            return;
        }
    };

    let assistant_message_id = new_message_id();
    if !send(
        &event_tx,
        AgentEvent::SessionStarted {
            harness: HarnessId::Pi,
            model: if model_name.is_empty() {
                request.model.clone().unwrap_or_default()
            } else {
                model_name
            },
            tools: Vec::new(),
            cwd: request.cwd.clone(),
            session_id: session_file.clone(),
            assistant_message_id: assistant_message_id.clone(),
        },
    )
    .await
    {
        shutdown_child(&mut child, kill_grace).await;
        return;
    }

    // ---- synthesized built-in command interception -------------------------
    // The current assistant message's streamed text (Done's `result` and the
    // error text for an `error` stopReason). The built-ins below also feed it.
    let mut last_assistant_text = String::new();
    // `/compact` and `/export-html` are pi built-in TUI commands with RPC
    // equivalents: pi's `get_commands` never advertises built-ins and sending
    // one as prompt text would not execute it. A same-name extension/prompt/
    // skill command discovered in the cache wins (interception skipped — pi
    // handles it); otherwise the harness dispatches the RPC directly and Dones
    // immediately (no agent stream to wait on, so no no-activity grace).
    // compaction_start/end events, if any, stay ignored like the main loop's.
    match intercept_builtin(
        &client,
        &event_tx,
        &request.prompt,
        &session_file,
        intercept,
        &mut last_assistant_text,
    )
    .await
    {
        InterceptOutcome::Handled => {
            shutdown_child(&mut child, kill_grace).await;
            return;
        }
        InterceptOutcome::Passthrough => {}
    }

    // The first prompt is dispatched FROM the main loop (same path as a
    // parked restart). Real pi only ACKs an extension command after its
    // handler returns — and handlers like `/subagent-config` block on
    // `ctx.ui.select` first. Awaiting the ACK here would deadlock: the
    // select arrives as `Incoming::UiRequest`, which is only drained in
    // the loop. Attachments are inlined ONLY on this first prompt: routed
    // mailbox messages carry none, and re-sending them would duplicate.
    let attachments_images = inline_images(&request.attachments);
    let mut prompt_params = Map::new();
    prompt_params.insert("message".into(), Value::String(request.prompt.clone()));
    if let Some(images) = &attachments_images {
        prompt_params.insert("images".into(), images.clone());
    }
    let first_prompt_client = client.clone();

    let mut run = PiRun {
        client,
        event_tx,
        session_file,
        mcp_servers,
        request_input,
        intercept,
        no_activity_grace,
        interrupt_grace,
        kill_grace,
        assistant_message_id,
        last_assistant_text,
        last_stop_reason: "stop".to_owned(),
        last_error_message: None,
        interrupted: false,
        interrupt_sent: false,
        done_sent: false,
        progress_last: HashMap::new(),
        progress_ended: HashSet::new(),
        throughput: throughput::ThroughputMeter::default(),
        agent_started: false,
        in_turn: true,
        steering_open: true,
        steers_queued: VecDeque::new(),
        handled_steers: 0,
        steer_call: None,
        steer_backlog: VecDeque::new(),
        atomic_steer: false,
        idle_prompt: Some(Box::pin(async move {
            first_prompt_client.request("prompt", prompt_params).await
        })),
        had_ui: false,
        prompt_is_command: request.prompt.trim_start().starts_with('/'),
        prompt_backlog: VecDeque::new(),
        escalation: None,
        no_activity: Box::pin(tokio::time::sleep(no_activity_grace)),
    };

    loop {
        match run.open_parked_turn().await {
            Some(Flow::Break) => break,
            Some(Flow::Continue) => continue,
            None => {}
        }
        let flow = tokio::select! {
            // biased: queued output drains before the no-activity grace can
            // fire (a zero grace still yields to `incoming` first).
            biased;
            res = prompt_response(&mut run.idle_prompt), if run.idle_prompt.is_some() => {
                run.on_prompt_response(res).await
            }
            inc = incoming.recv() => match inc {
                Some(Incoming::Event(ev)) => run.on_event(ev).await,
                Some(Incoming::Response { id, result }) => {
                    run.on_steer_response(id, result).await
                }
                Some(Incoming::UiRequest { id, method, payload }) => {
                    run.on_ui_request(id, method, payload).await
                }
                Some(Incoming::Eof) | None => {
                    run.on_eof(&mut child, &stderr_tail, agent_name).await
                }
            },
            steer = steering.recv(), if run.steering_open && !run.interrupted => {
                run.on_steer(steer)
            }
            _ = interrupt.cancelled(), if !run.interrupt_sent => run.on_interrupt(&child),
            _ = &mut run.no_activity,
                if !run.agent_started && !run.done_sent && run.idle_prompt.is_none() =>
            {
                run.on_no_activity().await
            }
            _ = run.event_tx.closed() => Flow::Break,
        };
        if let Flow::Break = flow {
            break;
        }
    }

    // Terminal bookkeeping: never end the stream without a Done unless the
    // consumer already hung up.
    if !run.event_tx.is_closed() && !run.done_sent {
        if run.interrupted {
            let _ = run
                .event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: Some(run.session_file.clone()),
                }))
                .await;
        } else {
            let status = child.try_wait().ok().flatten();
            let _ = run
                .event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(crash_message(agent_name, status, &stderr_tail)),
                    session_id: Some(run.session_file.clone()),
                }))
                .await;
        }
    }

    // Escalation dies BEFORE the child is reaped: after `shutdown_child`
    // waits the pid, a still-armed SIGTERM/SIGKILL timer would fire at a
    // freed (reusable) pid.
    if let Some(handle) = run.escalation {
        handle.abort();
    }
    shutdown_child(&mut child, run.kill_grace).await;
}

/// `RunRequest.attachments` (absolute paths already staged on the run device)
/// → pi `prompt`/`steer` `images` blocks. Only the formats provider vision
/// APIs accept inline (PNG/JPEG/GIF/WebP) — an image block in any other type
/// (BMP, SVG, TIFF…) is rejected by the provider and fails the whole turn.
/// Everything else is left to the prompt text refs.
pub(super) fn inline_images(paths: &[String]) -> Option<Value> {
    use base64::Engine as _;
    let mut images: Vec<Value> = Vec::new();
    for path in paths {
        let mime = mime_for_path(path);
        if !matches!(
            mime.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            tracing::debug!(target: "cypher_harness::pi", "attachment unreadable: {path}");
            continue;
        };
        images.push(json!({
            "type": "image",
            "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            "mimeType": mime,
        }));
    }
    (!images.is_empty()).then_some(Value::Array(images))
}

/// Guess a MIME type from the file extension (attachments carry no explicit
/// type). Unknown extensions default to octet-stream and are never inlined.
fn mime_for_path(path: &str) -> String {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        _ => "application/octet-stream",
    }
    .to_owned()
}
