//! `project_git`: one granted space's branch, upstream, recent commits and
//! working-tree changes.
//!
//! A space on this device is read with git directly. A space on another
//! device is read through the relay-forwardable git RPCs that device already
//! serves (`ListRefs`, `ListGitHistory`, `GetCheckoutDiff`), under one
//! bounded timeout; that device must be online. Those RPCs have no
//! ahead/behind count, so a remote one is estimated from the history page's
//! commit graph and its remote-branch decorations.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use cypher_proto::{DiffFileSummary, GitHistoryPage, GitHistoryRefKind, RepoRef, Space};
use cypher_rpc::{LinkCache, RpcError, methods};

use super::Bots;
use super::prompt::space_name;
use super::tools::{View, when};
use crate::EngineError;
use crate::git::repos::Repos;
use crate::util::lock;

/// Commits listed.
const COMMITS: usize = 10;
/// Changed files listed by path.
const FILES: usize = 15;
/// The history page a remote estimate reads.
const REMOTE_HISTORY: usize = 200;
/// The whole remote read, dial included.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct ProjectGit {
    project: String,
}

/// What one read learned; every part is optional because a fresh repo has
/// no commits and a remote read can lose one RPC but not the others.
#[derive(Debug)]
pub(super) struct GitFacts {
    /// `None` when HEAD is detached.
    pub branch: Option<String>,
    pub head: Option<String>,
    pub upstream: Option<Upstream>,
    pub commits: Vec<Commit>,
    pub diff: Result<DiffStats, String>,
}

#[derive(Debug)]
pub(super) struct Upstream {
    pub name: String,
    pub ahead: usize,
    pub behind: usize,
    /// Counted from a bounded history page rather than by git itself.
    pub estimated: bool,
}

