//! Device-scoped subagent profiles — the `agents/<name>.md` files the Pi
//! subagents extension (`pi-agent-squad`) discovers when a chat spawns a child.
//!
//! The extension owns this format, so the module below is a deliberate mirror
//! of its loader (`pi-agent-squad/agents.ts`) rather than a general Markdown or
//! YAML facility. The details that constrain every function here:
//!
//! - frontmatter is a flat block between `---` lines, parsed line by line — the
//!   extension splits on the FIRST colon and strips one layer of surrounding
//!   quotes. It is **not** YAML, so a value is always single-line and nothing
//!   may carry a newline into it.
//! - `name` and `description` are **required**: a file missing either is
//!   silently skipped, so writing one would make the agent vanish rather than
//!   fail loudly. [`PiSubagent::validate`] rejects that before it reaches disk.
//! - `tools` is one comma-separated scalar, not a list.
//! - read-only is spelled `readonly: true` *or* `access: read-only` and the
//!   extension ORs the two. This module therefore owns BOTH keys: clearing the
//!   flag has to erase a stale `access` line, or the file would keep asserting
//!   the opposite of what the UI shows.
//! - the body after the closing `---` is the system prompt, trimmed.
//!
//! Discovery mirrors the extension as well: its own `agents/` ships the
//! built-ins, `<agent_dir>/agents` holds the user's, and a user file WINS over
//! a built-in of the same name. So editing a built-in writes a user-level
//! override, and deleting that override restores the built-in — which is why
//! [`delete`] never touches the built-in directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pi_runtime::PiRuntimePaths;

/// The npm package that implements subagents and ships the built-in profiles.
const EXTENSION_PACKAGE: &str = "pi-agent-squad";

/// Frontmatter keys this module renders itself. Every other key in an existing
/// file is preserved verbatim so hand-authored extras survive a UI save.
const OWNED_KEYS: [&str; 7] = [
    "name",
    "description",
    "readonly",
    "access",
    "tools",
    "model",
    "thinking",
];

/// The extension's `MAX_NAME_LENGTH` equivalent for agent files.
const MAX_NAME: usize = 64;
const MAX_DESCRIPTION: usize = 1024;
/// Generous but bounded: a system prompt is a document, not a paste buffer.
const MAX_PROMPT: usize = 100_000;
const MAX_TOOLS: usize = 64;
const MAX_TOOL: usize = 64;
/// Matches the `StartSubagent` parameter limits in [`crate::rpc`].
const MAX_MODEL: usize = 200;
const MAX_THINKING: usize = 32;
/// A frontmatter block far past this is not a profile we wrote.
const MAX_EXTRA_LINES: usize = 64;

/// One discovered profile. Bounded on purpose — the same reasoning as
/// `cypher_proto::ChildAgentProfile`: a fixed set of fields the UI can present
/// honestly, never an arbitrary key/value bag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiSubagent {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default)]
    pub read_only: bool,
    /// Ships with the extension. Saving one writes a user override instead of
    /// editing the package, so this is a display fact, never a write target.
    #[serde(default)]
    pub builtin: bool,
    /// This user profile shadows a built-in of the same name — deleting it
    /// restores the built-in rather than removing the agent.
    #[serde(default)]
    pub overrides_builtin: bool,
}

