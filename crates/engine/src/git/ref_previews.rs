//! `ResolveGitRef`: what a pull request, issue or commit mentioned in a chat
//! reply is, answered on the chat's host device for its hover card.
//!
//! Commits come from the chat checkout's own git (no network); issue and pull
//! request numbers from GitHub with the device's usual credential
//! ([`Github::number_preview`], cached briefly). A bare `#123` or SHA means
//! the checkout's repository — its GitHub remote, its object database. Every
//! outcome that isn't a card is a quiet `notFound`/`unavailable` carrying the
//! page to open whenever one can be named.

use std::path::Path;
use std::time::Duration;

use cypher_proto::{GitCommitPreview, GitRef, GitRefResolution, GitRefUnavailable};

use super::github::{Github, github_repo};

/// One local git command of a lookup.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Resolve `git_ref` (already [`GitRef::is_valid`]) in the checkout at
/// `root`. `github` is `None` on an engine without GitHub.
pub async fn resolve(github: Option<&Github>, root: &Path, git_ref: &GitRef) -> GitRefResolution {
    let checkout_repo = github_repo(root).await;
    match git_ref {
        GitRef::Commit { repo, sha } => {
            resolve_commit(root, repo.as_deref(), sha, checkout_repo.as_deref()).await
        }
        GitRef::Issue { repo, number } => {
            let Some(repo) = repo.clone().or(checkout_repo) else {
                let reason = if is_checkout(root).await {
                    GitRefUnavailable::NoGithubRemote
                } else {
                    GitRefUnavailable::NoCheckout
                };
                return GitRefResolution::Unavailable { reason, url: None };
            };
            match github {
                Some(github) => github.number_preview(&repo, *number, Some(root)).await,
                None => GitRefResolution::Unavailable {
                    reason: GitRefUnavailable::SignedOut,
                    url: git_ref.web_url(Some(&repo)),
                },
            }
        }
    }
}

async fn resolve_commit(
    root: &Path,
    repo: Option<&str>,
    sha: &str,
    checkout_repo: Option<&str>,
) -> GitRefResolution {
    let named = GitRef::Commit {
        repo: repo.map(str::to_string),
        sha: sha.to_string(),
    };
    let url = named.web_url(checkout_repo);
    // A commit URL of a repository this checkout isn't.
    let foreign = repo.is_some_and(|repo| {
        checkout_repo.is_none_or(|checkout| !checkout.eq_ignore_ascii_case(repo))
    });
    match commit_preview(root, sha).await {
        CommitLookup::Found(mut commit) => {
            commit.url = GitRef::Commit {
                repo: repo.map(str::to_string),
                sha: commit.sha.clone(),
            }
            .web_url(checkout_repo);
            GitRefResolution::Commit(commit)
        }
        CommitLookup::Missing if !foreign => GitRefResolution::NotFound { url },
        CommitLookup::Missing => GitRefResolution::Unavailable {
            reason: GitRefUnavailable::OtherRepository,
            url,
        },
        CommitLookup::NoCheckout => GitRefResolution::Unavailable {
            reason: if foreign {
                GitRefUnavailable::OtherRepository
            } else {
                GitRefUnavailable::NoCheckout
            },
            url,
        },
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CommitLookup {
    Found(GitCommitPreview),
    /// No such commit (or an ambiguous abbreviation).
    Missing,
    /// `root` is no git checkout, or git failed in it.
    NoCheckout,
}

/// Run git under `root`, read-only and bounded; `Err(code)` on failure
/// (`None` = didn't run or timed out).
async fn git(root: &Path, args: &[&str]) -> Result<String, Option<i32>> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args([
        "-c",
        "log.showSignature=false",
        "-c",
        "core.quotePath=false",
    ])
    .args(args)
    .current_dir(root)
    .env("GIT_OPTIONAL_LOCKS", "0")
    .env("GIT_TERMINAL_PROMPT", "0")
    .stdin(std::process::Stdio::null())
    .kill_on_drop(true);
    let output = tokio::time::timeout(GIT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| None)?
        .map_err(|_| None)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(output.status.code())
    }
}

async fn is_checkout(root: &Path) -> bool {
    git(root, &["rev-parse", "--git-dir"]).await.is_ok()
}

/// The commit `sha` (7–40 lowercase hex, validated by the caller) names in
/// the checkout at `root`, `git show`-like: subject, author, time and the
/// diff stat against its first parent.
pub(crate) async fn commit_preview(root: &Path, sha: &str) -> CommitLookup {
    let checked = GitRef::Commit {
        repo: None,
        sha: sha.to_string(),
    };
    if !checked.is_valid() {
        return CommitLookup::Missing;
    }
    let rev = format!("{sha}^{{commit}}");
    let full = match git(root, &["rev-parse", "--verify", "--quiet", &rev]).await {
        Ok(full) => full.trim().to_string(),
        // `--verify --quiet` exits 1 for a missing or ambiguous object; 128
        // is "not a git repository" and the like.
        Err(Some(1)) => return CommitLookup::Missing,
        Err(_) => return CommitLookup::NoCheckout,
    };
    let shown = git(
        root,
        &[
            "log",
            "-1",
            "--no-color",
            "--root",
            "-m",
            "--first-parent",
            "--shortstat",
            "--format=%H%x00%h%x00%aN%x00%at%x00%s%x00",
            &full,
            "--",
        ],
    )
    .await;
    match shown {
        Ok(out) => parse_commit(&out).map_or(CommitLookup::NoCheckout, CommitLookup::Found),
        Err(_) => CommitLookup::NoCheckout,
    }
}

/// Parse `--format=%H%x00%h%x00%aN%x00%at%x00%s%x00` + `--shortstat` output.
pub(crate) fn parse_commit(out: &str) -> Option<GitCommitPreview> {
    let mut fields = out.splitn(6, '\0');
    let sha = fields.next()?.trim().to_string();
    let short_sha = fields.next()?.to_string();
    let author = fields.next()?.to_string();
    let author_time = fields.next()?.trim().parse().ok()?;
    let subject = fields.next()?.to_string();
    let (files_changed, insertions, deletions) = parse_shortstat(fields.next().unwrap_or_default());
    (!sha.is_empty()).then_some(GitCommitPreview {
        sha,
        short_sha,
        subject,
        author,
        author_time,
        files_changed,
        insertions,
        deletions,
        url: None,
    })
}

/// ` 3 files changed, 10 insertions(+), 2 deletions(-)`; any part may be
/// missing (an empty commit prints nothing).
fn parse_shortstat(text: &str) -> (u32, u32, u32) {
    let (mut files, mut insertions, mut deletions) = (0, 0, 0);
    let Some(line) = text.lines().find(|line| line.contains("changed")) else {
        return (0, 0, 0);
    };
    for part in line.split(',') {
        let part = part.trim();
        let Some((count, what)) = part.split_once(' ') else {
            continue;
        };
        let Ok(count) = count.parse::<u32>() else {
            continue;
        };
        if what.starts_with("file") {
            files = count;
        } else if what.starts_with("insertion") {
            insertions = count;
        } else if what.starts_with("deletion") {
            deletions = count;
        }
    }
    (files, insertions, deletions)
}

#[cfg(test)]
mod tests;
