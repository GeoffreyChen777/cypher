//! What the Pi plugins' per-chat switches are set to, for the composer's `/`
//! menu: GPT Fast mode (the runtime's cypher-fast-mode.ts), adaptive
//! orchestration (pi-agent-squad `/orchestrate`) and the current goal
//! (pi-goal).
//!
//! The plugins keep this state only as custom entries in the chat's Pi
//! session file, the last entry of each type winning, and report it nowhere
//! else; so the engine that hosts the chat reads it from there. Session files
//! only grow while a chat runs, so each file is scanned once and afterwards
//! only from where the last scan stopped. A file that shrank (rewritten) is
//! scanned again from the start.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Custom entry types, as the plugins write them. Fast mode keeps the type
/// of gpt-fast-pi, the package cypher-fast-mode.ts replaced.
const FAST_ENTRY: &str = "gpt-fast-pi.state";
const ORCHESTRATE_ENTRY: &str = "orchestrator-mode";
const GOAL_ENTRY: &str = "goal-state";

/// Fast mode's default: `settings.json` `"pi-gpt-fast-mode": {"enabled": true}`.
const FAST_DEFAULT_FIELD: &str = "pi-gpt-fast-mode";

/// Remembered scans. Past this many files the cache starts over; a rescan
/// only costs one read of a file.
const MAX_CACHED_FILES: usize = 256;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiSessionModes {
    /// GPT Fast mode: the chat's last `/fast`, else the agent's default.
    pub fast: bool,
    /// Adaptive subagent delegation: the chat's last `/orchestrate on|off`;
    /// off by default.
    pub orchestrate: bool,
    /// The chat's goal, unless none was set or it was cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<PiGoal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiGoal {
    /// pi-goal's status: `active`, `paused`, `blocked`, `usage_limited`,
    /// `budget_limited` or `complete`.
    pub status: String,
    /// The objective as the user wrote it.
    pub text: String,
}

/// What one file says so far. `None` fields: no entry of that type yet.
#[derive(Debug, Clone, Default)]
struct Scan {
    /// Bytes read, always ending on a complete line.
    len: u64,
    fast: Option<bool>,
    orchestrate: Option<bool>,
    goal: Option<Option<PiGoal>>,
}

static SCANS: LazyLock<Mutex<HashMap<PathBuf, Scan>>> = LazyLock::new(Default::default);

/// The switches of the chat whose Pi session is `session_file` (`None` for a
/// chat with no session yet), with the plugins' defaults from `agent_dir`.
/// Blocking: reads files.
pub fn read(session_file: Option<&Path>, agent_dir: &Path) -> PiSessionModes {
    let scan = session_file.map(scan_file).unwrap_or_default();
    PiSessionModes {
        fast: scan.fast.unwrap_or_else(|| default_fast(agent_dir)),
        orchestrate: scan.orchestrate.unwrap_or(false),
        goal: scan.goal.flatten(),
    }
}

fn scan_file(path: &Path) -> Scan {
    let Ok(len) = std::fs::metadata(path).map(|meta| meta.len()) else {
        return Scan::default();
    };
    let cached = SCANS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(path)
        .cloned();
    let prior = cached.filter(|scan| scan.len <= len).unwrap_or_default();
    if prior.len == len {
        return prior;
    }
    let scan = match continue_scan(path, prior) {
        Ok(scan) => scan,
        Err(err) => {
            tracing::debug!(path = %path.display(), error = %err, "Pi session scan failed");
            return Scan::default();
        }
    };
    let mut scans = SCANS.lock().unwrap_or_else(PoisonError::into_inner);
    if scans.len() >= MAX_CACHED_FILES && !scans.contains_key(path) {
        scans.clear();
    }
    scans.insert(path.to_path_buf(), scan.clone());
    scan
}

/// Read complete lines from `scan.len` on. A last line without its newline
/// is still being written: it is left for the next scan.
fn continue_scan(path: &Path, mut scan: Scan) -> std::io::Result<Scan> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(scan.len))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 || line.last() != Some(&b'\n') {
            break;
        }
        scan.len += read as u64;
        apply(&mut scan, &line);
    }
    Ok(scan)
}

fn apply(scan: &mut Scan, line: &[u8]) {
    // Most lines are messages, some of them megabytes: parse only the lines
    // that name a custom type at all. Inside a JSON string these quotes would
    // be escaped, so text that merely mentions an entry type never matches.
    if !contains(line, b"\"customType\":\"") {
        return;
    }
    let Ok(entry) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    if entry.get("type").and_then(Value::as_str) != Some("custom") {
        return;
    }
    let data = entry.get("data");
    let enabled = || data?.get("enabled")?.as_bool();
    match entry.get("customType").and_then(Value::as_str) {
        Some(FAST_ENTRY) => scan.fast = enabled().or(scan.fast),
        Some(ORCHESTRATE_ENTRY) => scan.orchestrate = enabled().or(scan.orchestrate),
        Some(GOAL_ENTRY) => {
            if let Some(goal) = data.and_then(|data| data.get("goal")) {
                scan.goal = Some(goal_of(goal));
            }
        }
        _ => {}
    }
}

