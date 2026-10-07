//! Model discovery: turning a `session/new` response's models and config
//! options into [`Model`] rows with trait options.

use super::*;

/// Map an advertised `thought_level` value id onto cypher's ladder.
fn reasoning_from_value(value: &str) -> Option<ReasoningLevel> {
    match norm_id(value).as_str() {
        "minimal" => Some(ReasoningLevel::Minimal),
        "low" => Some(ReasoningLevel::Low),
        "medium" => Some(ReasoningLevel::Medium),
        "high" => Some(ReasoningLevel::High),
        "xhigh" => Some(ReasoningLevel::XHigh),
        "max" => Some(ReasoningLevel::Max),
        "ultra" => Some(ReasoningLevel::Ultra),
        "ultracode" => Some(ReasoningLevel::Ultracode),
        "ultrathink" => Some(ReasoningLevel::Ultrathink),
        _ => None,
    }
}

/// Derive the model list a `session/new` response advertises. The `model`
/// config option's choices come FIRST, the legacy first-class `models` state
/// is only a fallback: the org adapters enumerate one `availableModels` entry
/// per model × effort combination on that deprecated surface (Zed dropped it
/// entirely), while their `configOptions` carry base model ids with effort as
/// a separate `thought_level` option. `[1m]`-suffixed long-context variants
/// collapse into the base model's Context Window trait, matching the static
/// catalogs. Traits come off the wire too — every select/boolean config
/// option outside mode/model/thought_level becomes a `ModelOption` — so
/// unmatched models keep fast mode etc.; the catalog only enriches matched
/// ids with label/description/per-model ladders.
pub(super) fn models_from_session(session_response: &Value, catalog: &[Model]) -> Vec<Model> {
    let config_options = session_response
        .get("configOptions")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default();

    let ladder: Vec<ReasoningLevel> = config_options
        .iter()
        .find(|o| o.get("category").and_then(Value::as_str) == Some("thought_level"))
        .and_then(|o| o.get("options").and_then(Value::as_array))
        .map(|opts| {
            opts.iter()
                .filter_map(|o| o.get("value").and_then(Value::as_str))
                .filter_map(reasoning_from_value)
                .collect()
        })
        .unwrap_or_default();
    let wire_options: Vec<ModelOption> = config_options
        .iter()
        .filter_map(trait_from_config_option)
        .collect();

    let exact = |id: &str| catalog.iter().find(|m| norm_id(&m.id) == norm_id(id));
    // Family-alias catalog row: the claude adapter advertises bare aliases
    // (`opus`, `sonnet`, `haiku`) meaning "the current generation" — match
    // them to the first (flagship-ordered) catalog row of that family so
    // the picker shows the curated label/ladder ("Opus 5") instead of the
    // terse alias. Alphabetic-only ids ONLY: versioned ids
    // (`gpt-5.2-codex`) must never fuzzy-match a foreign row.
    let alias = |id: &str| {
        let norm = norm_id(id);
        (!norm.is_empty() && norm.chars().all(|c| c.is_ascii_alphabetic()))
            .then(|| catalog.iter().find(|m| norm_id(&m.id).contains(&norm)))
            .flatten()
    };
    let build = |id: &str,
                 name: Option<&str>,
                 description: Option<&str>,
                 options: Vec<ModelOption>|
     -> Model {
        let exact = exact(id);
        let aliased = if exact.is_none() { alias(id) } else { None };
        let known = exact.or(aliased);
        // The wire name wins for real ids; an ALIAS row's terse wire name
        // ("Opus") loses to the curated family label/description.
        Model {
            id: id.to_owned(),
            label: aliased
                .map(|m| m.label.clone())
                .or_else(|| name.map(str::to_owned))
                .or_else(|| known.map(|m| m.label.clone()))
                .unwrap_or_else(|| id.to_owned()),
            description: aliased
                .and_then(|m| m.description.clone())
                .or_else(|| description.map(str::to_owned))
                .or_else(|| known.and_then(|m| m.description.clone())),
            reasoning_levels: match known.filter(|m| !m.reasoning_levels.is_empty()) {
                Some(m) => m.reasoning_levels.clone(),
                None => ladder.clone(),
            },
            options,
        }
    };

    let model_select: Vec<&Value> = config_options
        .iter()
        .find(|o| o.get("category").and_then(Value::as_str) == Some("model"))
        .and_then(|o| o.get("options").and_then(Value::as_array))
        .map(|opts| opts.iter().collect())
        .unwrap_or_default();
    if !model_select.is_empty() {
        let raw_ids: Vec<&str> = model_select
            .iter()
            .filter_map(|o| o.get("value").and_then(Value::as_str))
            .collect();
        // `default` is an ALIAS row (Claude Code's "Default (recommended)"),
        // duplicating whichever real model the CLI resolves it to — dropped
        // whenever a real row exists (it read as clutter in the picker, user
        // request). Send-side, a chat that saved `default` still matches the
        // advertised value exactly.
        let has_real = raw_ids.iter().any(|id| norm_id(id) != "default");
        return model_select
            .iter()
            .filter_map(|o| {
                let id = o.get("value").and_then(Value::as_str)?;
                if has_real && norm_id(id) == "default" {
                    return None;
                }
                let name = o.get("name").and_then(Value::as_str);
                let description = o.get("description").and_then(Value::as_str);
                let mut options = wire_options.clone();
                if let Some(base) = strip_context_hint(id) {
                    // A 1M variant with its bare base advertised too folds
                    // into THAT row's Context Window trait (added below).
                    if raw_ids.contains(&base) {
                        return None;
                    }
                    // Orphan 1M variant (`opus[1m]` with no bare `opus` —
                    // the CLI pins the 1M window): present it AS the base
                    // model with the trait defaulting to 1M, instead of a
                    // one-off "Opus (1M context)" row (user request). The
                    // send path recomposes the advertised id via
                    // `pick_model_value`'s compose/family fallback.
                    let mut window = crate::claude::catalog::context_window();
                    window.default_choice = "1m".into();
                    options.push(window);
                    return Some(build(
                        base,
                        name.map(strip_trailing_parenthetical)
                            .filter(|n| !n.is_empty()),
                        description,
                        options,
                    ));
                }
                if raw_ids
                    .iter()
                    .any(|raw| strip_context_hint(raw) == Some(id))
                {
                    options.push(crate::claude::catalog::context_window());
                }
                Some(build(id, name, description, options))
            })
            .collect();
    }

    // Legacy fallback for agents predating session config options. The
    // catalog's own option sets apply here — nothing arrives on the wire.
    session_response
        .get("models")
        .and_then(|m| m.get("availableModels"))
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            let id = m.get("modelId").and_then(Value::as_str)?;
            Some(build(
                id,
                m.get("name").and_then(Value::as_str),
                m.get("description").and_then(Value::as_str),
                exact(id).map(|k| k.options.clone()).unwrap_or_default(),
            ))
        })
        .collect()
}

