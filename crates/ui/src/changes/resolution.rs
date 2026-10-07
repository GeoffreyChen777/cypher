//! Diff resolution against the selected chat, and the pane states (pure).

use super::*;

/// The diff shown for a chat: `checkout_id` match first, then device+cwd,
/// then cwd alone (§1.11).
pub fn resolve_diff<'a>(diffs: &'a [CheckoutDiff], chat: &Chat) -> Option<&'a CheckoutDiff> {
    if let Some(checkout_id) = chat.checkout_id.as_deref()
        && let Some(diff) = diffs.iter().find(|d| d.checkout_id == checkout_id)
    {
        return Some(diff);
    }
    let cwd = chat.cwd.as_deref()?;
    diffs
        .iter()
        .find(|d| d.device_id == chat.device_id && d.cwd == cwd)
        .or_else(|| diffs.iter().find(|d| d.cwd == cwd))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffPhase {
    /// No diff for this checkout yet.
    Preparing,
    /// Diff arrived and it's empty — working tree clean.
    Clean,
    List,
}

pub fn diff_phase(resolved: Option<&CheckoutDiff>) -> DiffPhase {
    match resolved {
        None => DiffPhase::Preparing,
        Some(diff) if diff.patch.trim().is_empty() && diff.files.is_empty() => DiffPhase::Clean,
        Some(_) => DiffPhase::List,
    }
}

/// Header label: "N Uncommitted change(s)".
pub fn uncommitted_label(count: usize) -> String {
    if count == 1 {
        "1 Uncommitted change".to_string()
    } else {
        format!("{count} Uncommitted changes")
    }
}

/// What the pane diffs against (t3code's scope dropdown).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffScope {
    /// Uncommitted changes vs HEAD — the live watch stream.
    #[default]
    WorkingTree,
    /// Everything this branch adds over `merge-base(base_ref, HEAD)`,
    /// working tree included.
    Branch,
    /// Changes since the current chat's last turn started.
    LatestTurn,
    /// Repository commit graph. Hosted here until the right pane becomes tabs.
    History,
    /// One commit's own changes (parent vs commit) — the per-commit tab a
    /// History row click opens. Never listed in the scope menu
    /// ([`Self::ALL`]); a commit-pinned pane is born this way and stays.
    Commit,
}

impl DiffScope {
    pub const ALL: [DiffScope; 4] = [
        Self::WorkingTree,
        Self::Branch,
        Self::LatestTurn,
        Self::History,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::WorkingTree => "Working tree",
            Self::Branch => "Branch changes",
            Self::LatestTurn => "Latest turn",
            Self::History => "History",
            Self::Commit => "Commit",
        }
    }

    /// Wire value for `GetCheckoutDiff` `mode` (and parse-key discriminant).
    pub fn mode(self) -> &'static str {
        match self {
            Self::WorkingTree => "workingTree",
            Self::Branch => "branch",
            Self::LatestTurn => "turn",
            Self::History => "history",
            Self::Commit => "commit",
        }
    }
}

/// Header-strip label per scope.
pub fn scope_label(scope: DiffScope, count: usize, base: Option<&str>) -> String {
    let files = if count == 1 { "file" } else { "files" };
    match scope {
        DiffScope::WorkingTree => uncommitted_label(count),
        DiffScope::Branch => match base {
            Some(base) => format!("{count} Changed {files} vs {base}"),
            None => format!("{count} Changed {files}"),
        },
        DiffScope::LatestTurn => format!("{count} Changed {files} this turn"),
        DiffScope::History => "History".to_string(),
        DiffScope::Commit => format!("{count} Changed {files} in this commit"),
    }
}

/// The comparison ref the branch scope preselects. `branches` comes from
/// `ListBranches` with the repo's default branch first — but a repo with no
/// `origin/HEAD` falls back to the *checked-out* branch there, and comparing a
/// branch with itself is useless; prefer `main`/`master` in that case.
pub fn default_base_ref(branches: &[String], current: Option<&str>) -> Option<String> {
    let first = branches.first()?;
    if current != Some(first.as_str()) {
        return Some(first.clone());
    }
    for candidate in ["main", "master"] {
        if branches.iter().any(|b| b == candidate) {
            return Some(candidate.to_string());
        }
    }
    branches
        .iter()
        .find(|b| current != Some(b.as_str()))
        .or(Some(first))
        .cloned()
}

/// Empty-state copy per scope.
pub fn clean_message(scope: DiffScope, base: Option<&str>) -> String {
    match scope {
        DiffScope::WorkingTree => "No uncommitted changes".to_string(),
        DiffScope::Branch => match base {
            Some(base) => format!("No changes vs {base}"),
            None => "No branch changes".to_string(),
        },
        DiffScope::LatestTurn => "No changes this turn".to_string(),
        DiffScope::History => "No commits found".to_string(),
        DiffScope::Commit => "Empty commit".to_string(),
    }
}

