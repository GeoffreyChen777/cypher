//! Worktrees: isolated chat checkouts, their deterministic names and branch
//! renames, and removal.

use super::*;

const ADJECTIVES: &[&str] = &[
    "swift", "calm", "bright", "bold", "keen", "brave", "clever", "lucky", "quiet", "warm", "cool",
    "sharp", "gentle", "vivid", "amber", "cobalt",
];
const NOUNS: &[&str] = &[
    "otter", "harbor", "falcon", "cedar", "meadow", "cypher", "delta", "ember", "lynx", "maple",
    "onyx", "quartz", "raven", "summit", "willow", "aspen",
];

impl Repos {
    /// `git worktree add` an isolated checkout under
    /// `{worktrees_root}/<repoName>/<generatedName>`, on a fresh `cypher/<name>`
    /// branch off `branch`.
    pub async fn create_worktree(
        &self,
        repo_path: &Path,
        branch: &str,
    ) -> Result<Worktree, EngineError> {
        let repo_name = repo_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "repo".to_string());
        let base = self.inner.worktrees_root.join(&repo_name);
        std::fs::create_dir_all(&base)?;
        // Auto-generate a name colliding with neither an existing dir nor branch.
        let existing: HashSet<String> = self
            .branches(repo_path)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut name = None;
        for attempt in 0..50u64 {
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64)
                .unwrap_or(attempt)
                .wrapping_add(attempt.wrapping_mul(0x9E37_79B9));
            let candidate = format!(
                "{}-{}",
                ADJECTIVES[(seed % ADJECTIVES.len() as u64) as usize],
                NOUNS[((seed / 31) % NOUNS.len() as u64) as usize]
            );
            if !base.join(&candidate).exists() && !existing.contains(&format!("cypher/{candidate}"))
            {
                name = Some(candidate);
                break;
            }
        }
        let name =
            name.ok_or_else(|| EngineError::Other("Could not allocate a worktree name".into()))?;
        let path = base.join(&name);
        let branch_name = format!("cypher/{name}");
        self.git(
            &[
                "worktree",
                "add",
                "-b",
                &branch_name,
                &path.to_string_lossy(),
                branch,
            ],
            Some(repo_path),
        )
        .await?;
        let checkout = self.checkout_identity(&path).await?;
        Ok(Worktree {
            repo_path: repo_path.to_string_lossy().to_string(),
            path: path.to_string_lossy().to_string(),
            branch: branch_name,
            name,
            checkout_id: Some(checkout.id),
        })
    }
}

impl Repos {
    async fn branch_exists(&self, path: &Path, branch: &str) -> bool {
        self.git(
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
            Some(path),
        )
        .await
        .is_ok()
    }
}

impl Repos {
    /// Rename a cypher-created worktree branch after its chat's generated title
    /// (port of zeron's `renameWorktreeBranch`). Guards:
    /// - respect an external checkout/rename: only act while the worktree is still
    ///   on `expected_branch` AND that branch is the original `cypher/<folderName>`;
    /// - a title-slug collision gets a stable 6-hex suffix (hash of the worktree
    ///   path); a collision on THAT too fails.
    ///
    /// Returns the branch the worktree ends up on (re-read after the rename so a
    /// concurrent external checkout always wins the metadata race).
    pub async fn rename_worktree_branch(
        &self,
        worktree_path: &Path,
        expected_branch: &str,
        title: &str,
    ) -> Result<String, EngineError> {
        let current = self.current_branch(worktree_path).await?;
        let folder = worktree_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let original = format!("cypher/{folder}");
        if current != expected_branch || expected_branch != original {
            return Ok(current);
        }
        let preferred = worktree_branch_from_title(title);
        if preferred == current {
            return Ok(current);
        }
        let mut hasher = Sha256::new();
        hasher.update(worktree_path.to_string_lossy().as_bytes());
        let suffix = &hex(&hasher.finalize())[..6];
        let target = if self.branch_exists(worktree_path, &preferred).await {
            format!("{preferred}-{suffix}")
        } else {
            preferred
        };
        if self.branch_exists(worktree_path, &target).await {
            return Err(EngineError::Other(format!(
                "Branch already exists: {target}"
            )));
        }
        self.git(
            &["branch", "-m", "--", &current, &target],
            Some(worktree_path),
        )
        .await?;
        self.current_branch(worktree_path).await
    }
}

