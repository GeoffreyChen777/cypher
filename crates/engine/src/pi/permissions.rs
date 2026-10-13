//! Device-scoped settings for the Runtime's permission extension
//! (`pi-runtime/extensions/cypher-permissions.ts`, design in
//! `docs/design/permission-control.md`).
//!
//! The extension owns every decision; it reads `permissions.json` from the Pi
//! agent directory at the start of each turn. Its master switch, `enabled`,
//! is off unless the file sets it, and is read when Pi starts: while off the
//! extension registers nothing. The engine edits the part of
//! that file Settings → Permissions shows and leaves everything else as the
//! user wrote it (levels, `remove` lists, unknown fields). It also reports
//! whether this device can sandbox at all: the extension stands down where
//! Landstrip's `doctor` fails.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::pi::runtime::PiRuntimePaths;

const FILE_NAME: &str = "permissions.json";
const EXTENSION: &str = "extensions/cypher-permissions.ts";
const MAX_PATTERNS: usize = 256;
const MAX_PATTERN_CHARS: usize = 512;
const MAX_COMMAND_WORDS: usize = 32;
/// Programs a command rule cannot start with: each can run any other
/// command. Kept in step with `cypher-permissions/commands.ts`.
const BANNED_PROGRAMS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "csh",
    "tcsh",
    "fish",
    "nu",
    "pwsh",
    "powershell",
    "env",
    "sudo",
    "doas",
    "su",
    "xargs",
    "eval",
    "exec",
    "command",
    "builtin",
    "nohup",
    "time",
    "timeout",
    "nice",
    "watch",
    "script",
    "parallel",
    "caffeinate",
    "python",
    "python2",
    "python3",
    "node",
    "deno",
    "bun",
    "bunx",
    "npx",
    "pnpx",
    "uvx",
    "pipx",
    "ruby",
    "perl",
    "php",
    "lua",
    "tclsh",
    "Rscript",
    "osascript",
    "expect",
    "awk",
    "gawk",
    "sed",
    "find",
];
/// Builtins that never need to leave the sandbox.
const INERT_PROGRAMS: &[&str] = &[
    "cd", "pushd", "popd", "pwd", "true", "false", ":", "echo", "printf", "sleep",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalMode {
    /// Rejected at once.
    Deny,
    /// A human decides.
    Ask,
    /// The judge model may approve once; anything else goes to a human.
    Assisted,
}

/// How sandboxed commands reach the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkMode {
    /// Directly (off only while a command can read a secret).
    Open,
    /// Only through a proxy that lets allowed domains through and asks for
    /// the rest.
    Filtered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolDecision {
    Allow,
    Elevate,
    Deny,
}

/// Approval modes for the elevations the defaults produce. Sensitive
/// requests can never be `assisted` (the extension enforces the same rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionApprovals {
    pub external_write: ApprovalMode,
    pub protected_write: ApprovalMode,
    pub sensitive_read: ApprovalMode,
    pub sensitive_write: ApprovalMode,
    /// A domain outside the filter's allowlist, or direct network for one
    /// command.
    pub network: ApprovalMode,
    pub tools: ApprovalMode,
    /// Running a command outside the sandbox: `ask` or `deny`, never the
    /// judge.
    #[serde(default = "ask")]
    pub escalate: ApprovalMode,
}

fn ask() -> ApprovalMode {
    ApprovalMode::Ask
}

impl Default for PermissionApprovals {
    fn default() -> Self {
        Self {
            external_write: ApprovalMode::Assisted,
            protected_write: ApprovalMode::Assisted,
            sensitive_read: ApprovalMode::Ask,
            sensitive_write: ApprovalMode::Ask,
            network: ApprovalMode::Assisted,
            tools: ApprovalMode::Ask,
            escalate: ApprovalMode::Ask,
        }
    }
}

const APPROVAL_KEYS: [&str; 7] = [
    "external.write",
    "protected.write",
    "sensitive.read",
    "sensitive.write",
    "network",
    "tools",
    "escalate",
];