impl PiSubagent {
    /// Reject anything the extension would silently drop, plus anything that
    /// cannot survive a single-line `key: value` round trip.
    pub fn validate(&self) -> Result<(), String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Give the agent a name.".into());
        }
        if name.chars().count() > MAX_NAME {
            return Err(format!("Name must be {MAX_NAME} characters or fewer."));
        }
        if !is_safe_agent_name(name) {
            return Err(
                "Name may use only letters, numbers, dots, dashes and underscores, and cannot be \"main\"."
                    .into(),
            );
        }
        let description = self.description.trim();
        if description.is_empty() {
            // Not cosmetic: the extension skips a file with no description, so
            // saving one would delete the agent from the spawn menu.
            return Err(
                "Give the agent a description — the extension ignores a profile without one."
                    .into(),
            );
        }
        if description.chars().count() > MAX_DESCRIPTION {
            return Err(format!(
                "Description must be {MAX_DESCRIPTION} characters or fewer."
            ));
        }
        for (label, value) in [
            ("Description", description),
            ("Model", self.model.as_deref().unwrap_or_default()),
            ("Thinking", self.thinking.as_deref().unwrap_or_default()),
        ] {
            if value.contains(['\r', '\n', '\0']) {
                return Err(format!("{label} must be a single line."));
            }
        }
        if self
            .model
            .as_deref()
            .is_some_and(|model| model.chars().count() > MAX_MODEL)
        {
            return Err(format!("Model must be {MAX_MODEL} characters or fewer."));
        }
        if self
            .thinking
            .as_deref()
            .is_some_and(|level| level.chars().count() > MAX_THINKING)
        {
            return Err(format!(
                "Thinking level must be {MAX_THINKING} characters or fewer."
            ));
        }
        if self.tools.len() > MAX_TOOLS {
            return Err(format!("Select {MAX_TOOLS} tools or fewer."));
        }
        for tool in &self.tools {
            if tool.trim().is_empty() {
                return Err("Tool names cannot be blank.".into());
            }
            if tool.chars().count() > MAX_TOOL {
                return Err(format!(
                    "Tool names must be {MAX_TOOL} characters or fewer."
                ));
            }
            // The list is stored as one comma-separated scalar, so a comma
            // inside a name would silently split it into two tools.
            if tool.contains([',', '\r', '\n', '\0']) {
                return Err("Tool names cannot contain commas or line breaks.".into());
            }
        }
        if self.system_prompt.len() > MAX_PROMPT {
            return Err("The system prompt is too long.".into());
        }
        Ok(())
    }

    /// The name as it is written to disk and keyed by the extension.
    fn key(&self) -> String {
        self.name.trim().to_string()
    }
}

/// Mirrors the extension's `isSafeAgentName`, including its `main` exclusion
/// (that name addresses the parent in the messaging tools).
pub fn is_safe_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        && name != "."
        && name != ".."
        && name != "main"
}

/// `<agent_dir>/agents` — the user profiles, and the only directory we write.
pub fn user_dir(paths: &PiRuntimePaths) -> PathBuf {
    paths.agent_dir.join("agents")
}

/// The profiles shipped inside the installed extension package.
pub fn builtin_dir(paths: &PiRuntimePaths) -> PathBuf {
    paths
        .current
        .join("npm")
        .join("node_modules")
        .join(EXTENSION_PACKAGE)
        .join("agents")
}

/// Every profile the extension would discover, user overrides applied, sorted
/// by name. Unreadable or malformed files are skipped exactly as the extension
/// skips them — this list is a view of what Pi will actually offer.
pub fn list(paths: &PiRuntimePaths) -> Vec<PiSubagent> {
    let builtins = load_dir(&builtin_dir(paths));
    let builtin_names: Vec<String> = builtins
        .iter()
        .map(|(_, parsed)| parsed.agent.name.clone())
        .collect();
    let mut by_name: BTreeMap<String, PiSubagent> = BTreeMap::new();
    for (_, parsed) in builtins {
        let mut agent = parsed.agent;
        agent.builtin = true;
        by_name.insert(agent.name.clone(), agent);
    }
    for (_, parsed) in load_dir(&user_dir(paths)) {
        let mut agent = parsed.agent;
        agent.builtin = false;
        agent.overrides_builtin = builtin_names.contains(&agent.name);
        by_name.insert(agent.name.clone(), agent);
    }
    by_name.into_values().collect()
}

