//! HarnessRegistry — the engine's harness catalog: eager instances (mock) plus lazy
//! slots resolved on first use (Pi). Lazy slots carry a static descriptor so
//! `ListHarnesses` never forces a spawn.
//!
//! Also owns the device's harness ENABLEMENT (Settings → Agents): which harnesses
//! this device's composer offers, persisted in `{data_dir}/harness-prefs.json`.
//! Per-device because CLI installs are — a viewer retargets the settings page at
//! another device and edits THAT device's set over the forwarded RPCs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use cypher_harness::{Harness, HarnessError, mock::MockHarness};
use cypher_proto::{AgentEvent, DoneStatus, HarnessId, ReasoningLevel, SteeringMode};

/// Display name of a retired harness. Its id still decodes so legacy chats
/// load and sync, but no driver is registered for it.
fn retired_harness_name(id: HarnessId) -> Option<&'static str> {
    match id {
        HarnessId::ClaudeCode => Some("Claude Code"),
        HarnessId::Codex => Some("Codex"),
        HarnessId::Cursor => Some("Cursor"),
        HarnessId::Grok => Some("Grok"),
        HarnessId::Hermes => Some("Hermes"),
        HarnessId::Pi | HarnessId::Mock => None,
    }
}

/// What `ListHarnesses` reports per harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessDescriptor {
    pub id: HarnessId,
    pub name: String,
    pub supports_steering: bool,
    pub steering_mode: SteeringMode,
    pub reasoning_levels: Vec<ReasoningLevel>,
    /// Whether the agent's CLI is present on the listing device (the settings
    /// enable-gate). Defaults true so catalogs from engines predating the
    /// field never read as uninstallable.
    #[serde(default = "default_installed")]
    pub installed: bool,
    /// Whether the listing device offers this harness (Settings → Agents).
    /// `None` — the catalog came from an engine predating the setting — means
    /// "unknown": consumers fall back to [`default_enabled`] membership.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

fn default_installed() -> bool {
    true
}

/// The out-of-the-box enabled set. Pi is the only production harness.
pub fn default_enabled() -> Vec<HarnessId> {
    vec![HarnessId::Pi]
}

/// A descriptor's effective enabled flag ([`default_enabled`] membership when
/// the catalog predates the setting).
pub fn descriptor_enabled(descriptor: &HarnessDescriptor) -> bool {
    descriptor
        .enabled
        .unwrap_or_else(|| default_enabled().contains(&descriptor.id))
}

fn describe(harness: &dyn Harness) -> HarnessDescriptor {
    HarnessDescriptor {
        id: harness.id(),
        name: harness.display_name().to_string(),
        supports_steering: harness.supports_steering(),
        steering_mode: harness.steering_mode(),
        reasoning_levels: harness.reasoning_levels().to_vec(),
        installed: harness.installed(),
        enabled: None,
    }
}

/// The persisted shape of `harness-prefs.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct HarnessPrefsFile {
    /// `None` = the user never touched the setting → the default set.
    enabled: Option<Vec<HarnessId>>,
}

type Factory = Box<dyn Fn() -> Result<Arc<dyn Harness>, HarnessError> + Send + Sync>;
type InstalledProbe = Box<dyn Fn() -> bool + Send + Sync>;

enum Slot {
    Ready(Arc<dyn Harness>),
    Lazy {
        descriptor: HarnessDescriptor,
        /// Re-run on every `descriptors()` call — a CLI installed mid-session
        /// shows up on the next settings/picker open, no restart needed.
        installed: InstalledProbe,
        factory: Factory,
    },
}

pub struct HarnessRegistry {
    slots: Mutex<HashMap<HarnessId, Slot>>,
    order: Mutex<Vec<HarnessId>>,
    /// This device's enabled set; `None` inner value = the default set.
    prefs: Mutex<HarnessPrefsFile>,
    /// Where the prefs persist; `None` (tests, bare registries) skips writes.
    prefs_path: Mutex<Option<PathBuf>>,
}