impl PermissionApprovals {
    fn get(&self, key: &str) -> ApprovalMode {
        match key {
            "external.write" => self.external_write,
            "protected.write" => self.protected_write,
            "sensitive.read" => self.sensitive_read,
            "sensitive.write" => self.sensitive_write,
            "network" => self.network,
            "escalate" => self.escalate,
            _ => self.tools,
        }
    }

    fn set(&mut self, key: &str, mode: ApprovalMode) {
        match key {
            "external.write" => self.external_write = mode,
            "protected.write" => self.protected_write = mode,
            "sensitive.read" => self.sensitive_read = mode,
            "sensitive.write" => self.sensitive_write = mode,
            "network" => self.network = mode,
            "escalate" => self.escalate = mode,
            _ => self.tools = mode,
        }
    }
}

/// An "always allow" rule a human approval wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRule {
    pub capability: String,
    /// An absolute path (its subtree), or `tool:<name>`.
    pub path: String,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandDecision {
    /// Runs outside the sandbox without asking.
    Allow,
    /// Always asks before leaving the sandbox.
    Ask,
    /// Never runs.
    Deny,
}

/// A command rule: matching commands run outside the sandbox, ask first, or
/// never run (`cypher-permissions/commands.ts`). `Always allow` on an
/// escalation writes one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandRule {
    /// The program, then words: `*` globs within a word, `**` is any words.
    #[serde(rename = "match")]
    pub pattern: Vec<String>,
    pub decision: CommandDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The program's contents, for a program in a writable place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_at: String,
}

impl CommandRule {
    pub fn validate(&self) -> Result<(), String> {
        let words = &self.pattern;
        if words.is_empty() || words.len() > MAX_COMMAND_WORDS {
            return Err("A command pattern has 1 to 32 words.".into());
        }
        if words.iter().any(|word| {
            word.is_empty()
                || word.chars().count() > MAX_PATTERN_CHARS
                || word.contains(['\r', '\n', '\0'])
        }) {
            return Err("Each command pattern word is a non-empty line of text.".into());
        }
        let program = &words[0];
        if program == "**" || program.contains(['*', '?', '[']) {
            return Err("A command pattern starts with the literal program name.".into());
        }
        let name = program.rsplit('/').next().unwrap_or(program);
        if BANNED_PROGRAMS.contains(&name) || INERT_PROGRAMS.contains(&name) {
            return Err(format!("`{name}` cannot start a command pattern."));
        }
        if self
            .sha256
            .as_ref()
            .is_some_and(|hash| hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("A pinned program hash is 64 hex digits.".into());
        }
        if self
            .label
            .as_ref()
            .is_some_and(|label| label.chars().count() > 512 || label.contains('\0'))
        {
            return Err("A command rule's label is too long.".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PiPermissionSettings {
    /// The master switch. Off by default; applies from the next run.
    pub enabled: bool,
    pub approval: PermissionApprovals,
    /// `provider/model` for the judge; empty uses the chat's own model.
    pub judge_model: String,
    /// The default for MCP and other extension tools.
    pub tools_default: ToolDecision,
    /// Per tool (`mcp__server__tool`) or per MCP server (`mcp:server`).
    pub tool_overrides: BTreeMap<String, ToolDecision>,
    /// Patterns added to the built-in lists (`~/x`, `<root>/x`, `**/x`).
    pub sensitive: Vec<String>,
    pub protected: Vec<String>,
    pub writable_support: Vec<String>,
    /// Dev servers and tests on localhost.
    pub local_binding: bool,
    /// Unix sockets commands may use (`~/.docker/run/docker.sock`).
    pub unix_sockets: Vec<String>,
    pub network_mode: NetworkMode,
    /// Domains added to the filter's built-in allowlist (`*.x` covers
    /// subdomains).
    pub allow_domains: Vec<String>,
    pub rules: Vec<PermissionRule>,
    pub commands: Vec<CommandRule>,
}

impl Default for PiPermissionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            approval: PermissionApprovals::default(),
            judge_model: String::new(),
            tools_default: ToolDecision::Allow,
            tool_overrides: BTreeMap::new(),
            sensitive: Vec::new(),
            protected: Vec::new(),
            writable_support: Vec::new(),
            local_binding: true,
            unix_sockets: Vec::new(),
            network_mode: NetworkMode::Open,
            allow_domains: Vec::new(),
            rules: Vec::new(),
            commands: Vec::new(),
        }
    }
}

/// Whether this device enforces anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiPermissionStatus {
    /// The active Runtime ships the extension.
    pub installed: bool,
    /// Landstrip works here; without it the extension stands down.
    pub sandbox_available: bool,
    /// `seatbelt`, `landlock+seccomp`, …
    pub implementation: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiPermissionSnapshot {
    pub settings: PiPermissionSettings,
    pub status: PiPermissionStatus,
    /// `permissions.json` exists but is not valid JSON: saving would replace it.
    pub unreadable: bool,
}

pub fn settings_path(paths: &PiRuntimePaths) -> PathBuf {
    paths.agent_dir.join(FILE_NAME)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    let list = match value {
        Some(Value::Array(items)) => Some(items),
        Some(Value::Object(object)) => object.get("add").and_then(Value::as_array),
        _ => None,
    };
    list.map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    })
    .unwrap_or_default()
}