impl Repos {
    /// Best-effort worktree removal (if it still exists), then prune stale refs.
    /// Deletes the worktree's branch ONLY when cypher created it (`cypher/…`) —
    /// the user may have checked out their own branch inside the worktree.
    pub async fn delete_worktree(
        &self,
        repo_path: &Path,
        worktree_path: &Path,
    ) -> Result<(), EngineError> {
        let branch = if worktree_path.exists() {
            self.current_branch(worktree_path).await.unwrap_or_default()
        } else {
            String::new()
        };
        if worktree_path.exists() {
            let removed = self
                .git(
                    &[
                        "worktree",
                        "remove",
                        "--force",
                        &worktree_path.to_string_lossy(),
                    ],
                    Some(repo_path),
                )
                .await;
            if removed.is_err() {
                // git refused (or the dir is half-gone) — delete the folder directly
                // (off the runtime workers: a worktree can be a large tree).
                let path = worktree_path.to_path_buf();
                let _ = crate::util::off_runtime(move || std::fs::remove_dir_all(path)).await;
            }
        }
        let _ = self.git(&["worktree", "prune"], Some(repo_path)).await;
        if branch.starts_with("cypher/") {
            let _ = self.git(&["branch", "-D", &branch], Some(repo_path)).await;
        }
        Ok(())
    }
}

impl Repos {
    /// Absolute paths of the repo's LINKED worktrees (excluding the main
    /// checkout), parsed from `git worktree list --porcelain`. Detached
    /// worktrees are included — unlike [`Self::refs`] this is a path probe,
    /// not a branch-indexed map.
    async fn linked_worktree_paths(&self, repo_path: &Path) -> Vec<String> {
        let Ok(out) = self
            .git(&["worktree", "list", "--porcelain"], Some(repo_path))
            .await
        else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        let mut stanza = 0usize;
        for line in out.lines().map(str::trim) {
            if let Some(p) = line.strip_prefix("worktree ") {
                stanza += 1;
                // The first stanza is the main checkout, not a linked tree.
                if stanza > 1 {
                    paths.push(p.to_string());
                }
            }
        }
        paths
    }
}

impl Repos {
    /// Create (or reuse) the deterministic chat worktree a durable Run's
    /// [`cypher_proto::WorktreeSpec`] asks for: a `cypher/<name>` branch off
    /// `base_ref` in an isolated checkout under
    /// `{worktrees_root}/<repo>/<name>`, named stably from `chat_key` so a
    /// retry or duplicate spec-carrying Run for the same chat resolves to the
    /// same path/branch instead of minting a second worktree.
    ///
    /// Safe against half-finished creations: an existing path that is NOT a
    /// linked worktree of `repo_path` (a crashed `worktree add`) is removed
    /// and recreated; an existing `cypher/<name>` branch (an admin record that
    /// survived a cleanup) is adopted rather than re-minted. An invalid
    /// `base_ref` fails here, which the caller surfaces as a Rejected command
    /// — never a silent fallback to the base checkout.
    pub async fn ensure_chat_worktree(
        &self,
        repo_path: &Path,
        base_ref: &str,
        chat_key: &str,
        name_hint: Option<&str>,
    ) -> Result<Worktree, EngineError> {
        let repo_name = repo_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "repo".to_string());
        let name = chat_worktree_name(chat_key, name_hint);
        let base = self.inner.worktrees_root.join(&repo_name);
        let path = base.join(&name);
        let branch_name = format!("cypher/{name}");

