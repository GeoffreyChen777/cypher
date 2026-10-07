//! Per-agent specs: binaries, install paths, static catalogs and effort mapping
//! for each ACP agent, plus managed-adapter prewarming.

use super::*;

/// Per-agent configuration: which binary to spawn and what to tell the picker.
pub(super) struct AcpAgentSpec {
    pub(super) id: HarnessId,
    pub(super) display_name: &'static str,
    /// Binary name searched on PATH (and platform install dirs).
    pub(super) executable: &'static str,
    /// Env var overriding executable resolution (tests, custom installs).
    pub(super) env_override: &'static str,
    /// Arguments that put the binary in ACP-serving mode.
    pub(super) args: &'static [&'static str],
    /// Pinned npm package (`name@version`) installed ONCE into the managed
    /// adapters dir when the binary isn't already present — the launch then
    /// spawns `node <entry>` directly, keeping npm (and every way a user's
    /// npm state can break) out of chat turns. See [`crate::adapter_install`].
    pub(super) npm_package: Option<&'static str>,
    /// Extra install locations to probe after PATH.
    pub(super) extra_paths: fn() -> Vec<PathBuf>,
    /// The agent's own CLI binary (`claude`, `codex`, …) — what "installed"
    /// means to the user. Distinct from `executable` where the spawned adapter
    /// wraps the CLI (`claude-agent-acp`, `codex-acp`), and the npx
    /// fallback deliberately doesn't count: npx can fetch an adapter on
    /// demand, but an absent CLI still means no logins/config to drive.
    pub(super) cli_executable: &'static str,
    /// Extra install locations probed for [`Self::cli_executable`].
    pub(super) cli_extra_paths: fn() -> Vec<PathBuf>,
    /// Search summary + install hint for the NotInstalled error.
    pub(super) install_hint: &'static str,
    pub(super) models: fn() -> Vec<Model>,
    pub(super) steering_mode: SteeringMode,
    /// Effort ladder surfaced in the picker; applied per session via the
    /// `thought_level` config option (must mirror the registry descriptor).
    pub(super) reasoning_levels: &'static [ReasoningLevel],
    /// Transform applied to the initial prompt and every steer — Claude's
    /// Ultrathink is a prompt-prefix convention, not an effort flag.
    pub(super) prompt_transform: fn(Option<ReasoningLevel>, &str) -> String,
    /// Preference-ordered `thought_level` value ids for the run's reasoning
    /// (per-agent clamping, e.g. Claude xhigh→max off the xhigh family). The
    /// first value the agent actually advertises wins.
    pub(super) effort_values: fn(Option<ReasoningLevel>, Option<&str>) -> Vec<&'static str>,
    /// Levels appended to a DISCOVERED model's non-empty ladder: modes the
    /// wire can't advertise because they aren't `thought_level` values
    /// (Claude's Ultrathink rides prompts via `prompt_transform`).
    pub(super) ladder_extras: &'static [ReasoningLevel],
}

fn identity_transform(_reasoning: Option<ReasoningLevel>, text: &str) -> String {
    text.to_owned()
}

/// PATH + login-shell + extra dirs + node-version-manager scan for a binary.
pub(crate) fn find_on_paths(exe: &str, extra: Vec<PathBuf>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .filter(|d| !d.as_os_str().is_empty())
                .map(|d| d.join(exe))
                .collect()
        })
        .unwrap_or_default();
    if let Some(shell_path) = crate::shell_env::login_shell_path() {
        candidates.extend(
            std::env::split_paths(shell_path)
                .filter(|d| !d.as_os_str().is_empty())
                .map(|d| d.join(exe)),
        );
    }
    candidates.extend(extra);
    candidates.extend(
        crate::node_version_manager_bins()
            .into_iter()
            .map(|d| d.join(exe)),
    );
    candidates.into_iter().find(|p| p.exists())
}

