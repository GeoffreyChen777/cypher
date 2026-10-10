//! Model catalog discovery: mapping pi's model directory onto [`Model`] rows.

use std::collections::HashSet;
use std::time::Duration;

use cypher_proto::Model;
use serde_json::Value;

use super::model_ladder;

/// Human-readable context-window tag: `1M` at a million and up (`1.5M`
/// for fractional), `k` below that — never "1000k".
pub(super) fn context_window_tag(w: u64) -> String {
    if w >= 1_000_000 {
        let millions = w as f64 / 1_000_000.0;
        let text = if (millions - millions.round()).abs() < f64::EPSILON {
            format!("{}", millions.round() as u64)
        } else {
            format!("{millions:.1}")
        };
        format!("{text}M context")
    } else {
        format!("{}k context", w.div_ceil(1000))
    }
}

/// Map a `get_available_models` entry onto the picker [`Model`]:
/// `id = "{provider}/{modelId}"` (pi's CLI provider/id convention), `label =
/// name`, and `description = "{provider} · {n}k context"` — the picker
/// renders the description on the row's muted subline, which is what
/// distinguishes the same vendor model served by several providers (deepseek
/// vs opencode-go vs … all offering "DeepSeek V4 Flash"). The reasoning
/// ladder is the model's own ([`model_ladder`]).
fn model_from_wire(m: &Value) -> Option<Model> {
    let model_id = m.get("id").and_then(Value::as_str)?;
    let provider = m.get("provider").and_then(Value::as_str).unwrap_or("pi");
    let name = m.get("name").and_then(Value::as_str).unwrap_or(model_id);
    let description = match m.get("contextWindow").and_then(Value::as_u64) {
        Some(w) => format!("{provider} · {}", context_window_tag(w)),
        None => provider.to_owned(),
    };
    Some(Model {
        id: format!("{provider}/{model_id}"),
        label: name.to_owned(),
        description: Some(description),
        reasoning_levels: model_ladder(m),
        options: Vec::new(),
    })
}

fn catalog_providers(models: &[Model]) -> HashSet<String> {
    models
        .iter()
        .filter_map(|model| {
            model
                .id
                .split_once('/')
                .map(|(provider, _)| provider.to_string())
        })
        .collect()
}

/// Providers that should appear in a complete catalog for this agent dir:
/// NewAPI gateways plus pi-claude-bridge when that package is enabled.
pub(super) fn expected_model_providers(agent_dir: &std::path::Path) -> HashSet<String> {
    let mut expected = HashSet::new();
    let newapi = agent_dir.join("extension-settings/provider-newapi.json");
    if let Some(value) = std::fs::read_to_string(newapi)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        && let Some(providers) = value.get("providers").and_then(Value::as_object)
    {
        expected.extend(providers.keys().cloned());
    }
    let settings = agent_dir.join("settings.json");
    if let Some(value) = std::fs::read_to_string(settings)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        && let Some(packages) = value.get("packages").and_then(Value::as_array)
        && packages.iter().any(|entry| {
            let source = match entry {
                Value::String(source) => source.as_str(),
                Value::Object(object) => object.get("source").and_then(Value::as_str).unwrap_or(""),
                _ => "",
            };
            source.contains("pi-claude-bridge")
        })
    {
        expected.insert("claude-bridge".into());
    }
    expected
}

pub(super) fn catalog_covers(models: &[Model], expected: &HashSet<String>) -> bool {
    !expected.is_empty() && expected.is_subset(&catalog_providers(models))
}

pub(super) fn models_from_response(resp: &Value) -> Vec<Model> {
    resp.get("models")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(model_from_wire)
        .collect()
}

/// Build the picker catalog from pi's model directory response, falling back
/// to the model currently selected in `get_state` when the directory is empty.
///
/// pi can have a valid configured provider/model (including custom providers)
/// while `get_available_models` returns `{ models: [] }`. In that case the
/// current state is still enough to offer a concrete model rather than leaving
/// the Cypher picker empty.
pub(super) fn models_from_responses(available: &Value, state: &Value) -> Vec<Model> {
    let models = models_from_response(available);
    if !models.is_empty() {
        return models;
    }
    state
        .get("model")
        .and_then(model_from_wire)
        .into_iter()
        .collect()
}

/// Result of a short-lived pi model probe. `from_catalog` is false when the
/// directory snapshot stayed empty and we fell back to `get_state.model` —
/// that fallback must not be cached, or the picker would keep a single row
/// even after the catalog finishes loading.
pub(super) struct DiscoveredModels {
    pub(super) models: Vec<Model>,
    pub(super) from_catalog: bool,
}

/// Pause between `get_available_models` snapshots. 200ms is well under
/// the catalog refresh we measured (~3s) without spinning the child.
pub(super) const MODEL_CATALOG_POLL: Duration = Duration::from_millis(200);
/// After the first non-empty snapshot, keep polling this long without growth
/// so an instantly-registered extension catalog cannot hide gateway providers
/// that finish loading a moment later.
pub(super) const MODEL_CATALOG_STABLE: Duration = Duration::from_millis(2000);