/// Write `name.md` into the user directory. `original_name` is the profile the
/// editor opened: when it differs, the save is a rename and the old user file
/// is removed after the new one lands.
pub fn save(
    paths: &PiRuntimePaths,
    agent: &PiSubagent,
    original_name: Option<&str>,
) -> Result<(), String> {
    agent.validate()?;
    let key = agent.key();
    let original = original_name.map(str::trim).filter(|name| !name.is_empty());
    let directory = user_dir(paths);

    // Renaming onto — or creating over — a different existing profile would
    // silently replace it. Only the profile we opened may be overwritten.
    if original != Some(key.as_str()) && find_user(paths, &key).is_some() {
        return Err(format!("A subagent named \"{key}\" already exists."));
    }

    // Keep hand-authored keys this module does not model. They belong to the
    // file being edited, so a rename carries them along.
    let existing = original
        .and_then(|name| find_user(paths, name))
        .or_else(|| find_user(paths, &key));
    let extra = existing
        .as_ref()
        .map(|(_, parsed)| parsed.extra.clone())
        .unwrap_or_default();

    std::fs::create_dir_all(&directory).map_err(|err| err.to_string())?;
    let path = directory.join(format!("{key}.md"));
    write_atomic(&path, render(agent, &extra).as_bytes())?;

    // Rename: drop the file the editor opened, but never before the new one is
    // safely on disk, and never the built-in it may have been overriding.
    if let Some(original) = original.filter(|name| *name != key)
        && let Some((previous, _)) = find_user(paths, original)
        && previous != path
    {
        std::fs::remove_file(&previous).map_err(|err| err.to_string())?;
    }
    Ok(())
}

/// Remove a user profile. A built-in cannot be deleted — removing an override
/// simply restores it, which is what the caller sees on the next [`list`].
pub fn delete(paths: &PiRuntimePaths, name: &str) -> Result<(), String> {
    let name = name.trim();
    if !is_safe_agent_name(name) {
        return Err("Unknown subagent.".into());
    }
    let Some((path, _)) = find_user(paths, name) else {
        return Err(format!(
            "\"{name}\" is a built-in subagent and cannot be deleted."
        ));
    };
    std::fs::remove_file(&path).map_err(|err| err.to_string())
}

/// Locate the USER file declaring `name`. Identity is the frontmatter `name`,
/// not the filename — the extension keys on it, and a hand-authored file may
/// legitimately disagree with its own stem.
fn find_user(paths: &PiRuntimePaths, name: &str) -> Option<(PathBuf, Parsed)> {
    load_dir(&user_dir(paths))
        .into_iter()
        .find(|(_, parsed)| parsed.agent.name == name)
}

/// Parsed file plus the frontmatter lines we do not model.
#[derive(Debug, Clone)]
struct Parsed {
    agent: PiSubagent,
    /// Unknown `key: value` lines, verbatim and in file order.
    extra: Vec<String>,
}

/// Every readable, well-formed profile in one directory.
fn load_dir(directory: &Path) -> Vec<(PathBuf, Parsed)> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(parsed) = parse(&text) {
            found.push((path, parsed));
        }
    }
    found.sort_by(|(a, _), (b, _)| a.cmp(b));
    found
}

/// Parse one file the way the extension does, returning `None` for anything it
/// would skip (no frontmatter, or a missing/unsafe `name` or `description`).
fn parse(text: &str) -> Option<Parsed> {
    let normalized = text.replace("\r\n", "\n");
    let rest = normalized.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let raw = &rest[..end];
    let after = &rest[end + 4..];
    let body = after.strip_prefix('\n').unwrap_or(after);

    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    let mut extra = Vec::new();
    for line in raw.split('\n') {
        // `idx <= 0` in the extension: no colon, or a line that starts with
        // one, is not a field.
        let Some(index) = line.find(':').filter(|index| *index > 0) else {
            continue;
        };
        let key = line[..index].trim();
        if key.is_empty() {
            continue;
        }
        let value = unquote(line[index + 1..].trim());
        if OWNED_KEYS.contains(&key.to_ascii_lowercase().as_str()) {
            fields.insert(key.to_ascii_lowercase(), value);
        } else if extra.len() < MAX_EXTRA_LINES {
            extra.push(line.to_string());
        }
    }

    let name = fields.get("name")?.trim().to_string();
    let description = fields.get("description")?.trim().to_string();
    if name.is_empty() || description.is_empty() || !is_safe_agent_name(&name) {
        return None;
    }
    let tools = fields
        .get("tools")
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|tool| !tool.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let read_only = fields
        .get("readonly")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        || fields
            .get("access")
            .is_some_and(|value| value.eq_ignore_ascii_case("read-only"));

    Some(Parsed {
        agent: PiSubagent {
            name,
            description,
            system_prompt: body.trim().to_string(),
            tools,
            model: non_empty(fields.get("model")),
            thinking: non_empty(fields.get("thinking")),
            read_only,
            builtin: false,
            overrides_builtin: false,
        },
        extra,
    })
}