impl Default for HarnessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HarnessRegistry {
    pub fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            order: Mutex::new(Vec::new()),
            prefs: Mutex::new(HarnessPrefsFile::default()),
            prefs_path: Mutex::new(None),
        }
    }

    fn slots(&self) -> MutexGuard<'_, HashMap<HarnessId, Slot>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn order(&self) -> MutexGuard<'_, Vec<HarnessId>> {
        self.order.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn prefs(&self) -> MutexGuard<'_, HarnessPrefsFile> {
        self.prefs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Load `harness-prefs.json` from the engine data dir and remember the
    /// path for writes. Corrupt/missing files fall back to the default set.
    pub fn load_prefs(&self, data_dir: &Path) {
        let path = data_dir.join("harness-prefs.json");
        let loaded = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<HarnessPrefsFile>(&text).ok())
            .unwrap_or_default();
        *self.prefs() = loaded;
        *self
            .prefs_path
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(path);
    }

    /// The enabled set in effect (the default set until the user edits it).
    /// Ids no longer registered (a retired harness in an old prefs file) are
    /// ignored; a set left empty by that falls back to the default set.
    pub fn enabled_set(&self) -> Vec<HarnessId> {
        let set: Vec<HarnessId> = self.prefs().enabled.clone().unwrap_or_else(default_enabled);
        let slots = self.slots();
        let set: Vec<HarnessId> = set
            .into_iter()
            .filter(|id| slots.contains_key(id))
            .collect();
        if set.is_empty() {
            default_enabled()
        } else {
            set
        }
    }

    /// Whether this device's CLI probe passes for `id` (no spawn, no resolve).
    fn installed_for(&self, id: HarnessId) -> bool {
        match self.slots().get(&id) {
            Some(Slot::Ready(harness)) => harness.installed(),
            Some(Slot::Lazy { installed, .. }) => installed(),
            None => false,
        }
    }

    /// Flip one harness's enablement and persist. Refuses unknown harnesses,
    /// enabling one whose CLI is missing (the settings gate, enforced where
    /// the state lives), and disabling the last enabled harness.
    pub fn set_enabled(&self, id: HarnessId, on: bool) -> Result<(), String> {
        if !self.slots().contains_key(&id) {
            return Err(format!("unknown harness {id:?}"));
        }
        if on && !self.installed_for(id) {
            return Err(format!("{id:?} CLI is not installed on this device"));
        }
        let mut set = self.enabled_set();
        match (on, set.contains(&id)) {
            (true, false) => set.push(id),
            (false, true) => {
                if set.len() == 1 {
                    return Err("cannot disable the last enabled harness".into());
                }
                set.retain(|h| *h != id);
            }
            _ => return Ok(()),
        }
        self.prefs().enabled = Some(set);
        self.persist_prefs();
        Ok(())
    }

    /// Best-effort atomic write (temp + rename, the ui-settings pattern).
    fn persist_prefs(&self) {
        let Some(path) = self
            .prefs_path
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return;
        };
        let json = match serde_json::to_string_pretty(&*self.prefs()) {
            Ok(json) => json,
            Err(err) => {
                tracing::warn!(error = %err, "harness-prefs serialize failed");
                return;
            }
        };
        let tmp = path.with_extension("json.tmp");
        if let Err(err) = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &path)) {
            tracing::warn!(error = %err, "harness-prefs save failed");
        }
    }

    pub fn register(&self, harness: Arc<dyn Harness>) {
        let id = harness.id();
        if self.slots().insert(id, Slot::Ready(harness)).is_none() {
            self.order().push(id);
        }
    }

    /// Register a slot resolved on first `resolve` (the factory result is
    /// cached). `installed` is the CLI-presence probe run per `descriptors()`
    /// call; it must never spawn.
    pub fn register_lazy(
        &self,
        descriptor: HarnessDescriptor,
        installed: InstalledProbe,
        factory: Factory,
    ) {
        let id = descriptor.id;
        if self
            .slots()
            .insert(
                id,
                Slot::Lazy {
                    descriptor,
                    installed,
                    factory,
                },
            )
            .is_none()
        {
            self.order().push(id);
        }
    }

    /// Swap an existing lazy slot's factory in place, leaving its order entry
    /// and descriptor untouched. Used by [`default_registry_with_bridge`] to
    /// arm the Pi slot with the engine bridge URL — `remove` + `register_lazy`
    /// would push a second order entry (Pi listed twice).
    fn replace_lazy_factory(&self, id: HarnessId, factory: Factory) {
        let mut slots = self.slots();
        match slots.get_mut(&id) {
            Some(Slot::Lazy { factory: slot, .. }) => *slot = factory,
            _ => panic!("replace_lazy_factory: {id:?} has no lazy slot to replace"),
        }
    }

    /// Drop cached model/command discovery for an already-resolved harness.
    /// Lazy slots are left untouched — they have nothing cached yet.
    pub fn invalidate_discovery(&self, id: HarnessId) {
        let slots = self.slots();
        if let Some(Slot::Ready(harness)) = slots.get(&id) {
            harness.invalidate_discovery();
        }
    }

    pub fn resolve(&self, id: HarnessId) -> Result<Arc<dyn Harness>, HarnessError> {
        let mut slots = self.slots();
        match slots.get(&id) {
            Some(Slot::Ready(harness)) => Ok(harness.clone()),
            Some(Slot::Lazy { factory, .. }) => {
                let harness = factory()?;
                slots.insert(id, Slot::Ready(harness.clone()));
                Ok(harness)
            }
            None => Err(match retired_harness_name(id) {
                Some(name) => HarnessError::Unsupported(format!(
                    "{name} chats are no longer supported; start a new Pi chat"
                )),
                None => HarnessError::NotInstalled(format!("{id:?}")),
            }),
        }
    }

    /// Catalog for `ListHarnesses` — never forces a lazy resolve.
    pub fn descriptors(&self) -> Vec<HarnessDescriptor> {
        let enabled = self.enabled_set();
        let slots = self.slots();
        self.order()
            .iter()
            .filter_map(|id| {
                let mut descriptor = match slots.get(id) {
                    Some(Slot::Ready(harness)) => describe(harness.as_ref()),
                    Some(Slot::Lazy {
                        descriptor,
                        installed,
                        ..
                    }) => HarnessDescriptor {
                        installed: installed(),
                        ..descriptor.clone()
                    },
                    None => return None,
                };
                descriptor.enabled = Some(enabled.contains(id));
                Some(descriptor)
            })
            .collect()
    }
}