/// A session config option surfaced as a Traits-dropdown section. Mode is
/// cypher's own (forced to the no-prompts choice), model rides the model rows,
/// and thought_level is the Reasoning ladder — everything else the agent
/// advertises (fast mode, collaboration mode, agent persona, …) passes
/// through. `currentValue` doubles as the default: it is the state the
/// session opens in. Booleans render as an off/on select, mirroring the
/// catalogs (cypher never declares the boolean config capability, so adapters
/// send selects, but handle the shape defensively).
fn trait_from_config_option(option: &Value) -> Option<ModelOption> {
    if matches!(
        option.get("category").and_then(Value::as_str),
        Some("mode" | "model" | "thought_level")
    ) {
        return None;
    }
    let id = option.get("id").and_then(Value::as_str)?;
    let label = option.get("name").and_then(Value::as_str).unwrap_or(id);
    match option.get("type").and_then(Value::as_str)? {
        "select" => {
            let choices: Vec<ModelOptionChoice> = option
                .get("options")
                .and_then(Value::as_array)?
                .iter()
                .filter_map(|c| {
                    let id = c.get("value").and_then(Value::as_str)?;
                    Some(ModelOptionChoice {
                        id: id.to_owned(),
                        label: c
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or(id)
                            .to_owned(),
                    })
                })
                .collect();
            let default_choice = option
                .get("currentValue")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| choices.first().map(|c| c.id.clone()))?;
            (choices.len() > 1).then(|| ModelOption {
                id: id.to_owned(),
                label: label.to_owned(),
                choices,
                default_choice,
            })
        }
        "boolean" => Some(ModelOption {
            id: id.to_owned(),
            label: label.to_owned(),
            choices: vec![
                ModelOptionChoice {
                    id: "off".into(),
                    label: "Off".into(),
                },
                ModelOptionChoice {
                    id: "on".into(),
                    label: "On".into(),
                },
            ],
            default_choice: if option.get("currentValue") == Some(&Value::Bool(true)) {
                "on".into()
            } else {
                "off".into()
            },
        }),
        _ => None,
    }
}
