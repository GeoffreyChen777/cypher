//! Hover-card lookups of one issue or pull request (`ResolveGitRef`), with
//! the device's usual GitHub credential and a short-lived cache, so hovering
//! the same mention again costs nothing.
//!
//! Every failure — no credential, no access, offline, slow — becomes a quiet
//! [`GitRefResolution::Unavailable`] that still carries the page to open: a
//! card is a nicety, never an error.

use super::*;
use cypher_proto::{
    GitRefResolution, GitRefUnavailable, GithubChecksState, GithubChecksSummary,
    GithubIssuePreview, GithubPullPreview, GithubRefState,
};

/// How long a looked-up issue or pull request is reused.
pub(super) const PREVIEW_TTL: Duration = Duration::from_secs(3 * 60);
/// Cached lookups per device.
pub(super) const PREVIEW_CACHE_CAP: usize = 256;
/// One whole lookup (credential, pull request, checks) by default.
pub(super) const PREVIEW_TIMEOUT: Duration = Duration::from_secs(5);
/// Check runs read per pull request (GitHub's page maximum).
const CHECK_RUNS_PAGE: u64 = 100;

/// A bounded map whose entries expire `ttl` after insertion. `now` is passed
/// in, so expiry is testable without sleeping.
pub(super) struct TtlCache<K, V> {
    ttl: Duration,
    cap: usize,
    entries: HashMap<K, (Instant, V)>,
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> TtlCache<K, V> {
    pub(super) fn new(ttl: Duration, cap: usize) -> Self {
        Self {
            ttl,
            cap: cap.max(1),
            entries: HashMap::new(),
        }
    }

    pub(super) fn get(&mut self, key: &K, now: Instant) -> Option<V> {
        match self.entries.get(key) {
            Some((at, value)) if now.saturating_duration_since(*at) < self.ttl => {
                Some(value.clone())
            }
            Some(_) => {
                self.entries.remove(key);
                None
            }
            None => None,
        }
    }

    /// Full: expired entries go first, then the oldest.
    pub(super) fn insert(&mut self, key: K, value: V, now: Instant) {
        if self.entries.len() >= self.cap && !self.entries.contains_key(&key) {
            let ttl = self.ttl;
            self.entries
                .retain(|_, (at, _)| now.saturating_duration_since(*at) < ttl);
            if self.entries.len() >= self.cap
                && let Some(oldest) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(key, _)| key.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(key, (now, value));
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Lookups keyed by lowercase `owner/name` and number.
pub(super) type PreviewCache = TtlCache<(String, u64), GitRefResolution>;

pub(super) fn preview_cache() -> PreviewCache {
    TtlCache::new(PREVIEW_TTL, PREVIEW_CACHE_CAP)
}

// ── GitHub's JSON ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct RestPull {
    pub(super) number: u64,
    pub(super) title: String,
    #[serde(default)]
    pub(super) state: String,
    #[serde(default)]
    pub(super) draft: bool,
    #[serde(default)]
    pub(super) merged: bool,
    #[serde(default)]
    pub(super) merged_at: Option<String>,
    #[serde(default)]
    pub(super) user: Option<RestUser>,
    pub(super) head: RestBranch,
    pub(super) base: RestBranch,
    #[serde(default)]
    pub(super) html_url: String,
    #[serde(default)]
    pub(super) changed_files: Option<u64>,
    #[serde(default)]
    pub(super) additions: Option<u64>,
    #[serde(default)]
    pub(super) deletions: Option<u64>,
}

#[derive(Deserialize)]
pub(super) struct RestBranch {
    #[serde(rename = "ref")]
    pub(super) name: String,
    /// `owner:branch`.
    #[serde(default)]
    pub(super) label: String,
    #[serde(default)]
    pub(super) sha: String,
    /// `None` once a fork is deleted.
    #[serde(default)]
    pub(super) repo: Option<RestRepo>,
}

#[derive(Deserialize)]
pub(super) struct RestRepo {
    pub(super) full_name: String,
}

#[derive(Deserialize, Default)]
pub(super) struct RestCheckRuns {
    #[serde(default)]
    pub(super) check_runs: Vec<RestCheckRun>,
}

#[derive(Deserialize)]
pub(super) struct RestCheckRun {
    #[serde(default)]
    pub(super) status: String,
    #[serde(default)]
    pub(super) conclusion: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct RestCombinedStatus {
    #[serde(default)]
    pub(super) statuses: Vec<RestStatus>,
}

#[derive(Deserialize)]
pub(super) struct RestStatus {
    #[serde(default)]
    pub(super) state: String,
}

// ── shaping ────────────────────────────────────────────────────────────────

pub(super) fn pull_preview(
    repo: &str,
    pull: RestPull,
    checks: Option<GithubChecksSummary>,
) -> GithubPullPreview {
    let state = if pull.merged || pull.merged_at.is_some() {
        GithubRefState::Merged
    } else if pull.state == "closed" {
        GithubRefState::Closed
    } else if pull.draft {
        GithubRefState::Draft
    } else {
        GithubRefState::Open
    };
    // A fork's branch (or a deleted fork's) is named with its owner.
    let same_repo = pull
        .head
        .repo
        .as_ref()
        .is_some_and(|head| head.full_name.eq_ignore_ascii_case(repo));
    let head_branch = if same_repo || pull.head.label.is_empty() {
        pull.head.name
    } else {
        pull.head.label
    };
    GithubPullPreview {
        repo: repo.to_string(),
        number: pull.number,
        title: pull.title,
        state,
        author: login(pull.user),
        head_branch,
        base_branch: pull.base.name,
        url: pull.html_url,
        checks,
        changed_files: pull.changed_files,
        additions: pull.additions,
        deletions: pull.deletions,
    }
}

pub(super) fn issue_preview(repo: &str, issue: RestIssue) -> GithubIssuePreview {
    GithubIssuePreview {
        repo: repo.to_string(),
        number: issue.number,
        state: if issue.state == "closed" {
            GithubRefState::Closed
        } else {
            GithubRefState::Open
        },
        state_reason: issue.state_reason,
        title: issue.title,
        author: login(issue.user),
        url: issue.html_url,
    }
}

/// Count check runs and commit statuses alike, the way GitHub's own rollup
/// does; `None` when the commit has neither.
pub(super) fn checks_summary(
    runs: &[RestCheckRun],
    statuses: &[RestStatus],
) -> Option<GithubChecksSummary> {
    let (mut passed, mut failed, mut pending, mut skipped) = (0u32, 0u32, 0u32, 0u32);
    for run in runs {
        let bucket = if run.status != "completed" {
            &mut pending
        } else {
            match run.conclusion.as_deref() {
                Some("success") => &mut passed,
                Some("neutral" | "skipped") => &mut skipped,
                Some(
                    "failure" | "timed_out" | "cancelled" | "action_required" | "startup_failure",
                ) => &mut failed,
                // `stale`, or a conclusion not yet set.
                _ => &mut pending,
            }
        };
        *bucket += 1;
    }
    for status in statuses {
        let bucket = match status.state.as_str() {
            "success" => &mut passed,
            "failure" | "error" => &mut failed,
            _ => &mut pending,
        };
        *bucket += 1;
    }
    if passed + failed + pending + skipped == 0 {
        return None;
    }
    let state = if failed > 0 {
        GithubChecksState::Failure
    } else if pending > 0 {
        GithubChecksState::Pending
    } else {
        GithubChecksState::Success
    };
    Some(GithubChecksSummary {
        state,
        passed,
        failed,
        pending,
        skipped,
    })
}

/// Which quiet reason a failed lookup reports.
fn unavailable_reason(err: &RepoError) -> GitRefUnavailable {
    match err {
        RepoError::Gh(GhError::Unavailable(GithubUnavailable::SignedOut)) => {
            GitRefUnavailable::SignedOut
        }
        RepoError::Gh(GhError::Unavailable(GithubUnavailable::NoAccess))
        | RepoError::Api(ApiError::Status(401 | 403)) => GitRefUnavailable::NoAccess,
        RepoError::Gh(GhError::Unavailable(GithubUnavailable::NoGithubRemote)) => {
            GitRefUnavailable::NoGithubRemote
        }
        RepoError::Gh(GhError::Failed(_)) | RepoError::Api(_) => GitRefUnavailable::Unreachable,
    }
}

/// Worth reusing: an answer from GitHub, not a failure to get one.
fn cacheable(resolution: &GitRefResolution) -> bool {
    matches!(
        resolution,
        GitRefResolution::PullRequest(_)
            | GitRefResolution::Issue(_)
            | GitRefResolution::NotFound { .. }
    )
}

impl Github {
    /// The card for issue or pull request `number` of `repo` (`owner/name`,
    /// already validated). `cwd`: the checkout, for git's credential helper.
    pub async fn number_preview(
        &self,
        repo: &str,
        number: u64,
        cwd: Option<&Path>,
    ) -> GitRefResolution {
        let key = (repo.to_ascii_lowercase(), number);
        if let Some(hit) = lock(&self.inner.previews).get(&key, Instant::now()) {
            return hit;
        }
        let url = format!("{}/{repo}/issues/{number}", self.inner.config.web_base);
        let lookup = self.fetch_number(repo, number, cwd, &url);
        let timeout = self.inner.config.preview_timeout;
        let resolution = match tokio::time::timeout(timeout, lookup).await {
            Ok(Ok(resolution)) => resolution,
            Ok(Err(err)) => {
                tracing::debug!(%repo, number, error = ?err, "github: ref preview unavailable");
                GitRefResolution::Unavailable {
                    reason: unavailable_reason(&err),
                    url: Some(url),
                }
            }
            Err(_) => GitRefResolution::Unavailable {
                reason: GitRefUnavailable::Unreachable,
                url: Some(url),
            },
        };
        if cacheable(&resolution) {
            lock(&self.inner.previews).insert(key, resolution.clone(), Instant::now());
        }
        resolution
    }

    /// The pull request, else the issue (one number space), else not found.
    async fn fetch_number(
        &self,
        repo: &str,
        number: u64,
        cwd: Option<&Path>,
        url: &str,
    ) -> Result<GitRefResolution, RepoError> {
        let pull = self
            .repo_get::<RestPull>(repo, cwd, &format!("/repos/{repo}/pulls/{number}"), &[])
            .await;
        match pull {
            Ok(pull) => {
                let checks = self.head_checks(repo, cwd, &pull.head.sha).await;
                Ok(GitRefResolution::PullRequest(pull_preview(
                    repo, pull, checks,
                )))
            }
            Err(RepoError::Api(ApiError::Status(404))) => {
                let issue = self
                    .repo_get::<RestIssue>(
                        repo,
                        cwd,
                        &format!("/repos/{repo}/issues/{number}"),
                        &[],
                    )
                    .await;
                match issue {
                    Ok(issue) => Ok(GitRefResolution::Issue(issue_preview(repo, issue))),
                    Err(RepoError::Api(ApiError::Status(404 | 410))) => {
                        Ok(GitRefResolution::NotFound {
                            url: Some(url.to_string()),
                        })
                    }
                    Err(err) => Err(err),
                }
            }
            Err(err) => Err(err),
        }
    }

    /// The head commit's check runs and statuses; a card without checks
    /// beats no card, so either request may fail on its own.
    async fn head_checks(
        &self,
        repo: &str,
        cwd: Option<&Path>,
        sha: &str,
    ) -> Option<GithubChecksSummary> {
        if sha.is_empty() || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let runs_path = format!("/repos/{repo}/commits/{sha}/check-runs");
        let runs_query = [("per_page", CHECK_RUNS_PAGE.to_string())];
        let status_path = format!("/repos/{repo}/commits/{sha}/status");
        let (runs, statuses) = tokio::join!(
            self.repo_get::<RestCheckRuns>(repo, cwd, &runs_path, &runs_query),
            self.repo_get::<RestCombinedStatus>(repo, cwd, &status_path, &[]),
        );
        if runs.is_err() && statuses.is_err() {
            return None;
        }
        checks_summary(
            &runs.unwrap_or_default().check_runs,
            &statuses.unwrap_or_default().statuses,
        )
    }
}
