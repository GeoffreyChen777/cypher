//! Instructions appended to every Pi chat's system prompt (Settings → Agents →
//! System prompt): the agent dir's `APPEND_SYSTEM.md`, which Pi reads when a
//! process starts. On Claude models pi-claude-bridge appends it to Claude
//! Code's own prompt. A subagent whose profile has a prompt of its own launches
//! with `--append-system-prompt`, which replaces this file. Device-local.

use cypher_proto::AppendSystemPrompt;
use serde::Deserialize;

use crate::pi::runtime::PiRuntimePaths;

const FILE: &str = "APPEND_SYSTEM.md";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetAppendSystemPrompt {
    pub text: String,
}

fn path(paths: &PiRuntimePaths) -> std::path::PathBuf {
    paths.agent_dir.join(FILE)
}

/// The file's text; empty when it is absent or unreadable.
pub fn load(paths: &PiRuntimePaths) -> AppendSystemPrompt {
    let path = path(paths);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    AppendSystemPrompt {
        text: text.trim_end().to_string(),
        path: path.display().to_string(),
    }
}

/// Write the text, or remove the file when it is blank, so Pi appends
/// nothing rather than an empty section.
pub fn save(
    paths: &PiRuntimePaths,
    request: SetAppendSystemPrompt,
) -> Result<AppendSystemPrompt, String> {
    let text = request.text.trim();
    if text.len() > AppendSystemPrompt::MAX_BYTES {
        return Err(format!(
            "The system prompt is limited to {} KB; it is sent with every turn.",
            AppendSystemPrompt::MAX_BYTES / 1024
        ));
    }
    let path = path(paths);
    if text.is_empty() {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
            _ => {}
        }
    } else {
        std::fs::create_dir_all(&paths.agent_dir).map_err(|e| e.to_string())?;
        // Atomic: a Pi process starting mid-save reads the old text or the
        // new, never half of it.
        let tmp = path.with_extension("md.tmp");
        std::fs::write(&tmp, format!("{text}\n")).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    }
    Ok(load(paths))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(text: &str) -> SetAppendSystemPrompt {
        SetAppendSystemPrompt { text: text.into() }
    }

    #[test]
    fn saves_trimmed_text_and_blank_removes_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let paths = PiRuntimePaths::for_data_dir(temp.path());
        let initial = load(&paths);
        assert_eq!(initial.text, "", "no file means no text");
        assert!(initial.path.ends_with("pi-runtime/agent/APPEND_SYSTEM.md"));

        let saved = save(&paths, set("\n  Answer in English.\n\nBe brief.  \n\n")).unwrap();
        assert_eq!(saved.text, "Answer in English.\n\nBe brief.");
        assert_eq!(
            std::fs::read_to_string(path(&paths)).unwrap(),
            "Answer in English.\n\nBe brief.\n"
        );
        assert!(!path(&paths).with_extension("md.tmp").exists());

        let cleared = save(&paths, set(" \n ")).unwrap();
        assert_eq!(cleared.text, "");
        assert!(!path(&paths).exists(), "blank text removes the file");
        // Clearing again is not an error.
        save(&paths, set("")).unwrap();
    }

    #[test]
    fn oversized_text_is_refused_and_the_file_kept() {
        let temp = tempfile::tempdir().unwrap();
        let paths = PiRuntimePaths::for_data_dir(temp.path());
        save(&paths, set("keep")).unwrap();
        let huge = "x".repeat(AppendSystemPrompt::MAX_BYTES + 1);
        assert!(save(&paths, set(&huge)).is_err());
        assert_eq!(load(&paths).text, "keep");
    }
}
