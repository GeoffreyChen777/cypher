//! The memory tools (docs/design/memory.md §3, §5): `memory` with the
//! commands of Claude's memory tool, plus `search_history` and
//! `read_history` over the past conversations the reader may read.

use serde::Deserialize;

use cypher_proto::{MemoryFile, valid_memory_path};

use super::access::{Reader, Root};
use super::doc::StoreDoc;
use super::{Memory, Planned, Written, size_label};
use crate::EngineError;

/// One answer, like the workspace tools'.
const ANSWER_MAX_CHARS: usize = 48_000;
/// Lines `search` returns, and per file.
const SEARCH_LINES_MAX: usize = 30;
const SEARCH_LINES_PER_FILE: usize = 5;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct MemoryArgs {
    command: String,
    path: Option<String>,
    file_text: Option<String>,
    old_str: Option<String>,
    new_str: Option<String>,
    insert_line: Option<i64>,
    insert_text: Option<String>,
    old_path: Option<String>,
    new_path: Option<String>,
    view_range: Option<Vec<i64>>,
    query: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SearchHistory {
    query: String,
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ReadHistory {
    session_id: String,
    from: Option<usize>,
    limit: Option<usize>,
}

fn args<T: serde::de::DeserializeOwned>(value: &serde_json::Value) -> Result<T, EngineError> {
    serde_json::from_value(value.clone())
        .map_err(|err| EngineError::Other(format!("Invalid arguments: {err}")))
}

fn err(text: impl Into<String>) -> EngineError {
    EngineError::Other(text.into())
}

/// A store path from what a model or the app wrote: `/` first, `.md`
/// last, refused when unusable.
pub(super) fn normalize_file(path: &str) -> Result<String, EngineError> {
    let mut path = path.trim().replace('\\', "/");
    if !path.starts_with('/') {
        path.insert(0, '/');
    }
    while path.contains("//") {
        path = path.replace("//", "/");
    }
    if !path.ends_with(".md") {
        path.push_str(".md");
    }
    if valid_memory_path(&path) {
        Ok(path)
    } else {
        Err(err(format!(
            "\"{path}\" isn't a usable memory path: use letters, digits and - _ . in folder and \
             file names, ending in .md (e.g. /testing.md, /decisions/rounding.md)."
        )))
    }
}

fn numbered(text: &str, first: usize) -> String {
    text.lines()
        .enumerate()
        .map(|(i, line)| format!("{:>6}\t{line}", i + first))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The plan for a write command, against the store as it is now.
pub(super) fn plan_command(
    store: &StoreDoc,
    command: &str,
    path: &str,
    raw: &serde_json::Value,
) -> Result<Planned, EngineError> {
    let a: MemoryArgs = args(raw)?;
    let file = |path: &str| store.file(path);
    match command {
        "create" => {
            let text = a
                .file_text
                .ok_or_else(|| err("create needs file_text: the whole file."))?;
            Ok(Planned {
                path: path.to_string(),
                text: Some(text),
                from: None,
            })
        }
        "str_replace" => {
            let old = a
                .old_str
                .filter(|s| !s.is_empty())
                .ok_or_else(|| err("str_replace needs old_str: text the file holds once."))?;
            let new = a.new_str.unwrap_or_default();
            let current =
                file(path).ok_or_else(|| err(format!("No file {path}; create makes one.")))?;
            let count = current.text.matches(old.as_str()).count();
            match count {
                0 => Err(err(format!(
                    "old_str isn't in {path}. View the file and copy the text exactly."
                ))),
                1 => Ok(Planned {
                    path: path.to_string(),
                    text: Some(current.text.replacen(old.as_str(), &new, 1)),
                    from: None,
                }),
                n => {
                    let lines: Vec<String> = current
                        .text
                        .lines()
                        .enumerate()
                        .filter(|(_, l)| l.contains(old.as_str()))
                        .map(|(i, _)| (i + 1).to_string())
                        .collect();
                    Err(err(format!(
                        "old_str occurs {n} times in {path} (lines {}); include more of the text \
                         around it so it occurs once.",
                        lines.join(", ")
                    )))
                }
            }
        }
        "insert" => {
            let text = a
                .insert_text
                .ok_or_else(|| err("insert needs insert_text."))?;
            let current = file(path).map(|f| f.text).unwrap_or_default();
            let mut lines: Vec<&str> = current.lines().collect();
            let at = a.insert_line.unwrap_or(lines.len() as i64);
            if at < 0 || at as usize > lines.len() {
                return Err(err(format!(
                    "insert_line {at} is outside {path}, which has {} lines (0 inserts at the top).",
                    lines.len()
                )));
            }
            let added: Vec<&str> = text.trim_end_matches('\n').lines().collect();
            lines.splice(at as usize..at as usize, added);
            let mut out = lines.join("\n");
            out.push('\n');
            Ok(Planned {
                path: path.to_string(),
                text: Some(out),
                from: None,
            })
        }
        "delete" => {
            file(path).ok_or_else(|| err(format!("No file {path}.")))?;
            Ok(Planned {
                path: path.to_string(),
                text: None,
                from: None,
            })
        }
        other => Err(err(format!("Unknown memory command: {other}"))),
    }
}

/// Move `from` to `to` (both store paths) within one store.
pub(super) fn plan_rename(store: &StoreDoc, from: &str, to: &str) -> Result<Planned, EngineError> {
    let file = store
        .file(from)
        .ok_or_else(|| err(format!("No file {from}.")))?;
    if from == to {
        return Err(err("That is its name already."));
    }
    if store.file(to).is_some() {
        return Err(err(format!(
            "{to} exists; delete it first, or pick another name."
        )));
    }
    Ok(Planned {
        path: to.to_string(),
        text: Some(file.text),
        from: Some(from.to_string()),
    })
}

/// How well `query` words match `text`: the share of them it holds,
/// whole phrase a little higher; `None` when none.
fn score(text: &str, words: &[String], phrase: &str) -> Option<f32> {
    let lower = text.to_lowercase();
    let hits = words.iter().filter(|w| lower.contains(w.as_str())).count();
    if hits == 0 {
        return None;
    }
    let mut score = hits as f32 / words.len() as f32;
    if words.len() > 1 && lower.contains(phrase) {
        score += 0.5;
    }
    Some(score)
}

/// One file a search found: its score, full path and best `(line, text)`.
type FileHit = (f32, String, Vec<(usize, String)>);

/// Search the files of `roots`: the best lines, grouped by file.
fn search(roots: &[(&Root, Vec<MemoryFile>)], query: &str) -> String {
    let words: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .filter(|w| w.chars().count() >= 2)
        .map(String::from)
        .collect();
    if words.is_empty() {
        return "Give search a few words to look for.".into();
    }
    let phrase = words.join(" ");
    let mut found: Vec<FileHit> = Vec::new();
    for (root, files) in roots {
        for file in files {
            let whole = format!("{} {}", file.path, file.text);
            let Some(file_score) = score(&whole, &words, &phrase) else {
                continue;
            };
            let mut lines: Vec<(f32, usize, String)> = file
                .text
                .lines()
                .enumerate()
                .filter_map(|(i, line)| score(line, &words, &phrase).map(|s| (s, i + 1, line)))
                .map(|(s, n, line)| (s, n, line.trim().chars().take(240).collect()))
                .collect();
            lines.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            lines.truncate(SEARCH_LINES_PER_FILE);
            lines.sort_by_key(|l| l.1);
            found.push((
                file_score,
                format!("{}{}", root.path, file.path),
                lines.into_iter().map(|(_, n, l)| (n, l)).collect(),
            ));
        }
    }
    if found.is_empty() {
        return format!("Nothing in memory matches \"{}\".", query.trim());
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut out = vec![format!("Memory matching \"{}\", best first:", query.trim())];
    let mut shown = 0;
    for (_, path, lines) in found {
        out.push(format!("- {path}"));
        for (n, line) in lines {
            if shown >= SEARCH_LINES_MAX {
                break;
            }
            out.push(format!("  {n}: {line}"));
            shown += 1;
        }
    }
    out.join("\n")
}

impl Memory {
    /// Answer one memory tool call from the chat `chat_id`.
    pub async fn tool(
        &self,
        chat_id: &str,
        tool: &str,
        arguments: serde_json::Value,
        tool_call_id: Option<String>,
    ) -> Result<String, EngineError> {
        let mut reader = self
            .reader(chat_id)
            .ok_or_else(|| err("Memory isn't available in this session."))?;
        reader.tool_call_id = tool_call_id;
        let answer = match tool {
            "memory" => self.memory_command(&reader, &arguments).await,
            "search_history" => {
                let a: SearchHistory = args(&arguments)?;
                let chats = self.history_chats(&reader.history);
                if chats.is_empty() {
                    return Ok(
                        "There are no past conversations to search here (this session has no \
                         project)."
                            .into(),
                    );
                }
                self.inner
                    .bots
                    .search_history(chats, &a.query, a.limit.unwrap_or(10).clamp(1, 30))
                    .await
            }
            "read_history" => {
                let a: ReadHistory = args(&arguments)?;
                let id = a.session_id.trim();
                let chat = self
                    .history_chats(&reader.history)
                    .into_iter()
                    .find(|c| c.id == id)
                    .ok_or_else(|| {
                        err(format!("No past conversation \"{id}\" that you can read."))
                    })?;
                self.inner.bots.read_history(chat, a.from, a.limit).await
            }
            other => Err(err(format!("Unknown memory tool: {other}"))),
        }?;
        Ok(clip(&answer))
    }

    async fn memory_command(
        &self,
        reader: &Reader,
        raw: &serde_json::Value,
    ) -> Result<String, EngineError> {
        let a: MemoryArgs = args(raw)?;
        match a.command.as_str() {
            "view" => self.view_command(reader, a.path.as_deref().unwrap_or("/"), a.view_range),
            "search" => {
                let query = a.query.unwrap_or_default();
                let scoped = a.path.as_deref().filter(|p| p.trim() != "/");
                let mut stores = Vec::new();
                for root in &reader.roots {
                    if let Some(path) = scoped
                        && reader.split(path).is_none_or(|(r, _)| r.path != root.path)
                    {
                        continue;
                    }
                    stores.push((root, self.read_files(root)?));
                }
                Ok(search(&stores, &query))
            }
            "rename" => {
                let old = a
                    .old_path
                    .as_deref()
                    .ok_or_else(|| err("rename needs old_path and new_path."))?;
                let new = a
                    .new_path
                    .as_deref()
                    .ok_or_else(|| err("rename needs old_path and new_path."))?;
                let (root, from) = self.locate(reader, old)?;
                let (to_root, to) = self.locate(reader, new)?;
                if to_root.path != root.path {
                    return Err(err("rename moves a file within one store only."));
                }
                let (from, to) = (normalize_file(&from)?, normalize_file(&to)?);
                let root = root.clone();
                self.written(
                    self.write(reader, &root, "rename", raw, |store| {
                        plan_rename(store, &from, &to)
                    })
                    .await?,
                )
            }
            command @ ("create" | "str_replace" | "insert" | "delete") => {
                let path = a
                    .path
                    .as_deref()
                    .ok_or_else(|| err(format!("{command} needs a path.")))?;
                let (root, rest) = self.locate(reader, path)?;
                let root = root.clone();
                if command == "delete" && !rest.ends_with(".md") {
                    return self.delete_folder(reader, &root, &rest, raw).await;
                }
                let path = normalize_file(&rest)?;
                let command = command.to_string();
                self.written(
                    self.write(reader, &root, &command, raw, |store| {
                        plan_command(store, &command, &path, raw)
                    })
                    .await?,
                )
            }
            "" => Err(err(
                "Give memory a command: view, create, str_replace, insert, delete, rename or search.",
            )),
            other => Err(err(format!("Unknown memory command: {other}"))),
        }
    }

    fn written(&self, written: Written) -> Result<String, EngineError> {
        Ok(match written {
            Written::Applied(text) | Written::Held(text) => text,
        })
    }

    /// The root and store path `path` names; an unknown root lists the
    /// ones there are.
    fn locate<'a>(
        &self,
        reader: &'a Reader,
        path: &str,
    ) -> Result<(&'a Root, String), EngineError> {
        reader.split(path).ok_or_else(|| {
            let roots: Vec<&str> = reader.roots.iter().map(|r| r.path.as_str()).collect();
            err(format!(
                "{path} isn't in your memory. Paths start with one of: {}.",
                roots.join(", ")
            ))
        })
    }

    pub(super) fn read_files(&self, root: &Root) -> Result<Vec<MemoryFile>, EngineError> {
        let handle = self.open(&self.store_id(&root.scope))?;
        Ok(StoreDoc::new(handle.doc().doc()).files_list())
    }

    fn view_command(
        &self,
        reader: &Reader,
        path: &str,
        range: Option<Vec<i64>>,
    ) -> Result<String, EngineError> {
        if path.trim() == "/" || path.trim().is_empty() {
            let mut out = vec!["Your memory:".to_string()];
            for root in &reader.roots {
                let files = self.read_files(root)?;
                let bytes: usize = files.iter().map(|f| f.text.len()).sum();
                out.push(format!(
                    "- {} — {} · {} file{}, {:.1} KB{}",
                    root.path,
                    root.title,
                    files.len(),
                    if files.len() == 1 { "" } else { "s" },
                    bytes as f64 / 1024.0,
                    if root.write { "" } else { " · read-only" }
                ));
            }
            return Ok(out.join("\n"));
        }
        let (root, rest) = self.locate(reader, path)?;
        let files = self.read_files(root)?;
        if !rest.ends_with(".md") {
            let folder = format!("{rest}/");
            let inside: Vec<&MemoryFile> = files
                .iter()
                .filter(|f| rest.is_empty() || f.path.starts_with(&folder))
                .collect();
            if inside.is_empty() {
                return Ok(if rest.is_empty() {
                    format!(
                        "{} is empty. Start it with create {}/MEMORY.md.",
                        root.path, root.path
                    )
                } else {
                    format!("Nothing in {}{rest}.", root.path)
                });
            }
            let mut out = vec![format!(
                "{}{rest} ({}{}):",
                root.path,
                root.title,
                if root.write { "" } else { ", read-only" }
            )];
            for file in inside {
                out.push(format!(
                    "- {}{} · {} · {} lines · by {}",
                    root.path,
                    file.path,
                    size_label(file),
                    file.text.lines().count(),
                    file.author.label()
                ));
            }
            return Ok(out.join("\n"));
        }
        let path = normalize_file(&rest)?;
        let file = files.iter().find(|f| f.path == path).ok_or_else(|| {
            err(format!(
                "No file {}{path}. view {} lists the files.",
                root.path, root.path
            ))
        })?;
        if file.text.is_empty() {
            return Ok(format!("{}{path} is empty.", root.path));
        }
        let lines: Vec<&str> = file.text.lines().collect();
        let (first, last) = match range.as_deref() {
            Some([a, b]) => {
                let first = (*a).max(1) as usize;
                let last = if *b < 0 {
                    lines.len()
                } else {
                    (*b as usize).min(lines.len())
                };
                if first > last {
                    return Err(err(format!(
                        "view_range [{a}, {b}] is outside the file's {} lines.",
                        lines.len()
                    )));
                }
                (first, last)
            }
            _ => (1, lines.len()),
        };
        let body = lines[first - 1..last].join("\n");
        Ok(format!(
            "{}{path} ({} lines{}):\n{}",
            root.path,
            lines.len(),
            if root.write { "" } else { ", read-only" },
            numbered(&body, first)
        ))
    }

    /// `delete` on a folder: every file in it, one write each.
    async fn delete_folder(
        &self,
        reader: &Reader,
        root: &Root,
        folder: &str,
        raw: &serde_json::Value,
    ) -> Result<String, EngineError> {
        if folder.is_empty() {
            return Err(err(format!(
                "Delete files, not the whole of {}; or say what is wrong in MEMORY.md.",
                root.path
            )));
        }
        let prefix = format!("{folder}/");
        let paths: Vec<String> = self
            .read_files(root)?
            .into_iter()
            .filter(|f| f.path.starts_with(&prefix))
            .map(|f| f.path)
            .collect();
        if paths.is_empty() {
            return Err(err(format!("Nothing in {}{folder}.", root.path)));
        }
        let mut last = String::new();
        for path in &paths {
            let written = self
                .write(reader, root, "delete", raw, |store| {
                    plan_command(store, "delete", path, raw)
                })
                .await?;
            last = self.written(written)?;
        }
        Ok(if paths.len() == 1 {
            last
        } else {
            format!("Deleted {} files in {}{folder}.", paths.len(), root.path)
        })
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= ANSWER_MAX_CHARS {
        return text.to_string();
    }
    let kept: String = text.chars().take(ANSWER_MAX_CHARS).collect();
    format!("{kept}… [cut; view a smaller range]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_proto::MemoryAuthor;
    use loro::LoroDoc;

    fn store_with(files: &[(&str, &str)]) -> LoroDoc {
        let doc = LoroDoc::new();
        let store = StoreDoc::new(&doc);
        for (path, text) in files {
            store.put(path, text, &MemoryAuthor::Owner, 1).unwrap();
        }
        store.commit();
        doc
    }

    #[test]
    fn paths_are_normalized_or_refused() {
        assert_eq!(normalize_file("testing").unwrap(), "/testing.md");
        assert_eq!(normalize_file("/a//b.md").unwrap(), "/a/b.md");
        assert_eq!(normalize_file("decisions\\x").unwrap(), "/decisions/x.md");
        assert!(normalize_file("/../x").is_err());
        assert!(normalize_file("/.secret").is_err());
    }

    #[test]
    fn str_replace_needs_exactly_one_match() {
        let doc = store_with(&[("/MEMORY.md", "- a\n- b\n- a\n")]);
        let store = StoreDoc::new(&doc);
        let replace = |old: &str| {
            plan_command(
                &store,
                "str_replace",
                "/MEMORY.md",
                &serde_json::json!({"command": "str_replace", "old_str": old, "new_str": "- c"}),
            )
        };
        let twice = replace("- a").unwrap_err().to_string();
        assert!(
            twice.contains("2 times") && twice.contains("lines 1, 3"),
            "{twice}"
        );
        assert!(replace("- z").is_err());
        let planned = replace("- b").unwrap();
        assert_eq!(planned.text.as_deref(), Some("- a\n- c\n- a\n"));
    }

    #[test]
    fn insert_goes_after_the_line_given() {
        let doc = store_with(&[("/MEMORY.md", "one\ntwo\n")]);
        let store = StoreDoc::new(&doc);
        let insert = |line: i64| {
            plan_command(
                &store,
                "insert",
                "/MEMORY.md",
                &serde_json::json!({"command": "insert", "insert_line": line, "insert_text": "new"}),
            )
        };
        assert_eq!(insert(0).unwrap().text.as_deref(), Some("new\none\ntwo\n"));
        assert_eq!(insert(2).unwrap().text.as_deref(), Some("one\ntwo\nnew\n"));
        assert!(insert(5).is_err());
    }

    #[test]
    fn rename_refuses_to_overwrite() {
        let doc = store_with(&[("/a.md", "A"), ("/b.md", "B")]);
        let store = StoreDoc::new(&doc);
        assert!(plan_rename(&store, "/a.md", "/b.md").is_err());
        let planned = plan_rename(&store, "/a.md", "/c.md").unwrap();
        assert_eq!(planned.from.as_deref(), Some("/a.md"));
        assert_eq!(planned.text.as_deref(), Some("A"));
    }

    #[test]
    fn search_ranks_files_and_lines() {
        let root = Root {
            path: "/project".into(),
            scope: cypher_proto::MemoryScope::You,
            write: true,
            title: String::new(),
        };
        let files = vec![
            MemoryFile {
                path: "/MEMORY.md".into(),
                text: "- Tests: run cargo test\n- Style: small PRs\n".into(),
                author: MemoryAuthor::Owner,
                created_at: 0,
                updated_at: 0,
            },
            MemoryFile {
                path: "/testing.md".into(),
                text: "Run cargo test -p cypher-ui before pushing.\nFlaky: bot_schedule.\n".into(),
                author: MemoryAuthor::Owner,
                created_at: 0,
                updated_at: 0,
            },
        ];
        let out = search(&[(&root, files)], "cargo test");
        assert!(out.starts_with("Memory matching \"cargo test\""), "{out}");
        assert!(out.contains("/project/testing.md"));
        assert!(out.contains("1: Run cargo test"));
        assert!(search(&[(&root, Vec::new())], "zzz").starts_with("Nothing in memory"));
    }

    #[test]
    fn views_number_their_lines() {
        assert_eq!(numbered("a\nb", 4), "     4\ta\n     5\tb");
    }
}
