//! Pure helpers: the draft config, default resolution, labels + traits
//! summary, folder-browser navigation and the model/harness catalog hygiene.

use super::*;

/// Everything a new chat is configured with before the first send. The folder
/// and device come from the selected SPACE — the draft only carries the git
/// extras (ref + checkout kind) and the run config.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DraftConfig {
    pub harness: Option<HarnessId>,
    pub model: Option<String>,
    pub reasoning: Option<ReasoningLevel>,
    /// option id → choice id (only non-defaults are meaningful).
    pub model_options: serde_json::Map<String, serde_json::Value>,
    /// The picked ref (base branch in NewWorktree mode; a worktree's branch
    /// when reusing one). `None` = the repo's current branch.
    pub branch: Option<String>,
    /// Where the new session runs (the t3code env-mode).
    pub checkout: CheckoutKind,
}

/// Where a new session runs (t3code's env-mode: `local | worktree`). "Current
/// worktree" is NOT a third mode — it's `Local` when the picked ref is already
/// materialized as a worktree (the session reuses that checkout's path).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CheckoutKind {
    /// The space's own folder — or the picked ref's existing worktree.
    #[default]
    Local,
    /// A fresh isolated worktree created off the picked base ref on send.
    NewWorktree,
}

/// The resolved on-send checkout action (composer consumes this — see
/// [`Pickers::checkout_plan`]).
#[derive(Debug, Clone, PartialEq)]
pub enum CheckoutPlan {
    /// Run in the space folder as-is. `branch` is the checkout's branch (the
    /// picked or current ref), carried onto `createChat` so the session names
    /// it from the first frame; `None` = refs never loaded.
    CurrentCheckout { branch: Option<String> },
    /// Reuse the picked ref's existing worktree (a cwd override; no git).
    /// `branch` is optional — a detached worktree has no branch to name the
    /// session from.
    ReuseWorktree {
        path: String,
        branch: Option<String>,
    },
    /// `CreateWorktree` off `base` on send (cypher mints a `cypher/<name>`
    /// branch). `base: None` = refs never loaded — send falls back to the
    /// space folder rather than failing.
    NewWorktree { base: Option<String> },
}

/// A Picker-owned, space-scoped checkout target pinned programmatically — the
/// sidebar's hover add buttons. Authoritative WITHOUT the refs list (a new
/// session lands in this checkout even before ListRefs ever loads), and only
/// applies to the new-session canvas of its own space: the state observer
/// clears it on any project/chat change, and the pickers clear it on any
/// explicit user ref/checkout pick.
pub(super) struct PinnedCheckout {
    /// Space id this pin belongs to (checked against the canvas's selection
    /// before it may apply — it never leaks onto another project).
    pub(super) space: String,
    /// The checkout to target: an existing worktree path or the project's
    /// ordinary/current checkout.
    pub(super) plan: CheckoutPlan,
}

/// The fully-resolved run configuration the composer sends: concrete harness,
/// model and reasoning (never a "default" passthrough once the catalog is
/// loaded), plus the explicit non-default option picks.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedRunConfig {
    pub harness: Option<HarnessId>,
    pub model: Option<String>,
    pub reasoning: Option<ReasoningLevel>,
    pub model_options: serde_json::Map<String, serde_json::Value>,
}

impl ResolvedRunConfig {
    /// The `ChatConfig` recorded on `Mutate createChat` (needs a known harness).
    pub fn chat_config(&self) -> Option<ChatConfig> {
        Some(ChatConfig {
            harness: self.harness?,
            model: self.model.clone(),
            reasoning: self.reasoning,
            model_options: self.model_options.clone(),
            sandbox: SandboxLevel::WorkspaceWrite,
        })
    }
}

// ---------------------------------------------------------------------------
// Pure: default resolution (no "Default" placeholders — a concrete pick always)
// ---------------------------------------------------------------------------

/// The harness's default model: the first catalog row (both curated catalogs
/// lead with the flagship — zeron's `pickDefaultModel` Opus preference maps to
/// the same row here).
pub fn default_model(models: &[Model]) -> Option<&Model> {
    models.first()
}

/// A model's default reasoning: X-High when the ladder offers it (zeron
/// `DEFAULT_REASONING = "xhigh"`), else High, else the ladder's first entry.
/// `None` only for ladder-less models (e.g. Haiku's thinking toggle instead).
pub fn default_reasoning(ladder: &[ReasoningLevel]) -> Option<ReasoningLevel> {
    // The recommended default is High (user-corrected — not X-High globally);
    // fall to Medium then the ladder's first entry for shorter ladders.
    if ladder.contains(&ReasoningLevel::High) {
        return Some(ReasoningLevel::High);
    }
    if ladder.contains(&ReasoningLevel::Medium) {
        return Some(ReasoningLevel::Medium);
    }
    ladder.first().copied()
}