/// Generic effort ladder for agents without their own clamping rules.
fn default_effort_values(
    reasoning: Option<ReasoningLevel>,
    _model: Option<&str>,
) -> Vec<&'static str> {
    let Some(level) = reasoning else {
        return Vec::new();
    };
    match level {
        ReasoningLevel::Minimal => vec!["minimal", "low"],
        ReasoningLevel::Low => vec!["low", "minimal"],
        ReasoningLevel::Medium => vec!["medium"],
        ReasoningLevel::High => vec!["high"],
        ReasoningLevel::XHigh => vec!["xhigh", "x-high", "high"],
        ReasoningLevel::Max => vec!["max", "xhigh", "high"],
        ReasoningLevel::Ultra | ReasoningLevel::Ultracode | ReasoningLevel::Ultrathink => {
            vec!["ultra", "max", "high"]
        }
    }
}

pub(super) fn claude_spec() -> AcpAgentSpec {
    AcpAgentSpec {
        id: HarnessId::ClaudeCode,
        display_name: "Claude Code",
        executable: "claude-agent-acp",
        env_override: "CLAUDE_ACP_EXECUTABLE",
        args: &[],
        npm_package: Some("@agentclientprotocol/claude-agent-acp@0.66.0"),
        extra_paths: npm_global_paths("claude-agent-acp"),
        cli_executable: "claude",
        cli_extra_paths: || {
            let mut dirs = npm_global_bins("claude");
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                // The native installer's launcher location.
                dirs.push(home.join(".claude").join("local").join("claude"));
            }
            dirs
        },
        install_hint: "claude-agent-acp (searched PATH, the login shell's PATH, npm \
             global bins, and fnm/nvm/volta/pnpm/bun install dirs; cypher installs \
             the pinned @agentclientprotocol/claude-agent-acp automatically when \
             npm is available; install npm/node, or \
             `npm install -g @agentclientprotocol/claude-agent-acp`, or set \
             CLAUDE_ACP_EXECUTABLE to override)",
        models: crate::claude::catalog::static_models,
        // `_session/steering` advertised by the adapter: priority-`now`
        // injection, pre-empting the current generation.
        steering_mode: SteeringMode::StepBoundary,
        reasoning_levels: &[
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
            ReasoningLevel::XHigh,
            ReasoningLevel::Max,
        ],
        prompt_transform: crate::claude::catalog::apply_ultrathink,
        effort_values: |reasoning, model| {
            crate::claude::catalog::to_effort(reasoning, model)
                .into_iter()
                .collect()
        },
        // Ultrathink is a prompt-prefix mode (see `prompt_transform`), so it
        // never appears among the adapter's `thought_level` values.
        ladder_extras: &[ReasoningLevel::Ultrathink],
    }
}

pub(super) fn codex_spec() -> AcpAgentSpec {
    AcpAgentSpec {
        id: HarnessId::Codex,
        display_name: "Codex",
        executable: "codex-acp",
        env_override: "CODEX_ACP_EXECUTABLE",
        args: &[],
        npm_package: Some("@agentclientprotocol/codex-acp@1.1.14"),
        extra_paths: npm_global_paths("codex-acp"),
        cli_executable: "codex",
        cli_extra_paths: || npm_global_bins("codex"),
        install_hint: "codex-acp (searched PATH, the login shell's PATH, npm global \
             bins, and fnm/nvm/volta/pnpm/bun install dirs; cypher installs the \
             pinned @agentclientprotocol/codex-acp automatically when npm is \
             available; install npm/node, or \
             `npm install -g @agentclientprotocol/codex-acp`, or set \
             CODEX_ACP_EXECUTABLE to override)",
        models: crate::codex::catalog::static_models,
        steering_mode: SteeringMode::StepBoundary,
        reasoning_levels: crate::codex::catalog::REASONING_LEVELS,
        prompt_transform: identity_transform,
        effort_values: |reasoning, _model| {
            crate::codex::catalog::to_effort(reasoning)
                .into_iter()
                .collect()
        },
        ladder_extras: &[],
    }
}

/// npm-global bin dirs for an adapter binary (`npm i -g` installs).
fn npm_global_paths(exe: &'static str) -> fn() -> Vec<PathBuf> {
    // fn pointers can't capture; probe the fixed npm-global locations and
    // append the exe at call time via a small per-exe shim table.
    match exe {
        "claude-agent-acp" => || npm_global_bins("claude-agent-acp"),
        "codex-acp" => || npm_global_bins("codex-acp"),
        _ => || Vec::new(),
    }
}

