//! Artifact patches: a fence tagged `artifact-patch`
//! (```` ```html artifact-patch title="…" ````) edits a named page's latest
//! version in place instead of writing it again. Its body is SEARCH/REPLACE
//! hunks, applied in order:
//!
//! ```text
//! <<<<<<< SEARCH
//! lines copied from the page
//! =======
//! the lines that replace them
//! >>>>>>> REPLACE
//! ```
//!
//! A hunk applies where its SEARCH text matches exactly one place: verbatim
//! first, then line by line ignoring each line's surrounding whitespace (the
//! drift a model copying indentation makes). A hunk that matches nowhere or
//! in several places is skipped; the rest still apply.
//!
//! The iPhone app mirrors these rules (`apps/ios/Cypher/Markdown/ArtifactPatch.swift`);
//! both run `protocol/vectors/artifact-patch-v1.json`.

/// One SEARCH/REPLACE pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub search: String,
    pub replace: String,
}

/// A patch applied to a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub code: String,
    pub applied: usize,
    pub total: usize,
}

/// The patch's complete hunks; one still streaming (no REPLACE marker yet)
/// is left out.
pub fn hunks(patch: &str) -> Vec<Hunk> {
    enum State {
        Outside,
        Search(Vec<String>),
        Replace(Vec<String>, Vec<String>),
    }
    let mut hunks = Vec::new();
    let mut state = State::Outside;
    for line in patch.lines() {
        let marker = line.trim();
        state = match state {
            State::Outside if is_search(marker) => State::Search(Vec::new()),
            State::Outside => State::Outside,
            State::Search(search) if is_divider(marker) => State::Replace(search, Vec::new()),
            State::Search(mut search) => {
                search.push(line.to_string());
                State::Search(search)
            }
            State::Replace(search, replace) if is_replace(marker) => {
                hunks.push(Hunk {
                    search: search.join("\n"),
                    replace: replace.join("\n"),
                });
                State::Outside
            }
            State::Replace(search, mut replace) => {
                replace.push(line.to_string());
                State::Replace(search, replace)
            }
        };
    }
    hunks
}

fn is_search(marker: &str) -> bool {
    marker.starts_with("<<<<<<<") && marker.trim_start_matches('<').trim() == "SEARCH"
}

fn is_divider(marker: &str) -> bool {
    marker.len() >= 7 && marker.bytes().all(|b| b == b'=')
}

fn is_replace(marker: &str) -> bool {
    marker.starts_with(">>>>>>>") && marker.trim_start_matches('>').trim() == "REPLACE"
}

/// `base` with `patch`'s hunks applied in order, and how many took.
pub fn apply(base: &str, patch: &str) -> Applied {
    let hunks = hunks(patch);
    let mut code = base.to_string();
    let mut applied = 0;
    for hunk in &hunks {
        if let Some(next) = apply_hunk(&code, hunk) {
            code = next;
            applied += 1;
        }
    }
    Applied {
        code,
        applied,
        total: hunks.len(),
    }
}

fn apply_hunk(code: &str, hunk: &Hunk) -> Option<String> {
    if hunk.search.trim().is_empty() {
        return None;
    }
    let mut exact = code.match_indices(hunk.search.as_str());
    if let Some((at, _)) = exact.next() {
        if exact.next().is_some() {
            return None;
        }
        let end = at + hunk.search.len();
        return Some(format!("{}{}{}", &code[..at], hunk.replace, &code[end..]));
    }
    apply_by_lines(code, hunk)
}

/// The fallback: SEARCH's lines against the page's, each compared trimmed,
/// matching one run of whole lines.
fn apply_by_lines(code: &str, hunk: &Hunk) -> Option<String> {
    let search: Vec<&str> = hunk.search.lines().map(str::trim).collect();
    let lines: Vec<&str> = code.split_inclusive('\n').collect();
    if search.len() > lines.len() {
        return None;
    }
    let mut found = None;
    for start in 0..=lines.len() - search.len() {
        let matches = search
            .iter()
            .zip(&lines[start..])
            .all(|(want, line)| line.trim() == *want);
        if matches {
            if found.is_some() {
                return None;
            }
            found = Some(start);
        }
    }
    let start = found?;
    let end = start + search.len();
    let mut out: String = lines[..start].concat();
    out.push_str(&hunk.replace);
    if lines[end - 1].ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&lines[end..].concat());
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_vectors() {
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../protocol/vectors/artifact-patch-v1.json"
        ))
        .unwrap();
        for v in vectors["apply"].as_array().unwrap() {
            let name = v["name"].as_str().unwrap();
            let applied = apply(v["base"].as_str().unwrap(), v["patch"].as_str().unwrap());
            assert_eq!(applied.code, v["code"].as_str().unwrap(), "{name}");
            assert_eq!(applied.applied as u64, v["applied"], "{name}");
            assert_eq!(applied.total as u64, v["total"], "{name}");
        }
    }

    const PAGE: &str = "<h1>Explainer</h1>\n<ul>\n  <li>parser</li>\n  <li>index</li>\n</ul>\n";

    fn patch(hunks: &[(&str, &str)]) -> String {
        hunks
            .iter()
            .map(|(search, replace)| {
                format!("<<<<<<< SEARCH\n{search}\n=======\n{replace}\n>>>>>>> REPLACE\n")
            })
            .collect()
    }

    #[test]
    fn hunks_apply_in_order() {
        let applied = apply(
            PAGE,
            &patch(&[
                ("  <li>index</li>", "  <li>index</li>\n  <li>patches</li>"),
                ("<li>patches</li>", "<li>patches, folded</li>"),
            ]),
        );
        assert_eq!((applied.applied, applied.total), (2, 2));
        assert_eq!(
            applied.code,
            "<h1>Explainer</h1>\n<ul>\n  <li>parser</li>\n  <li>index</li>\n  <li>patches, folded</li>\n</ul>\n"
        );
    }

    #[test]
    fn indentation_drift_still_matches_whole_lines() {
        let applied = apply(
            PAGE,
            &patch(&[("<ul>\n<li>parser</li>", "<ol>\n  <li>parser</li>")]),
        );
        assert_eq!(applied.applied, 1);
        assert_eq!(
            applied.code,
            "<h1>Explainer</h1>\n<ol>\n  <li>parser</li>\n  <li>index</li>\n</ul>\n"
        );
    }

    #[test]
    fn missing_or_ambiguous_hunks_are_skipped_and_the_rest_apply() {
        let applied = apply(
            PAGE,
            &patch(&[
                ("<li>nowhere</li>", "x"),
                ("li>", "ambiguous"),
                ("", "empty"),
                ("<h1>Explainer</h1>", "<h1>Session</h1>"),
            ]),
        );
        assert_eq!((applied.applied, applied.total), (1, 4));
        assert!(applied.code.starts_with("<h1>Session</h1>\n<ul>"));
    }

    #[test]
    fn a_streaming_hunk_is_not_a_hunk_yet() {
        let streaming = "<<<<<<< SEARCH\n<h1>Explainer</h1>\n=======\n<h1>Ses";
        assert!(hunks(streaming).is_empty());
        let one_done = format!("{}{streaming}", patch(&[("a", "b")]));
        assert_eq!(hunks(&one_done).len(), 1);
        // Text around the hunks is ignored; an empty REPLACE deletes.
        let applied = apply(
            PAGE,
            &format!("note\n{}", patch(&[("  <li>index</li>\n", "")])),
        );
        assert_eq!(applied.applied, 1);
        assert_eq!(
            applied.code,
            "<h1>Explainer</h1>\n<ul>\n  <li>parser</li>\n</ul>\n"
        );
    }
}
