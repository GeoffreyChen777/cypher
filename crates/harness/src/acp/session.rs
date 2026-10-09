//! One live ACP session: initialization, model/config selection, server-request
//! handling and the `run_session` event loop.

use super::*;

pub(super) struct Session {
    pub(super) child: Child,
    pub(super) client: RpcClient,
    pub(super) incoming: mpsc::Receiver<Incoming>,
    pub(super) event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    pub(super) controls: RunControls,
    pub(super) request: RunRequest,
    pub(super) harness: HarnessId,
    pub(super) agent_name: &'static str,
    pub(super) prompt_transform: fn(Option<ReasoningLevel>, &str) -> String,
    pub(super) effort_values: fn(Option<ReasoningLevel>, Option<&str>) -> Vec<&'static str>,
    pub(super) interrupt_grace: Duration,
    pub(super) kill_grace: Duration,
    pub(super) handshake_timeout: Duration,
    pub(super) stderr_tail: crate::StderrTail,
}

pub(super) fn initialize_params(harness: HarnessId) -> Value {
    let mut capabilities = json!({
        "fs": { "readTextFile": false, "writeTextFile": false },
        "terminal": false,
    });
    // Cursor's ACP server exposes Auto's Optimize For (and other model
    // parameters) when the client opts into parameterizedModelPicker;
    // without it the catalog uses exploded variant ids.
    if harness == HarnessId::Cursor {
        capabilities["_meta"] = json!({ "parameterizedModelPicker": true });
    }
    json!({
        "protocolVersion": 1,
        "clientInfo": {
            "name": "cypher",
            "title": "Cypher",
            "version": env!("CARGO_PKG_VERSION"),
        },
        // Declined: agents fall back to their own fs/terminal access, which
        // is what cypher wants — the working tree is the source of truth for
        // the diff pane, and commands belong to the agent's own sandbox.
        "clientCapabilities": capabilities,
    })
}

/// `initialize._meta.steering.supported` — the `_session/steering` extension
/// both org-maintained adapters advertise (not part of the v1 spec).
pub(super) fn steering_supported(init: &Value) -> bool {
    init.get("_meta")
        .and_then(|m| m.get("steering"))
        .and_then(|s| s.get("supported"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Depth-limited scan for an `availableCommands` array anywhere in a response
/// (agents differ on where the handshake advertises them: top level, inside
/// `agentCapabilities`, or `_meta`).
pub(super) fn scan_available_commands(value: &Value) -> Vec<SlashCommand> {
    fn scan(value: &Value, depth: u8) -> Option<&Value> {
        if depth == 0 {
            return None;
        }
        let obj = value.as_object()?;
        if let Some(cmds) = obj.get("availableCommands").filter(|c| c.is_array()) {
            return Some(cmds);
        }
        obj.values().find_map(|v| scan(v, depth - 1))
    }
    parse_commands(scan(value, 4))
}

fn new_message_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Rotate the assistant message id; returns (previous, next).
fn rotate(id: &mut String) -> (String, String) {
    let prev = std::mem::replace(id, new_message_id());
    (prev, id.clone())
}

pub(super) async fn send(
    tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    ev: AgentEvent,
) -> bool {
    tx.send(Ok(ev)).await.is_ok()
}

/// Normalize an option/model-option id for matching across naming styles
/// (`fastMode` == `fast_mode` == `fast-mode`).
pub(super) fn norm_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Whether a model id carries the 1M-context hint, in either spelling: the
/// display form `opus[1m]` or the SDK-id form `claude-opus-4-6-1m`.
fn context_hint_1m(id: &str) -> bool {
    id.contains("[1m]") || id.ends_with("-1m")
}

/// The id with a trailing long-context hint removed; `None` when it carries
/// none.
pub(super) fn strip_context_hint(id: &str) -> Option<&str> {
    id.strip_suffix("[1m]").or_else(|| id.strip_suffix("-1m"))
}

/// A wire display name with its trailing parenthetical removed
/// ("Opus (1M context)" → "Opus") — a folded base row must not keep the
/// variant tag.
pub(super) fn strip_trailing_parenthetical(name: &str) -> &str {
    match name.rfind(" (") {
        Some(at) if name.ends_with(')') => name[..at].trim_end(),
        _ => name,
    }
}

/// Pick the advertised model value for a requested model id. Agents differ in
/// what they advertise: full ids (`claude-opus-5`), SDK aliases
/// (`opus`, `sonnet`, `haiku` — the claude adapter), and long-context
/// variants in either hint spelling. Exact match first (with the 1M compose
/// when the run selects the 1M window), then a family-token fallback that
/// prefers a variant matching the requested context window.
pub(super) fn pick_model_value(
    requested: &str,
    available: &[&str],
    context_1m: bool,
) -> Option<String> {
    if context_1m {
        for composed in [format!("{requested}[1m]"), format!("{requested}-1m")] {
            if available.contains(&composed.as_str()) {
                return Some(composed);
            }
        }
    }
    if available.contains(&requested) {
        return Some(requested.to_owned());
    }
    // Family fallback: "claude-opus-5" → "opus" matches "opus[1m]".
    let family = ["fable", "opus", "sonnet", "haiku", "gpt"]
        .into_iter()
        .find(|f| norm_id(requested).contains(f))?;
    let candidates: Vec<&&str> = available
        .iter()
        .filter(|v| norm_id(v).contains(family))
        .collect();
    candidates
        .iter()
        .find(|v| context_hint_1m(v) == context_1m)
        .or_else(|| candidates.first())
        .map(|v| (**v).to_owned())
}

/// The `session/set_config_option` calls a session response's `configOptions`
/// warrant for this run:
/// - the requested model (category `model`; a `contextWindow: "1m"` model
///   option composes the `<model>[1m]` id first, the CLI's own convention),
/// - the effort (category `thought_level`, first advertised value from the
///   spec's preference list),
/// - any remaining `model_options` matched by normalized id — selects take
///   the choice id, booleans take `on`/`true` truthiness (fastMode, thinking).
///
/// Matched against advertised values and skipped when already current. Pure
/// so it's testable; the returned value is the request's flattened `value`
/// payload (select: `{"value": id}`, boolean: `{"type":"boolean","value": b}`).
pub(super) fn config_option_sets(
    session_response: &Value,
    model: Option<&str>,
    efforts: &[&'static str],
    model_options: &serde_json::Map<String, Value>,
) -> Vec<(String, Value)> {
    let Some(options) = session_response
        .get("configOptions")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let context_1m = model_options
        .get("contextWindow")
        .and_then(Value::as_str)
        .is_some_and(|w| w.eq_ignore_ascii_case("1m"));
    let mut sets = Vec::new();
    for option in options {
        let Some(config_id) = option.get("id").and_then(Value::as_str) else {
            continue;
        };
        let kind = option.get("type").and_then(Value::as_str).unwrap_or("");
        let category = option.get("category").and_then(Value::as_str);
        let current = option.get("currentValue");
        let available: Vec<&str> = option
            .get("options")
            .and_then(Value::as_array)
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|o| o.get("value").and_then(Value::as_str))
            .collect();

        let wanted: Option<Value> = match (kind, category) {
            ("select", Some("model")) => {
                // Parameterized Cursor uses base ids; a saved exploded id
                // (`auto-smart[optimize_for=cost]`) still has to match. When
                // the catalog is still exploded, effort siblings switch via
                // `pick_model_id`.
                let requested = model.map(cursor::strip_variant_suffix);
                requested
                    .and_then(|m| cursor::pick_model_id(m, efforts, &available))
                    .or_else(|| requested.and_then(|m| pick_model_value(m, &available, context_1m)))
                    .or_else(|| model.and_then(|m| pick_model_value(m, &available, context_1m)))
                    .map(Value::String)
            }
            // Unattended parity with the retired custom adapters (claude
            // bypassPermissions / codex approvalPolicy never): pick the
            // no-prompts mode when the agent offers one. claude-agent-acp
            // calls it `bypassPermissions`, codex-acp `agent-full-access`
            // (approvalPolicy "never" + danger-full-access sandbox).
            // Cursor instead exposes agent/plan/ask — those arrive as a
            // Traits "Mode" option and win when the run selected one.
            ("select", Some("mode")) => model_options
                .get("mode")
                .and_then(Value::as_str)
                .filter(|c| available.contains(c))
                .map(|c| Value::String(c.to_owned()))
                .or_else(|| {
                    [
                        "bypassPermissions",
                        "bypass_permissions",
                        "yolo",
                        "agent-full-access",
                        "danger-full-access",
                        "full-access",
                    ]
                    .into_iter()
                    .find(|v| available.contains(v))
                    .map(|v| Value::String(v.to_owned()))
                }),
            ("select", Some("thought_level")) => efforts
                .iter()
                .find(|c| available.contains(*c))
                .map(|c| Value::String((*c).to_owned())),
            // Everything else: best-effort match against the run's
            // model-option selections by normalized id.
            _ => model_options.iter().find_map(|(opt_id, choice)| {
                if norm_id(opt_id) != norm_id(config_id) || opt_id == "contextWindow" {
                    return None;
                }
                match kind {
                    "select" => choice
                        .as_str()
                        .filter(|c| available.contains(c))
                        .map(|c| Value::String(c.to_owned())),
                    "boolean" => {
                        let on = choice == &Value::Bool(true)
                            || choice
                                .as_str()
                                .is_some_and(|c| c.eq_ignore_ascii_case("on"));
                        Some(Value::Bool(on))
                    }
                    _ => None,
                }
            }),
        };
        if let Some(value) = wanted
            && current != Some(&value)
        {
            let payload = match value {
                Value::Bool(b) => serde_json::json!({ "type": "boolean", "value": b }),
                other => serde_json::json!({ "value": other }),
            };
            sets.push((config_id.to_owned(), payload));
        }
    }
    sets
}

/// The events of one `session/update` notification, session-filtered.
fn session_update_events(params: &Value, session_id: &str) -> Vec<AgentEvent> {
    if params.get("sessionId").and_then(Value::as_str) != Some(session_id) {
        return Vec::new();
    }
    map_update(params.get("update").unwrap_or(&Value::Null))
}

/// Per-turn token usage from a settled `session/prompt` response, when the
/// adapter attaches it (tolerant of both field spellings; absent → nothing).
fn usage_from_response(res: &Result<Value, HarnessError>) -> Option<AgentEvent> {
    let usage = res.as_ref().ok()?.get("usage")?;
    let count = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| usage.get(*k))
            .and_then(Value::as_u64)
    };
    let input = count(&["inputTokens", "input_tokens"]);
    let output = count(&["outputTokens", "output_tokens"]);
    (input.is_some() || output.is_some()).then(|| AgentEvent::Usage {
        input_tokens: input.unwrap_or(0),
        output_tokens: output.unwrap_or(0),
    })
}

/// Map a finished `session/prompt` result to the run's terminal status.
fn stop_outcome(
    res: &Result<Value, HarnessError>,
    interrupted: bool,
) -> (DoneStatus, Option<String>) {
    if interrupted {
        return (DoneStatus::Interrupted, None);
    }
    match res {
        Ok(resp) => match resp.get("stopReason").and_then(Value::as_str) {
            Some("cancelled") => (DoneStatus::Interrupted, None),
            Some("refusal") => (
                DoneStatus::Errored,
                Some("The agent refused to continue.".to_owned()),
            ),
            // end_turn / max_tokens / max_turn_requests: the turn ended;
            // partial output is already in the doc.
            _ => (DoneStatus::Completed, None),
        },
        Err(e) => (DoneStatus::Errored, Some(e.to_string())),
    }
}

/// One turn: `session/prompt` whose response (the `stopReason`) ends it.
fn prompt_turn(
    client: RpcClient,
    session_id: String,
    text: String,
) -> BoxFuture<'static, Result<Value, HarnessError>> {
    Box::pin(async move {
        client
            .request(
                "session/prompt",
                json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": text }],
                }),
            )
            .await
    })
}

