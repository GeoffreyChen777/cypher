//! A run's memory section (docs/design/memory.md §4): each visible store's
//! `MEMORY.md`, within a budget, and a listing of its other files.
//!
//! The guidance stays short and unsteered: it says how memory is laid out
//! and to keep it true, not what kinds of things to save.

use cypher_proto::{
    MEMORY_INDEX, MEMORY_INDEX_BYTES, MEMORY_INDEX_LINES, MEMORY_PROMPT_BYTES, MemoryFile,
};

use super::Memory;
use super::access::{Reader, Root};

/// Other files named per store before "and N more".
const LISTED_FILES: usize = 40;

const GUIDANCE: &str = "You have persistent memory: folders of markdown files you keep across \
sessions with the `memory` tool. Each folder's MEMORY.md is shown below; read its other files \
with `memory view` when they matter to the task.
- When the user asks you to remember something, save it now, before you answer. What is about \
the user (their preferences, their life, how they like to work) goes in /you, which every \
session reads: a lasting fact in /you/MEMORY.md (\"Likes hazelnut coffee\"), a dated event in \
a topic file such as /you/journal.md (\"2026-11-09: had a hazelnut coffee\").
- Save what you or a later session will need again. Keep MEMORY.md a short index (under 200 \
lines): put detail in topic files and link to them from it.
- Correct or delete memory that turns out wrong or stale, so it stays true. Never save secrets \
(keys, tokens, passwords).
- When asked what was said or decided before, look in memory first, then search_history and \
read_history, which find it in past conversations.";

/// The start of `text` within `lines` and `bytes`, and whether it was cut.
pub(super) fn budgeted(text: &str, lines: usize, bytes: usize) -> (String, bool) {
    let mut out = String::new();
    let mut cut = false;
    for (n, line) in text.lines().enumerate() {
        if n >= lines || out.len() + line.len() + 1 > bytes {
            cut = true;
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    (out.trim_end().to_string(), cut)
}

/// The section for `roots` with their files, or `None` when there are no
/// roots. `today` dates what gets saved ("Monday, November 9, 2026"); a
/// day, not a time, so the prompt stays the same all day.
pub(super) fn section(
    roots: &[(&Root, Vec<MemoryFile>)],
    extra: &[String],
    today: &str,
) -> Option<String> {
    if roots.is_empty() {
        return None;
    }
    let mut out = vec![
        "<memory>".to_string(),
        GUIDANCE.to_string(),
        format!("- Today is {today} (local time); date what you save with it."),
    ];
    out.extend(extra.iter().cloned());
    let mut budget = MEMORY_PROMPT_BYTES;
    for (root, files) in roots {
        out.push(String::new());
        out.push(format!(
            "## {} — {}{}",
            root.path,
            root.title,
            if root.write { "" } else { " (read-only)" }
        ));
        let index = files.iter().find(|f| f.path == MEMORY_INDEX);
        match index.map(|f| f.text.trim()).filter(|t| !t.is_empty()) {
            None => out.push(format!(
                "(No MEMORY.md yet{}.)",
                if root.write {
                    format!("; start one with `memory create {}/MEMORY.md`", root.path)
                } else {
                    String::new()
                }
            )),
            Some(text) => {
                let allowed = MEMORY_INDEX_BYTES.min(budget);
                let (shown, cut) = budgeted(text, MEMORY_INDEX_LINES, allowed);
                budget = budget.saturating_sub(shown.len());
                out.push(shown);
                if cut {
                    let total = text.lines().count();
                    out.push(format!(
                        "(MEMORY.md continues: {total} lines in all. view {}/MEMORY.md for the \
                         rest, and move detail into topic files.)",
                        root.path
                    ));
                }
            }
        }
        let others: Vec<&MemoryFile> = files.iter().filter(|f| f.path != MEMORY_INDEX).collect();
        if !others.is_empty() {
            // Names only: a size would change the prompt (and spend its
            // cache) on every edit of any file.
            let mut names: Vec<String> = others
                .iter()
                .take(LISTED_FILES)
                .map(|f| format!("{}{}", root.path, f.path))
                .collect();
            if others.len() > LISTED_FILES {
                names.push(format!("and {} more", others.len() - LISTED_FILES));
            }
            out.push(format!("Other files: {}", names.join(" · ")));
        }
    }
    out.push("</memory>".to_string());
    Some(out.join("\n"))
}

impl Memory {
    /// The memory section of a run in `chat_id`; `None` when the chat has
    /// no memory (a peer thread). Called when the run spawns.
    pub fn prompt_for(&self, chat_id: &str) -> Option<String> {
        // A new run starts clean: what made the last one untrusted is over.
        self.inner.taint.clear(chat_id);
        let reader = self.reader(chat_id)?;
        let roots = self.prompt_roots(&reader);
        let mut extra = Vec::new();
        if reader.bot.is_some() && reader.roots.iter().any(|r| !r.write) {
            extra.push(
                "- /projects/<name> is each project's own memory, written by its sessions: read \
                 it, never change it. Keep what you learn about a project in \
                 /workspace/projects/<name>.md."
                    .to_string(),
            );
        }
        let today = chrono::Local::now().format("%A, %B %-d, %Y").to_string();
        section(&roots, &extra, &today)
    }

    /// The roots a prompt shows: all of them, but a workspace's read-only
    /// project stores only when the run works in that project (a work
    /// thread whose plan names it) or the store has content; the rest are
    /// a click away with `memory view /`.
    fn prompt_roots<'a>(&self, reader: &'a Reader) -> Vec<(&'a Root, Vec<MemoryFile>)> {
        let planned = self.planned_projects(reader);
        reader
            .roots
            .iter()
            .filter_map(|root| {
                let files = self.read_files(root).ok()?;
                let shown = root.write
                    || match &root.scope {
                        cypher_proto::MemoryScope::Project { space_id } => planned
                            .as_ref()
                            .map_or(!files.is_empty(), |ids| ids.contains(space_id)),
                        _ => true,
                    };
                shown.then_some((root, files))
            })
            .collect()
    }

    /// For a work thread: the projects its plan's steps name. `None` for
    /// every other reader.
    fn planned_projects(&self, reader: &Reader) -> Option<Vec<String>> {
        let marker = reader.chat.bot.as_ref()?;
        let cypher_proto::BotThread::Work { work_id } = &marker.thread else {
            return None;
        };
        let work = self
            .inner
            .workspace
            .record::<cypher_proto::BotWork>(cypher_proto::KIND_BOT_WORK, work_id)?;
        Some(
            work.steps
                .iter()
                .filter_map(|s| s.project.clone())
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::{MemoryAuthor, MemoryScope};

    fn file(path: &str, text: &str) -> MemoryFile {
        MemoryFile {
            path: path.into(),
            text: text.into(),
            author: MemoryAuthor::Owner,
            created_at: 0,
            updated_at: 0,
        }
    }

    const TODAY: &str = "Monday, November 9, 2026";

    fn root(path: &str, write: bool) -> Root {
        Root {
            path: path.into(),
            scope: MemoryScope::You,
            write,
            title: "your memory".into(),
        }
    }

    #[test]
    fn the_index_is_cut_at_its_line_budget_and_says_so() {
        let long: String = (0..250).map(|i| format!("- line {i}\n")).collect();
        let (shown, cut) = budgeted(&long, MEMORY_INDEX_LINES, MEMORY_INDEX_BYTES);
        assert!(cut);
        assert_eq!(shown.lines().count(), MEMORY_INDEX_LINES);
        let you = root("/you", true);
        let text = section(&[(&you, vec![file(MEMORY_INDEX, &long)])], &[], TODAY).unwrap();
        assert!(text.contains("MEMORY.md continues: 250 lines"), "{text}");
        assert!(text.contains("- line 199") && !text.contains("- line 200\n"));
    }

    #[test]
    fn stores_are_headed_listed_and_marked_read_only() {
        let you = root("/you", true);
        let project = root("/projects/api", false);
        let text = section(
            &[
                (&you, vec![file(MEMORY_INDEX, "- answer in English")]),
                (
                    &project,
                    vec![file(MEMORY_INDEX, "- run make"), file("/testing.md", "x")],
                ),
            ],
            &[],
            TODAY,
        )
        .unwrap();
        assert!(text.starts_with("<memory>\nYou have persistent memory"));
        assert!(text.contains("When the user asks you to remember something, save it now"));
        assert!(text.contains("- Today is Monday, November 9, 2026 (local time)"));
        assert!(text.contains("## /you — your memory\n- answer in English"));
        assert!(text.contains("## /projects/api — your memory (read-only)"));
        assert!(text.contains("Other files: /projects/api/testing.md\n"));
        assert!(text.ends_with("</memory>"));
        let empty = section(&[(&you, Vec::new())], &[], TODAY).unwrap();
        assert!(empty.contains("start one with `memory create /you/MEMORY.md`"));
        assert!(section(&[], &[], TODAY).is_none());
    }
}