/// A stored goal, or `None` for `null` and shapes this reader does not know.
fn goal_of(goal: &Value) -> Option<PiGoal> {
    Some(PiGoal {
        status: goal.get("status")?.as_str()?.to_owned(),
        text: goal.get("text")?.as_str()?.to_owned(),
    })
}

fn default_fast(agent_dir: &Path) -> bool {
    std::fs::read_to_string(agent_dir.join("settings.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|settings| settings.get(FAST_DEFAULT_FIELD)?.get("enabled")?.as_bool())
        .unwrap_or(false)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn custom(kind: &str, data: Value) -> String {
        format!(
            "{}\n",
            serde_json::json!({
                "type": "custom", "customType": kind, "data": data,
                "id": "x", "parentId": null, "timestamp": "2026-10-03T00:00:00Z",
            })
        )
    }

    #[test]
    fn defaults_without_a_session() {
        let agent = tempfile::tempdir().unwrap();
        assert_eq!(read(None, agent.path()), PiSessionModes::default());
        std::fs::write(
            agent.path().join("settings.json"),
            r#"{"pi-gpt-fast-mode": {"enabled": true}}"#,
        )
        .unwrap();
        assert!(read(None, agent.path()).fast);
        // A missing file reads like no session.
        let missing = agent.path().join("gone.jsonl");
        assert!(read(Some(&missing), agent.path()).fast);
    }

    #[test]
    fn the_last_entry_of_each_type_wins_and_appends_are_picked_up() {
        let agent = tempfile::tempdir().unwrap();
        let path = agent.path().join("session.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        write!(
            file,
            "{}{}{}{}",
            r#"{"type":"session","version":3,"id":"s"}"#.to_owned() + "\n",
            custom(
                FAST_ENTRY,
                serde_json::json!({ "version": 1, "enabled": true })
            ),
            custom(ORCHESTRATE_ENTRY, serde_json::json!({ "enabled": true })),
            // A message quoting an entry type is not an entry.
            r#"{"type":"message","message":{"content":"\"customType\":\"orchestrator-mode\""}}"#
                .to_owned()
                + "\n",
        )
        .unwrap();
        file.flush().unwrap();
        let modes = read(Some(&path), agent.path());
        assert!(modes.fast && modes.orchestrate && modes.goal.is_none());

        // Appended later: the scan continues where it stopped.
        write!(
            file,
            "{}{}",
            custom(ORCHESTRATE_ENTRY, serde_json::json!({ "enabled": false })),
            custom(
                GOAL_ENTRY,
                serde_json::json!({ "goal": { "id": "g", "text": "Ship it", "status": "paused" } })
            ),
        )
        .unwrap();
        file.flush().unwrap();
        let modes = read(Some(&path), agent.path());
        assert!(modes.fast && !modes.orchestrate);
        assert_eq!(
            modes.goal,
            Some(PiGoal {
                status: "paused".into(),
                text: "Ship it".into()
            })
        );

        // A cleared goal, and a half-written line that is not read yet.
        file.write_all(custom(GOAL_ENTRY, serde_json::json!({ "goal": null })).as_bytes())
            .unwrap();
        file.write_all(
            br#"{"type":"custom","customType":"gpt-fast-pi.state","data":{"enabled":fal"#,
        )
        .unwrap();
        file.flush().unwrap();
        let modes = read(Some(&path), agent.path());
        assert!(modes.goal.is_none());
        assert!(modes.fast, "the unfinished line does not count yet");
        file.write_all(b"se}}\n").unwrap();
        file.flush().unwrap();
        assert!(!read(Some(&path), agent.path()).fast);
    }

    #[test]
    fn a_rewritten_file_is_scanned_again() {
        let agent = tempfile::tempdir().unwrap();
        let path = agent.path().join("session.jsonl");
        std::fs::write(
            &path,
            custom(ORCHESTRATE_ENTRY, serde_json::json!({ "enabled": true })).repeat(3),
        )
        .unwrap();
        assert!(read(Some(&path), agent.path()).orchestrate);
        std::fs::write(&path, "{\"type\":\"session\"}\n").unwrap();
        assert!(!read(Some(&path), agent.path()).orchestrate);
    }
}
