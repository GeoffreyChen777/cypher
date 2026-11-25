//! Pull request, issue and commit references in chat text, and the wire
//! shapes of `ResolveGitRef`, which turns one into a hover card.
//!
//! [`find_git_refs`] is the one detector every client runs, so a mention
//! lights up the same way everywhere: the desktop calls it, the iPhone app
//! mirrors it, and both run every case of
//! `protocol/vectors/git-refs-v1.json` (`protocol/README.md`). Change the
//! rules, their mirror and the vectors together.
//!
//! # What counts as a reference
//!
//! Prose (`in_code == false`), scanned left to right, matches never overlap:
//!
//! - **GitHub URLs** — `http(s)://github.com/<owner>/<repo>/pull/<n>`,
//!   `…/issues/<n>` (an [`GitRef::Issue`] of that repository) and
//!   `…/commit/<sha>` or `…/pull/<n>/commits/<sha>` (a [`GitRef::Commit`]),
//!   with any trailing path, query or fragment. The match is the whole URL.
//!   A URL ends at whitespace, a non-ASCII character or one of ``<>"'` ``;
//!   trailing `.,;:!?*_~` and unbalanced closing brackets are left out of
//!   it. Every other URL is opaque: nothing inside it matches (so a
//!   `…/page#123` fragment is no issue).
//! - **`owner/repo#<n>`** — an issue or pull request of that repository.
//!   `owner` is GitHub's `[A-Za-z0-9-]`, `repo` `[A-Za-z0-9._-]`, and the
//!   owner must not continue a longer path (`a/b/c#1` is nothing).
//! - **`#<n>`** — an issue or pull request of the chat's repository. The `#`
//!   must not follow an ASCII letter, digit, `_`, `#`, `&`, `/`, `\` or `@`
//!   (`C#`, `F#7`, `##1`, `&#123;` are not refs; CJK text right before it is
//!   fine). `<n>` is 1–9 digits without a leading zero, and must not run on
//!   into a word (`#12abc`, `#1.5`, `#12-3` are not refs).
//! - **Commit SHAs** — a whole ASCII word (a maximal run of letters, digits,
//!   `_` and `-`) of 7–40 *lowercase* hex characters with at least one digit
//!   AND at least one letter `a`–`f`. So `deadbeef`, `1234567`, `ABC1234`,
//!   UUIDs (`550e8400-e29b-…` is one word with dashes), `0x1f2e3d4c`, 64-char
//!   digests and hex colours (`#a1b2c3d4`) are not SHAs. The word also must
//!   not be part of a path, address or file name: no `/`, `\` or `@` on
//!   either side, no `#`, `$`, `%` or `&` before it, and no `.`/`:` joining it
//!   to another word (`a1b2c3d.png`, `key:a1b2c3d`). A range
//!   `a1b2c3d..e4f5a6b` is two SHAs.
//!
//! Inline code (`in_code == true`) is all or nothing: the span, trimmed of
//! whitespace, must be exactly one GitHub URL, `#<n>` or `owner/repo#<n>` as
//! above, or a 7–40 char lowercase hex SHA — looser than prose, it may be all
//! letters (`deadbeef`), but not all digits. `git show a1b2c3d` in code is
//! nothing.
//!
//! # Context
//!
//! A bare `#123` or SHA ([`GitRef::repo`] is `None`) only means something in
//! the chat's git checkout: clients run detection only for a chat that has
//! one, and the host resolves those refs against the checkout (its GitHub
//! remote, its object database). Qualified refs and URLs name their
//! repository themselves.

use std::ops::Range;

use serde::{Deserialize, Serialize};

/// Shortest and longest commit abbreviation recognised.
pub const MIN_SHA_LEN: usize = 7;
pub const MAX_SHA_LEN: usize = 40;
/// Longest issue number recognised, in digits.
pub const MAX_NUMBER_DIGITS: usize = 9;

const GITHUB_WEB: &str = "https://github.com";

/// One reference found in chat text; also the `ref` of `ResolveGitRef`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum GitRef {
    /// An issue or pull request number (GitHub numbers both from one
    /// sequence; resolving tells them apart).
    #[serde(rename_all = "camelCase")]
    Issue {
        /// `owner/name`; `None` = the chat checkout's GitHub repository.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
        number: u64,
    },
    /// A commit, full or abbreviated, lowercase.
    #[serde(rename_all = "camelCase")]
    Commit {
        /// `owner/name` from a commit URL; `None` = the chat's checkout.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
        sha: String,
    },
}