fn decision(value: Option<&Value>) -> Option<ToolDecision> {
    value.and_then(|value| serde_json::from_value(value.clone()).ok())
}

/// The UI's view of a parsed file. Fields the UI does not own are ignored here
/// and preserved by [`save`].
pub fn from_json(root: &Value) -> PiPermissionSettings {
    let mut settings = PiPermissionSettings {
        enabled: root.get("enabled").and_then(Value::as_bool) == Some(true),
        ..PiPermissionSettings::default()
    };
    if let Some(approval) = root.get("approval").and_then(Value::as_object) {
        for key in APPROVAL_KEYS {
            if let Some(mode) = approval
                .get(key)
                .and_then(|value| serde_json::from_value::<ApprovalMode>(value.clone()).ok())
            {
                settings.approval.set(key, mode);
            }
        }
    }
    settings.approval = clamp_sensitive(settings.approval);
    if let Some(model) = root
        .get("judge")
        .and_then(|judge| judge.get("model"))
        .and_then(Value::as_str)
    {
        settings.judge_model = model.to_owned();
    }
    if let Some(tools) = root.get("tools") {
        if let Some(default) = decision(tools.get("mcp")) {
            settings.tools_default = default;
        }
        if let Some(overrides) = tools.get("overrides").and_then(Value::as_object) {
            settings.tool_overrides = overrides
                .iter()
                .filter_map(|(name, value)| Some((name.clone(), decision(Some(value))?)))
                .collect();
        }
    }
    settings.sensitive = strings(root.get("sensitive"));
    settings.protected = strings(root.get("protected"));
    settings.writable_support = strings(root.get("writableSupport"));
    if let Some(network) = root.get("network") {
        if let Some(binding) = network.get("localBinding").and_then(Value::as_bool) {
            settings.local_binding = binding;
        }
        settings.unix_sockets = strings(network.get("unixSockets"));
        if let Some(mode) = network
            .get("mode")
            .and_then(|value| serde_json::from_value::<NetworkMode>(value.clone()).ok())
        {
            settings.network_mode = mode;
        }
        settings.allow_domains = strings(network.get("allowDomains"));
    }
    if let Some(rules) = root.get("rules").and_then(Value::as_array) {
        settings.rules = rules
            .iter()
            .filter_map(|rule| serde_json::from_value(rule.clone()).ok())
            .collect();
    }
    if let Some(commands) = root.get("commands").and_then(Value::as_array) {
        settings.commands = commands
            .iter()
            .filter_map(|rule| serde_json::from_value::<CommandRule>(rule.clone()).ok())
            .filter(|rule| rule.validate().is_ok())
            .collect();
    }
    settings
}

/// Sensitive requests and commands leaving the sandbox never go to the judge.
fn clamp_sensitive(mut approvals: PermissionApprovals) -> PermissionApprovals {
    for mode in [
        &mut approvals.sensitive_read,
        &mut approvals.sensitive_write,
        &mut approvals.escalate,
    ] {
        if *mode == ApprovalMode::Assisted {
            *mode = ApprovalMode::Ask;
        }
    }
    approvals
}