#[derive(Debug)]
pub(super) struct Commit {
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
pub(super) struct DiffStats {
    pub files: Vec<DiffFileSummary>,
    pub additions: u32,
    pub deletions: u32,
    pub truncated: bool,
}

impl Bots {
    pub(super) async fn project_git(
        &self,
        view: &View,
        request: ProjectGit,
    ) -> Result<String, EngineError> {
        let space = resolve_space(view, &request.project)?.clone();
        let device = view.device(&space.device_id).to_string();
        let facts = if space.device_id == self.device_id() {
            local_facts(&self.inner.repos, Path::new(&space.path))
                .await
                .map_err(|err| git_failure(&space, &device, &err.to_string()))?
        } else {
            let links = lock(&self.inner.links).clone();
            remote_facts(links, &space.device_id, &space.path)
                .await
                .map_err(|failure| match failure {
                    Failure::Offline => EngineError::Other(format!(
                        "{device} is offline, so its git state can't be read right now."
                    )),
                    Failure::Git(message) => git_failure(&space, &device, &message),
                })?
        };
        Ok(render(view, &space, &facts))
    }
}

/// Exactly one visible space; an unknown or out-of-scope one answers like
/// [`View::resolve_project`] does.
pub(super) fn resolve_space<'a>(view: &'a View, project: &str) -> Result<&'a Space, EngineError> {
    if project.trim().is_empty() {
        return Err(EngineError::Other(
            "Name a project for project_git (its id or name). Use list_projects.".into(),
        ));
    }
    let ids = view.resolve_project(Some(project))?.unwrap_or_default();
    let spaces: Vec<&Space> = ids.iter().filter_map(|id| view.space(Some(id))).collect();
    match spaces.as_slice() {
        [space] => Ok(space),
        _ => Err(EngineError::Other(format!(
            "\"{}\" names {} projects: {}. Pass one id.",
            project.trim(),
            spaces.len(),
            spaces
                .iter()
                .map(|s| format!("{} on {}", s.id, view.device(&s.device_id)))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn git_failure(space: &Space, device: &str, message: &str) -> EngineError {
    let message = message.strip_prefix("git: ").unwrap_or(message);
    if message.contains("not a git repository") {
        return EngineError::Other(format!(
            "{} on {device} is not a git repository.",
            space_name(space)
        ));
    }
    EngineError::Other(format!(
        "Couldn't read the git state of {} on {device}: {message}",
        space_name(space)
    ))
}

// ── this device ─────────────────────────────────────────────────────────────

async fn local_facts(repos: &Repos, path: &Path) -> Result<GitFacts, EngineError> {
    repos
        .git(&["rev-parse", "--is-inside-work-tree"], Some(path))
        .await?;
    let branch = repos
        .current_branch(path)
        .await
        .ok()
        .filter(|b| b != "HEAD");
    let head = repos
        .git(&["rev-parse", "--short", "HEAD"], Some(path))
        .await
        .ok()
        .filter(|s| !s.is_empty());
    let upstream = match repos
        .git(
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ],
            Some(path),
        )
        .await
    {
        Ok(name) if !name.is_empty() => repos
            .git(
                &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
                Some(path),
            )
            .await
            .ok()
            .and_then(|counts| {
                let (ahead, behind) = counts.split_once(char::is_whitespace)?;
                Some(Upstream {
                    name,
                    ahead: ahead.trim().parse().ok()?,
                    behind: behind.trim().parse().ok()?,
                    estimated: false,
                })
            }),
        _ => None,
    };
    let commits = if head.is_some() {
        let count = format!("--max-count={COMMITS}");
        let log = repos
            .git(
                &[
                    "log",
                    count.as_str(),
                    "--no-color",
                    "--no-show-signature",
                    "--format=%h%x00%s%x00%an%x00%aI%x00",
                ],
                Some(path),
            )
            .await
            .unwrap_or_default();
        parse_log(&log)
    } else {
        Vec::new()
    };
    let diff = match repos.checkout_identity(path).await {
        Ok(identity) => crate::git::diff_sync::capture_diff(repos, &identity.root)
            .await
            .map(|snapshot| DiffStats {
                files: snapshot.files,
                additions: snapshot.additions,
                deletions: snapshot.deletions,
                truncated: snapshot.truncated,
            })
            .map_err(|err| err.to_string()),
        Err(err) => Err(err.to_string()),
    };
    Ok(GitFacts {
        branch,
        head,
        upstream,
        commits,
        diff,
    })
}

fn parse_log(log: &str) -> Vec<Commit> {
    let fields: Vec<&str> = log.split('\0').collect();
    fields
        .chunks(4)
        .filter(|record| record.len() == 4)
        .filter_map(|record| {
            let sha = record[0].trim();
            (!sha.is_empty()).then(|| Commit {
                sha: sha.to_string(),
                subject: record[1].to_string(),
                author: record[2].to_string(),
                at: parse_time(record[3]),
            })
        })
        .collect()
}

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

// ── another device ──────────────────────────────────────────────────────────

pub(super) enum Failure {
    /// The device could not be reached (or answered too late).
    Offline,
    /// It answered with a git error.
    Git(String),
}

async fn remote_facts(
    links: Option<Arc<LinkCache>>,
    device_id: &str,
    path: &str,
) -> Result<GitFacts, Failure> {
    let links = links.ok_or(Failure::Offline)?;
    let read = async {
        let client = links.client(device_id).await.map_err(|err| {
            tracing::debug!(device = %device_id, error = %err, "bot git: device unreachable");
            Failure::Offline
        })?;
        let call = |method: &'static str, params: serde_json::Value| {
            let client = client.clone();
            async move { client.call(method, params).await }
        };
        let (refs, history, diff) = tokio::join!(
            call(
                methods::LIST_REFS,
                serde_json::json!({ "targetDeviceId": device_id, "repoPath": path }),
            ),
            call(
                methods::LIST_GIT_HISTORY,
                serde_json::json!({
                    "targetDeviceId": device_id, "cwd": path, "limit": REMOTE_HISTORY,
                }),
            ),
            call(
                methods::GET_CHECKOUT_DIFF,
                serde_json::json!({ "targetDeviceId": device_id, "cwd": path }),
            ),
        );
        Ok::<_, Failure>((refs, history, diff))
    };
    let (refs, history, diff) = tokio::time::timeout(REMOTE_TIMEOUT, read)
        .await
        .map_err(|_| Failure::Offline)??;
    let transport = |result: &Result<serde_json::Value, RpcError>| {
        matches!(result, Err(RpcError::Closed | RpcError::Transport(_)))
    };
    if transport(&refs) || transport(&history) {
        links.invalidate(device_id);
        return Err(Failure::Offline);
    }
    let history: GitHistoryPage = match history {
        Ok(value) => serde_json::from_value(value)
            .map_err(|err| Failure::Git(format!("unreadable history: {err}")))?,
        Err(err) => return Err(Failure::Git(err.to_string())),
    };
    let current = match refs
        .ok()
        .and_then(|value| serde_json::from_value::<Vec<RepoRef>>(value).ok())
    {
        Some(refs) => refs.into_iter().find(|r| r.current).map(|r| r.name),
        // No ref list (an older engine): the one local branch on HEAD.
        None => {
            let head = history.head_sha.as_deref();
            let mut on_head = history
                .commits
                .iter()
                .filter(|c| Some(c.sha.as_str()) == head)
                .flat_map(|c| c.refs.iter())
                .filter(|r| r.kind == GitHistoryRefKind::Branch);
            match (on_head.next(), on_head.next()) {
                (Some(only), None) => Some(only.label.clone()),
                _ => None,
            }
        }
    };
    let diff = match diff {
        Ok(value) => serde_json::from_value::<cypher_proto::CheckoutDiff>(value)
            .map(|diff| DiffStats {
                files: diff.files,
                additions: diff.additions,
                deletions: diff.deletions,
                truncated: diff.truncated,
            })
            .map_err(|err| err.to_string()),
        Err(err) => Err(err.to_string()),
    };
    Ok(facts_from_history(current, &history, diff))
}

/// Facts from a remote history page: HEAD's own commits come first in the
/// topological page, and the upstream is the `<remote>/<branch>` decoration
/// (`origin` preferred), counted against HEAD within the page.
pub(super) fn facts_from_history(
    branch: Option<String>,
    history: &GitHistoryPage,
    diff: Result<DiffStats, String>,
) -> GitFacts {
    let parents: HashMap<&str, &[String]> = history
        .commits
        .iter()
        .map(|c| (c.sha.as_str(), c.parent_shas.as_slice()))
        .collect();
    let reachable = |from: &str| {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack = vec![from];
        while let Some(sha) = stack.pop() {
            let Some((key, next)) = parents.get_key_value(sha) else {
                continue;
            };
            if seen.insert(key) {
                stack.extend(next.iter().map(String::as_str));
            }
        }
        seen
    };
    let head = history.head_sha.as_deref();
    let from_head = head.map(reachable).unwrap_or_default();
    let commits = history
        .commits
        .iter()
        .filter(|c| from_head.contains(c.sha.as_str()))
        .take(COMMITS)
        .map(|c| Commit {
            sha: c.sha.chars().take(7).collect(),
            subject: c.subject.clone(),
            author: c.author_name.clone(),
            at: parse_time(&c.authored_at),
        })
        .collect();
    let upstream = branch.as_deref().and_then(|branch| {
        let suffix = format!("/{branch}");
        let mut candidates: Vec<(&str, &str)> = history
            .commits
            .iter()
            .flat_map(|c| {
                c.refs
                    .iter()
                    .filter(|r| r.kind == GitHistoryRefKind::Remote && r.label.ends_with(&suffix))
                    .map(move |r| (r.label.as_str(), c.sha.as_str()))
            })
            .collect();
        candidates.sort_by_key(|(label, _)| !label.starts_with("origin/"));
        let (name, sha) = candidates.first()?;
        let upstream = reachable(sha);
        Some(Upstream {
            name: name.to_string(),
            ahead: from_head.difference(&upstream).count(),
            behind: upstream.difference(&from_head).count(),
            estimated: true,
        })
    });
    GitFacts {
        branch,
        head: head.map(|sha| sha.chars().take(7).collect()),
        upstream,
        commits,
        diff,
    }
}

// ── the answer ──────────────────────────────────────────────────────────────

pub(super) fn render(view: &View, space: &Space, facts: &GitFacts) -> String {
    let mut out = vec![format!(
        "Git state of {} (id {}) on {} · {}",
        space_name(space),
        space.id,
        view.device(&space.device_id),
        space.path
    )];
    let mut branch = match (&facts.branch, &facts.head) {
        (Some(branch), _) => format!("Branch: {branch}"),
        (None, Some(head)) => format!("Branch: none (detached HEAD at {head})"),
        (None, None) => "Branch: none".to_string(),
    };
    if facts.head.is_none() {
        branch.push_str(" · no commits yet");
    }
    match &facts.upstream {
        Some(up) => {
            let state = match (up.ahead, up.behind) {
                (0, 0) => format!("up to date with {}", up.name),
                (ahead, 0) => format!("{ahead} ahead of {}", up.name),
                (0, behind) => format!("{behind} behind {}", up.name),
                (ahead, behind) => format!("{ahead} ahead, {behind} behind {}", up.name),
            };
            branch.push_str(&format!(" · {state}"));
            if up.estimated {
                branch.push_str(&format!(
                    " (estimated from the last {REMOTE_HISTORY} commits)"
                ));
            }
        }
        None if facts.branch.is_some() && facts.head.is_some() => {
            branch.push_str(" · no upstream branch");
        }
        None => {}
    }
    out.push(branch);

    match &facts.diff {
        Ok(diff) if diff.files.is_empty() => out.push("Working tree: clean.".into()),
        Ok(diff) => {
            let added = diff.files.iter().filter(|f| f.status == "added").count();
            let deleted = diff.files.iter().filter(|f| f.status == "deleted").count();
            let mut kinds = Vec::new();
            if added > 0 {
                kinds.push(format!("{added} new"));
            }
            if deleted > 0 {
                kinds.push(format!("{deleted} deleted"));
            }
            out.push(format!(
                "Working tree: {} file(s) changed, +{} −{}{}{}:",
                diff.files.len(),
                diff.additions,
                diff.deletions,
                if kinds.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", kinds.join(", "))
                },
                if diff.truncated {
                    " (diff too large; counts are partial)"
                } else {
                    ""
                }
            ));
            for file in diff.files.iter().take(FILES) {
                let path = match &file.old_path {
                    Some(old) => format!("{old} → {}", file.path),
                    None => file.path.clone(),
                };
                out.push(if file.binary {
                    format!("  {} {path} (binary)", file.status)
                } else {
                    format!(
                        "  {} {path} +{} −{}",
                        file.status, file.additions, file.deletions
                    )
                });
            }
            if diff.files.len() > FILES {
                out.push(format!("  … and {} more", diff.files.len() - FILES));
            }
        }
        Err(err) => out.push(format!("Working tree: couldn't be read ({err}).")),
    }

    if !facts.commits.is_empty() {
        out.push(format!(
            "Recent commits on {}:",
            facts.branch.as_deref().unwrap_or("HEAD")
        ));
        for commit in &facts.commits {
            let at = commit
                .at
                .map(|at| format!(", {}", when(at, view.now)))
                .unwrap_or_default();
            out.push(format!(
                "- {} {} — {}{at}",
                commit.sha,
                commit.subject.chars().take(200).collect::<String>(),
                commit.author
            ));
        }
    }
    out.join("\n")
}