/// Answer a server→client request. Permission requests are auto-accepted with
/// the agent's preferred allow option — parity with the claude harness's
/// bypassPermissions and the codex harness's approvalPolicy "never" (zeron
/// sessions run unattended). Everything else (fs, terminal, elicitation) was
/// declined at initialize, so a stray request gets method-not-found rather
/// than wedging the agent.
fn handle_server_request(
    client: &RpcClient,
    id: Value,
    method: &str,
    params: &Value,
) -> Vec<AgentEvent> {
    match method {
        "session/request_permission" => {
            let options: Vec<Value> = params
                .get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            match preferred_allow_option(&options) {
                Some(option_id) => client.respond(
                    &id,
                    json!({ "outcome": { "outcome": "selected", "optionId": option_id } }),
                ),
                None => client.respond(&id, json!({ "outcome": { "outcome": "cancelled" } })),
            }
            Vec::new()
        }
        // Cursor blocks the turn on plan approval; unattended parity means
        // accepting it and rendering the phases as a todo chip.
        CURSOR_CREATE_PLAN => {
            client.respond(&id, json!({ "outcome": { "outcome": "accepted" } }));
            cursor_todo_events(params, CURSOR_PLAN_CHIP)
        }
        CURSOR_UPDATE_TODOS => {
            let todos = params
                .get("todos")
                .cloned()
                .unwrap_or(Value::Array(Vec::new()));
            client.respond(
                &id,
                json!({ "outcome": { "outcome": "accepted", "todos": todos } }),
            );
            cursor_todo_events(params, CURSOR_TODOS_CHIP)
        }
        // Subagent tasks run inside cursor-agent; this only reports one
        // finished. Image generation has nowhere to land in a cypher session.
        CURSOR_TASK => {
            client.respond(&id, json!({ "outcome": { "outcome": "completed" } }));
            Vec::new()
        }
        CURSOR_GENERATE_IMAGE => {
            client.respond(
                &id,
                json!({ "outcome": { "outcome": "rejected", "reason": "cypher cannot render generated images" } }),
            );
            Vec::new()
        }
        _ => {
            tracing::debug!(target: "cypher_harness::acp", "unhandled server request: {method}");
            client.respond_error(&id, -32601, &format!("unsupported method: {method}"));
            Vec::new()
        }
    }
}

type RequestInputFn = Box<
    dyn Fn(Vec<UserInputQuestion>) -> tokio::sync::oneshot::Receiver<Vec<UserInputAnswer>>
        + Send
        + Sync,
>;

/// A permission request is a QUESTION (not a tool permission) when any of
/// its options lacks an allow/reject kind — that's how the agent relays
/// user-facing choices (Claude's AskUserQuestion arrives this way through
/// the adapter). Every option carrying an allow/reject kind means a real
/// tool permission, which auto-accepts (unattended parity); kinds may
/// legitimately repeat — codex sends two `allow_always` options ("Allow for
/// Session" and a prefix-rule amendment) on every exec approval.
pub(super) fn is_user_question(options: &[Value]) -> bool {
    options.iter().any(|option| {
        !matches!(
            option.get("kind").and_then(Value::as_str),
            Some("allow_once" | "allow_always" | "reject_once" | "reject_always")
        )
    })
}