impl PiPermissionSettings {
    pub fn validate(&self) -> Result<(), String> {
        if self.approval.sensitive_read == ApprovalMode::Assisted
            || self.approval.sensitive_write == ApprovalMode::Assisted
        {
            return Err("Sensitive requests cannot be model-assisted.".into());
        }
        if self.approval.escalate == ApprovalMode::Assisted {
            return Err("Running outside the sandbox cannot be model-assisted.".into());
        }
        if self.commands.len() > 4096 {
            return Err("Too many command rules.".into());
        }
        for rule in &self.commands {
            rule.validate()?;
        }
        if self.judge_model.chars().count() > 256 || self.judge_model.contains(['\r', '\n', '\0']) {
            return Err("The judge model id is invalid.".into());
        }
        for (label, list) in [
            ("sensitive patterns", &self.sensitive),
            ("protected patterns", &self.protected),
            ("writable paths", &self.writable_support),
            ("Unix sockets", &self.unix_sockets),
            ("allowed domains", &self.allow_domains),
        ] {
            if list.len() > MAX_PATTERNS {
                return Err(format!("Too many {label}."));
            }
            for pattern in list {
                if pattern.trim().is_empty()
                    || pattern.chars().count() > MAX_PATTERN_CHARS
                    || pattern.contains(['\r', '\n', '\0'])
                {
                    return Err(format!("Invalid entry in {label}: {pattern:?}"));
                }
            }
        }
        // An exception widens the built-in sensitive list; that is the
        // `remove` list's job, edited in the file itself.
        if self
            .sensitive
            .iter()
            .any(|pattern| pattern.starts_with('!'))
        {
            return Err("Sensitive patterns cannot start with \"!\".".into());
        }
        if self.allow_domains.iter().any(|domain| {
            let name = domain.strip_prefix("*.").unwrap_or(domain);
            name.is_empty()
                || name.contains(['/', ':', '*', ' '])
                || !name.contains('.') && name != "localhost"
        }) {
            return Err("Allowed domains are host names like example.com or *.example.com.".into());
        }
        if self.tool_overrides.len() > MAX_PATTERNS
            || self.tool_overrides.keys().any(|name| {
                name.is_empty()
                    || name.chars().count() > 256
                    || name.contains(['\r', '\n', '\0', ' '])
            })
        {
            return Err("Invalid tool override.".into());
        }
        if self.rules.len() > 4096
            || self.rules.iter().any(|rule| {
                !matches!(rule.capability.as_str(), "read" | "write")
                    || !(rule.path.starts_with('/')
                        || rule.path.starts_with("tool:")
                        || rule.path.starts_with("net:"))
            })
        {
            return Err("Invalid always-allow rule.".into());
        }
        Ok(())
    }

    /// Write the UI-owned fields into `root`, keeping everything else.
    fn apply(&self, root: &mut Map<String, Value>) {
        root.entry("version").or_insert(Value::from(1));
        root.insert("enabled".into(), Value::from(self.enabled));
        let approval = object_at(root, "approval");
        for key in APPROVAL_KEYS {
            approval.insert(
                key.into(),
                serde_json::to_value(self.approval.get(key)).expect("mode serializes"),
            );
        }
        let judge = object_at(root, "judge");
        if self.judge_model.trim().is_empty() {
            judge.insert("model".into(), Value::Null);
        } else {
            judge.insert("model".into(), Value::from(self.judge_model.trim()));
        }
        let tools = object_at(root, "tools");
        tools.insert(
            "mcp".into(),
            serde_json::to_value(self.tools_default).expect("decision serializes"),
        );
        tools.insert(
            "overrides".into(),
            serde_json::to_value(&self.tool_overrides).expect("overrides serialize"),
        );
        for (key, list) in [
            ("sensitive", &self.sensitive),
            ("protected", &self.protected),
            ("writableSupport", &self.writable_support),
        ] {
            let edit = object_at(root, key);
            edit.insert("add".into(), Value::from(list.clone()));
            edit.entry("remove").or_insert(Value::Array(Vec::new()));
        }
        let network = object_at(root, "network");
        network.insert("localBinding".into(), Value::from(self.local_binding));
        network.insert("unixSockets".into(), Value::from(self.unix_sockets.clone()));
        network.insert(
            "mode".into(),
            serde_json::to_value(self.network_mode).expect("mode serializes"),
        );
        let domains = network
            .entry("allowDomains")
            .or_insert_with(|| Value::Object(Map::new()));
        if !domains.is_object() {
            *domains = Value::Object(Map::new());
        }
        let domains = domains.as_object_mut().expect("object");
        domains.insert("add".into(), Value::from(self.allow_domains.clone()));
        domains.entry("remove").or_insert(Value::Array(Vec::new()));
        root.insert(
            "rules".into(),
            serde_json::to_value(&self.rules).expect("rules serialize"),
        );
        root.insert(
            "commands".into(),
            serde_json::to_value(&self.commands).expect("commands serialize"),
        );
    }
}

