//! Scripted replies, for seeding a development instance with data that
//! reads like real work (a workspace's plans, routines and sessions).
//!
//! Two pieces, both off unless asked for:
//!
//! - **A scripted reply.** A prompt holding `[mock:script]<json>[/mock:script]`
//!   is answered with the steps the JSON lists, in order, instead of the
//!   harness's stock reply. A step is one of
//!   `{"text": "…"}`, `{"think": "…"}`,
//!   `{"tool": {"id": "…", "call": <ToolCall>, "output": "…", "error": false}}`
//!   (the call, then its result), `{"ask": [<UserInputQuestion>, …]}` (the
//!   run waits for the answer, a permission prompt included) and
//!   `{"hold": "<command>"}` (the command starts and the run keeps working
//!   until it is stopped; nothing after it runs).
//! - **Rules.** `CYPHER_MOCK_RULES` names a JSON file, read on every run:
//!   `[{"match": ["…", "…"], "script": [<step>, …]}, …]`. The first rule
//!   whose `match` strings all occur in the prompt answers it, with its
//!   `script` or as if the prompt were its `prompt`. So prompts a seed
//!   script cannot write itself (a plan's brief, a routine's run, the notice
//!   that a job ended) get a fitting reply, and no marker shows in a
//!   transcript.

use cypher_proto::{AgentEvent, ToolCall, UserInputQuestion};
use serde_json::Value;

/// Opens a scripted reply's steps in a prompt.
pub const MARKER: &str = "[mock:script]";
/// Closes them.
pub const END: &str = "[/mock:script]";

/// One step of a scripted reply.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ScriptStep {
    Event(AgentEvent),
    Ask(Vec<UserInputQuestion>),
    /// The command left running.
    Hold(String),
}

/// The steps a prompt scripts; `None` without the marker, or when its JSON
/// does not read (the stock reply answers, and the log says why).
pub(super) fn steps(prompt: &str) -> Option<Vec<ScriptStep>> {
    let (_, rest) = prompt.split_once(MARKER)?;
    let json = rest.split_once(END).map_or(rest, |(json, _)| json);
    match serde_json::from_str::<Value>(json.trim()) {
        Ok(value) => match parse(&value) {
            Ok(steps) => Some(steps),
            Err(why) => {
                tracing::warn!(%why, "mock script ignored");
                None
            }
        },
        Err(err) => {
            tracing::warn!(error = %err, "mock script is not JSON");
            None
        }
    }
}

fn parse(value: &Value) -> Result<Vec<ScriptStep>, String> {
    let list = value.as_array().ok_or("the script is not a list")?;
    let mut steps = Vec::new();
    for step in list {
        let step = step.as_object().ok_or("a step is not an object")?;
        let (kind, body) = step.iter().next().ok_or("a step is empty")?;
        let text = || {
            body.as_str()
                .map(str::to_owned)
                .ok_or(format!("\"{kind}\" takes a string"))
        };
        match kind.as_str() {
            "text" => steps.push(ScriptStep::Event(AgentEvent::TextDelta { text: text()? })),
            "think" => steps.push(ScriptStep::Event(AgentEvent::ReasoningDelta {
                text: text()?,
            })),
            "hold" => steps.push(ScriptStep::Hold(text()?)),
            "ask" => steps.push(ScriptStep::Ask(
                serde_json::from_value(body.clone()).map_err(|err| format!("ask: {err}"))?,
            )),
            "tool" => {
                let id = body
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("a tool needs an id")?
                    .to_owned();
                let call: ToolCall =
                    serde_json::from_value(body.get("call").cloned().unwrap_or(Value::Null))
                        .map_err(|err| format!("tool call: {err}"))?;
                steps.push(ScriptStep::Event(AgentEvent::ToolCall {
                    id: id.clone(),
                    call,
                }));
                steps.push(ScriptStep::Event(AgentEvent::ToolResult {
                    id,
                    is_error: body.get("error").and_then(Value::as_bool).unwrap_or(false),
                    output: body
                        .get("output")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    diff: None,
                    permission: None,
                }));
            }
            other => return Err(format!("unknown step \"{other}\"")),
        }
    }
    Ok(steps)
}

/// The prompt the rules file answers `prompt` as, when one of its rules
/// matches ([`rewrite_with`]). `None` with no rules file, or none matching.
pub(super) fn rewrite(prompt: &str) -> Option<String> {
    let path = cypher_env::var("MOCK_RULES")?;
    let rules = match std::fs::read_to_string(&path) {
        Ok(rules) => rules,
        Err(err) => {
            tracing::warn!(%path, error = %err, "mock rules not read");
            return None;
        }
    };
    match serde_json::from_str::<Value>(&rules) {
        Ok(rules) => rewrite_with(&rules, prompt),
        Err(err) => {
            tracing::warn!(%path, error = %err, "mock rules are not JSON");
            None
        }
    }
}