/// The live-run request handler: tool permissions auto-accept like
/// [`handle_server_request`], but question-shaped requests block on the
/// engine's input bridge (in a subtask so the message loop keeps flowing)
/// and answer with the option whose name matches the chosen label. A dropped
/// resolver degrades to `cancelled` — never a silent allow.
fn handle_server_request_live(
    client: &RpcClient,
    id: Value,
    method: &str,
    params: &Value,
    request_input: &std::sync::Arc<RequestInputFn>,
    open_questions: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> Vec<AgentEvent> {
    if method == CURSOR_ASK_QUESTION {
        ask_cursor_questions(client, id, params, request_input, open_questions);
        return Vec::new();
    }
    if method != "session/request_permission" {
        return handle_server_request(client, id, method, params);
    }
    let options: Vec<Value> = params
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !is_user_question(&options) {
        return handle_server_request(client, id, method, params);
    }
    let names: Vec<String> = options
        .iter()
        .map(|o| {
            o.get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    let question = UserInputQuestion {
        id: new_message_id(),
        header: "Agent question".into(),
        question: params
            .get("toolCall")
            .and_then(|t| t.get("title"))
            .and_then(Value::as_str)
            .unwrap_or("The agent needs your input.")
            .to_owned(),
        options: names.clone(),
        multi_select: false,
    };
    let client = client.clone();
    let request_input = std::sync::Arc::clone(request_input);
    // Pending questions block the agent — the quiet-settle must not read
    // that silence as a finished turn.
    let open_questions = std::sync::Arc::clone(open_questions);
    open_questions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    tokio::spawn(async move {
        let answers = (request_input)(vec![question.clone()])
            .await
            .unwrap_or_default();
        let picked = answers
            .iter()
            .find(|a| a.question_id == question.id)
            .and_then(|a| a.labels.first())
            .and_then(|label| {
                options
                    .iter()
                    .find(|o| o.get("name").and_then(Value::as_str) == Some(label.as_str()))
            })
            .and_then(|o| o.get("optionId").and_then(Value::as_str));
        match picked {
            Some(option_id) => client.respond(
                &id,
                json!({ "outcome": { "outcome": "selected", "optionId": option_id } }),
            ),
            None => client.respond(&id, json!({ "outcome": { "outcome": "cancelled" } })),
        }
        open_questions.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    });
    Vec::new()
}

/// Cursor's ACP extension methods (`https://cursor.com/docs/cli/acp`).
/// `ask_question` and `create_plan` BLOCK the agent until answered, so every
/// one of these gets a response even when zeron has nothing to do with it.
const CURSOR_ASK_QUESTION: &str = "cursor/ask_question";
pub(super) const CURSOR_CREATE_PLAN: &str = "cursor/create_plan";
pub(super) const CURSOR_UPDATE_TODOS: &str = "cursor/update_todos";
pub(super) const CURSOR_TASK: &str = "cursor/task";
pub(super) const CURSOR_GENERATE_IMAGE: &str = "cursor/generate_image";

/// Stable chip ids so repeated todo/plan updates refresh in place, matching
/// the `acp-plan` convention in the normalizer.
pub(super) const CURSOR_PLAN_CHIP: &str = "cursor-plan";
pub(super) const CURSOR_TODOS_CHIP: &str = "cursor-todos";

/// The same extension methods arriving without an id: nothing to answer, and
/// only the todo-carrying ones have anything to render. The docs describe
/// todos/task/image as fire-and-forget while also giving them response
/// shapes, so both arrival styles are handled.
pub(super) fn cursor_notification_events(method: &str, params: &Value) -> Vec<AgentEvent> {
    match method {
        CURSOR_UPDATE_TODOS => cursor_todo_events(params, CURSOR_TODOS_CHIP),
        CURSOR_CREATE_PLAN => cursor_todo_events(params, CURSOR_PLAN_CHIP),
        _ => Vec::new(),
    }
}

/// `cursor/ask_question` → the engine's input bridge. Unlike a permission
/// question this carries a LIST of questions, each with its own labelled
/// options and multi-select flag, and answers go back as option ids. Handled
/// in a subtask so the message loop keeps draining while the user decides;
/// a dropped resolver degrades to `cancelled`, never a silent pick.
fn ask_cursor_questions(
    client: &RpcClient,
    id: Value,
    params: &Value,
    request_input: &std::sync::Arc<RequestInputFn>,
    open_questions: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let asked = cursor_questions(params);
    if asked.is_empty() {
        client.respond(
            &id,
            json!({ "outcome": { "outcome": "skipped", "reason": "no answerable questions" } }),
        );
        return;
    }
    let client = client.clone();
    let request_input = std::sync::Arc::clone(request_input);
    let open_questions = std::sync::Arc::clone(open_questions);
    open_questions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    tokio::spawn(async move {
        let answers = (request_input)(asked.iter().map(|q| q.question.clone()).collect())
            .await
            .unwrap_or_default();
        client.respond(&id, cursor_answer_outcome(&asked, &answers));
        open_questions.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    });
}

/// One `cursor/ask_question` entry: the cypher-side question plus what is
/// needed to answer it — the wire id, and the label→optionId table (zeron's
/// input bridge speaks labels, cursor expects option ids).
pub(super) struct CursorQuestion {
    wire_id: String,
    pub(super) question: UserInputQuestion,
    pub(super) choices: Vec<(String, String)>,
}

pub(super) fn cursor_questions(params: &Value) -> Vec<CursorQuestion> {
    let header = params
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Agent question");
    params
        .get("questions")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|q| {
            let wire_id = q.get("id").and_then(Value::as_str)?.to_owned();
            let choices: Vec<(String, String)> = q
                .get("options")
                .and_then(Value::as_array)
                .map(|a| a.as_slice())
                .unwrap_or_default()
                .iter()
                .filter_map(|o| {
                    let oid = o.get("id").and_then(Value::as_str)?;
                    let label = o.get("label").and_then(Value::as_str).unwrap_or(oid);
                    Some((label.to_owned(), oid.to_owned()))
                })
                .collect();
            // An option-less question has no answer cypher could send back.
            if choices.is_empty() {
                return None;
            }
            Some(CursorQuestion {
                wire_id,
                question: UserInputQuestion {
                    // Cypher-minted: cursor's ids ("q1") repeat across turns.
                    id: new_message_id(),
                    header: header.to_owned(),
                    question: q
                        .get("prompt")
                        .and_then(Value::as_str)
                        .unwrap_or("The agent needs your input.")
                        .to_owned(),
                    options: choices.iter().map(|(label, _)| label.clone()).collect(),
                    multi_select: q
                        .get("allowMultiple")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                },
                choices,
            })
        })
        .collect()
}

/// Chosen labels → the `answered` outcome. Nothing recognisable coming back
/// (dropped resolver, unknown labels) degrades to `cancelled` so the agent
/// unblocks without cypher inventing a pick.
pub(super) fn cursor_answer_outcome(
    asked: &[CursorQuestion],
    answers: &[UserInputAnswer],
) -> Value {
    let picked: Vec<Value> = asked
        .iter()
        .filter_map(|asked| {
            let labels = &answers
                .iter()
                .find(|a| a.question_id == asked.question.id)?
                .labels;
            let ids: Vec<&str> = labels
                .iter()
                .filter_map(|label| {
                    asked
                        .choices
                        .iter()
                        .find(|(l, _)| l == label)
                        .map(|(_, oid)| oid.as_str())
                })
                .collect();
            (!ids.is_empty())
                .then(|| json!({ "questionId": asked.wire_id, "selectedOptionIds": ids }))
        })
        .collect();
    if picked.is_empty() {
        json!({ "outcome": { "outcome": "cancelled" } })
    } else {
        json!({ "outcome": { "outcome": "answered", "answers": picked } })
    }
}

/// Await a setup request while draining incoming messages, so a `session/load`
/// whose replay outruns the incoming channel's capacity can't deadlock the
/// reader. Replayed `session/update`s are dropped (the doc already holds the
/// history); server requests are answered.
async fn request_draining(
    client: &RpcClient,
    incoming: &mut mpsc::Receiver<Incoming>,
    method: &'static str,
    params: Value,
) -> Result<Value, HarnessError> {
    let mut fut = prompt_like_request(client.clone(), method, params);
    let res = loop {
        tokio::select! {
            res = &mut fut => break res,
            inc = incoming.recv() => match inc {
                Some(Incoming::Request { id, method, params }) => {
                    handle_server_request(client, id, &method, &params);
                }
                Some(_) => {}
                None => {
                    return Err(HarnessError::Protocol(format!(
                        "{method}: agent exited during setup"
                    )));
                }
            },
        }
    };
    // Responses resolve through the pending map, not the incoming queue, so
    // replay updates the reader forwarded BEFORE the response line may still
    // sit in the buffer — flush them now or they'd leak into the live turn.
    while let Ok(inc) = incoming.try_recv() {
        if let Incoming::Request { id, method, params } = inc {
            handle_server_request(client, id, &method, &params);
        }
    }
    res
}

fn prompt_like_request(
    client: RpcClient,
    method: &'static str,
    params: Value,
) -> BoxFuture<'static, Result<Value, HarnessError>> {
    Box::pin(async move { client.request(method, params).await })
}

/// Track the liveness signals the blanket quiet-settle keys on: content
/// proves the turn produced something; an open tool call or a pending
/// question proves silence is legitimate.
fn track_turn_signals(
    ev: &AgentEvent,
    content_seen: &mut bool,
    open_tools: &mut std::collections::HashSet<String>,
) {
    match ev {
        AgentEvent::TextDelta { text } if !text.is_empty() => *content_seen = true,
        AgentEvent::ToolCall { id, .. } => {
            *content_seen = true;
            open_tools.insert(id.clone());
        }
        AgentEvent::ToolResult { id, .. } => {
            open_tools.remove(id);
        }
        _ => {}
    }
}

/// True for the session's terminal accounting frame: a `usage_update`
/// carrying `cost`, which claude-agent-acp derives once per turn from the
/// CLI's result message — the turn-is-over tell that survives even when the
/// prompt response itself is dropped (the starved-turn bug).
fn is_turn_end_cost_update(params: &Value, session_id: &str) -> bool {
    params.get("sessionId").and_then(Value::as_str) == Some(session_id)
        && params.get("update").is_some_and(|u| {
            u.get("sessionUpdate").and_then(Value::as_str) == Some("usage_update")
                && u.get("cost").is_some()
        })
}

/// A mid-turn `_session/steering` call. `idleBehavior: promptRequired`
/// covers the turn-ended race: the agent hands the text back instead of
/// firing an untracked turn.
fn steering_call_future(
    client: &RpcClient,
    session_id: &str,
    text: &str,
) -> BoxFuture<'static, Result<Value, HarnessError>> {
    let params = json!({
        "sessionId": session_id,
        "prompt": [{ "type": "text", "text": text }],
        "_meta": { "steering": { "idleBehavior": "promptRequired" } },
    });
    prompt_like_request(client.clone(), "_session/steering", params)
}