impl GitRef {
    /// The repository the ref names itself, if any.
    pub fn repo(&self) -> Option<&str> {
        match self {
            GitRef::Issue { repo, .. } | GitRef::Commit { repo, .. } => repo.as_deref(),
        }
    }

    /// Well-formed enough to put in a URL or a git argument: a valid
    /// `owner/name`, a number of at most [`MAX_NUMBER_DIGITS`] digits, or a
    /// 7–40 char lowercase hex SHA.
    pub fn is_valid(&self) -> bool {
        let repo_ok = self.repo().is_none_or(valid_repo);
        repo_ok
            && match self {
                GitRef::Issue { number, .. } => {
                    *number > 0 && *number < 10u64.pow(MAX_NUMBER_DIGITS as u32)
                }
                GitRef::Commit { sha, .. } => is_sha_like(sha) && !has_upper(sha),
            }
    }

    /// The github.com page for the ref, given the chat checkout's GitHub
    /// repository for an unqualified one. `/issues/<n>` serves pull requests
    /// too (GitHub redirects).
    pub fn web_url(&self, checkout_repo: Option<&str>) -> Option<String> {
        let repo = self.repo().or(checkout_repo)?;
        Some(match self {
            GitRef::Issue { number, .. } => format!("{GITHUB_WEB}/{repo}/issues/{number}"),
            GitRef::Commit { sha, .. } => format!("{GITHUB_WEB}/{repo}/commit/{sha}"),
        })
    }
}

/// A reference and the byte range of `text` it was found at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRefMatch {
    pub range: Range<usize>,
    pub git_ref: GitRef,
}

/// Every reference in `text`, in order (see the module docs for the rules).
/// `in_code`: `text` is one inline code span.
pub fn find_git_refs(text: &str, in_code: bool) -> Vec<GitRefMatch> {
    if in_code {
        return code_ref(text).into_iter().collect();
    }
    let bytes = text.as_bytes();
    let urls = url_spans(text);
    let in_url = |at: usize| urls.iter().any(|url| url.contains(&at));
    let mut found: Vec<GitRefMatch> = urls
        .iter()
        .filter_map(|url| {
            github_url_ref(&text[url.clone()]).map(|git_ref| GitRefMatch {
                range: url.clone(),
                git_ref,
            })
        })
        .collect();
    for (at, _) in text.match_indices('#') {
        if !in_url(at)
            && let Some(found_ref) = number_ref_at(bytes, at)
        {
            found.push(found_ref);
        }
    }
    let mut at = 0;
    while at < bytes.len() {
        if !is_word_byte(bytes[at]) {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && is_word_byte(bytes[at]) {
            at += 1;
        }
        if !in_url(start)
            && let Some(sha) = prose_sha(bytes, start, at)
        {
            found.push(GitRefMatch {
                range: start..at,
                git_ref: GitRef::Commit { repo: None, sha },
            });
        }
    }
    found.sort_by_key(|found| found.range.start);
    found
}

// ── inline code ────────────────────────────────────────────────────────────

fn code_ref(text: &str) -> Option<GitRefMatch> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
        return None;
    }
    let start = text.len() - text.trim_start().len();
    let range = start..start + trimmed.len();
    let git_ref = if has_scheme(trimmed.as_bytes(), 0) {
        // The whole span must be the URL (no trailing punctuation cut).
        url_spans(trimmed)
            .first()
            .filter(|url| url.len() == trimmed.len())
            .and_then(|_| github_url_ref(trimmed))?
    } else if trimmed.contains('#') {
        let hash = trimmed.rfind('#')?;
        number_ref_at(trimmed.as_bytes(), hash)
            .filter(|found| found.range == (0..trimmed.len()))?
            .git_ref
    } else if is_sha_like(trimmed)
        && !has_upper(trimmed)
        && !trimmed.bytes().all(|b| b.is_ascii_digit())
    {
        GitRef::Commit {
            repo: None,
            sha: trimmed.to_string(),
        }
    } else {
        return None;
    };
    Some(GitRefMatch { range, git_ref })
}