/// The object at `key`, replacing a non-object value (a bare list becomes `{ "add": … }`).
fn object_at<'a>(root: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    let entry = root
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        let previous = std::mem::replace(entry, Value::Object(Map::new()));
        if let Value::Array(items) = previous {
            entry
                .as_object_mut()
                .expect("just replaced")
                .insert("add".into(), Value::Array(items));
        }
    }
    entry.as_object_mut().expect("object")
}

fn read_root(path: &Path) -> (Map<String, Value>, bool) {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(object)) => (object, false),
            _ => (Map::new(), true),
        },
        Err(_) => (Map::new(), false),
    }
}

pub fn load(paths: &PiRuntimePaths) -> PiPermissionSnapshot {
    let (root, unreadable) = read_root(&settings_path(paths));
    PiPermissionSnapshot {
        settings: from_json(&Value::Object(root)),
        status: status(paths),
        unreadable,
    }
}

pub fn save(
    paths: &PiRuntimePaths,
    settings: PiPermissionSettings,
) -> Result<PiPermissionSnapshot, String> {
    settings.validate()?;
    std::fs::create_dir_all(&paths.agent_dir).map_err(|err| err.to_string())?;
    let path = settings_path(paths);
    let (mut root, _) = read_root(&path);
    settings.apply(&mut root);
    let bytes = serde_json::to_vec_pretty(&Value::Object(root)).map_err(|err| err.to_string())?;
    let temporary = path.with_extension("json.tmp");
    write_private(&temporary, &bytes)?;
    std::fs::rename(&temporary, &path).map_err(|err| err.to_string())?;
    Ok(load(paths))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).map_err(|err| err.to_string())?;
    file.write_all(bytes).map_err(|err| err.to_string())
}

/// `@landstrip/landstrip-<os>-<arch>` inside the active Runtime.
fn landstrip_binary(paths: &PiRuntimePaths) -> PathBuf {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    paths
        .current
        .join("npm/node_modules/@landstrip")
        .join(format!("landstrip-{os}-{arch}"))
        .join("bin/landstrip")
}

pub fn status(paths: &PiRuntimePaths) -> PiPermissionStatus {
    let installed = paths.current.join(EXTENSION).is_file();
    if !installed {
        return PiPermissionStatus {
            installed,
            sandbox_available: false,
            implementation: None,
            error: Some("This device's Pi Runtime has no permission control; update it.".into()),
        };
    }
    let binary = landstrip_binary(paths);
    if !binary.is_file() {
        return PiPermissionStatus {
            installed,
            sandbox_available: false,
            implementation: None,
            error: Some(format!("Landstrip is missing ({}).", binary.display())),
        };
    }
    let (sandbox_available, implementation, error) = doctor(&binary);
    PiPermissionStatus {
        installed,
        sandbox_available,
        implementation,
        error,
    }
}