/// The per-run event loop: one task multiplexing agent messages, the pending
/// turn, the steering mailbox, the interrupt token, and consumer liveness.
pub(super) async fn run_session(session: Session) {
    let Session {
        mut child,
        client,
        mut incoming,
        event_tx,
        controls,
        request,
        harness,
        agent_name,
        prompt_transform,
        effort_values,
        interrupt_grace,
        kill_grace,
        handshake_timeout,
        stderr_tail,
    } = session;
    let RunControls {
        request_input,
        mut steering,
        interrupt,
        ..
    } = controls;
    let request_input = std::sync::Arc::new(request_input);

    // ---- handshake + session (interruptible) ------------------------------
    let setup = async {
        let init = client
            .request("initialize", initialize_params(harness))
            .await?;
        let steer_ext = steering_supported(&init);
        let init_commands = scan_available_commands(&init);

        let session_params = json!({ "cwd": request.cwd, "mcpServers": [] });
        let (session_id, session_response) = if let Some(resume) = &request.resume {
            let mut load = session_params.clone();
            load["sessionId"] = Value::String(resume.clone());
            match request_draining(&client, &mut incoming, "session/load", load).await {
                Ok(resp) => (resume.clone(), resp),
                // A missing/foreign session falls back to a fresh one.
                Err(e) => {
                    tracing::debug!(
                        target: "cypher_harness::acp",
                        "session/load failed (starting fresh): {e}"
                    );
                    let new = request_draining(
                        &client,
                        &mut incoming,
                        "session/new",
                        session_params.clone(),
                    )
                    .await?;
                    (
                        new.get("sessionId")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        new,
                    )
                }
            }
        } else {
            let new =
                request_draining(&client, &mut incoming, "session/new", session_params).await?;
            (
                new.get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                new,
            )
        };
        if session_id.is_empty() {
            return Err(HarnessError::Protocol(
                "session/new returned no sessionId".into(),
            ));
        }
        // Apply the run's model + effort + model options through the
        // session's advertised config options (ACP has no per-prompt model
        // field). Best-effort: a rejected set is logged, never fatal — the
        // agent's default runs.
        //
        // Cursor parameterized mode only lists parameters for the *current*
        // model. Set the model first, then apply optimize_for / effort / fast
        // against the refreshed configOptions in the response.
        let efforts = effort_values(request.reasoning, request.model.as_deref());
        let requested_model = request.model.as_deref().map(cursor::strip_variant_suffix);
        let mut options_snapshot = session_response;
        if harness == HarnessId::Cursor {
            let model_sets = config_option_sets(
                &options_snapshot,
                requested_model,
                &[],
                &serde_json::Map::new(),
            );
            if let Some((_, payload)) = model_sets.iter().find(|(id, _)| id == "model") {
                let mut params = serde_json::Map::new();
                params.insert("sessionId".into(), session_id.clone().into());
                params.insert("configId".into(), "model".into());
                if let Some(payload) = payload.as_object() {
                    for (k, v) in payload {
                        params.insert(k.clone(), v.clone());
                    }
                }
                match request_draining(
                    &client,
                    &mut incoming,
                    "session/set_config_option",
                    Value::Object(params),
                )
                .await
                {
                    Ok(resp) if resp.get("configOptions").is_some() => {
                        options_snapshot = resp;
                    }
                    Err(e) => {
                        tracing::debug!(
                            target: "cypher_harness::acp",
                            "session/set_config_option model rejected (agent default runs): {e}"
                        );
                    }
                    _ => {}
                }
            }
        }
        for (config_id, payload) in config_option_sets(
            &options_snapshot,
            requested_model,
            &efforts,
            &request.model_options,
        ) {
            if harness == HarnessId::Cursor && config_id == "model" {
                continue;
            }
            let mut params = serde_json::Map::new();
            params.insert("sessionId".into(), session_id.clone().into());
            params.insert("configId".into(), config_id.clone().into());
            if let Some(payload) = payload.as_object() {
                for (k, v) in payload {
                    params.insert(k.clone(), v.clone());
                }
            }
            if let Err(e) = request_draining(
                &client,
                &mut incoming,
                "session/set_config_option",
                Value::Object(params),
            )
            .await
            {
                tracing::debug!(
                    target: "cypher_harness::acp",
                    "session/set_config_option {config_id}={payload} rejected (agent default runs): {e}"
                );
            }
        }
        Ok::<(String, bool, Vec<SlashCommand>), HarnessError>((
            session_id,
            steer_ext,
            init_commands,
        ))
    };
    let (session_id, steer_ext, init_commands) = tokio::select! {
        res = tokio::time::timeout(handshake_timeout, setup) => {
            let res = res.unwrap_or_else(|_| {
                // A hung handshake (agent waiting on a login it can never
                // get, a wedged adapter) used to spin "Working" forever —
                // the false "thinking for 2+ minutes then nothing" class of
                // report. Bound it and say what was reached.
                Err(HarnessError::Protocol(format!(
                    "{agent_name} did not complete the ACP handshake within {}s \
                     (the agent may be waiting for a login — try running it once \
                     in a terminal)",
                    handshake_timeout.as_secs()
                )))
            });
            match res {
                Ok(v) => v,
                Err(e) => {
                    // A child that dies before the handshake used to surface only
                    // the RPC-side symptom ("transport closed") — its exit status
                    // and stderr, both already in hand, were dropped, leaving
                    // startup crashes undiagnosable (user report). When the child
                    // is already gone, give the reader task a beat to drain the
                    // pipe, then append the crash text; the Done carrying it is
                    // journaled, so the cause survives for later inspection. A
                    // still-live child (the timeout) contributes its stderr tail.
                    let error = match child.try_wait() {
                        Ok(Some(status)) => {
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            format!(
                                "{e}; {}",
                                crate::crash_message(agent_name, Some(status), &stderr_tail)
                            )
                        }
                        _ => match stderr_tail.snapshot() {
                            Some(tail) => format!("{e}; stderr: {tail}"),
                            None => e.to_string(),
                        },
                    };
                    tracing::warn!(target: "cypher_harness::acp", %error, "agent setup failed");
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

    let mut assistant_message_id = new_message_id();
    if !send(
        &event_tx,
        AgentEvent::SessionStarted {
            harness,
            model: request.model.clone().unwrap_or_default(),
            tools: Vec::new(),
            cwd: request.cwd.clone(),
            session_id: session_id.clone(),
            assistant_message_id: assistant_message_id.clone(),
        },
    )
    .await
    {
        shutdown_child(&mut child, kill_grace).await;
        return;
    }
    if !init_commands.is_empty()
        && !send(
            &event_tx,
            AgentEvent::AvailableCommands {
                commands: init_commands,
            },
        )
        .await
    {
        shutdown_child(&mut child, kill_grace).await;
        return;
    }

    // ---- main loop --------------------------------------------------------
    let mut turn: Option<BoxFuture<'static, Result<Value, HarnessError>>> = Some(prompt_turn(
        client.clone(),
        session_id.clone(),
        prompt_transform(request.reasoning, &request.prompt),
    ));
    // Steers waiting for the turn boundary (agents without the extension, or
    // extension steers that lost the turn-end race).
    let mut queued_steers: VecDeque<String> = VecDeque::new();
    // The in-flight `_session/steering` call (text + response future), plus
    // followers awaiting their turn. Polled from the main select so the loop
    // keeps draining `incoming` while the agent responds — awaiting inline
    // deadlocks against a full incoming channel when the agent floods
    // updates (the reader blocks on the channel and never parses the
    // steering response).
    let mut steering_call: Option<(String, BoxFuture<'static, Result<Value, HarnessError>>)> = None;
    let mut steer_backlog: VecDeque<String> = VecDeque::new();
    let mut steering_open = true;
    let mut interrupted = false;
    let mut interrupt_sent = false;
    let mut done_current = false;
    let mut done_after_interrupt = false;
    let mut escalation: Option<tokio::task::JoinHandle<()>> = None;
    // Starved-turn recovery: a
    // `session/prompt` sent while the agent runs a SELF-CONTINUED turn (a
    // background-task re-invocation no prompt started) starves —
    // claude-agent-acp does not track turns it did not start, so the merged
    // turn's result is never attributed to the pending prompt (reproduced
    // against 0.66.0; the prompt's TEXT still reaches the model, queued by
    // the CLI). The tell is protocol evidence, not timing: a steering call
    // answered `promptRequired`/`noRunningTurn` while OUR prompt is
    // outstanding means the adapter has no turn that could ever settle it.
    // A short grace covers the true turn-end race (its response lands within
    // milliseconds); past it, the dead prompt is closed out with a Done and
    // the queued steer is promoted to a fresh turn.
    const STARVE_GRACE: Duration = Duration::from_secs(2);
    let mut starve_deadline: Option<tokio::time::Instant> = None;
    // Deterministic turn-end hint (claude-agent-acp, verified against
    // 0.66.0): the adapter derives exactly one cost-bearing `usage_update`
    // per turn from the CLI's terminal result — INCLUDING turns whose
    // prompt response it then drops (the starve above; the cost frame and
    // the response share a timestamp in every healthy trace). While our
    // prompt is outstanding, that update means the turn is already over:
    // give the real response a short head start (it lands within
    // milliseconds when it lands at all), then settle via the recovery arm.
    // This is what keeps a dropped reply's stuck-Working window near zero
    // instead of watchdog-length. Gated to Claude — the one adapter whose
    // cost semantics are verified end-of-turn-only.
    const COST_HINT_GRACE: Duration = Duration::from_secs(1);
    let cost_hint_enabled = harness == HarnessId::ClaudeCode;
    // BLANKET dropped-reply settle, adapter-agnostic: any ACP agent whose
    // prompt response goes missing must not strand the turn. Signals that
    // exist in core ACP stand in for the adapter-specific cost frame: once
    // the turn has streamed content, every tool call it opened has resolved,
    // no permission/question round-trip is pending, and the stream has been
    // quiet past the window, the turn is settled through the same recovery
    // arm. A false settle is only PARTLY recoverable: the engine folds any
    // later output as a self-continued segment and re-arms Working, but the
    // turn is orphaned — the real response resolves a closed channel, no
    // Done ever comes, and the session strands Working until the engine's
    // quiesce watchdog parks it.
    //
    // Claude is EXEMPT, even from the env knob: claude-agent-acp forwards
    // no thinking traffic, so a long silent reasoning stretch in exactly
    // the "looks finished" state (content streamed, every tool resolved)
    // is indistinguishable from a dropped reply — 30s of quiet falsely
    // settles live turns mid-thought, producing both a premature Done and
    // the stuck-Working orphan above. Claude's
    // genuinely dropped replies already settle deterministically (the
    // cost-frame hint above, `noRunningTurn` steering evidence); the
    // engine watchdog backstops anything left.
    // `CYPHER_ACP_QUIET_SETTLE_MS` overrides; 0 disables.
    let quiet_settle: Option<Duration> = if cost_hint_enabled {
        None
    } else {
        match cypher_env::var("ACP_QUIET_SETTLE_MS").and_then(|v| v.parse::<u64>().ok()) {
            Some(0) => None,
            Some(ms) => Some(Duration::from_millis(ms)),
            None => Some(Duration::from_secs(30)),
        }
    };
    let mut last_update_at = tokio::time::Instant::now();
    let mut turn_content_seen = false;
    // A steering injection makes the cost hint unsafe for the REST of the
    // turn: the adapter emits a cost-bearing usage_update for the injected
    // message itself, mid-turn, indistinguishable in shape from the
    // terminal one (verified against 0.66.0 — premature Done exactly one
    // grace after injection). Steered turns settle off their real response
    // (healthy in every trace); the engine's quiesce watchdog backstops
    // them (the quiet settle used to, before the Claude exemption above).
    let mut steered_this_turn = false;
    let mut open_tools: std::collections::HashSet<String> = std::collections::HashSet::new();
    let open_questions = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // PREVENTION, ahead of all the recovery above: never send a
    // `session/prompt` into a session that is visibly mid SELF-CONTINUED
    // turn — that prompt's reply is what the adapter drops (the verified
    // starve). Visibly busy = an open tool call, or stream traffic within
    // BUSY_RECENT, with no prompt of ours outstanding. The discipline is
    // Zed's, verified against the real adapter: `session/cancel` the
    // unowned turn, give it CANCEL_FLUSH to die and drain, then prompt.
    // This makes the interactive path starve-free; the settle layers below
    // remain for the notification race a client cannot see coming.
    const BUSY_RECENT: Duration = Duration::from_secs(3);
    const CANCEL_FLUSH: Duration = Duration::from_secs(2);
    let mut cancel_flush_deadline: Option<tokio::time::Instant> = None;

    'main: loop {
        tokio::select! {
            res = async { turn.as_mut().expect("guarded by if").await }, if turn.is_some() => {
                turn = None;
                starve_deadline = None;
                // Settle an in-flight `_session/steering` call BEFORE closing
                // the turn: its response rides the same stdout as the prompt
                // response, so by now it is (nearly always) already parsed —
                // the select just hadn't polled it yet. Deciding it here keeps
                // the ordering deterministic: an injection that landed in this
                // turn emits its Steered boundary now, ahead of the drained
                // tail and the Done (a Steered AFTER Done re-armed the
                // consumer with no next turn — the stranded-Working bug); a
                // rejected/unsettled call redelivers as the next turn. The
                // timeout guards the flooded-incoming edge (reader blocked on
                // a full channel never parses the response): past it the call
                // is abandoned and the steer redelivered.
                if let Some((text, mut fut)) = steering_call.take() {
                    let outcome = match tokio::time::timeout(
                        Duration::from_millis(1000),
                        &mut fut,
                    )
                    .await
                    {
                        Ok(Ok(resp)) => resp
                            .get("outcome")
                            .and_then(Value::as_str)
                            .unwrap_or("injected")
                            .to_owned(),
                        Ok(Err(_)) | Err(_) => "promptRequired".to_owned(),
                    };
                    if interrupted {
                        // Winding down; abandoned like any queued steer.
                    } else if outcome != "promptRequired" {
                        let (prev, next) = rotate(&mut assistant_message_id);
                        if !send(
                            &event_tx,
                            AgentEvent::Steered {
                                assistant_message_id: Some(prev),
                                next_assistant_message_id: Some(next),
                            },
                        )
                        .await
                        {
                            break 'main;
                        }
                    } else {
                        queued_steers.push_back(text);
                    }
                    // Followers waiting on the settled call have no live turn
                    // to inject into anymore: boundary delivery.
                    while let Some(next_text) = steer_backlog.pop_front() {
                        queued_steers.push_back(next_text);
                    }
                }
                // Updates streamed before the prompt response are already
                // queued in stdout order — fold them into the turn before
                // closing it (responses bypass the incoming queue).
                let mut consumer_gone = false;
                while let Ok(inc) = incoming.try_recv() {
                    match inc {
                        Incoming::Notification { method, params } => {
                            let events = if method == "session/update" {
                                session_update_events(&params, &session_id)
                            } else {
                                cursor_notification_events(&method, &params)
                            };
                            for ev in events {
                                if !send(&event_tx, ev).await {
                                    consumer_gone = true;
                                    break;
                                }
                            }
                        }
                        Incoming::Request { id, method, params } => {
                            for ev in handle_server_request_live(
                                &client,
                                id,
                                &method,
                                &params,
                                &request_input,
                                &open_questions,
                            ) {
                                if !send(&event_tx, ev).await {
                                    consumer_gone = true;
                                    break;
                                }
                            }
                        }
                        _ => {}
                    }
                    if consumer_gone {
                        break;
                    }
                }
                if consumer_gone {
                    break 'main;
                }
                let (prev, _next) = rotate(&mut assistant_message_id);
                if !send(
                    &event_tx,
                    AgentEvent::AssistantMessageCompleted { assistant_message_id: prev, model: None },
                )
                .await
                {
                    break 'main;
                }
                // Per-turn token usage, when the adapter settles the prompt
                // with it (claude-agent-acp and codex-acp both do).
                if let Some(usage) = usage_from_response(&res)
                    && !send(&event_tx, usage).await
                {
                    break 'main;
                }
                let (status, error) = stop_outcome(&res, interrupted);
                done_current = true;
                if interrupted {
                    done_after_interrupt = true;
                }
                if !send(
                    &event_tx,
                    AgentEvent::Done {
                        status,
                        result: None,
                        error,
                        session_id: Some(session_id.clone()),
                    },
                )
                .await
                {
                    break 'main;
                }
                if interrupted || res.is_err() {
                    break 'main;
                }
                // Persistent session: a queued steer becomes the next turn;
                // otherwise stay alive for the mailbox — the caller owns
                // teardown (mirrors the codex harness).
                if let Some(text) = queued_steers.pop_front() {
                    let (prev, next) = rotate(&mut assistant_message_id);
                    if !send(
                        &event_tx,
                        AgentEvent::Steered {
                            assistant_message_id: Some(prev),
                            next_assistant_message_id: Some(next),
                        },
                    )
                    .await
                    {
                        break 'main;
                    }
                    done_current = false;
                    turn_content_seen = false;
                    steered_this_turn = false;
                    open_tools.clear();
                    last_update_at = tokio::time::Instant::now();
                    turn = Some(prompt_turn(client.clone(), session_id.clone(), text));
                } else if !steering_open {
                    break 'main;
                }
            },

            inc = incoming.recv() => match inc {
                Some(Incoming::Notification { method, params }) => {
                    last_update_at = tokio::time::Instant::now();
                    // Turn-end cost hint (see COST_HINT_GRACE above): arm the
                    // fast settle when the turn's terminal accounting frame
                    // arrives with our prompt still unsettled.
                    if cost_hint_enabled
                        && turn.is_some()
                        && !interrupted
                        && !steered_this_turn
                        // An in-flight steering call means an injection cost
                        // frame may already be on the wire ahead of its
                        // response (select order is not the pipe order).
                        && steering_call.is_none()
                        && starve_deadline.is_none()
                        && method == "session/update"
                        && is_turn_end_cost_update(&params, &session_id)
                    {
                        tracing::debug!(
                            target: "cypher_harness::acp",
                            "turn-end cost update observed with the prompt \
                             unsettled; arming fast settle"
                        );
                        starve_deadline =
                            Some(tokio::time::Instant::now() + COST_HINT_GRACE);
                    }
                    // Other notifications (other sessions, agent noise) are
                    // tolerated by design.
                    let events = if method == "session/update" {
                        session_update_events(&params, &session_id)
                    } else if method == "_session/turn_ended" {
                        // Autonomous turn-end (claude-agent-acp extension,
                        // `_`-prefixed like `_session/steering`): a turn the
                        // agent started on its own — a background-task wake —
                        // has no `session/prompt` to settle, so its SDK-side
                        // turn-end would otherwise vanish at the adapter and
                        // leave the engine's quiesce watchdog as the only
                        // settle path (≤2min of phantom Working per
                        // notification). Gated to BETWEEN prompts: a
                        // live turn settles through its own response.
                        if turn.is_none()
                            && !interrupted
                            && params.get("sessionId").and_then(Value::as_str)
                                == Some(session_id.as_str())
                        {
                            vec![AgentEvent::Done {
                                status: DoneStatus::Completed,
                                result: None,
                                error: None,
                                session_id: Some(session_id.clone()),
                            }]
                        } else {
                            Vec::new()
                        }
                    } else {
                        cursor_notification_events(&method, &params)
                    };
                    for ev in events {
                        track_turn_signals(&ev, &mut turn_content_seen, &mut open_tools);
                        if !send(&event_tx, ev).await {
                            break 'main;
                        }
                    }
                }
                Some(Incoming::Request { id, method, params }) => {
                    for ev in handle_server_request_live(
                        &client,
                        id,
                        &method,
                        &params,
                        &request_input,
                        &open_questions,
                    ) {
                        if !send(&event_tx, ev).await {
                            break 'main;
                        }
                    }
                }
                Some(Incoming::Eof) | None => {
                    // The turn ends via a request RESPONSE, which races EOF
                    // through a different channel than notifications: an agent
                    // exiting right after its final response must read as a
                    // clean finish, not a crash. The response (if any) is
                    // already resolved by the reader before it sends Eof.
                    // Only a RESOLVED response is a clean finish; a request
                    // failed by the reader's EOF cleanup falls through to the
                    // crash-message bookkeeping below (stderr tail intact).
                    if let Some(mut fut) = turn.take()
                        && let Ok(res @ Ok(_)) =
                            tokio::time::timeout(Duration::from_millis(50), &mut fut).await
                    {
                        let (prev, _next) = rotate(&mut assistant_message_id);
                        let _ = send(
                            &event_tx,
                            AgentEvent::AssistantMessageCompleted { assistant_message_id: prev, model: None },
                        )
                        .await;
                        if let Some(usage) = usage_from_response(&res) {
                            let _ = send(&event_tx, usage).await;
                        }
                        let (status, error) = stop_outcome(&res, interrupted);
                        done_current = true;
                        if interrupted {
                            done_after_interrupt = true;
                        }
                        let _ = send(
                            &event_tx,
                            AgentEvent::Done {
                                status,
                                result: None,
                                error,
                                session_id: Some(session_id.clone()),
                            },
                        )
                        .await;
                    }
                    break 'main;
                }
            },

            res = async { steering_call.as_mut().expect("guarded by if").1.as_mut().await },
                if steering_call.is_some() =>
            {
                let (text, _) = steering_call.take().expect("guarded by if");
                let outcome = match &res {
                    Ok(resp) => resp
                        .get("outcome")
                        .and_then(Value::as_str)
                        .unwrap_or("injected")
                        .to_owned(),
                    Err(e) => {
                        tracing::debug!(
                            target: "cypher_harness::acp",
                            "_session/steering failed (redelivering): {e}"
                        );
                        // Failed calls redeliver like a lost turn-end race.
                        "promptRequired".to_owned()
                    }
                };
                if interrupted {
                    // The run is winding down; the steer is abandoned like
                    // any queued steer at interrupt.
                } else if outcome != "promptRequired" {
                    // Injected into a live turn → a Steered boundary. But if
                    // the turn ended while the call was in flight, the
                    // injection was consumed by THAT turn — its output
                    // already streamed and the turn's Done already closed the
                    // segment. Emitting Steered after that Done re-armed the
                    // consumer (parked session → Working) with no next turn
                    // and no Done ever coming — the stranded-Working /
                    // eternal-timer bug. Post-turn: nothing left to do.
                    if turn.is_some() {
                        steered_this_turn = true;
                        // The injection proves the turn is LIVE: any settle
                        // deadline armed off a cost frame that raced this
                        // response is invalid evidence.
                        starve_deadline = None;
                        // Pre-injection updates can still sit in `incoming`
                        // (responses bypass that queue): drain them into the
                        // CURRENT segment first, or text the agent streamed
                        // before the injection landed folds after the split —
                        // the transcript attributes it to the reply-to-steer.
                        let mut consumer_gone = false;
                        while let Ok(inc) = incoming.try_recv() {
                            match inc {
                                Incoming::Notification { method, params } => {
                                    let events = if method == "session/update" {
                                        session_update_events(&params, &session_id)
                                    } else {
                                        cursor_notification_events(&method, &params)
                                    };
                                    for ev in events {
                                        if !send(&event_tx, ev).await {
                                            consumer_gone = true;
                                            break;
                                        }
                                    }
                                }
                                Incoming::Request { id, method, params } => {
                                    for ev in handle_server_request_live(
                                        &client,
                                        id,
                                        &method,
                                        &params,
                                        &request_input,
                                        &open_questions,
                                    ) {
                                        if !send(&event_tx, ev).await {
                                            consumer_gone = true;
                                            break;
                                        }
                                    }
                                }
                                _ => {}
                            }
                            if consumer_gone {
                                break;
                            }
                        }
                        if consumer_gone {
                            break 'main;
                        }
                        let (prev, next) = rotate(&mut assistant_message_id);
                        if !send(
                            &event_tx,
                            AgentEvent::Steered {
                                assistant_message_id: Some(prev),
                                next_assistant_message_id: Some(next),
                            },
                        )
                        .await
                        {
                            break 'main;
                        }
                    }
                } else if turn.is_some() {
                    // Raced the turn end: redeliver at the boundary the
                    // loop is about to hit. `noRunningTurn` is stronger —
                    // the adapter says nothing is running while our prompt
                    // is still outstanding: the starved-turn signature. Arm
                    // the grace deadline; if the prompt's response does not
                    // land first, the recovery arm below settles the dead
                    // turn and promotes this steer.
                    if res
                        .as_ref()
                        .ok()
                        .and_then(|r| r.get("reason"))
                        .and_then(Value::as_str)
                        == Some("noRunningTurn")
                    {
                        tracing::warn!(
                            target: "cypher_harness::acp",
                            "steering answered noRunningTurn with a prompt \
                             outstanding; arming starved-turn recovery"
                        );
                        starve_deadline =
                            Some(tokio::time::Instant::now() + STARVE_GRACE);
                    }
                    queued_steers.push_back(text);
                } else {
                    // The turn ended while the call was in flight and its
                    // boundary already passed — the steer becomes the next
                    // turn directly.
                    let (prev, next) = rotate(&mut assistant_message_id);
                    if !send(
                        &event_tx,
                        AgentEvent::Steered {
                            assistant_message_id: Some(prev),
                            next_assistant_message_id: Some(next),
                        },
                    )
                    .await
                    {
                        break 'main;
                    }
                    done_current = false;
                    turn_content_seen = false;
                    steered_this_turn = false;
                    open_tools.clear();
                    last_update_at = tokio::time::Instant::now();
                    turn = Some(prompt_turn(client.clone(), session_id.clone(), text));
                }
                while let Some(next_text) = steer_backlog.pop_front() {
                    if turn.is_some() && !interrupted {
                        let fut = steering_call_future(&client, &session_id, &next_text);
                        steering_call = Some((next_text, fut));
                        break;
                    }
                    // No live turn to inject into: boundary delivery.
                    queued_steers.push_back(next_text);
                }
            },

            // Busy-session cancel flushed (see BUSY_RECENT/CANCEL_FLUSH
            // above): the unowned self-continued turn had its cancel and a
            // drain window; the queued steer becomes a fresh prompt on a
            // now-idle agent.
            _ = tokio::time::sleep_until(
                cancel_flush_deadline.unwrap_or_else(tokio::time::Instant::now)
            ), if cancel_flush_deadline.is_some() && !interrupted => {
                cancel_flush_deadline = None;
                if turn.is_none()
                    && let Some(text) = queued_steers.pop_front()
                {
                    let (prev, next) = rotate(&mut assistant_message_id);
                    if !send(
                        &event_tx,
                        AgentEvent::Steered {
                            assistant_message_id: Some(prev),
                            next_assistant_message_id: Some(next),
                        },
                    )
                    .await
                    {
                        break 'main;
                    }
                    done_current = false;
                    turn_content_seen = false;
                    steered_this_turn = false;
                    open_tools.clear();
                    last_update_at = tokio::time::Instant::now();
                    turn = Some(prompt_turn(client.clone(), session_id.clone(), text));
                } else if turn.is_none() && !steering_open {
                    // Mailbox closed while the flush waited: nothing left.
                    break 'main;
                }
            },

            // BLANKET quiet settle (see `quiet_settle` above), adapter-
            // agnostic: content streamed, every tool resolved, no question
            // pending, stream quiet past the window with the prompt still
            // unsettled. Feeds the recovery arm below by expiring its
            // deadline — one settle path for all three evidence sources.
            _ = tokio::time::sleep_until(
                last_update_at + quiet_settle.unwrap_or_default()
            ), if quiet_settle.is_some()
                && starve_deadline.is_none()
                && turn.is_some()
                && !interrupted
                && turn_content_seen
                && open_tools.is_empty()
                && open_questions.load(std::sync::atomic::Ordering::SeqCst) == 0 =>
            {
                tracing::warn!(
                    target: "cypher_harness::acp",
                    quiet_ms = quiet_settle.unwrap_or_default().as_millis() as u64,
                    "turn quiet past the settle window with completed output; \
                     treating the prompt response as dropped"
                );
                starve_deadline = Some(tokio::time::Instant::now());
            },

            // Starved-turn recovery: the grace elapsed with the prompt still
            // unsettled after turn-end evidence — the turn's terminal cost
            // frame (COST_HINT_GRACE, ~immediate), a steering call answered
            // noRunningTurn (STARVE_GRACE), or the blanket quiet settle
            // above. Close the dead turn out
            // with a Done — its output already streamed as session/updates
            // and its text was delivered via the CLI's own queue — then
            // promote any queued steer to a fresh prompt, which settles
            // normally on a now-idle agent (verified against the real
            // adapter).
            _ = tokio::time::sleep_until(
                starve_deadline.unwrap_or_else(tokio::time::Instant::now)
            ), if starve_deadline.is_some() && turn.is_some() && !interrupted => {
                starve_deadline = None;
                tracing::warn!(
                    target: "cypher_harness::acp",
                    "prompt response missing past turn-end evidence; settling \
                     the dead turn (and promoting any queued steer)"
                );
                // Drop the dead future: a response that somehow arrives later
                // resolves a closed channel harmlessly.
                turn = None;
                let (prev, _next) = rotate(&mut assistant_message_id);
                if !send(
                    &event_tx,
                    AgentEvent::AssistantMessageCompleted { assistant_message_id: prev, model: None },
                )
                .await
                {
                    break 'main;
                }
                done_current = true;
                if !send(
                    &event_tx,
                    AgentEvent::Done {
                        status: DoneStatus::Completed,
                        result: None,
                        error: None,
                        session_id: Some(session_id.clone()),
                    },
                )
                .await
                {
                    break 'main;
                }
                if let Some(text) = queued_steers.pop_front() {
                    let (prev, next) = rotate(&mut assistant_message_id);
                    if !send(
                        &event_tx,
                        AgentEvent::Steered {
                            assistant_message_id: Some(prev),
                            next_assistant_message_id: Some(next),
                        },
                    )
                    .await
                    {
                        break 'main;
                    }
                    done_current = false;
                    turn_content_seen = false;
                    steered_this_turn = false;
                    open_tools.clear();
                    last_update_at = tokio::time::Instant::now();
                    turn = Some(prompt_turn(client.clone(), session_id.clone(), text));
                } else if !steering_open {
                    // Mirror the normal turn-settled exit: mailbox closed
                    // and nothing left to run — the session is over.
                    break 'main;
                }
            },

            steer = steering.recv(), if steering_open && !interrupted => match steer {
                Some(msg) => {
                    // Same transform as the initial prompt: Claude's
                    // Ultrathink prefix rides every steer too.
                    let text = prompt_transform(request.reasoning, &msg.prompt);
                    if turn.is_none() && cancel_flush_deadline.is_some() {
                        // A busy-session cancel is already in flight: this
                        // steer lines up behind it and dispatches at flush.
                        queued_steers.push_back(text);
                    } else if turn.is_none()
                        && !cost_hint_enabled
                        && (!open_tools.is_empty()
                            || last_update_at.elapsed() < BUSY_RECENT)
                    {
                        // Mid self-continued turn (see BUSY_RECENT above):
                        // cancel it rather than prompt into the starve.
                        //
                        // Claude skips this branch ON PURPOSE and prompts
                        // straight in — its NATIVE semantics: the CLI queues
                        // the message and folds it into the running turn (no
                        // work lost, verified from live session data). The
                        // adapter drops that prompt's reply, and the
                        // cost-frame settle reconstructs it ~1s after the
                        // merged turn really ends. Only adapters with no
                        // verified turn-end frame pay the cancel.
                        tracing::info!(
                            target: "cypher_harness::acp",
                            "steer into a self-continuing session; cancelling \
                             the unowned turn before prompting"
                        );
                        client.notify(
                            "session/cancel",
                            Some(json!({ "sessionId": session_id })),
                        );
                        queued_steers.push_back(text);
                        cancel_flush_deadline =
                            Some(tokio::time::Instant::now() + CANCEL_FLUSH);
                    } else if turn.is_none() {
                        // Idle between turns: a steer is simply the next turn.
                        let (prev, next) = rotate(&mut assistant_message_id);
                        if !send(
                            &event_tx,
                            AgentEvent::Steered {
                                assistant_message_id: Some(prev),
                                next_assistant_message_id: Some(next),
                            },
                        )
                        .await
                        {
                            break 'main;
                        }
                        done_current = false;
                        turn_content_seen = false;
                        steered_this_turn = false;
                        open_tools.clear();
                        last_update_at = tokio::time::Instant::now();
                        turn = Some(prompt_turn(client.clone(), session_id.clone(), text));
                    } else if steer_ext {
                        // Mid-turn injection via the `_session/steering`
                        // extension: start the call, resolved by its own
                        // select branch. One call in flight at a time;
                        // followers wait in the backlog.
                        if steering_call.is_some() {
                            steer_backlog.push_back(text);
                        } else {
                            let fut = steering_call_future(&client, &session_id, &text);
                            steering_call = Some((text, fut));
                        }
                    } else {
                        // No extension (Grok today): turn-boundary delivery.
                        queued_steers.push_back(text);
                    }
                }
                None => {
                    steering_open = false;
                    if turn.is_none() && queued_steers.is_empty() {
                        break 'main;
                    }
                }
            },

            _ = interrupt.cancelled(), if !interrupt_sent => {
                interrupt_sent = true;
                interrupted = true;
                if turn.is_some() {
                    client.notify("session/cancel", Some(json!({ "sessionId": session_id })));
                    // Escalate if the agent doesn't wind down (stopReason
                    // "cancelled") within the grace periods.
                    if let Some(pid) = child.id() {
                        escalation = Some(tokio::spawn(async move {
                            tokio::time::sleep(interrupt_grace).await;
                            send_signal(pid, Signal::Term);
                            tokio::time::sleep(kill_grace).await;
                            send_signal(pid, Signal::Kill);
                        }));
                    }
                } else {
                    // Idle between turns: nothing to cancel — the terminal
                    // bookkeeping below still guarantees Done { Interrupted }.
                    break 'main;
                }
            },

            _ = event_tx.closed() => break 'main,
        }
    }

    // Terminal bookkeeping: never end the stream without a Done unless the
    // consumer already hung up.
    if !event_tx.is_closed() {
        if interrupted && !done_after_interrupt {
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: Some(session_id.clone()),
                }))
                .await;
        } else if !interrupted && !done_current {
            // A child killed mid-turn must not read as a silent success.
            let status = child.try_wait().ok().flatten();
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(crate::crash_message(agent_name, status, &stderr_tail)),
                    session_id: Some(session_id.clone()),
                }))
                .await;
        }
    }

    // Escalation dies BEFORE the child is reaped: after `shutdown_child`
    // waits the pid, a still-armed SIGTERM/SIGKILL timer would fire at a
    // freed (reusable) pid.
    if let Some(handle) = escalation {
        handle.abort();
    }
    shutdown_child(&mut child, kill_grace).await;
}