/// The production registry: MockHarness (hidden from production pickers) plus lazy
/// slots resolved through `cypher_harness` on first use (subprocess discovery only
/// happens when a run/model call actually needs it).
///
/// `pi_sessions_root` is the cypher-owned pi session store the pi harness points
/// `pi --mode rpc --session-dir` at (callers pass
/// `profile.store_root().join("agent-sessions")`).
/// [`default_registry`] with the optional Cypher bridge: production assembly
/// passes the instance's private Unix socket path so pi children get `CYPHER_ENGINE_SOCKET`
/// and the subagents extension can reach the engine's `StartSubagent` /
/// `WatchAgentEvents` bridge. Test-only assembly keeps `None` (no IPC server).
pub fn default_registry_with_bridge(
    pi_sessions_root: PathBuf,
    engine_socket: Option<String>,
) -> HarnessRegistry {
    default_registry_with_bridge_and_runtime(pi_sessions_root, engine_socket, None)
}

/// Production registry with a Cypher-owned Pi runtime. The executable path is
/// fixed even before first-run installation; the Pi descriptor therefore
/// transitions from unavailable to available as soon as the runtime manager
/// atomically publishes `current`, without ever falling back to system Pi.
pub fn default_registry_with_bridge_and_runtime(
    pi_sessions_root: PathBuf,
    engine_socket: Option<String>,
    runtime: Option<crate::pi_runtime::PiRuntimePaths>,
) -> HarnessRegistry {
    let registry = default_registry_with_runtime(pi_sessions_root.clone(), runtime.clone());
    if let Some(url) = engine_socket {
        // Re-arm the plain Pi slot IN PLACE with a bridge-aware PiHarness:
        // every pi child gets the local engine IPC WebSocket as
        // `CYPHER_ENGINE_SOCKET` (the subagents extension's StartSubagent /
        // WatchAgentEvents bridge). The slot's order entry is untouched, so
        // Pi stays listed exactly once.
        let pi_sessions = pi_sessions_root;
        registry.replace_lazy_factory(
            HarnessId::Pi,
            Box::new(move || {
                let mut harness = cypher_harness::pi::PiHarness::new(pi_sessions.clone())
                    .with_engine_bridge(Some(url.clone()));
                if let Some(runtime) = &runtime {
                    harness = harness
                        .with_executable(runtime.executable.clone())
                        .with_runtime_environment(
                            runtime.agent_dir.clone(),
                            runtime.package_dir.clone(),
                        );
                }
                Ok(Arc::new(harness) as Arc<dyn Harness>)
            }),
        );
    }
    registry
}

pub fn default_registry(pi_sessions_root: PathBuf) -> HarnessRegistry {
    default_registry_with_runtime(pi_sessions_root, None)
}