/// The first rule of `rules` whose `match` strings (one, or a list) all
/// occur in `prompt`: its `script` as a scripted prompt, else its `prompt`.
fn rewrite_with(rules: &Value, prompt: &str) -> Option<String> {
    rules.as_array()?.iter().find_map(|rule| {
        let wanted = rule.get("match")?;
        let matches = match wanted {
            Value::String(one) => prompt.contains(one.as_str()),
            Value::Array(all) => {
                !all.is_empty()
                    && all
                        .iter()
                        .all(|part| part.as_str().is_some_and(|part| prompt.contains(part)))
            }
            _ => false,
        };
        if !matches {
            return None;
        }
        match rule.get("script") {
            Some(script) => Some(format!("{MARKER}{script}{END}")),
            None => rule.get("prompt")?.as_str().map(str::to_owned),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_script_lists_its_steps_in_order() {
        let script = json!([
            { "think": "Look first." },
            { "tool": { "id": "t1", "call": { "kind": "exec", "command": "ls" }, "output": "a\nb" } },
            { "tool": { "id": "t2", "call": { "kind": "unknown", "name": "propose_plan" }, "error": true } },
            { "ask": [{ "id": "q", "header": "Table", "question": "Which?", "options": ["a", "b"] }] },
            { "text": "Done." },
            { "hold": "cargo test" },
        ]);
        let prompt = format!("Do it. {MARKER}{script}{END} (and a note after)");
        let steps = steps(&prompt).expect("a script");
        assert_eq!(steps.len(), 8, "a tool is its call and its result");
        assert!(
            matches!(&steps[0], ScriptStep::Event(AgentEvent::ReasoningDelta { text }) if text == "Look first.")
        );
        assert!(
            matches!(&steps[1], ScriptStep::Event(AgentEvent::ToolCall { id, call: ToolCall::Exec { command } }) if id == "t1" && command == "ls")
        );
        assert!(
            matches!(&steps[2], ScriptStep::Event(AgentEvent::ToolResult { id, is_error: false, output: Some(output), .. }) if id == "t1" && output == "a\nb")
        );
        assert!(matches!(
            &steps[4],
            ScriptStep::Event(AgentEvent::ToolResult {
                is_error: true,
                output: None,
                ..
            })
        ));
        assert!(
            matches!(&steps[5], ScriptStep::Ask(questions) if questions[0].options == ["a", "b"])
        );
        assert_eq!(steps[7], ScriptStep::Hold("cargo test".into()));
    }

    #[test]
    fn a_prompt_without_a_readable_script_gets_the_stock_reply() {
        assert_eq!(steps("Just words."), None);
        assert_eq!(steps(&format!("{MARKER}not json{END}")), None);
        assert_eq!(steps(&format!("{MARKER}[{{\"dance\": 1}}]{END}")), None);
        assert_eq!(steps(&format!("{MARKER}[{{\"text\": 1}}]{END}")), None);
        // An unclosed script reads to the prompt's end.
        assert_eq!(
            steps(&format!("{MARKER}[{{\"text\": \"hi\"}}]")),
            Some(vec![ScriptStep::Event(AgentEvent::TextDelta {
                text: "hi".into()
            })])
        );
    }

    #[test]
    fn the_first_rule_whose_strings_all_occur_answers() {
        let rules = json!([
            { "match": ["A notice", "Morning brief"], "script": [{ "text": "Nothing needs you." }] },
            { "match": "Morning brief", "prompt": "Summarise. [mock:hold]" },
            { "match": [], "prompt": "never" },
            { "match": 7, "prompt": "never" },
        ]);
        let notice = rewrite_with(
            &rules,
            "[Routine \"Morning brief\" finished] … (A notice …)",
        )
        .expect("the notice's rule");
        assert_eq!(
            steps(&notice),
            Some(vec![ScriptStep::Event(AgentEvent::TextDelta {
                text: "Nothing needs you.".into()
            })])
        );
        assert_eq!(
            rewrite_with(&rules, "Routine \"Morning brief\" · Monday").as_deref(),
            Some("Summarise. [mock:hold]")
        );
        assert_eq!(rewrite_with(&rules, "Something else"), None);
        assert_eq!(rewrite_with(&json!({}), "Morning brief"), None);
    }
}