/// Fixed npm-global bin locations for a CLI name (`npm i -g` installs). Also
/// used by the pi harness's executable resolution.
pub(crate) fn npm_global_bins(exe: &str) -> Vec<PathBuf> {
    crate::well_known_cli_dirs()
        .into_iter()
        .map(|dir| dir.join(exe))
        .collect()
}

fn grok_install_paths() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".local").join("bin").join("grok"));
        dirs.push(home.join(".grok").join("bin").join("grok"));
        dirs.push(home.join(".npm-global").join("bin").join("grok"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin/grok"));
    dirs.push(PathBuf::from("/usr/local/bin/grok"));
    dirs
}

pub(super) fn grok_spec() -> AcpAgentSpec {
    AcpAgentSpec {
        id: HarnessId::Grok,
        display_name: "Grok",
        executable: "grok",
        env_override: "GROK_EXECUTABLE",
        args: &["agent", "stdio"],
        npm_package: Some("@xai-official/grok@1.0.0"),
        extra_paths: grok_install_paths,
        cli_executable: "grok",
        cli_extra_paths: grok_install_paths,
        install_hint: "grok (searched PATH, the login shell's PATH, ~/.local/bin, \
             ~/.grok/bin, ~/.npm-global/bin, /opt/homebrew/bin, /usr/local/bin, and \
             fnm/nvm/volta/pnpm/bun install dirs; install with \
             `curl -fsSL https://x.ai/cli/install.sh | bash` or \
             `npm install -g @xai-official/grok`; set GROK_EXECUTABLE to override)",
        models: || {
            vec![Model {
                id: "grok-4.5".into(),
                label: "Grok 4.5".into(),
                description: Some("xAI's coding model — 500k context".into()),
                reasoning_levels: vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High,
                ],
                options: Vec::new(),
            }]
        },
        // No `_session/steering` extension: steers deliver at turn boundaries.
        steering_mode: SteeringMode::TurnBoundary,
        // Grok Build's advertised efforts (default high); applied through the
        // session's `thought_level` config option.
        reasoning_levels: &[
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
        ],
        prompt_transform: identity_transform,
        effort_values: default_effort_values,
        ladder_extras: &[],
    }
}