/// Clamp a picked/remembered level to what the model actually offers: keep it
/// when the ladder lists it, else fall to the model's default (never a stale
/// or foreign level — zeron use-run-config.ts's derived-model discipline).
pub fn clamp_reasoning(
    level: Option<ReasoningLevel>,
    ladder: &[ReasoningLevel],
) -> Option<ReasoningLevel> {
    match level {
        Some(level) if ladder.contains(&level) => Some(level),
        _ => default_reasoning(ladder),
    }
}

// ---------------------------------------------------------------------------
// Pure: labels + traits summary
// ---------------------------------------------------------------------------

pub fn reasoning_label(level: ReasoningLevel) -> &'static str {
    match level {
        ReasoningLevel::Minimal => "Minimal",
        ReasoningLevel::Low => "Low",
        ReasoningLevel::Medium => "Medium",
        ReasoningLevel::High => "High",
        ReasoningLevel::XHigh => "X-High",
        ReasoningLevel::Max => "Max",
        ReasoningLevel::Ultra => "Ultra",
        ReasoningLevel::Ultracode => "Ultracode",
        ReasoningLevel::Ultrathink => "Ultrathink",
    }
}

/// The TraitsPicker trigger summary: the effective reasoning level plus every
/// model option's effective choice — the explicit pick when one is saved and
/// still offered, else the option's default — joined with " · " ("High · 1M ·
/// Fast", Cursor's "Agent · Balance"). Defaults are spelled out rather than
/// hidden so the run's configuration reads without opening the popover; `None`
/// only when the model has nothing to describe (no ladder, no options).
pub fn traits_summary(
    model: Option<&Model>,
    reasoning: Option<ReasoningLevel>,
    selections: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(level) = reasoning {
        parts.push(reasoning_label(level).to_string());
    }
    if let Some(model) = model {
        for option in &model.options {
            let choice_id = selections
                .get(&option.id)
                .and_then(|v| v.as_str())
                .filter(|id| option.choices.iter().any(|c| c.id == *id))
                .unwrap_or(&option.default_choice);
            if let Some(choice) = option.choices.iter().find(|c| c.id == choice_id) {
                parts.push(choice.label.clone());
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// Whether any trait departs from its default — the trigger brightens only
/// then, so a customized run still stands out now that the summary always
/// names the effective choices.
pub fn traits_customized(
    model: Option<&Model>,
    reasoning: Option<ReasoningLevel>,
    ladder: &[ReasoningLevel],
    selections: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    if reasoning != default_reasoning(ladder) {
        return true;
    }
    model.is_some_and(|model| {
        model.options.iter().any(|option| {
            selections
                .get(&option.id)
                .and_then(|v| v.as_str())
                .is_some_and(|id| {
                    id != option.default_choice && option.choices.iter().any(|c| c.id == id)
                })
        })
    })
}

// ---------------------------------------------------------------------------
// Pure: folder-browser navigation (used by the shell's add-space flow)
// ---------------------------------------------------------------------------

/// Parent of an absolute path; `None` at the filesystem root.
pub fn parent_path(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None; // was "/" (or empty)
    }
    match trimmed.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(at) => Some(trimmed[..at].to_string()),
        None => None,
    }
}

/// Join a listing path and an entry name.
pub fn child_path(base: &str, name: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// Byte length of `name`'s prefix matching `query`, compared char-for-char
/// case-insensitively; `None` when `query` isn't a prefix of `name`. The
/// length indexes into `name` (not `query`) so the completion suffix keeps
/// the folder's real casing: `("Documents", "doc") → Some(3)` → `"uments"`.
pub fn completion_prefix_len(name: &str, query: &str) -> Option<usize> {
    let mut len = 0;
    let mut name_chars = name.chars();
    for qc in query.chars() {
        let nc = name_chars.next()?;
        if !nc.to_lowercase().eq(qc.to_lowercase()) {
            return None;
        }
        len += nc.len_utf8();
    }
    Some(len)
}

/// Resolve a typed path segment against folder `names` (slash-descend):
/// exact match first — case-SENSITIVE before case-insensitive, so `GitHub/`
/// picks a `GitHub` sibling over `github` — then a unique case-insensitive
/// prefix. Ambiguity resolves to `None`: the slash stays in the query.
pub fn segment_target(names: &[&str], query: &str) -> Option<usize> {
    if let Some(ix) = names.iter().position(|n| *n == query) {
        return Some(ix);
    }
    if let Some(ix) = names
        .iter()
        .position(|n| completion_prefix_len(n, query) == Some(n.len()))
    {
        return Some(ix);
    }
    let mut hits = names
        .iter()
        .enumerate()
        .filter(|(_, n)| completion_prefix_len(n, query).is_some());
    let (ix, _) = hits.next()?;
    hits.next().is_none().then_some(ix)
}

/// Breadcrumb segments for a path: `(label, full path)`, root first.
pub fn breadcrumbs(path: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![("/".to_string(), "/".to_string())];
    let mut acc = String::new();
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        acc.push('/');
        acc.push_str(segment);
        out.push((segment.to_string(), acc.clone()));
    }
    out
}

/// Directory rows of a listing (files never render in the browser).
pub fn browser_rows(listing: &FolderListing) -> Vec<&cypher_proto::FolderEntry> {
    listing.entries.iter().filter(|e| e.is_dir).collect()
}

/// Display-side model-list hygiene for catalogs that still carry alias and
/// 1M-variant rows (the space's device may run any version): the `default`
/// alias row drops when a
/// real row exists, and an orphan `<model>[1m]` variant presents as its base
/// id with the Context Window trait pinned to 1M. Idempotent over
/// already-clean lists. The send path recomposes the advertised id from the
/// base + trait (`pick_model_value`), so a folded pick still runs.
pub(crate) fn normalize_model_rows(models: Vec<Model>) -> Vec<Model> {
    fn strip_1m(id: &str) -> Option<&str> {
        id.strip_suffix("[1m]").or_else(|| id.strip_suffix("-1m"))
    }
    let ids: Vec<String> = models.iter().map(|m| m.id.clone()).collect();
    let has_real = ids.iter().any(|id| !id.eq_ignore_ascii_case("default"));
    models
        .into_iter()
        .filter_map(|mut model| {
            if has_real && model.id.eq_ignore_ascii_case("default") {
                return None;
            }
            if let Some(base) = strip_1m(&model.id.clone()) {
                if ids.iter().any(|other| other == base) {
                    // The bare base is listed too — the engine already gave
                    // it the Context Window trait; the variant row is noise.
                    return None;
                }
                model.id = base.to_string();
                // "Opus (1M context)" → "Opus".
                if let Some(at) = model.label.rfind(" (")
                    && model.label.ends_with(')')
                {
                    model.label.truncate(at);
                    while model.label.ends_with(' ') {
                        model.label.pop();
                    }
                }
                if !model.options.iter().any(|o| o.id == "contextWindow") {
                    model.options.push(cypher_proto::ModelOption {
                        id: "contextWindow".into(),
                        label: "Context Window".into(),
                        choices: vec![
                            cypher_proto::ModelOptionChoice {
                                id: "200k".into(),
                                label: "200K".into(),
                            },
                            cypher_proto::ModelOptionChoice {
                                id: "1m".into(),
                                label: "1M".into(),
                            },
                        ],
                        default_choice: "1m".into(),
                    });
                }
            }
            Some(model)
        })
        .collect()
}

pub(crate) fn harness_brand_icon(harness: HarnessId) -> (&'static str, Option<gpui::Hsla>) {
    match harness {
        HarnessId::ClaudeCode | HarnessId::Mock => (
            crate::kit::icons::CLAUDE_MARK,
            Some(crate::kit::icons::claude_brand()),
        ),
        HarnessId::Codex => (crate::kit::icons::OPENAI_MARK, None),
        // Retired harnesses without a brand asset (legacy chats only).
        HarnessId::Cursor | HarnessId::Grok | HarnessId::Hermes => {
            (crate::kit::icons::GLOBAL, None)
        }
        HarnessId::Pi => (crate::kit::icons::PI_MARK, None),
    }
}

/// `CYPHER_HARNESS=mock` (the e2e/dev rig) opts the mock harness into the UI;
/// production launches never set it, so the mock never surfaces there.
fn mock_harness_enabled() -> bool {
    cypher_env::var("HARNESS").as_deref().map(str::trim) == Some("mock")
}

/// Production currently supports only Pi. Keep Mock available for the
/// explicit e2e/dev rig, but never surface the other harnesses in user-facing
/// pickers.
pub(super) fn visible_harnesses_impl(
    list: &[HarnessDescriptor],
    allow_mock: bool,
) -> Vec<HarnessDescriptor> {
    list.iter()
        .filter(|d| d.id == HarnessId::Pi || (allow_mock && d.id == HarnessId::Mock))
        .cloned()
        .collect()
}

/// What the composer actually offers: [`visible_harnesses_impl`] narrowed to the
/// catalog device's enabled set (Settings → Agents — per-device state, so a
/// space on another device follows THAT device's toggles). The dev-rig mock
/// opt-in survives the filter, and a catalog where nothing is enabled (or
/// that predates the flag entirely and defaults empty) falls back to
/// everything visible rather than an empty rail.
pub fn offered_harnesses(list: &[HarnessDescriptor]) -> Vec<HarnessDescriptor> {
    offered_harnesses_impl(list, mock_harness_enabled())
}

pub(super) fn offered_harnesses_impl(
    list: &[HarnessDescriptor],
    allow_mock: bool,
) -> Vec<HarnessDescriptor> {
    let visible = visible_harnesses_impl(list, allow_mock);
    let offered: Vec<HarnessDescriptor> = visible
        .iter()
        .filter(|d| {
            cypher_engine::registry::descriptor_enabled(d)
                || (allow_mock && d.id == HarnessId::Mock)
        })
        .cloned()
        .collect();
    if offered.is_empty() { visible } else { offered }
}