fn non_empty(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// The extension's `replace(/^["']|["']$/g, "")`: at most one quote stripped
/// from each end, independently.
fn unquote(value: &str) -> String {
    let stripped = value.strip_prefix(['"', '\'']).unwrap_or(value);
    stripped
        .strip_suffix(['"', '\''])
        .unwrap_or(stripped)
        .to_string()
}

/// Render a profile back to the extension's format: owned keys in a stable
/// order, then any preserved lines, then the prompt body.
fn render(agent: &PiSubagent, extra: &[String]) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {}\n", agent.name.trim()));
    out.push_str(&format!("description: {}\n", agent.description.trim()));
    if let Some(model) = agent
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        out.push_str(&format!("model: {model}\n"));
    }
    if let Some(thinking) = agent
        .thinking
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        out.push_str(&format!("thinking: {thinking}\n"));
    }
    if !agent.tools.is_empty() {
        let tools: Vec<&str> = agent.tools.iter().map(|tool| tool.trim()).collect();
        out.push_str(&format!("tools: {}\n", tools.join(", ")));
    }
    // Always explicit. The `access` key is intentionally never re-emitted, so
    // this line is the only statement about read-only in the file.
    out.push_str(&format!("readonly: {}\n", agent.read_only));
    for line in extra {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("---\n\n");
    out.push_str(agent.system_prompt.trim());
    out.push('\n');
    out
}

