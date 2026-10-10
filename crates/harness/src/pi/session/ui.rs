//! Extension UI requests: dialogs bridged to the engine's input requests,
//! notify output, and the structured `setStatus` projections.

use super::*;

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
    // Pi resolves a dialog with a `timeout` to its default on its own and
    // tells no one. Give up on the answer at the same moment: dropping the
    // receiver withdraws the question, instead of leaving it open for an
    // answer Pi would ignore.
    let timeout = payload
        .get("timeout")
        .and_then(Value::as_f64)
        .filter(|ms| *ms > 0.0)
        .and_then(|ms| std::time::Duration::try_from_secs_f64(ms / 1000.0).ok());
    let client = client.clone();
    let request_input = std::sync::Arc::clone(&request_input);
    // Owned copies for the spawned task (the caller's refs are not 'static).
    let id = id.to_owned();
    let method = method.to_owned();
    tokio::spawn(async move {
        let answer = (request_input)(vec![question.clone()]);
        let answers = match timeout {
            Some(limit) => match tokio::time::timeout(limit, answer).await {
                Ok(answers) => answers.unwrap_or_default(),
                Err(_) => return,
            },
            None => answer.await.unwrap_or_default(),
        };
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
pub fn ui_response_payload(method: &str, picked: Option<&str>) -> Value {
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

impl PiRun {
    pub(super) async fn on_ui_request(
        &mut self,
        id: String,
        method: String,
        payload: Value,
    ) -> Flow {
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
            // classification table in docs/design/pi-rpc.md).
            _ => {}
        }
        Flow::Continue
    }
}