/// Fold a `WatchCheckoutDiffs` frame into the diff set. Accepts either a full
/// list (replace) or a single `CheckoutDiff` (upsert by checkout id) — the
/// contract streams `CheckoutDiff` items, but list frames cost nothing to
/// support. Returns whether anything changed.
pub fn apply_diff_frame(diffs: &mut Vec<CheckoutDiff>, value: serde_json::Value) -> bool {
    if let Ok(all) = serde_json::from_value::<Vec<CheckoutDiff>>(value.clone()) {
        if *diffs != all {
            *diffs = all;
            return true;
        }
        return false;
    }
    match serde_json::from_value::<CheckoutDiff>(value) {
        Ok(one) => {
            if let Some(existing) = diffs.iter_mut().find(|d| d.checkout_id == one.checkout_id) {
                if *existing == one {
                    return false;
                }
                *existing = one;
            } else {
                diffs.push(one);
            }
            true
        }
        Err(err) => {
            tracing::warn!(error = %err, "changes: dropping malformed diff frame");
            false
        }
    }
}

pub(super) fn hash64(parts: &[&str]) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    for p in parts {
        p.hash(&mut hasher);
    }
    hasher.finish()
}

const MAX_EXCERPT_SOURCE_LINES: usize = 200_000;

fn excerpt_side(
    file: &FileDiff,
    side: SourceSide,
    language: Lang,
    path: &str,
) -> Option<Arc<cypher_syntax::HighlightedDocument>> {
    let max_line = file
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter_map(|line| match side {
            SourceSide::Old => line.old_no,
            SourceSide::New => line.new_no,
        })
        .max()
        .unwrap_or(0) as usize;
    if max_line > MAX_EXCERPT_SOURCE_LINES {
        return None;
    }
    let mut lines = vec![Vec::new(); max_line];
    for hunk in &file.hunks {
        let visible = hunk
            .lines
            .iter()
            .filter_map(|line| {
                let number = match side {
                    SourceSide::Old => line.old_no,
                    SourceSide::New => line.new_no,
                }?;
                (line.kind != LineKind::Meta).then_some((number, line.text.as_str()))
            })
            .collect::<Vec<_>>();
        if visible.is_empty() {
            continue;
        }
        let source = visible
            .iter()
            .map(|(_, text)| *text)
            .collect::<Vec<_>>()
            .join("\n");
        let document = cypher_syntax::highlight(cypher_syntax::HighlightRequest {
            source: &source,
            path: Some(path),
            fence_tag: None,
        })
        .ok()?;
        for ((number, _), spans) in visible.into_iter().zip(document.lines) {
            lines[number as usize - 1] = spans;
        }
    }
    Some(Arc::new(cypher_syntax::HighlightedDocument {
        language,
        lines,
    }))
}

pub(super) fn excerpt_highlights(file: &FileDiff, language: Lang) -> Option<DiffHighlights> {
    if !cypher_syntax::supports_language(language) {
        return None;
    }
    let old = if file.status == FileStatus::Added {
        None
    } else {
        Some(excerpt_side(
            file,
            SourceSide::Old,
            language,
            file.old_path.as_deref().unwrap_or(&file.path),
        )?)
    };
    let new = if file.status == FileStatus::Deleted {
        None
    } else {
        Some(excerpt_side(file, SourceSide::New, language, &file.path)?)
    };
    Some(DiffHighlights { old, new })
}

pub(super) fn sources_match_patch(
    file: &FileDiff,
    response: &cypher_proto::CheckoutFileDiffText,
) -> bool {
    let old = response
        .old_text
        .as_deref()
        .map(|source| source.lines().collect::<Vec<_>>());
    let new = response
        .new_text
        .as_deref()
        .map(|source| source.lines().collect::<Vec<_>>());
    file.hunks.iter().flat_map(|hunk| &hunk.lines).all(|line| {
        let actual = match line.kind {
            LineKind::Del => line
                .old_no
                .and_then(|number| old.as_ref()?.get(number as usize - 1).copied()),
            LineKind::Add => line
                .new_no
                .and_then(|number| new.as_ref()?.get(number as usize - 1).copied()),
            LineKind::Context => line
                .new_no
                .and_then(|number| new.as_ref()?.get(number as usize - 1).copied())
                .or_else(|| {
                    line.old_no
                        .and_then(|number| old.as_ref()?.get(number as usize - 1).copied())
                }),
            LineKind::Meta => return true,
        };
        actual == Some(line.text.as_str())
    })
}

pub(super) fn full_highlights(
    file: &FileDiff,
    language: Lang,
    response: &cypher_proto::CheckoutFileDiffText,
) -> Option<DiffHighlights> {
    if response.stale
        || response.binary
        || response.truncated
        || !sources_match_patch(file, response)
    {
        return None;
    }
    let parse = |source: &str, path: &str| {
        cypher_syntax::highlight(cypher_syntax::HighlightRequest {
            source,
            path: Some(path),
            fence_tag: None,
        })
        .ok()
        .map(Arc::new)
    };
    let old = match response.old_text.as_deref() {
        Some(source) => Some(parse(
            source,
            file.old_path.as_deref().unwrap_or(&file.path),
        )?),
        None => None,
    };
    let new = match response.new_text.as_deref() {
        Some(source) => Some(parse(source, &file.path)?),
        None => None,
    };
    if old.is_none() && new.is_none() && cypher_syntax::supports_language(language) {
        return None;
    }
    Some(DiffHighlights { old, new })
}