fn default_registry_with_runtime(
    pi_sessions_root: PathBuf,
    runtime: Option<crate::pi_runtime::PiRuntimePaths>,
) -> HarnessRegistry {
    // Warm the login-shell PATH snapshot in the background so the first
    // CLI resolve doesn't pay the shell-startup latency inline.
    cypher_harness::shell_env::prewarm();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness {
        script: vec![
            AgentEvent::TextDelta {
                text: "## Streaming pipeline\n\nEvery turn flows through the same path:\n\n".into(),
            },
            AgentEvent::TextDelta {
                text: "1. **Doc command** — the composer queues a durable `run` entry\n2. **Host executor** — the chat's host device marks it processed, then dispatches\n3. **Fold** — events fold into parts and diff into the Loro doc every 120ms\n\n".into(),
            },
            AgentEvent::ToolCall {
                id: "mock-tool-1".into(),
                call: cypher_proto::ToolCall::Exec {
                    command: "cargo test --workspace".into(),
                },
            },
            AgentEvent::ToolResult {
                id: "mock-tool-1".into(),
                is_error: false,
                output: None,
                diff: None,
            },
            AgentEvent::ToolCall {
                id: "mock-tool-2".into(),
                call: cypher_proto::ToolCall::Exec {
                    command: "git log -5 --oneline --decorate && git merge-base HEAD origin/main"
                        .into(),
                },
            },
            AgentEvent::ToolResult {
                id: "mock-tool-2".into(),
                is_error: false,
                output: None,
                diff: None,
            },
            AgentEvent::TextDelta {
                text: "The `SegmentWriter` appends into `LoroText` so the oplog stays RLE-merged:\n\n```rust\nfolded = fold_event_into_parts(&folded, &event);\nwriter.sync(&folded)?; // 120ms coalesced commits\n```\n\nSynced to every device through the session room. *Mock harness reporting in.*".into(),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            },
        ],
    }));
    let runtime_for_installed = runtime.clone();
    let runtime_for_factory = runtime;
    // pi over its native RPC (`pi --mode rpc`, the `crates/harness/src/pi`
    // harness), lazy: the static descriptor mirrors PiHarness exactly — step-boundary steering (pi delivers a steer
    // after the current assistant message's tool calls, before the next LLM
    // call), pi's thinking ladder minus its "off" tier. The lazy closure
    // captures the cypher-owned session root for `--session-dir`.
    let pi_sessions = pi_sessions_root.clone();
    registry.register_lazy(
        HarnessDescriptor {
            id: HarnessId::Pi,
            name: "Pi".into(),
            supports_steering: true,
            steering_mode: SteeringMode::StepBoundary,
            reasoning_levels: vec![
                ReasoningLevel::Minimal,
                ReasoningLevel::Low,
                ReasoningLevel::Medium,
                ReasoningLevel::High,
                ReasoningLevel::XHigh,
                ReasoningLevel::Max,
            ],
            installed: true,
            enabled: None,
        },
        Box::new(move || {
            let mut harness = cypher_harness::pi::PiHarness::new(pi_sessions_root.clone());
            if let Some(runtime) = &runtime_for_installed {
                harness = harness
                    .with_executable(runtime.executable.clone())
                    .with_runtime_environment(
                        runtime.agent_dir.clone(),
                        runtime.package_dir.clone(),
                    );
            }
            harness.installed()
        }),
        Box::new(move || {
            let mut harness = cypher_harness::pi::PiHarness::new(pi_sessions.clone());
            if let Some(runtime) = &runtime_for_factory {
                harness = harness
                    .with_executable(runtime.executable.clone())
                    .with_runtime_environment(
                        runtime.agent_dir.clone(),
                        runtime.package_dir.clone(),
                    );
            }
            Ok(Arc::new(harness) as Arc<dyn Harness>)
        }),
    );
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lazy_slot_lists_without_resolving() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let registry = HarnessRegistry::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        registry.register_lazy(
            HarnessDescriptor {
                id: HarnessId::Mock,
                name: "Lazy Mock".into(),
                supports_steering: true,
                steering_mode: SteeringMode::StepBoundary,
                reasoning_levels: vec![],
                installed: true,
                enabled: None,
            },
            Box::new(|| false),
            Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                Err(HarnessError::NotInstalled("nope".into()))
            }),
        );
        let listed = registry.descriptors();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Lazy Mock");
        // The listing runs the probe, not the stored placeholder.
        assert!(!listed[0].installed);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "listing must not force a resolve"
        );
        assert!(registry.resolve(HarnessId::Mock).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn default_registry_lists_mock_and_pi_slots() {
        let dir = tempfile::tempdir().unwrap();
        let registry = default_registry(dir.path().join("agent-sessions"));
        let ids: Vec<HarnessId> = registry.descriptors().iter().map(|d| d.id).collect();
        assert_eq!(ids, vec![HarnessId::Mock, HarnessId::Pi]);
        assert!(registry.resolve(HarnessId::Mock).is_ok());
        let pi = registry.resolve(HarnessId::Pi).unwrap();
        assert_eq!(pi.id(), HarnessId::Pi);
        assert_eq!(pi.display_name(), "Pi");
        // Native RPC harness: steer lands mid-turn (after the current
        // assistant message's tool calls), so the descriptor is StepBoundary.
        assert_eq!(pi.steering_mode(), SteeringMode::StepBoundary);
        // Pi ladders are per model (its `thinkingLevelMap`), never harness-wide.
        assert!(pi.reasoning_levels().is_empty());
    }

    /// Retired harness ids still decode, but resolving one fails with a
    /// user-facing "start a new Pi chat" message instead of a missing binary.
    #[test]
    fn retired_harnesses_resolve_to_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let registry = default_registry(dir.path().join("agent-sessions"));
        let Err(err) = registry.resolve(HarnessId::ClaudeCode) else {
            panic!("claude-code must not resolve");
        };
        assert_eq!(
            err.to_string(),
            "Claude Code chats are no longer supported; start a new Pi chat"
        );
        // A rejected legacy send shows the message without the "harness: "
        // prefix other harness errors keep.
        assert_eq!(
            crate::EngineError::from(err).to_string(),
            "Claude Code chats are no longer supported; start a new Pi chat"
        );
        assert_eq!(
            crate::EngineError::from(HarnessError::Protocol("x".into())).to_string(),
            "harness: harness protocol error: x"
        );
        for id in [
            HarnessId::Codex,
            HarnessId::Cursor,
            HarnessId::Grok,
            HarnessId::Hermes,
        ] {
            assert!(matches!(
                registry.resolve(id),
                Err(HarnessError::Unsupported(_))
            ));
        }
    }

    /// `default_registry_with_bridge` re-arms the Pi slot IN PLACE: Pi must be
    /// listed exactly once, in the same position as the plain default registry
    /// (no duplicate order entry from a remove + re-register), and the resolved
    /// slot is still the native RPC PiHarness (bridge-armed).
    #[test]
    fn bridge_registry_lists_pi_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("agent-sessions");
        let plain = default_registry(sessions.clone());
        let bridge = default_registry_with_bridge(
            sessions,
            Some("/tmp/cypher-ipc-fixture/engine.sock".into()),
        );
        let plain_ids: Vec<HarnessId> = plain.descriptors().iter().map(|d| d.id).collect();
        let bridge_ids: Vec<HarnessId> = bridge.descriptors().iter().map(|d| d.id).collect();
        assert_eq!(
            bridge_ids, plain_ids,
            "the bridge must not disturb the catalog order"
        );
        let pi_count = |ids: &[HarnessId]| ids.iter().filter(|id| **id == HarnessId::Pi).count();
        assert_eq!(pi_count(&bridge_ids), 1, "Pi listed exactly once");
        assert_eq!(pi_count(&bridge_ids), pi_count(&plain_ids));
        // The resolved slot is the native RPC harness, still bridge-armed.
        let pi = bridge.resolve(HarnessId::Pi).unwrap();
        assert_eq!(pi.id(), HarnessId::Pi);
        assert_eq!(pi.display_name(), "Pi");
        assert_eq!(pi.steering_mode(), SteeringMode::StepBoundary);
    }

    /// Catalogs serialized by engines that predate the `installed`/`enabled`
    /// fields must keep deserializing — installed, and enabled per the
    /// default-set fallback (Pi yes, anything else no).
    #[test]
    fn descriptor_without_new_fields_parses_with_fallbacks() {
        let parse = |id: &str| -> HarnessDescriptor {
            serde_json::from_str(&format!(
                r#"{{
                    "id": "{id}",
                    "name": "x",
                    "supportsSteering": true,
                    "steeringMode": "step-boundary",
                    "reasoningLevels": []
                }}"#
            ))
            .unwrap()
        };
        let pi = parse("pi");
        assert!(pi.installed);
        assert_eq!(pi.enabled, None);
        assert!(descriptor_enabled(&pi));
        assert!(!descriptor_enabled(&parse("mock")));
    }

    /// A registry slot for the tests below: installed probe fixed, factory
    /// never expected to run.
    fn test_slot(registry: &HarnessRegistry, id: HarnessId, installed: bool) {
        registry.register_lazy(
            HarnessDescriptor {
                id,
                name: format!("{id:?}"),
                supports_steering: true,
                steering_mode: SteeringMode::StepBoundary,
                reasoning_levels: vec![],
                installed: true,
                enabled: None,
            },
            Box::new(move || installed),
            Box::new(|| Err(HarnessError::NotInstalled("test slot".into()))),
        );
    }

    /// `descriptors()` stamps the per-device enabled flag; `set_enabled`
    /// guards the gate (no enabling missing CLIs, no disabling the last one)
    /// and persists across a reload.
    #[test]
    fn enablement_stamps_guards_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let registry = HarnessRegistry::new();
        registry.load_prefs(dir.path());
        test_slot(&registry, HarnessId::Mock, true);
        test_slot(&registry, HarnessId::Pi, true);

        // Default set stamped: Pi on, everything else off.
        let flags: Vec<(HarnessId, Option<bool>)> = registry
            .descriptors()
            .into_iter()
            .map(|d| (d.id, d.enabled))
            .collect();
        assert_eq!(
            flags,
            vec![(HarnessId::Mock, Some(false)), (HarnessId::Pi, Some(true))]
        );

        // Unregistered ids refuse; enabling an enabled harness is a no-op.
        assert!(registry.set_enabled(HarnessId::Hermes, true).is_err());
        assert!(registry.set_enabled(HarnessId::Pi, true).is_ok());
        // Installed CLIs toggle both ways; no-op flips are fine.
        registry.set_enabled(HarnessId::Mock, true).unwrap();
        registry.set_enabled(HarnessId::Mock, true).unwrap();
        // Pi can't be disabled while it is the last enabled harness.
        registry.set_enabled(HarnessId::Mock, false).unwrap();
        assert!(registry.set_enabled(HarnessId::Pi, false).is_err());
        assert_eq!(registry.enabled_set(), vec![HarnessId::Pi]);

        // A fresh registry over the same data dir reads the persisted set.
        let reloaded = HarnessRegistry::new();
        reloaded.load_prefs(dir.path());
        assert_eq!(reloaded.enabled_set(), vec![HarnessId::Pi]);
    }

    /// A prefs file naming a retired harness: the stale id neither counts
    /// toward the last-enabled guard nor leaves the catalog with nothing on.
    #[test]
    fn retired_ids_in_prefs_do_not_count_as_enabled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("harness-prefs.json"),
            r#"{"enabled":["pi","claude-code"]}"#,
        )
        .unwrap();
        let registry = HarnessRegistry::new();
        registry.load_prefs(dir.path());
        test_slot(&registry, HarnessId::Mock, true);
        test_slot(&registry, HarnessId::Pi, true);
        assert_eq!(registry.enabled_set(), vec![HarnessId::Pi]);
        assert!(registry.set_enabled(HarnessId::Pi, false).is_err());
        assert!(
            registry
                .descriptors()
                .iter()
                .any(|d| d.id == HarnessId::Pi && d.enabled == Some(true))
        );

        // Only retired ids left: the default set applies.
        std::fs::write(
            dir.path().join("harness-prefs.json"),
            r#"{"enabled":["claude-code"]}"#,
        )
        .unwrap();
        registry.load_prefs(dir.path());
        assert_eq!(registry.enabled_set(), vec![HarnessId::Pi]);
        assert!(registry.set_enabled(HarnessId::Pi, false).is_err());
    }

    /// A missing CLI can't be enabled (the settings gate).
    #[test]
    fn enabling_a_missing_cli_is_refused() {
        let registry = HarnessRegistry::new();
        test_slot(&registry, HarnessId::Mock, false);
        test_slot(&registry, HarnessId::Pi, true);
        assert!(registry.set_enabled(HarnessId::Mock, true).is_err());
        assert_eq!(registry.enabled_set(), vec![HarnessId::Pi]);
    }
}