// ── URLs ───────────────────────────────────────────────────────────────────

fn has_scheme(bytes: &[u8], at: usize) -> bool {
    let rest = &bytes[at..];
    let starts = |prefix: &[u8]| {
        rest.len() >= prefix.len() && rest[..prefix.len()].eq_ignore_ascii_case(prefix)
    };
    starts(b"https://") || starts(b"http://")
}

/// Byte ranges of the `http(s)://` URLs in `text`.
fn url_spans(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let boundary = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        if !(boundary && has_scheme(bytes, at)) {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && is_url_byte(bytes[at]) {
            at += 1;
        }
        let mut end = at;
        while let Some(&last) = bytes[start..end].last() {
            let unbalanced = |open: u8| {
                let span = &bytes[start..end];
                span.iter().filter(|&&b| b == last).count()
                    > span.iter().filter(|&&b| b == open).count()
            };
            let trim = match last {
                b'.' | b',' | b';' | b':' | b'!' | b'?' | b'*' | b'_' | b'~' => true,
                b')' => unbalanced(b'('),
                b']' => unbalanced(b'['),
                b'}' => unbalanced(b'{'),
                _ => false,
            };
            if !trim {
                break;
            }
            end -= 1;
        }
        spans.push(start..end);
    }
    spans
}

fn is_url_byte(b: u8) -> bool {
    b.is_ascii_graphic() && !matches!(b, b'<' | b'>' | b'"' | b'\'' | b'`')
}