fn doctor(binary: &Path) -> (bool, Option<String>, Option<String>) {
    let output = match std::process::Command::new(binary).arg("doctor").output() {
        Ok(output) => output,
        Err(err) => return (false, None, Some(format!("Landstrip did not start: {err}"))),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report = stdout
        .lines()
        .last()
        .and_then(|line| serde_json::from_str::<Value>(line).ok());
    let ok = output.status.success()
        && report
            .as_ref()
            .and_then(|report| report.get("ok"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let implementation = report
        .as_ref()
        .and_then(|report| report.get("implementation"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if ok {
        return (true, implementation, None);
    }
    let message = report
        .as_ref()
        .and_then(|report| report.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            if stderr.is_empty() {
                "The OS sandbox does not work on this device.".into()
            } else {
                stderr
            }
        });
    (false, implementation, Some(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> PiRuntimePaths {
        PiRuntimePaths {
            root: dir.join("pi-runtime"),
            current: dir.join("pi-runtime/current"),
            executable: dir.join("pi-runtime/current/bin/pi"),
            npm_executable: dir.join("pi-runtime/current/bin/npm"),
            package_dir: dir.join("pi-runtime/current/pi"),
            agent_dir: dir.join("pi-runtime/agent"),
        }
    }

    #[test]
    fn a_missing_file_reads_as_the_extension_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = load(&paths(dir.path()));
        assert_eq!(snapshot.settings, PiPermissionSettings::default());
        assert_eq!(
            snapshot.settings.approval.external_write,
            ApprovalMode::Assisted
        );
        assert!(!snapshot.settings.enabled, "off by default");
        assert!(!snapshot.unreadable);
        assert!(!snapshot.status.installed);
    }

    #[test]
    fn the_master_switch_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::create_dir_all(&paths.agent_dir).unwrap();
        std::fs::write(
            settings_path(&paths),
            r#"{ "enabled": "yes", "custom": 1 }"#,
        )
        .unwrap();
        assert!(!load(&paths).settings.enabled, "only true turns it on");
        let mut settings = load(&paths).settings;
        settings.enabled = true;
        save(&paths, settings).unwrap();
        let root: Value =
            serde_json::from_slice(&std::fs::read(settings_path(&paths)).unwrap()).unwrap();
        assert_eq!(root["enabled"], true);
        assert_eq!(root["custom"], 1);
        assert!(load(&paths).settings.enabled);
        let mut settings = load(&paths).settings;
        settings.enabled = false;
        save(&paths, settings).unwrap();
        assert!(!load(&paths).settings.enabled);
    }

    #[test]
    fn saving_keeps_what_the_ui_does_not_own() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::create_dir_all(&paths.agent_dir).unwrap();
        std::fs::write(
            settings_path(&paths),
            r#"{
              "levels": { "standard": { "external": { "write": "deny" } } },
              "sensitive": { "add": ["**/a.yaml"], "remove": ["~/.npmrc"] },
              "protected": ["<root>/Makefile"],
              "approval": { "external.write": "ask", "sensitive.read": "assisted" },
              "custom": 1
            }"#,
        )
        .unwrap();
        let mut settings = load(&paths).settings;
        assert_eq!(settings.approval.external_write, ApprovalMode::Ask);
        assert_eq!(
            settings.approval.sensitive_read,
            ApprovalMode::Ask,
            "never assisted"
        );
        assert_eq!(settings.sensitive, vec!["**/a.yaml".to_string()]);
        assert_eq!(settings.protected, vec!["<root>/Makefile".to_string()]);
        settings.sensitive.push("**/b.yaml".into());
        settings
            .tool_overrides
            .insert("mcp:github".into(), ToolDecision::Elevate);
        settings.judge_model = "openai/gpt-6-mini".into();
        settings.network_mode = NetworkMode::Filtered;
        settings.allow_domains.push("*.example.com".into());
        save(&paths, settings).unwrap();

        let root: Value =
            serde_json::from_slice(&std::fs::read(settings_path(&paths)).unwrap()).unwrap();
        assert_eq!(root["levels"]["standard"]["external"]["write"], "deny");
        assert_eq!(root["sensitive"]["remove"], serde_json::json!(["~/.npmrc"]));
        assert_eq!(
            root["sensitive"]["add"],
            serde_json::json!(["**/a.yaml", "**/b.yaml"])
        );
        assert_eq!(
            root["protected"]["add"],
            serde_json::json!(["<root>/Makefile"])
        );
        assert_eq!(root["tools"]["overrides"]["mcp:github"], "elevate");
        assert_eq!(root["judge"]["model"], "openai/gpt-6-mini");
        assert_eq!(root["approval"]["sensitive.read"], "ask");
        assert_eq!(root["approval"]["network"], "assisted");
        assert_eq!(root["network"]["mode"], "filtered");
        assert_eq!(
            root["network"]["allowDomains"]["add"],
            serde_json::json!(["*.example.com"])
        );
        assert_eq!(root["custom"], 1);
        assert_eq!(
            load(&paths).settings.network_mode,
            NetworkMode::Filtered,
            "round-trips"
        );
    }

    #[test]
    fn invalid_settings_are_rejected() {
        let mut settings = PiPermissionSettings::default();
        settings.approval.sensitive_write = ApprovalMode::Assisted;
        assert!(settings.validate().is_err());
        let mut settings = PiPermissionSettings::default();
        settings.sensitive.push("!**/.env".into());
        assert!(settings.validate().is_err());
        let mut settings = PiPermissionSettings::default();
        settings.allow_domains.push("https://example.com".into());
        assert!(settings.validate().is_err());
        let mut settings = PiPermissionSettings::default();
        settings.rules.push(PermissionRule {
            capability: "write".into(),
            path: "relative".into(),
            created_at: String::new(),
        });
        assert!(settings.validate().is_err());
    }

    #[test]
    fn command_rules_round_trip_and_bad_ones_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::create_dir_all(&paths.agent_dir).unwrap();
        std::fs::write(
            settings_path(&paths),
            r#"{
              "commands": [
                { "match": ["git", "push", "**"], "decision": "allow", "label": "Push", "createdAt": "t" },
                { "match": ["bash", "-c", "**"], "decision": "allow" },
                { "match": ["kubectl", "delete", "**"], "decision": "deny" }
              ],
              "approval": { "escalate": "assisted" }
            }"#,
        )
        .unwrap();
        let mut settings = load(&paths).settings;
        assert_eq!(
            settings
                .commands
                .iter()
                .map(|rule| rule.pattern[0].as_str())
                .collect::<Vec<_>>(),
            ["git", "kubectl"],
            "a rule on a shell is dropped"
        );
        assert_eq!(
            settings.approval.escalate,
            ApprovalMode::Ask,
            "never assisted"
        );
        settings.commands.remove(1);
        save(&paths, settings).unwrap();
        let root: Value =
            serde_json::from_slice(&std::fs::read(settings_path(&paths)).unwrap()).unwrap();
        assert_eq!(
            root["commands"],
            serde_json::json!([{ "match": ["git", "push", "**"], "decision": "allow", "label": "Push", "createdAt": "t" }])
        );
        assert_eq!(root["approval"]["escalate"], "ask");

        for pattern in [
            vec!["**"],
            vec!["git*", "push"],
            vec!["/bin/bash", "-c"],
            vec!["cd", "x"],
            vec![""],
        ] {
            let mut settings = PiPermissionSettings::default();
            settings.commands.push(CommandRule {
                pattern: pattern.iter().map(|word| word.to_string()).collect(),
                decision: CommandDecision::Allow,
                label: None,
                sha256: None,
                created_at: String::new(),
            });
            assert!(settings.validate().is_err(), "{pattern:?}");
        }
        let mut settings = PiPermissionSettings::default();
        settings.approval.escalate = ApprovalMode::Assisted;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn a_runtime_without_landstrip_reports_why() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::create_dir_all(paths.current.join("extensions")).unwrap();
        std::fs::write(paths.current.join(EXTENSION), "").unwrap();
        let status = status(&paths);
        assert!(status.installed);
        assert!(!status.sandbox_available);
        assert!(status.error.unwrap().contains("Landstrip is missing"));
    }
}