/// Replace `path` in one step, and keep the file owner-only — a system prompt
/// is the user's private instruction set.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("md.tmp");
    std::fs::write(&temporary, bytes).map_err(|err| err.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&temporary, path).map_err(|err| {
        let _ = std::fs::remove_file(&temporary);
        err.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &Path) -> PiRuntimePaths {
        let current = root.join("current");
        PiRuntimePaths {
            root: root.into(),
            current: current.clone(),
            executable: current.join("bin/pi"),
            npm_executable: current.join("bin/npm"),
            package_dir: current.join("pi"),
            agent_dir: root.join("agent"),
        }
    }

    fn sample() -> PiSubagent {
        PiSubagent {
            name: "planner".into(),
            description: "Resolve a design decision".into(),
            system_prompt: "You are the design-decision specialist.".into(),
            tools: vec!["read".into(), "bash".into()],
            model: Some("claude-bridge/claude-fable-5-1".into()),
            thinking: Some("xhigh".into()),
            read_only: true,
            builtin: false,
            overrides_builtin: false,
        }
    }

    /// The real `planner.md` closes its frontmatter with the body abutting the
    /// delimiter (`---You are …`), which the extension's regex accepts.
    #[test]
    fn parses_a_body_abutting_the_closing_delimiter() {
        let text = "---\nname: planner\ndescription: Plan\n---You are the planner.\n";
        let parsed = parse(text).expect("parses");
        assert_eq!(parsed.agent.name, "planner");
        assert_eq!(parsed.agent.system_prompt, "You are the planner.");
    }

    #[test]
    fn parses_the_conventional_form() {
        let text = "---\nname: planner\ndescription: Plan\n---\n\nYou are the planner.\n";
        let parsed = parse(text).expect("parses");
        assert_eq!(parsed.agent.system_prompt, "You are the planner.");
    }

    #[test]
    fn skips_files_the_extension_would_skip() {
        // No frontmatter at all.
        assert!(parse("You are the planner.\n").is_none());
        // Missing description.
        assert!(parse("---\nname: planner\n---\nBody\n").is_none());
        // Missing name.
        assert!(parse("---\ndescription: Plan\n---\nBody\n").is_none());
        // Unsafe name.
        assert!(parse("---\nname: main\ndescription: Plan\n---\nBody\n").is_none());
        assert!(parse("---\nname: a b\ndescription: Plan\n---\nBody\n").is_none());
    }

    #[test]
    fn reads_tools_as_one_comma_separated_scalar() {
        let parsed = parse("---\nname: a\ndescription: d\ntools: read, bash , grep\n---\nB\n")
            .expect("parses");
        assert_eq!(parsed.agent.tools, ["read", "bash", "grep"]);
    }

    #[test]
    fn honors_both_spellings_of_read_only() {
        let readonly =
            parse("---\nname: a\ndescription: d\nreadonly: TRUE\n---\nB\n").expect("parses");
        assert!(readonly.agent.read_only);
        let access =
            parse("---\nname: a\ndescription: d\naccess: Read-Only\n---\nB\n").expect("parses");
        assert!(access.agent.read_only);
        let neither = parse("---\nname: a\ndescription: d\n---\nB\n").expect("parses");
        assert!(!neither.agent.read_only);
    }

    #[test]
    fn strips_one_layer_of_quotes_like_the_extension() {
        let parsed =
            parse("---\nname: a\ndescription: \"A quoted one\"\n---\nB\n").expect("parses");
        assert_eq!(parsed.agent.description, "A quoted one");
    }

    #[test]
    fn a_description_may_contain_colons() {
        let parsed =
            parse("---\nname: a\ndescription: Do this: then that\n---\nB\n").expect("parses");
        assert_eq!(parsed.agent.description, "Do this: then that");
    }

    #[test]
    fn round_trips_through_render() {
        let agent = sample();
        let parsed = parse(&render(&agent, &[])).expect("parses");
        assert_eq!(parsed.agent, agent);
    }

    #[test]
    fn clearing_read_only_erases_a_stale_access_key() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        std::fs::create_dir_all(user_dir(&paths)).unwrap();
        std::fs::write(
            user_dir(&paths).join("a.md"),
            "---\nname: a\ndescription: d\naccess: read-only\n---\nBody\n",
        )
        .unwrap();
        assert!(list(&paths)[0].read_only);

        let mut agent = list(&paths).into_iter().next().unwrap();
        agent.read_only = false;
        save(&paths, &agent, Some("a")).unwrap();

        let text = std::fs::read_to_string(user_dir(&paths).join("a.md")).unwrap();
        assert!(
            !text.contains("access:"),
            "stale access key survived: {text}"
        );
        assert!(!list(&paths)[0].read_only);
    }

    #[test]
    fn preserves_unknown_frontmatter_keys() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        std::fs::create_dir_all(user_dir(&paths)).unwrap();
        std::fs::write(
            user_dir(&paths).join("a.md"),
            "---\nname: a\ndescription: d\ncolor: blue\n---\nBody\n",
        )
        .unwrap();

        let mut agent = list(&paths).into_iter().next().unwrap();
        agent.description = "changed".into();
        save(&paths, &agent, Some("a")).unwrap();

        let text = std::fs::read_to_string(user_dir(&paths).join("a.md")).unwrap();
        assert!(text.contains("color: blue"), "{text}");
        assert!(text.contains("description: changed"), "{text}");
    }

    #[test]
    fn a_user_profile_overrides_a_builtin_of_the_same_name() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        std::fs::create_dir_all(builtin_dir(&paths)).unwrap();
        std::fs::write(
            builtin_dir(&paths).join("planner.md"),
            "---\nname: planner\ndescription: built in\n---\nBuiltin body\n",
        )
        .unwrap();

        let listed = list(&paths);
        assert_eq!(listed.len(), 1);
        assert!(listed[0].builtin);
        assert_eq!(listed[0].description, "built in");

        let mut agent = listed.into_iter().next().unwrap();
        agent.description = "mine".into();
        save(&paths, &agent, Some("planner")).unwrap();

        let listed = list(&paths);
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].builtin);
        assert!(listed[0].overrides_builtin);
        assert_eq!(listed[0].description, "mine");

        // Deleting the override restores the built-in rather than the agent
        // disappearing.
        delete(&paths, "planner").unwrap();
        let listed = list(&paths);
        assert_eq!(listed.len(), 1);
        assert!(listed[0].builtin);
        assert_eq!(listed[0].description, "built in");
    }

    #[test]
    fn a_builtin_cannot_be_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        std::fs::create_dir_all(builtin_dir(&paths)).unwrap();
        std::fs::write(
            builtin_dir(&paths).join("planner.md"),
            "---\nname: planner\ndescription: built in\n---\nBody\n",
        )
        .unwrap();
        assert!(delete(&paths, "planner").is_err());
        assert_eq!(list(&paths).len(), 1);
    }

    #[test]
    fn renaming_moves_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let mut agent = sample();
        save(&paths, &agent, None).unwrap();
        assert!(user_dir(&paths).join("planner.md").is_file());

        agent.name = "strategist".into();
        save(&paths, &agent, Some("planner")).unwrap();
        assert!(!user_dir(&paths).join("planner.md").exists());
        assert!(user_dir(&paths).join("strategist.md").is_file());
        let listed = list(&paths);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "strategist");
    }

    #[test]
    fn a_rename_cannot_clobber_another_profile() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        save(&paths, &sample(), None).unwrap();
        let mut other = sample();
        other.name = "reviewer".into();
        other.description = "Review".into();
        save(&paths, &other, None).unwrap();

        other.name = "planner".into();
        let error = save(&paths, &other, Some("reviewer")).expect_err("must refuse");
        assert!(error.contains("already exists"), "{error}");
        // Both survive, unchanged.
        assert_eq!(list(&paths).len(), 2);
        assert_eq!(list(&paths)[0].description, "Resolve a design decision");
    }

    #[test]
    fn creating_over_an_existing_name_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        save(&paths, &sample(), None).unwrap();
        assert!(save(&paths, &sample(), None).is_err());
    }

    #[test]
    fn identity_is_the_frontmatter_name_not_the_filename() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        std::fs::create_dir_all(user_dir(&paths)).unwrap();
        std::fs::write(
            user_dir(&paths).join("misnamed.md"),
            "---\nname: planner\ndescription: d\n---\nBody\n",
        )
        .unwrap();
        assert_eq!(list(&paths)[0].name, "planner");
        delete(&paths, "planner").unwrap();
        assert!(!user_dir(&paths).join("misnamed.md").exists());
    }

    #[test]
    fn validation_rejects_what_the_extension_would_drop() {
        let mut agent = sample();
        agent.description = "  ".into();
        assert!(agent.validate().is_err());

        let mut agent = sample();
        agent.name = "has space".into();
        assert!(agent.validate().is_err());

        let mut agent = sample();
        agent.description = "two\nlines".into();
        assert!(agent.validate().is_err());

        let mut agent = sample();
        agent.tools = vec!["read,write".into()];
        assert!(agent.validate().is_err());

        assert!(sample().validate().is_ok());
    }

    #[test]
    fn a_saved_profile_is_owner_only() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        save(&paths, &sample(), None).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(user_dir(&paths).join("planner.md"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "profile is readable by others");
        }
    }

    #[test]
    fn a_missing_runtime_lists_nothing() {
        let temp = tempfile::tempdir().unwrap();
        assert!(list(&paths(temp.path())).is_empty());
    }
}