fn cursor_install_paths() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".local").join("bin").join("cursor-agent"));
        dirs.push(home.join(".cursor").join("bin").join("cursor-agent"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin/cursor-agent"));
    dirs.push(PathBuf::from("/usr/local/bin/cursor-agent"));
    dirs
}

pub(super) fn cursor_spec() -> AcpAgentSpec {
    AcpAgentSpec {
        id: HarnessId::Cursor,
        display_name: "Cursor",
        executable: "cursor-agent",
        env_override: "CURSOR_EXECUTABLE",
        // Native ACP server — no adapter package in between.
        args: &["acp"],
        npm_package: None,
        extra_paths: cursor_install_paths,
        cli_executable: "cursor-agent",
        cli_extra_paths: cursor_install_paths,
        install_hint: "cursor-agent (searched PATH, the login shell's PATH, ~/.local/bin, \
             ~/.cursor/bin, /opt/homebrew/bin, and /usr/local/bin; install with \
             `curl https://cursor.com/install -fsS | bash`, then `cursor-agent login`; \
             set CURSOR_EXECUTABLE to override)",
        // Fallback only: `session/new` advertises the account's models and
        // the wire always wins. Keep this list to well-known public ids.
        models: || {
            vec![
                Model {
                    id: "auto-smart".into(),
                    label: "Auto".into(),
                    description: Some("Cursor picks the model per request".into()),
                    reasoning_levels: Vec::new(),
                    options: vec![cursor::optimize_for_option(Some("balanced"))],
                },
                Model {
                    id: "composer-2.5".into(),
                    label: "Composer 2.5".into(),
                    description: Some("Cursor's own fast coding model".into()),
                    reasoning_levels: Vec::new(),
                    options: Vec::new(),
                },
            ]
        },
        // No `_session/steering` extension: steers deliver at turn boundaries.
        steering_mode: SteeringMode::TurnBoundary,
        // Descriptor ladder stays empty; live discovery fills it for families
        // that actually advertise effort variants (see `cursor::enrich_models`).
        reasoning_levels: &[],
        prompt_transform: identity_transform,
        // Cursor has no thought_level config option — effort rides the model
        // id. When a collapsed family exposes a Reasoning ladder, these
        // tokens pick the matching sibling via `cursor::pick_model_id`.
        effort_values: |reasoning, _| reasoning.map(cursor::effort_tokens).unwrap_or_default(),
        ladder_extras: &[],
    }
}

fn hermes_install_paths() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".local").join("bin").join("hermes"));
        dirs.push(home.join(".hermes").join("bin").join("hermes"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin/hermes"));
    dirs.push(PathBuf::from("/usr/local/bin/hermes"));
    dirs
}

pub(super) fn hermes_spec() -> AcpAgentSpec {
    AcpAgentSpec {
        id: HarnessId::Hermes,
        display_name: "Hermes",
        executable: "hermes",
        env_override: "HERMES_EXECUTABLE",
        args: &["acp"],
        // Python/uv install — no npm fallback exists.
        npm_package: None,
        extra_paths: hermes_install_paths,
        cli_executable: "hermes",
        cli_extra_paths: hermes_install_paths,
        install_hint: "hermes (searched PATH, the login shell's PATH, ~/.local/bin, \
             ~/.hermes/bin, /opt/homebrew/bin, /usr/local/bin, and fnm/nvm/volta/pnpm/bun \
             install dirs; install with \
             `curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash`, then \
             `cd ~/.hermes/hermes-agent && uv pip install -e '.[acp]'` for the ACP \
             server; set HERMES_EXECUTABLE to override)",
        // Hermes derives its model list from the providers the user has
        // authenticated (`hermes model`); these are the Nous flagships every
        // portal account gets. Ids the agent doesn't advertise are skipped by
        // the config-option set, falling back to the agent's own default.
        models: || {
            vec![
                Model {
                    id: "hermes-4-405b".into(),
                    label: "Hermes 4 405B".into(),
                    description: Some("Nous Research's hybrid-reasoning flagship".into()),
                    reasoning_levels: Vec::new(),
                    options: Vec::new(),
                },
                Model {
                    id: "hermes-4-70b".into(),
                    label: "Hermes 4 70B".into(),
                    description: Some("Faster Hermes 4 — same post-training, 70B".into()),
                    reasoning_levels: Vec::new(),
                    options: Vec::new(),
                },
            ]
        },
        // No `_session/steering` extension: steers deliver at turn boundaries.
        steering_mode: SteeringMode::TurnBoundary,
        // Hermes exposes no effort config over ACP today (hybrid reasoning is
        // model-internal); revisit when the adapter advertises a ladder.
        reasoning_levels: &[],
        prompt_transform: identity_transform,
        effort_values: default_effort_values,
        ladder_extras: &[],
    }
}

/// Background-install managed npm adapters for agents whose CLI is present
/// on this device, so a first chat never pays (or trips over) an npm run.
/// Skips agents whose adapter is already resolvable; failures are logged and
/// retried on the next daemon start or blocking launch. A no-op outside a
/// tokio runtime.
pub fn prewarm_managed_adapters() {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    for spec in [claude_spec(), codex_spec(), grok_spec()] {
        let Some(pkg) = spec.npm_package else {
            continue;
        };
        let pin = crate::adapter_install::NpmPin::parse(pkg);
        if find_on_paths(spec.executable, (spec.extra_paths)()).is_some()
            || crate::adapter_install::installed_entry(&pin, spec.executable).is_some()
            || find_on_paths(spec.cli_executable, (spec.cli_extra_paths)()).is_none()
            || crate::adapter_install::find_npm().is_none()
        {
            continue;
        }
        let (bin_name, display_name) = (spec.executable, spec.display_name);
        handle.spawn(async move {
            match crate::adapter_install::ensure_installed(pin, bin_name, display_name).await {
                Ok(entry) => tracing::info!(
                    target: "cypher_harness::adapter_install",
                    adapter = %entry.display(),
                    "prewarmed {display_name} ACP adapter"
                ),
                Err(e) => tracing::warn!(
                    target: "cypher_harness::adapter_install",
                    "prewarm of the {display_name} ACP adapter failed: {e}"
                ),
            }
        });
    }
}
