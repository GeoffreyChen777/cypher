//! Model and slash-command discovery through short-lived pi children, and
//! the built-in commands the harness synthesizes on top of them.

use super::*;

/// Built-in TUI slash commands with RPC equivalents, synthesized into the
/// discovery result. pi's `get_commands` lists only extension / prompt /
/// skill commands — built-in TUI commands (`/compact`, `/export-html`, …) are
/// NOT advertised, and sending one as prompt text would not execute it (pi
/// only executes `get_commands` results via `prompt`). The harness advertises
/// the ones it can dispatch over RPC itself; a same-name discovered command
/// always wins (dedup) and is left to pi.
const SYNTHESIZED_COMMANDS: [(&str, &str, &str); 2] = [
    (
        "compact",
        "Compact the conversation context (pi built-in)",
        "custom instructions",
    ),
    (
        "export-html",
        "Export the session to an HTML file (pi built-in)",
        "output path",
    ),
];

/// Append the synthesized built-ins to the discovered commands, skipping any
/// whose name a discovered extension/prompt/skill command already owns. The
/// synthesized entries land at the tail. Hide/show for the composer `/` menu
/// is a UI preference (Settings → Commands), not a harness filter.
pub(super) fn synthesize_commands(discovered: &[SlashCommand]) -> Vec<SlashCommand> {
    let mut commands = discovered.to_vec();
    for (name, description, hint) in SYNTHESIZED_COMMANDS {
        if discovered.iter().any(|c| c.name == name) {
            continue;
        }
        commands.push(SlashCommand {
            name: name.to_owned(),
            description: description.to_owned(),
            input_hint: Some(hint.to_owned()),
        });
    }
    commands
}

impl PiHarness {
    pub(super) fn cached_commands(&self) -> Option<Vec<SlashCommand>> {
        crate::lock(&self.commands).clone()
    }

    /// Short-lived discovery run for [`Harness::models`]: `get_state` (a
    /// liveness probe — the child is up and serving) then poll
    /// `get_available_models` until configured providers appear (or the wait
    /// expires). Size-stability is only a fallback when we do not know which
    /// providers to expect.
    ///
    /// pi's RPC handler returns `modelRuntime.getAvailableSnapshot()` with no
    /// await; `--list-models` instead awaits `getAvailable()`. A probe that
    /// reads the snapshot immediately after spawn therefore sees `[]` even
    /// when the CLI lists dozens of models a moment later. An extension such
    /// as pi-claude-bridge can also fill the snapshot before gateway
    /// providers finish registering — returning at first non-empty would
    /// cache a Claude-only catalog.
    pub(super) async fn discover_models(&self) -> Result<DiscoveredModels, HarnessError> {
        let (mut child, _stderr) = self
            .spawn_child(None, &RunHostContext::default(), None, None, None)
            .await?;
        let (client, _incoming) = match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => PiClient::new(stdin, stdout),
            _ => {
                shutdown_child(&mut child, self.kill_grace).await;
                return Err(HarnessError::Protocol("pi child has no stdio".into()));
            }
        };
        let wait = self.model_catalog_wait;
        let expected = self
            .agent_dir
            .as_deref()
            .map(expected_model_providers)
            .unwrap_or_default();
        let discovery = async {
            let state = client.request("get_state", Map::new()).await?;
            let deadline = Instant::now() + wait;
            let stable_for = wait.min(MODEL_CATALOG_STABLE);
            let mut best: Vec<Model> = Vec::new();
            let mut unchanged_since: Option<Instant> = None;
            loop {
                let available = match client.request("get_available_models", Map::new()).await {
                    Ok(value) => value,
                    Err(_err) if !best.is_empty() => {
                        return Ok(DiscoveredModels {
                            models: best,
                            from_catalog: true,
                        });
                    }
                    Err(err) => return Err(err),
                };
                let models = models_from_response(&available);
                let now = Instant::now();
                if models.len() > best.len() {
                    best = models;
                    unchanged_since = Some(now);
                }
                if catalog_covers(&best, &expected) {
                    return Ok(DiscoveredModels {
                        models: best,
                        from_catalog: true,
                    });
                }
                if expected.is_empty() && !best.is_empty() {
                    let since = unchanged_since.get_or_insert(now);
                    if now.duration_since(*since) >= stable_for {
                        return Ok(DiscoveredModels {
                            models: best,
                            from_catalog: true,
                        });
                    }
                }
                if now >= deadline {
                    let from_catalog = !best.is_empty();
                    return Ok(DiscoveredModels {
                        models: if from_catalog {
                            best
                        } else {
                            models_from_responses(&available, &state)
                        },
                        from_catalog,
                    });
                }
                tokio::time::sleep(MODEL_CATALOG_POLL).await;
            }
        };
        // Outer bound is wait + one RPC round-trip + poll slack, so a hung
        // child still cannot pin discovery past the picker timeout.
        let result = tokio::time::timeout(wait + Duration::from_secs(2), discovery).await;
        shutdown_child(&mut child, self.kill_grace).await;
        match result {
            Ok(inner) => inner,
            Err(_) => Err(HarnessError::Protocol("model discovery timed out".into())),
        }
    }

    /// Short-lived discovery run for [`Harness::commands`]: `get_commands`
    /// (extension / prompt / skill commands, all three sources).
    pub(super) async fn discover_commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        let (mut child, _stderr) = self
            .spawn_child(None, &RunHostContext::default(), None, None, None)
            .await?;
        let (client, _incoming) = match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => PiClient::new(stdin, stdout),
            _ => {
                shutdown_child(&mut child, self.kill_grace).await;
                return Err(HarnessError::Protocol("pi child has no stdio".into()));
            }
        };
        let discovery = async {
            let resp = client.request("get_commands", Map::new()).await?;
            Ok::<Vec<SlashCommand>, HarnessError>(parse_commands(resp.get("commands")))
        };
        let result = tokio::time::timeout(Duration::from_secs(10), discovery).await;
        shutdown_child(&mut child, self.kill_grace).await;
        match result {
            Ok(inner) => inner,
            Err(_) => Err(HarnessError::Protocol("command discovery timed out".into())),
        }
    }
}