        // Reuse: the deterministic path is already a linked worktree of this
        // repo (a previous run, or the user kept it). Return it as-is with its
        // ACTUAL current branch (the user may have checked out their own).
        let canonical_target = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let linked = self.linked_worktree_paths(repo_path).await;
        if linked.iter().any(|p| {
            std::fs::canonicalize(Path::new(p))
                .map(|c| c == canonical_target)
                .unwrap_or(false)
        }) {
            let branch = self
                .current_branch(&path)
                .await
                .unwrap_or_else(|_| branch_name.clone());
            let checkout = self.checkout_identity(&path).await?;
            tracing::info!(path = %path.display(), branch = %branch, "chat worktree reused");
            return Ok(Worktree {
                repo_path: repo_path.to_string_lossy().to_string(),
                path: path.to_string_lossy().to_string(),
                branch,
                name,
                checkout_id: Some(checkout.id),
            });
        }

        // Semi-finished safety: a stale dir that is not a worktree of this
        // repo is a half-created checkout under OUR managed root — clear it
        // and start over. Prune dangling admin records too.
        if path.exists() {
            tracing::warn!(path = %path.display(), "removing stale half-created chat worktree");
            let stale = path.to_path_buf();
            crate::util::off_runtime(move || std::fs::remove_dir_all(stale))
                .await
                .map_err(EngineError::Other)??;
        }
        self.git(&["worktree", "prune"], Some(repo_path)).await?;
        std::fs::create_dir_all(&base)?;

        // Adopt an existing branch (an admin record that survived cleanup) or
        // mint a fresh one off the requested base ref. An invalid base ref
        // fails here — the caller Rejects the command rather than degrading.
        let result = if self.branch_exists(repo_path, &branch_name).await {
            self.git(
                &["worktree", "add", &path.to_string_lossy(), &branch_name],
                Some(repo_path),
            )
            .await
            .map(drop)
        } else {
            self.git(
                &[
                    "worktree",
                    "add",
                    "-b",
                    &branch_name,
                    &path.to_string_lossy(),
                    base_ref,
                ],
                Some(repo_path),
            )
            .await
            .map(drop)
        };
        if let Err(err) = result {
            // Do not leave an empty managed repo directory behind when
            // `worktree add` rejects the base ref. Only remove the directory
            // itself, never a base containing another chat's worktrees.
            let _ = std::fs::remove_dir(&base);
            return Err(err);
        }
        let checkout = self.checkout_identity(&path).await?;
        tracing::info!(
            path = %path.display(),
            branch = %branch_name,
            "chat worktree materialized"
        );
        Ok(Worktree {
            repo_path: repo_path.to_string_lossy().to_string(),
            path: path.to_string_lossy().to_string(),
            branch: branch_name,
            name,
            checkout_id: Some(checkout.id),
        })
    }
}

/// Turn a generated chat title into the semantic portion of a Cypher branch
/// (port of zeron's `worktreeBranchFromTitle`). Zeron NFKD-normalizes accented
/// letters first; native keeps it ASCII-only (generated titles are Title Case
/// English), so non-ASCII characters collapse into the `-` separator.
pub fn worktree_branch_from_title(title: &str) -> String {
    let mut slug = String::new();
    for c in title.trim().chars() {
        if matches!(c, '\'' | '"' | '`') {
            continue; // dropped entirely (cafe's → cafes), not a separator
        }
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(48);
    let slug = slug.trim_matches('-');
    format!("cypher/{}", if slug.is_empty() { "update" } else { slug })
}

/// Stable, deterministic worktree name for a chat: a slug of the chat key (or
/// the spec's optional name hint) plus an 8-hex hash of the chat key, so
/// retries and duplicate spec-carrying Runs for the same chat resolve to the
/// same path and `cypher/<name>` branch — never a second worktree.
pub fn chat_worktree_name(chat_key: &str, name_hint: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(chat_key.as_bytes());
    let hash = &hex(&hasher.finalize())[..8];
    let slug: String = name_hint
        .unwrap_or(chat_key)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(32)
        .collect::<String>();
    let slug = if slug.is_empty() { "chat" } else { &slug };
    format!("{slug}-{hash}")
}