/// The ref a github.com URL names, if it names one.
fn github_url_ref(url: &str) -> Option<GitRef> {
    let rest = url.split_once("://")?.1;
    let (host, path) = rest.split_once('/')?;
    if !matches!(
        host.to_ascii_lowercase().as_str(),
        "github.com" | "www.github.com"
    ) {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    let repo = format!("{owner}/{name}");
    if !valid_repo(&repo) {
        return None;
    }
    let kind = parts.next()?;
    let id = parts.next()?;
    match kind {
        "pull" if parts.next() == Some("commits") => {
            parse_number(id)?;
            let sha = parts.next()?;
            is_sha_like(sha).then(|| GitRef::Commit {
                repo: Some(repo),
                sha: sha.to_ascii_lowercase(),
            })
        }
        "pull" | "issues" => Some(GitRef::Issue {
            repo: Some(repo),
            number: parse_number(id)?,
        }),
        "commit" => is_sha_like(id).then(|| GitRef::Commit {
            repo: Some(repo),
            sha: id.to_ascii_lowercase(),
        }),
        _ => None,
    }
}

// ── numbers ────────────────────────────────────────────────────────────────

fn parse_number(digits: &str) -> Option<u64> {
    (!digits.is_empty()
        && digits.len() <= MAX_NUMBER_DIGITS
        && !digits.starts_with('0')
        && digits.bytes().all(|b| b.is_ascii_digit()))
    .then(|| digits.parse().ok())
    .flatten()
}

/// `#<n>` or `owner/repo#<n>` with its `#` at `hash`.
fn number_ref_at(bytes: &[u8], hash: usize) -> Option<GitRefMatch> {
    let digits_end = hash
        + 1
        + bytes[hash + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
    let number = parse_number(std::str::from_utf8(&bytes[hash + 1..digits_end]).ok()?)?;
    // The number must not run on into a word: `#12abc`, `#1.5`, `#12-3`.
    match bytes.get(digits_end) {
        Some(b) if is_word_byte(*b) && *b != b'-' => return None,
        Some(b'.' | b'-')
            if bytes
                .get(digits_end + 1)
                .is_some_and(u8::is_ascii_alphanumeric) =>
        {
            return None;
        }
        _ => {}
    }
    let before = hash.checked_sub(1).map(|at| bytes[at]);
    let (start, repo) = match before {
        None => (hash, None),
        // Only a repository may run up to the `#` (`C#`, `x_#4` are nothing).
        Some(b) if b.is_ascii_alphanumeric() || b == b'_' => {
            let (start, repo) = qualified_repo(bytes, hash)?;
            (start, Some(repo))
        }
        // A repository name may end in `.`/`-`; otherwise `#1-#3` is a range.
        Some(b'.' | b'-') => match qualified_repo(bytes, hash) {
            Some((start, repo)) => (start, Some(repo)),
            None => (hash, None),
        },
        Some(b'#' | b'&' | b'/' | b'\\' | b'@') => return None,
        Some(_) => (hash, None),
    };
    Some(GitRefMatch {
        range: start..digits_end,
        git_ref: GitRef::Issue { repo, number },
    })
}

/// The `owner/repo` that ends right before `hash`, and where it starts.
fn qualified_repo(bytes: &[u8], hash: usize) -> Option<(usize, String)> {
    let is_repo_byte = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-');
    let is_owner_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'-';
    let mut at = hash;
    while at > 0 && is_repo_byte(bytes[at - 1]) {
        at -= 1;
    }
    let slash = at.checked_sub(1).filter(|&slash| bytes[slash] == b'/')?;
    let mut start = slash;
    while start > 0 && is_owner_byte(bytes[start - 1]) {
        start -= 1;
    }
    // The owner must not continue a longer word or path.
    if start > 0 {
        let b = bytes[start - 1];
        if b.is_ascii_alphanumeric()
            || matches!(b, b'_' | b'-' | b'.' | b'/' | b'\\' | b'@' | b':' | b'#')
        {
            return None;
        }
    }
    let repo = std::str::from_utf8(&bytes[start..hash]).ok()?;
    let owner = &repo[..slash - start];
    (owner.len() <= 39 && valid_repo(repo)).then(|| (start, repo.to_string()))
}

// ── SHAs ───────────────────────────────────────────────────────────────────

/// ASCII word bytes: a SHA must be a whole run of them.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')
}

fn is_sha_like(text: &str) -> bool {
    (MIN_SHA_LEN..=MAX_SHA_LEN).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn has_upper(text: &str) -> bool {
    text.bytes().any(|b| b.is_ascii_uppercase())
}

/// The prose SHA spanning the word `bytes[start..end]`, if it is one.
fn prose_sha(bytes: &[u8], start: usize, end: usize) -> Option<String> {
    let word = std::str::from_utf8(&bytes[start..end]).ok()?;
    if !is_sha_like(word)
        || has_upper(word)
        || !word.bytes().any(|b| b.is_ascii_digit())
        || !word.bytes().any(|b| b.is_ascii_alphabetic())
    {
        return None;
    }
    let before = start.checked_sub(1).map(|at| bytes[at]);
    let after = bytes.get(end).copied();
    // `.`/`:` joining this word to another: a file name, `key:value`…
    let joined = |sep: Option<u8>, beyond: Option<u8>| {
        matches!(sep, Some(b'.' | b':')) && beyond.is_some_and(|b| b.is_ascii_alphanumeric())
    };
    let rejected = matches!(
        before,
        Some(b'#' | b'/' | b'\\' | b'@' | b'$' | b'%' | b'&')
    ) || matches!(after, Some(b'/' | b'\\' | b'@'))
        || joined(before, start.checked_sub(2).map(|at| bytes[at]))
        || joined(after, bytes.get(end + 1).copied());
    (!rejected).then(|| word.to_string())
}

// ── repositories ───────────────────────────────────────────────────────────

/// `owner/name` with GitHub's allowed characters.
fn valid_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let owner_ok = !owner.is_empty()
        && !owner.starts_with('-')
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let name_ok = !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    owner_ok && name_ok
}

// ── ResolveGitRef replies ──────────────────────────────────────────────────

/// `ResolveGitRef` reply. Only `commit`, `pullRequest` and `issue` make a
/// card; `notFound` and `unavailable` render as a plain link to `url` (when
/// there is one). Clients treat an RPC error (e.g. an older engine without
/// the method) like `unavailable`: a hover card never raises an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum GitRefResolution {
    Commit(GitCommitPreview),
    PullRequest(GithubPullPreview),
    Issue(GithubIssuePreview),
    /// The checkout has no such commit, or GitHub no such issue or pull
    /// request.
    #[serde(rename_all = "camelCase")]
    NotFound {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    /// Couldn't look it up right now.
    #[serde(rename_all = "camelCase")]
    Unavailable {
        reason: GitRefUnavailable,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
}

impl GitRefResolution {
    /// The page to open on click, whatever the outcome.
    pub fn url(&self) -> Option<&str> {
        match self {
            GitRefResolution::Commit(commit) => commit.url.as_deref(),
            GitRefResolution::PullRequest(pull) => Some(&pull.url),
            GitRefResolution::Issue(issue) => Some(&issue.url),
            GitRefResolution::NotFound { url } | GitRefResolution::Unavailable { url, .. } => {
                url.as_deref()
            }
        }
    }
}

/// Why a ref couldn't be resolved (diagnostic; every reason renders the
/// same plain link).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GitRefUnavailable {
    /// The chat's folder is no git checkout.
    NoCheckout,
    /// A bare `#123`, but the checkout has no github.com remote.
    NoGithubRemote,
    /// The host device has no GitHub credential.
    SignedOut,
    /// The host's credentials can't see the repository.
    NoAccess,
    /// GitHub didn't answer (offline, timed out, rate limited, error).
    Unreachable,
    /// A commit of another repository that this checkout doesn't have.
    OtherRepository,
    /// A reason this client doesn't know yet.
    #[serde(other)]
    Unknown,
}

/// A commit, read from the chat checkout's local git (no network).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitPreview {
    pub sha: String,
    pub short_sha: String,
    /// The first line of the message.
    pub subject: String,
    pub author: String,
    /// Author time, unix seconds.
    pub author_time: i64,
    /// Against the first parent (the empty tree for a root commit).
    pub files_changed: u32,
    pub insertions: u32,
    pub deletions: u32,
    /// The commit on github.com, when the checkout has a GitHub remote (or
    /// the ref came from a commit URL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// An issue's or pull request's state as a card shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GithubRefState {
    Open,
    /// An open draft pull request.
    Draft,
    /// A merged pull request.
    Merged,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubPullPreview {
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub state: GithubRefState,
    pub author: String,
    /// `branch`, or `owner:branch` from a fork.
    pub head_branch: String,
    pub base_branch: String,
    pub url: String,
    /// The head commit's checks; `None` when it has none or they couldn't
    /// be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<GithubChecksSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_files: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GithubChecksState {
    /// Every check passed (or was skipped/neutral).
    Success,
    /// At least one check failed.
    Failure,
    /// None failed, at least one still running or queued.
    Pending,
}

/// Check runs and commit statuses of a pull request's head commit, counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubChecksSummary {
    pub state: GithubChecksState,
    pub passed: u32,
    pub failed: u32,
    pub pending: u32,
    /// Skipped or neutral.
    pub skipped: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubIssuePreview {
    pub repo: String,
    pub number: u64,
    pub title: String,
    /// `open` or `closed`.
    pub state: GithubRefState,
    /// GitHub's `state_reason` (`completed`, `not_planned`, `reopened`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_reason: Option<String>,
    pub author: String,
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Vectors {
        detect: Vec<Row>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Row {
        name: String,
        text: String,
        in_code: bool,
        expect: Vec<Expected>,
    }

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Expected {
        #[serde(rename = "match")]
        matched: String,
        kind: String,
        #[serde(default)]
        repo: Option<String>,
        #[serde(default)]
        number: Option<u64>,
        #[serde(default)]
        sha: Option<String>,
    }

    fn expected(text: &str, found: &GitRefMatch) -> Expected {
        let matched = text[found.range.clone()].to_string();
        match &found.git_ref {
            GitRef::Issue { repo, number } => Expected {
                matched,
                kind: "issue".into(),
                repo: repo.clone(),
                number: Some(*number),
                sha: None,
            },
            GitRef::Commit { repo, sha } => Expected {
                matched,
                kind: "commit".into(),
                repo: repo.clone(),
                number: None,
                sha: Some(sha.clone()),
            },
        }
    }

    #[test]
    fn detection_runs_the_shared_vectors() {
        let vectors: Vectors =
            serde_json::from_str(include_str!("../../../protocol/vectors/git-refs-v1.json"))
                .expect("vectors parse");
        let rows = vectors.detect;
        assert!(rows.len() >= 30, "the vectors cover the rules");
        let mut failures = Vec::new();
        for row in &rows {
            let found: Vec<Expected> = find_git_refs(&row.text, row.in_code)
                .iter()
                .map(|found| expected(&row.text, found))
                .collect();
            if found != row.expect {
                failures.push(format!(
                    "{:?} (inCode {}, {}): got {found:?}, want {:?}",
                    row.text, row.in_code, row.name, row.expect
                ));
            }
            for found in find_git_refs(&row.text, row.in_code) {
                assert!(found.git_ref.is_valid(), "{:?}", found.git_ref);
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn ranges_are_byte_offsets_around_multibyte_text() {
        let text = "修复了 #42，见 a1b2c3d。";
        let found = find_git_refs(text, false);
        assert_eq!(found.len(), 2);
        assert_eq!(&text[found[0].range.clone()], "#42");
        assert_eq!(&text[found[1].range.clone()], "a1b2c3d");
    }

    #[test]
    fn refs_round_trip_on_the_wire() {
        let issue = GitRef::Issue {
            repo: None,
            number: 7,
        };
        assert_eq!(
            serde_json::to_value(&issue).unwrap(),
            serde_json::json!({"kind": "issue", "number": 7})
        );
        let commit: GitRef = serde_json::from_value(
            serde_json::json!({"kind": "commit", "repo": "o/r", "sha": "a1b2c3d"}),
        )
        .unwrap();
        assert_eq!(
            commit,
            GitRef::Commit {
                repo: Some("o/r".into()),
                sha: "a1b2c3d".into()
            }
        );
        assert_eq!(
            commit.web_url(None).as_deref(),
            Some("https://github.com/o/r/commit/a1b2c3d")
        );
        assert_eq!(
            issue.web_url(Some("me/app")).as_deref(),
            Some("https://github.com/me/app/issues/7")
        );
        assert_eq!(issue.web_url(None), None);
    }

    #[test]
    fn validity_guards_what_reaches_urls_and_git() {
        let issue = |repo: Option<&str>, number| GitRef::Issue {
            repo: repo.map(str::to_string),
            number,
        };
        let commit = |sha: &str| GitRef::Commit {
            repo: None,
            sha: sha.into(),
        };
        assert!(issue(None, 1).is_valid());
        assert!(issue(Some("o/r.js"), 999_999_999).is_valid());
        assert!(!issue(None, 0).is_valid());
        assert!(!issue(None, 1_000_000_000).is_valid());
        assert!(!issue(Some("o/r/x"), 1).is_valid());
        assert!(!issue(Some("../r"), 1).is_valid());
        assert!(commit("a1b2c3d").is_valid());
        assert!(!commit("a1b2c3").is_valid());
        assert!(!commit("A1B2C3D").is_valid());
        assert!(!commit("--output=x").is_valid());
    }

    #[test]
    fn resolutions_serialize_with_a_kind_tag() {
        let unavailable = GitRefResolution::Unavailable {
            reason: GitRefUnavailable::SignedOut,
            url: Some("https://github.com/o/r/issues/1".into()),
        };
        assert_eq!(
            serde_json::to_value(&unavailable).unwrap(),
            serde_json::json!({
                "kind": "unavailable",
                "reason": "signedOut",
                "url": "https://github.com/o/r/issues/1"
            })
        );
        let pull = GitRefResolution::PullRequest(GithubPullPreview {
            repo: "o/r".into(),
            number: 3,
            title: "T".into(),
            state: GithubRefState::Draft,
            author: "me".into(),
            head_branch: "topic".into(),
            base_branch: "main".into(),
            url: "https://github.com/o/r/pull/3".into(),
            checks: Some(GithubChecksSummary {
                state: GithubChecksState::Pending,
                passed: 1,
                failed: 0,
                pending: 2,
                skipped: 0,
            }),
            changed_files: None,
            additions: None,
            deletions: None,
        });
        let value = serde_json::to_value(&pull).unwrap();
        assert_eq!(value["kind"], "pullRequest");
        assert_eq!(value["state"], "draft");
        assert_eq!(value["headBranch"], "topic");
        assert_eq!(value["checks"]["state"], "pending");
        assert_eq!(
            serde_json::from_value::<GitRefResolution>(value).unwrap(),
            pull
        );
        // A reason from a newer engine still decodes.
        let newer: GitRefResolution =
            serde_json::from_value(serde_json::json!({"kind": "unavailable", "reason": "quota"}))
                .unwrap();
        assert_eq!(
            newer,
            GitRefResolution::Unavailable {
                reason: GitRefUnavailable::Unknown,
                url: None
            }
        );
    }
}
