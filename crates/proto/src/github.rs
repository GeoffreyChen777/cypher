//! GitHub: per-device sign-in and the composer's `#` issue references.
//!
//! The project's HOST device answers every GitHub call with its own
//! credential — the Cypher GitHub App login made in Settings → GitHub, or,
//! failing that, the `gh` CLI or git credential already on that device — so no
//! GitHub token ever crosses the relay; only issue text does.

use serde::{Deserialize, Serialize};

/// Why a project can't list GitHub issues. Each maps to one actionable line
/// in the composer popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GithubUnavailable {
    /// No GitHub credential on the device at all.
    SignedOut,
    /// The checkout has no github.com remote.
    NoGithubRemote,
    /// Credentials exist, but none of them can see the repository (the
    /// Cypher GitHub App isn't installed on it, or the account lacks access).
    NoAccess,
}

/// One row of the `#` popup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubIssueSummary {
    pub number: u64,
    pub title: String,
    /// `open` / `closed`.
    pub state: String,
    #[serde(default)]
    pub labels: Vec<String>,
    /// Assigned to the signed-in user (only known for an empty query).
    #[serde(default)]
    pub assigned_to_me: bool,
}

/// `SearchGithubIssues` reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum GithubIssueSearch {
    #[serde(rename_all = "camelCase")]
    Ok {
        /// `owner/name` of the repository the issues belong to.
        repo: String,
        issues: Vec<GithubIssueSummary>,
    },
    #[serde(rename_all = "camelCase")]
    Unavailable {
        reason: GithubUnavailable,
        /// `owner/name`, when the checkout has a GitHub remote.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
        /// Where to install the Cypher GitHub App ([`GithubUnavailable::NoAccess`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        install_url: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubIssueComment {
    pub author: String,
    pub created_at: String,
    pub body: String,
}

/// `GetGithubIssue` reply: a bounded snapshot of one issue, taken at send
/// time and embedded in the agent prompt as untrusted reference context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubIssueSnapshot {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub state: String,
    pub url: String,
    pub author: String,
    #[serde(default)]
    pub labels: Vec<String>,
    pub body: String,
    /// The newest comments that fit the budget, oldest first.
    #[serde(default)]
    pub comments: Vec<GithubIssueComment>,
    /// Older comments left out to stay within the budget.
    #[serde(default)]
    pub omitted_comments: usize,
}

/// Where a device's GitHub credential came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GithubCredentialSource {
    /// Signed in with the Cypher GitHub App (Settings → GitHub).
    Cypher,
    /// The GitHub CLI's login (`gh auth token`).
    GhCli,
    /// Git's credential helper for github.com (Keychain, Credential Manager…).
    GitCredential,
}

/// `GithubAccountStatus` reply for one device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubAccountStatus {
    /// This build can sign in with the Cypher GitHub App (a client id is
    /// configured).
    pub sign_in_available: bool,
    /// The Cypher sign-in, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    /// A working credential Cypher found on the device without a sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<GithubFallback>,
    /// Where to install the Cypher GitHub App on more repositories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubFallback {
    pub source: GithubCredentialSource,
    pub login: String,
}

/// `StartGithubLogin` reply: show `user_code`, open `verification_uri`, then
/// poll `PollGithubLogin` until the device's engine has the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubLoginStart {
    pub login_id: String,
    pub user_code: String,
    pub verification_uri: String,
    /// Seconds until the code expires.
    pub expires_in: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GithubLoginState {
    Pending,
    Done,
    Error,
}

/// `PollGithubLogin` reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubLoginPoll {
    pub state: GithubLoginState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
