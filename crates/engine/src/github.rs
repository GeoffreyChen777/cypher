//! GitHub for this device: the Cypher GitHub App sign-in (Settings → GitHub)
//! and the REST calls behind the composer's `#` issue references.
//!
//! Every device holds its own GitHub credential, like it holds its own git
//! checkouts: a token never syncs and never crosses the relay. Sign-in uses
//! GitHub's device flow, so the TARGET device's engine talks to GitHub
//! directly — the desktop only shows the code and opens the approval page —
//! and no client secret ships anywhere (device-flow tokens also refresh
//! without one).
//!
//! When the device has no Cypher sign-in, the credentials it already has for
//! github.com serve instead: the `gh` CLI's login, then git's credential
//! helper. For each repository the engine uses the first credential that can
//! actually see it and remembers that choice for a while.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cypher_proto::{
    GithubAccountStatus, GithubCredentialSource, GithubFallback, GithubIssueComment,
    GithubIssueSearch, GithubIssueSnapshot, GithubIssueSummary, GithubLoginPoll, GithubLoginStart,
    GithubLoginState, GithubUnavailable,
};
use serde::{Deserialize, Serialize};

/// One GitHub API request. Issue search is keystroke-driven, so a wedged
/// network must fail the popup rather than spin it forever.
const API_TIMEOUT: Duration = Duration::from_secs(15);
/// One local credential lookup (`gh auth token`, `git credential fill`).
const LOCAL_CREDENTIAL_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a repository keeps the credential that last worked for it.
const REPO_ACCESS_TTL: Duration = Duration::from_secs(10 * 60);
/// How long local credentials are reused before asking `gh`/git again.
const LOCAL_CREDENTIALS_TTL: Duration = Duration::from_secs(60);
/// Refresh a Cypher token this long before it expires.
const REFRESH_MARGIN_SECS: i64 = 5 * 60;
/// Rows per popup query (the composer shows them in one scroll list).
const SEARCH_LIMIT: usize = 20;
/// Assigned-to-me rows that lead an empty query.
const ASSIGNED_LIMIT: usize = 10;
pub const MAX_QUERY_CHARS: usize = 256;
/// Snapshot budgets (chars): the issue body, one comment, and all comments.
const BODY_MAX_CHARS: usize = 12 * 1024;
const COMMENT_MAX_CHARS: usize = 4 * 1024;
const COMMENTS_BUDGET_CHARS: usize = 32 * 1024;
/// Comments per page when snapshotting (GitHub's maximum).
const COMMENTS_PAGE: u64 = 100;
const ACCOUNT_FILE: &str = "github-account.json";
const API_VERSION: &str = "2022-11-28";

/// The Cypher GitHub App (App ID 5079720). Its client id is public — device
/// flow needs no secret — so it ships in the binary, like the WorkOS client
/// id.
pub const DEFAULT_GITHUB_CLIENT_ID: &str = "Iv23liRz0sZ3RqxYjxqy";

/// The GitHub App this build signs in with: [`DEFAULT_GITHUB_CLIENT_ID`]
/// unless `CYPHER_GITHUB_CLIENT_ID` names another app (empty = sign-in off);
/// `CYPHER_GITHUB_APP_SLUG` names the app for install links.
#[derive(Debug, Clone)]
pub struct GithubConfig {
    pub client_id: Option<String>,
    pub app_slug: Option<String>,
    pub api_base: String,
    pub web_base: String,
    /// Also use the `gh` login and git credentials on this device.
    pub local_fallbacks: bool,
    /// One device-flow `interval` unit (a second against GitHub).
    pub poll_unit: Duration,
}

impl GithubConfig {
    pub fn detect() -> Self {
        let nonempty = |value: Option<&str>| {
            value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        Self {
            client_id: match std::env::var("CYPHER_GITHUB_CLIENT_ID") {
                Ok(value) => nonempty(Some(&value)),
                Err(_) => Some(DEFAULT_GITHUB_CLIENT_ID.to_string()),
            },
            app_slug: cypher_env::var("GITHUB_APP_SLUG")
                .or_else(|| nonempty(option_env!("CYPHER_GITHUB_APP_SLUG"))),
            api_base: "https://api.github.com".into(),
            web_base: "https://github.com".into(),
            local_fallbacks: true,
            poll_unit: Duration::from_secs(1),
        }
    }

    fn install_url(&self) -> Option<String> {
        self.app_slug
            .as_ref()
            .map(|slug| format!("{}/apps/{slug}/installations/new", self.web_base))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhError {
    Unavailable(GithubUnavailable),
    Failed(String),
}

impl std::fmt::Display for GhError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GhError::Unavailable(GithubUnavailable::SignedOut) => write!(
                f,
                "this device has no GitHub login — sign in under Settings → GitHub"
            ),
            GhError::Unavailable(GithubUnavailable::NoAccess) => {
                write!(f, "this device's GitHub login can't see that repository")
            }
            GhError::Unavailable(GithubUnavailable::NoGithubRemote) => {
                write!(f, "this project has no GitHub remote")
            }
            GhError::Failed(message) => write!(f, "{message}"),
        }
    }
}

/// A failed API request.
#[derive(Debug)]
enum ApiError {
    Status(u16),
    RateLimited,
    Other(String),
}

impl From<ApiError> for GhError {
    fn from(err: ApiError) -> Self {
        GhError::Failed(match err {
            ApiError::Status(status) => format!("GitHub answered {status}"),
            ApiError::RateLimited => "GitHub's rate limit was reached — try again shortly".into(),
            ApiError::Other(message) => message,
        })
    }
}

/// A token and where it came from. Never printed.
#[derive(Clone, PartialEq, Eq)]
struct Credential {
    source: GithubCredentialSource,
    token: String,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// The Cypher sign-in on disk (`github-account.json`, 0600).
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredAccount {
    /// The app the token belongs to (refreshes must use the same one).
    client_id: String,
    login: String,
    access_token: String,
    /// Unix seconds; `None` for tokens that don't expire.
    #[serde(default)]
    expires_at: Option<i64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    refresh_expires_at: Option<i64>,
}

impl StoredAccount {
    fn refreshable(&self, now: i64) -> bool {
        self.refresh_token.is_some() && self.refresh_expires_at.is_none_or(|at| at > now)
    }

    fn needs_refresh(&self, now: i64) -> bool {
        self.expires_at
            .is_some_and(|at| at - now < REFRESH_MARGIN_SECS && self.refreshable(now))
    }

    fn expired(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|at| at <= now)
    }
}

#[derive(Default)]
struct AccountSlot {
    loaded: bool,
    account: Option<StoredAccount>,
}

struct LoginEntry {
    state: GithubLoginState,
    login: Option<String>,
    message: Option<String>,
    task: Option<tokio::task::AbortHandle>,
}

/// Local credentials per `git credential` working directory, with when they
/// were read.
type LocalCredentials = HashMap<Option<PathBuf>, (Instant, Vec<Credential>)>;

struct Inner {
    config: GithubConfig,
    account_path: PathBuf,
    http: reqwest::Client,
    /// Serializes load/refresh/sign-out: refresh tokens rotate, so two
    /// concurrent refreshes would invalidate each other.
    account: tokio::sync::Mutex<AccountSlot>,
    local: std::sync::Mutex<LocalCredentials>,
    repo_access: std::sync::Mutex<HashMap<String, (GithubCredentialSource, Instant)>>,
    logins: std::sync::Mutex<HashMap<String, LoginEntry>>,
}

#[derive(Clone)]
pub struct Github {
    inner: Arc<Inner>,
}

// ── GitHub's JSON ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RestLabel {
    name: String,
}

#[derive(Deserialize)]
struct RestUser {
    login: String,
}

#[derive(Deserialize)]
struct RestIssue {
    number: u64,
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    labels: Vec<RestLabel>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    user: Option<RestUser>,
    #[serde(default)]
    body: Option<String>,
    /// The comment COUNT (the comments themselves are a separate request).
    #[serde(default)]
    comments: u64,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
    #[serde(default)]
    repository_url: Option<String>,
}

impl RestIssue {
    fn summary(self, assigned_to_me: bool) -> GithubIssueSummary {
        GithubIssueSummary {
            number: self.number,
            title: self.title,
            state: self.state,
            labels: self.labels.into_iter().map(|label| label.name).collect(),
            assigned_to_me,
        }
    }

    /// Search results for another repository (a `repo:` qualifier typed into
    /// the query) never become rows of this one.
    fn in_repo(&self, repo: &str) -> bool {
        self.repository_url.as_deref().is_none_or(|url| {
            url.to_ascii_lowercase()
                .ends_with(&format!("/repos/{}", repo.to_ascii_lowercase()))
        })
    }
}

#[derive(Deserialize)]
struct RestComment {
    #[serde(default)]
    user: Option<RestUser>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    created_at: String,
}

#[derive(Deserialize)]
struct SearchPage {
    #[serde(default)]
    items: Vec<RestIssue>,
}

#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_interval() -> u64 {
    5
}

#[derive(Deserialize, Default)]
struct TokenResponse {
    access_token: Option<String>,
    expires_in: Option<i64>,
    refresh_token: Option<String>,
    refresh_token_expires_in: Option<i64>,
    error: Option<String>,
    error_description: Option<String>,
    interval: Option<u64>,
}

impl TokenResponse {
    fn describe(&self) -> String {
        self.error_description
            .clone()
            .or_else(|| self.error.clone())
            .unwrap_or_else(|| "GitHub returned no token".into())
    }
}

/// A deleted account's content stays attributed to GitHub's placeholder.
fn login(user: Option<RestUser>) -> String {
    user.map_or_else(|| "ghost".to_string(), |user| user.login)
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Github {
    /// `data_dir` is the device root (the account is device-scoped, like
    /// agent CLI logins).
    pub fn new(config: GithubConfig, data_dir: &Path) -> Self {
        let http = reqwest::Client::builder()
            .timeout(API_TIMEOUT)
            .user_agent(concat!("cypher/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            inner: Arc::new(Inner {
                config,
                account_path: data_dir.join(ACCOUNT_FILE),
                http,
                account: Default::default(),
                local: Default::default(),
                repo_access: Default::default(),
                logins: Default::default(),
            }),
        }
    }

    // ── account ─────────────────────────────────────────────────────────

    fn load_account(&self) -> Option<StoredAccount> {
        let bytes = std::fs::read(&self.inner.account_path).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(account) => Some(account),
            Err(err) => {
                tracing::warn!(error = %err, "github: unreadable account file; treating as signed out");
                None
            }
        }
    }

    fn save_account(&self, account: Option<&StoredAccount>) {
        let path = &self.inner.account_path;
        let outcome = match account {
            Some(account) => serde_json::to_vec(account)
                .map_err(std::io::Error::other)
                .and_then(|bytes| write_private_atomic(path, &bytes)),
            None => match std::fs::remove_file(path) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            },
        };
        if let Err(err) = outcome {
            tracing::warn!(error = %err, "github: failed to persist the account");
        }
    }

    /// The Cypher sign-in's token, refreshed when it is about to expire. A
    /// refresh GitHub rejects signs the device out; a network failure keeps
    /// the current token while it is still valid.
    async fn cypher_credential(&self) -> Option<(Credential, String)> {
        let mut slot = self.inner.account.lock().await;
        if !slot.loaded {
            slot.account = self.load_account();
            slot.loaded = true;
        }
        let account = slot.account.clone()?;
        let now = now_secs();
        let account = if account.needs_refresh(now) {
            match self.refresh(&account).await {
                Ok(fresh) => {
                    self.save_account(Some(&fresh));
                    slot.account = Some(fresh.clone());
                    fresh
                }
                Err(Some(rejected)) => {
                    tracing::warn!(reason = %rejected, "github: refresh rejected; signing out");
                    self.save_account(None);
                    slot.account = None;
                    return None;
                }
                Err(None) if account.expired(now) => return None,
                Err(None) => account,
            }
        } else if account.expired(now) {
            // Expired with nothing to refresh it with.
            self.save_account(None);
            slot.account = None;
            return None;
        } else {
            account
        };
        Some((
            Credential {
                source: GithubCredentialSource::Cypher,
                token: account.access_token.clone(),
            },
            account.login,
        ))
    }

    /// `Err(Some(reason))` = GitHub rejected the refresh token;
    /// `Err(None)` = transient failure.
    async fn refresh(&self, account: &StoredAccount) -> Result<StoredAccount, Option<String>> {
        let refresh_token = account.refresh_token.as_deref().ok_or(None)?;
        let response = self
            .inner
            .http
            .post(format!(
                "{}/login/oauth/access_token",
                self.inner.config.web_base
            ))
            .header("Accept", "application/json")
            .form(&[
                ("client_id", account.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .map_err(|err| {
                tracing::warn!(error = %err, "github: token refresh failed");
                None
            })?;
        let tokens: TokenResponse = response.json().await.map_err(|_| None)?;
        match tokens.access_token.clone() {
            Some(access_token) => Ok(stored_from(
                &account.client_id,
                account.login.clone(),
                access_token,
                &tokens,
            )),
            None => Err(Some(tokens.describe())),
        }
    }

    // ── credentials ─────────────────────────────────────────────────────

    /// The device's existing github.com credentials, deduped, in preference
    /// order: the `gh` login, then git's credential helper (run in `cwd`, so
    /// repository-level helper config applies).
    async fn local_credentials(&self, cwd: Option<&Path>) -> Vec<Credential> {
        if !self.inner.config.local_fallbacks {
            return Vec::new();
        }
        let key = cwd.map(Path::to_path_buf);
        if let Some((at, cached)) = self.inner.local.lock().unwrap().get(&key)
            && at.elapsed() < LOCAL_CREDENTIALS_TTL
        {
            return cached.clone();
        }
        let (gh, git) = tokio::join!(gh_token(), git_credential(cwd));
        let mut found: Vec<Credential> = Vec::new();
        for (source, token) in [
            (GithubCredentialSource::GhCli, gh),
            (GithubCredentialSource::GitCredential, git),
        ] {
            if let Some(token) = token
                && !found.iter().any(|known| known.token == token)
            {
                found.push(Credential { source, token });
            }
        }
        self.inner
            .local
            .lock()
            .unwrap()
            .insert(key, (Instant::now(), found.clone()));
        found
    }

    async fn credentials(&self, cwd: Option<&Path>) -> Vec<Credential> {
        let mut all: Vec<Credential> = self
            .cypher_credential()
            .await
            .map(|(credential, _)| credential)
            .into_iter()
            .collect();
        for credential in self.local_credentials(cwd).await {
            if !all.iter().any(|known| known.token == credential.token) {
                all.push(credential);
            }
        }
        all
    }

    fn forget(&self, repo: &str) {
        self.inner.repo_access.lock().unwrap().remove(repo);
        self.inner.local.lock().unwrap().clear();
    }

    fn forget_all(&self) {
        self.inner.repo_access.lock().unwrap().clear();
        self.inner.local.lock().unwrap().clear();
    }

    /// The first credential that can see `repo`.
    async fn repo_credential(&self, repo: &str, cwd: Option<&Path>) -> Result<Credential, GhError> {
        let candidates = self.credentials(cwd).await;
        if candidates.is_empty() {
            return Err(GhError::Unavailable(GithubUnavailable::SignedOut));
        }
        let cached = self
            .inner
            .repo_access
            .lock()
            .unwrap()
            .get(repo)
            .filter(|(_, at)| at.elapsed() < REPO_ACCESS_TTL)
            .map(|(source, _)| *source);
        if let Some(credential) =
            cached.and_then(|source| candidates.iter().find(|c| c.source == source))
        {
            return Ok(credential.clone());
        }
        for credential in candidates {
            match self
                .get::<serde_json::Value>(&credential.token, &format!("/repos/{repo}"), &[])
                .await
            {
                Ok(_) => {
                    self.inner
                        .repo_access
                        .lock()
                        .unwrap()
                        .insert(repo.to_string(), (credential.source, Instant::now()));
                    return Ok(credential);
                }
                Err(ApiError::Status(401 | 403 | 404)) => continue,
                Err(err) => return Err(err.into()),
            }
        }
        Err(GhError::Unavailable(GithubUnavailable::NoAccess))
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        token: &str,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, ApiError> {
        let response = self
            .inner
            .http
            .get(format!("{}{path}", self.inner.config.api_base))
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .query(query)
            .send()
            .await
            .map_err(|err| {
                ApiError::Other(if err.is_timeout() {
                    "GitHub didn't answer in time".into()
                } else {
                    format!("couldn't reach GitHub: {err}")
                })
            })?;
        let status = response.status().as_u16();
        let exhausted = response
            .headers()
            .get("x-ratelimit-remaining")
            .is_some_and(|remaining| remaining == "0");
        if status == 429 || (status == 403 && exhausted) {
            return Err(ApiError::RateLimited);
        }
        if !response.status().is_success() {
            return Err(ApiError::Status(status));
        }
        response
            .json()
            .await
            .map_err(|err| ApiError::Other(format!("unexpected GitHub response: {err}")))
    }

    /// GET under `repo` with whichever credential can see it; a token revoked
    /// since the choice was cached is dropped and the next one tried once.
    async fn repo_get<T: serde::de::DeserializeOwned>(
        &self,
        repo: &str,
        cwd: Option<&Path>,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, RepoError> {
        let mut retried = false;
        loop {
            let credential = self
                .repo_credential(repo, cwd)
                .await
                .map_err(RepoError::Gh)?;
            match self.get(&credential.token, path, query).await {
                Err(ApiError::Status(401)) if !retried => {
                    retried = true;
                    self.forget(repo);
                }
                other => return other.map_err(RepoError::Api),
            }
        }
    }

    // ── issues ──────────────────────────────────────────────────────────

    /// Popup rows for `query` in the checkout at `root`: an empty query lists
    /// the user's assigned open issues first, then recently updated ones; a
    /// numeric query (`482`) puts that exact issue first, whatever its state.
    pub async fn search_issues(
        &self,
        root: &Path,
        query: &str,
    ) -> Result<GithubIssueSearch, GhError> {
        let Some(repo) = github_repo(root).await else {
            return Ok(GithubIssueSearch::Unavailable {
                reason: GithubUnavailable::NoGithubRemote,
                repo: None,
                install_url: None,
            });
        };
        match self.search_rows(&repo, root, query.trim()).await {
            Ok(issues) => Ok(GithubIssueSearch::Ok { repo, issues }),
            Err(GhError::Unavailable(reason)) => Ok(GithubIssueSearch::Unavailable {
                reason,
                install_url: (reason == GithubUnavailable::NoAccess)
                    .then(|| self.inner.config.install_url())
                    .flatten(),
                repo: Some(repo),
            }),
            Err(err) => Err(err),
        }
    }

    async fn search(
        &self,
        repo: &str,
        root: &Path,
        qualifiers: &str,
        sort_updated: bool,
        limit: usize,
    ) -> Result<Vec<RestIssue>, GhError> {
        let mut query = vec![
            ("q", format!("repo:{repo} is:issue {qualifiers}")),
            ("per_page", limit.to_string()),
        ];
        if sort_updated {
            query.push(("sort", "updated".into()));
            query.push(("order", "desc".into()));
        }
        let page: SearchPage = self
            .repo_get(repo, Some(root), "/search/issues", &query)
            .await
            .map_err(GhError::from)?;
        Ok(page
            .items
            .into_iter()
            .filter(|issue| issue.pull_request.is_none() && issue.in_repo(repo))
            .collect())
    }

    async fn search_rows(
        &self,
        repo: &str,
        root: &Path,
        query: &str,
    ) -> Result<Vec<GithubIssueSummary>, GhError> {
        // Resolve the credential once up front, so the concurrent requests
        // below share one access probe instead of racing their own.
        self.repo_credential(repo, Some(root)).await?;
        if query.is_empty() {
            let recent = async {
                let issues: Vec<RestIssue> = self
                    .repo_get(
                        repo,
                        Some(root),
                        &format!("/repos/{repo}/issues"),
                        &[
                            ("state", "open".into()),
                            ("sort", "updated".into()),
                            ("direction", "desc".into()),
                            ("per_page", (SEARCH_LIMIT * 2).to_string()),
                        ],
                    )
                    .await
                    .map_err(GhError::from)?;
                Ok::<_, GhError>(
                    issues
                        .into_iter()
                        .filter(|issue| issue.pull_request.is_none())
                        .take(SEARCH_LIMIT)
                        .collect::<Vec<_>>(),
                )
            };
            let (assigned, recent) = tokio::join!(
                self.search(repo, root, "is:open assignee:@me", true, ASSIGNED_LIMIT),
                recent
            );
            return Ok(merge_issues(assigned?, Vec::new(), recent?));
        }
        let exact = async {
            let Ok(number) = query.trim_start_matches('#').parse::<u64>() else {
                return Ok(None);
            };
            match self
                .repo_get::<RestIssue>(
                    repo,
                    Some(root),
                    &format!("/repos/{repo}/issues/{number}"),
                    &[],
                )
                .await
            {
                Ok(issue) if issue.pull_request.is_none() => Ok(Some(issue)),
                // A pull request, or a number that isn't an issue, just isn't
                // a row.
                Ok(_) | Err(RepoError::Api(ApiError::Status(_))) => Ok(None),
                Err(err) => Err(GhError::from(err)),
            }
        };
        let qualifiers = format!("is:open {query}");
        let (exact, found) = tokio::join!(
            exact,
            self.search(repo, root, &qualifiers, false, SEARCH_LIMIT)
        );
        Ok(merge_issues(
            Vec::new(),
            exact?.into_iter().collect(),
            found?,
        ))
    }

    /// A bounded snapshot of `repo#number` for the agent prompt.
    pub async fn issue_snapshot(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<GithubIssueSnapshot, GhError> {
        if !valid_repo(repo) || number == 0 {
            return Err(GhError::Failed("invalid issue reference".into()));
        }
        let issue: RestIssue = self
            .repo_get(repo, None, &format!("/repos/{repo}/issues/{number}"), &[])
            .await
            .map_err(|err| match err {
                RepoError::Api(ApiError::Status(404 | 410)) => {
                    GhError::Failed(format!("{repo}#{number} doesn't exist or was deleted"))
                }
                err => err.into(),
            })?;
        let total = issue.comments;
        let mut comments: Vec<RestComment> = Vec::new();
        if total > 0 {
            // The newest comments live on the last page; a short last page is
            // topped up from the one before it.
            let last = total.div_ceil(COMMENTS_PAGE);
            let short = !total.is_multiple_of(COMMENTS_PAGE) && total % COMMENTS_PAGE < 20;
            let first = if last > 1 && short { last - 1 } else { last };
            for page in first..=last {
                let batch: Vec<RestComment> = self
                    .repo_get(
                        repo,
                        None,
                        &format!("/repos/{repo}/issues/{number}/comments"),
                        &[
                            ("per_page", COMMENTS_PAGE.to_string()),
                            ("page", page.to_string()),
                        ],
                    )
                    .await
                    .map_err(GhError::from)?;
                comments.extend(batch);
            }
        }
        Ok(snapshot_from(repo, issue, comments, total as usize))
    }

    // ── sign-in ─────────────────────────────────────────────────────────

    /// What Settings → GitHub shows for this device.
    pub async fn status(&self) -> GithubAccountStatus {
        let login = self.cypher_credential().await.map(|(_, login)| login);
        let mut fallback = None;
        if login.is_none() {
            for credential in self.local_credentials(None).await {
                if let Ok(user) = self.get::<RestUser>(&credential.token, "/user", &[]).await {
                    fallback = Some(GithubFallback {
                        source: credential.source,
                        login: user.login,
                    });
                    break;
                }
            }
        }
        GithubAccountStatus {
            sign_in_available: self.inner.config.client_id.is_some(),
            login,
            fallback,
            install_url: self.inner.config.install_url(),
        }
    }

    /// Start a device-flow sign-in. The engine polls GitHub itself and
    /// stores the token when the user approves; `PollGithubLogin` reports
    /// progress. Starting again cancels an unfinished attempt.
    pub async fn start_login(&self) -> Result<GithubLoginStart, GhError> {
        let client_id = self.inner.config.client_id.clone().ok_or_else(|| {
            GhError::Failed("GitHub sign-in isn't set up in this build of Cypher".into())
        })?;
        let response = self
            .inner
            .http
            .post(format!("{}/login/device/code", self.inner.config.web_base))
            .header("Accept", "application/json")
            .form(&[("client_id", client_id.as_str())])
            .send()
            .await
            .map_err(|err| GhError::Failed(format!("couldn't reach GitHub: {err}")))?;
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|err| GhError::Failed(format!("unexpected GitHub response: {err}")))?;
        let code: DeviceCode = serde_json::from_value(body.clone()).map_err(|_| {
            GhError::Failed(
                body.get("error_description")
                    .or_else(|| body.get("error"))
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(
                        || "GitHub didn't start the sign-in".to_string(),
                        |message| format!("GitHub didn't start the sign-in: {message}"),
                    ),
            )
        })?;
        let login_id = uuid::Uuid::new_v4().to_string();
        let start = GithubLoginStart {
            login_id: login_id.clone(),
            user_code: code.user_code.clone(),
            verification_uri: code.verification_uri.clone(),
            expires_in: code.expires_in,
        };
        let task = tokio::spawn(
            self.clone()
                .await_approval(login_id.clone(), client_id, code),
        );
        let mut logins = self.inner.logins.lock().unwrap();
        for entry in logins.values_mut() {
            if entry.state == GithubLoginState::Pending {
                if let Some(task) = entry.task.take() {
                    task.abort();
                }
                entry.state = GithubLoginState::Error;
                entry.message = Some("Replaced by a newer sign-in".into());
            }
        }
        logins.retain(|_, entry| entry.state == GithubLoginState::Pending);
        logins.insert(
            login_id,
            LoginEntry {
                state: GithubLoginState::Pending,
                login: None,
                message: None,
                task: Some(task.abort_handle()),
            },
        );
        Ok(start)
    }

    fn finish_login(&self, login_id: &str, result: Result<String, String>) {
        let mut logins = self.inner.logins.lock().unwrap();
        if let Some(entry) = logins.get_mut(login_id)
            && entry.state == GithubLoginState::Pending
        {
            entry.task = None;
            match result {
                Ok(login) => {
                    entry.state = GithubLoginState::Done;
                    entry.login = Some(login);
                }
                Err(message) => {
                    entry.state = GithubLoginState::Error;
                    entry.message = Some(message);
                }
            }
        }
    }

    async fn await_approval(self, login_id: String, client_id: String, code: DeviceCode) {
        let deadline = Instant::now() + Duration::from_secs(code.expires_in);
        let mut interval = code.interval.max(1);
        let outcome = loop {
            tokio::time::sleep(self.inner.config.poll_unit * interval as u32).await;
            if Instant::now() >= deadline {
                break Err("The code expired before it was approved — start again".to_string());
            }
            let response = self
                .inner
                .http
                .post(format!(
                    "{}/login/oauth/access_token",
                    self.inner.config.web_base
                ))
                .header("Accept", "application/json")
                .form(&[
                    ("client_id", client_id.as_str()),
                    ("device_code", code.device_code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await;
            // Network trouble while waiting is not a verdict; keep polling
            // until the code expires.
            let Ok(response) = response else { continue };
            let Ok(tokens) = response.json::<TokenResponse>().await else {
                continue;
            };
            if let Some(access_token) = tokens.access_token.clone() {
                let user = match self.get::<RestUser>(&access_token, "/user", &[]).await {
                    Ok(user) => user,
                    Err(err) => break Err(format!("Signed in, but {}", GhError::from(err))),
                };
                let account = stored_from(&client_id, user.login.clone(), access_token, &tokens);
                let mut slot = self.inner.account.lock().await;
                self.save_account(Some(&account));
                slot.account = Some(account);
                slot.loaded = true;
                drop(slot);
                self.forget_all();
                break Ok(user.login);
            }
            match tokens.error.as_deref() {
                Some("authorization_pending") => {}
                Some("slow_down") => interval = tokens.interval.unwrap_or(interval + 5).max(1),
                Some("expired_token" | "token_expired") => {
                    break Err("The code expired before it was approved — start again".into());
                }
                Some("access_denied") => break Err("The sign-in was cancelled on GitHub".into()),
                _ => break Err(tokens.describe()),
            }
        };
        match &outcome {
            Ok(login) => tracing::info!(%login, "github: signed in"),
            Err(message) => tracing::info!(%message, "github: sign-in ended"),
        }
        self.finish_login(&login_id, outcome);
    }

    pub fn poll_login(&self, login_id: &str) -> GithubLoginPoll {
        match self.inner.logins.lock().unwrap().get(login_id) {
            Some(entry) => GithubLoginPoll {
                state: entry.state,
                login: entry.login.clone(),
                message: entry.message.clone(),
            },
            None => GithubLoginPoll {
                state: GithubLoginState::Error,
                login: None,
                message: Some("No sign-in is in progress on this device".into()),
            },
        }
    }

    pub fn cancel_login(&self, login_id: &str) {
        if let Some(mut entry) = self.inner.logins.lock().unwrap().remove(login_id)
            && let Some(task) = entry.task.take()
        {
            task.abort();
        }
    }

    /// Forget the Cypher sign-in on this device. (Revoking the grant needs
    /// the app's secret, so the user does that on github.com if they want.)
    pub async fn sign_out(&self) {
        let mut slot = self.inner.account.lock().await;
        self.save_account(None);
        slot.account = None;
        slot.loaded = true;
        drop(slot);
        self.forget_all();
    }
}

/// A failure under one repository: no usable credential, or the request
/// itself.
#[derive(Debug)]
enum RepoError {
    Gh(GhError),
    Api(ApiError),
}

impl From<RepoError> for GhError {
    fn from(err: RepoError) -> Self {
        match err {
            RepoError::Gh(err) => err,
            RepoError::Api(err) => err.into(),
        }
    }
}

fn stored_from(
    client_id: &str,
    login: String,
    access_token: String,
    tokens: &TokenResponse,
) -> StoredAccount {
    let now = now_secs();
    StoredAccount {
        client_id: client_id.to_string(),
        login,
        access_token,
        expires_at: tokens.expires_in.map(|secs| now + secs),
        refresh_token: tokens.refresh_token.clone(),
        refresh_expires_at: tokens.refresh_token_expires_in.map(|secs| now + secs),
    }
}

/// Write `bytes` readable only by the owner, atomically (temp + rename).
fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        #[cfg(unix)]
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(tmp, path)
}

// ── local credentials ──────────────────────────────────────────────────────

fn plausible_token(token: &str) -> Option<String> {
    let token = token.trim();
    (!token.is_empty() && token.len() <= 512 && !token.chars().any(char::is_whitespace))
        .then(|| token.to_string())
}

/// The `gh` CLI's github.com token, when `gh` is installed and signed in.
async fn gh_token() -> Option<String> {
    let exe = tokio::task::spawn_blocking(|| cypher_harness::resolve_cli("gh"))
        .await
        .ok()
        .flatten()?;
    let mut cmd = tokio::process::Command::new(&exe);
    cypher_harness::compose_child_path(&mut cmd, &exe);
    cmd.args(["auth", "token", "--hostname", "github.com"])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(LOCAL_CREDENTIAL_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    output
        .status
        .success()
        .then(|| plausible_token(&String::from_utf8_lossy(&output.stdout)))
        .flatten()
}

/// The password git's credential helper stores for https://github.com —
/// asked strictly non-interactively: no terminal prompt, no askpass dialog.
/// Never `approve`/`reject`: the user's stored credential is only read.
async fn git_credential(cwd: Option<&Path>) -> Option<String> {
    use tokio::io::AsyncWriteExt;
    let mut cmd = tokio::process::Command::new("git");
    cmd.args([
        "-c",
        "credential.interactive=false",
        "-c",
        "core.askPass=",
        "credential",
        "fill",
    ])
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GCM_INTERACTIVE", "never")
    .env_remove("GIT_ASKPASS")
    .env_remove("SSH_ASKPASS")
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null())
    .kill_on_drop(true);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let run = async {
        let mut child = cmd.spawn().ok()?;
        let mut stdin = child.stdin.take()?;
        stdin
            .write_all(b"protocol=https\nhost=github.com\n\n")
            .await
            .ok()?;
        drop(stdin);
        child.wait_with_output().await.ok()
    };
    let output = tokio::time::timeout(LOCAL_CREDENTIAL_TIMEOUT, run)
        .await
        .ok()??;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("password="))
        .and_then(plausible_token)
}

// ── repositories ───────────────────────────────────────────────────────────

/// `owner/name` with GitHub's allowed characters — the only repository form
/// accepted from a client (it becomes part of an API path).
pub fn valid_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    [owner, name].iter().all(|part| {
        !part.is_empty()
            && *part != "."
            && *part != ".."
            && !part.starts_with('-')
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    })
}

/// `owner/name` of a github.com remote URL (https, ssh, or scp-like); `None`
/// for any other host.
pub fn parse_github_url(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = match rest.split_once('@') {
        Some((user, host_path)) if !user.contains('/') => host_path,
        _ => rest,
    };
    let sep = rest.find(['/', ':'])?;
    let host = rest[..sep].to_ascii_lowercase();
    if !matches!(
        host.as_str(),
        "github.com" | "www.github.com" | "ssh.github.com"
    ) {
        return None;
    }
    let mut path = &rest[sep + 1..];
    // `ssh://git@ssh.github.com:443/owner/name` — skip a numeric port.
    if rest[sep..].starts_with(':')
        && let Some((port, after)) = path.split_once('/')
        && !port.is_empty()
        && port.chars().all(|c| c.is_ascii_digit())
    {
        path = after;
    }
    let path = path.trim_start_matches('/').trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    valid_repo(path).then(|| path.to_string())
}

/// Pick the checkout's GitHub repository from
/// `git config --get-regexp '^remote\..*\.(url|gh-resolved)$'` output:
/// the `gh repo set-default` choice first, then gh's own remote-name
/// preference (`upstream`, `github`, `origin`), then the first github.com
/// remote.
pub fn pick_github_repo(config: &str) -> Option<String> {
    struct Remote {
        name: String,
        repo: Option<String>,
        resolved: Option<String>,
    }
    let mut remotes: Vec<Remote> = Vec::new();
    for line in config.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        let Some((name, field)) = key
            .strip_prefix("remote.")
            .and_then(|key| key.rsplit_once('.'))
        else {
            continue;
        };
        let at = match remotes.iter().position(|remote| remote.name == name) {
            Some(at) => at,
            None => {
                remotes.push(Remote {
                    name: name.to_string(),
                    repo: None,
                    resolved: None,
                });
                remotes.len() - 1
            }
        };
        match field {
            "url" if remotes[at].repo.is_none() => remotes[at].repo = parse_github_url(value),
            "gh-resolved" => remotes[at].resolved = Some(value.trim().to_string()),
            _ => {}
        }
    }
    for remote in &remotes {
        match remote.resolved.as_deref() {
            Some("base") if remote.repo.is_some() => return remote.repo.clone(),
            Some(explicit) if valid_repo(explicit) => return Some(explicit.to_string()),
            _ => {}
        }
    }
    for preferred in ["upstream", "github", "origin"] {
        if let Some(repo) = remotes
            .iter()
            .find(|remote| remote.name == preferred)
            .and_then(|remote| remote.repo.clone())
        {
            return Some(repo);
        }
    }
    remotes.into_iter().find_map(|remote| remote.repo)
}

/// The GitHub repository of the checkout at `root`, if any.
async fn github_repo(root: &Path) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(["config", "--get-regexp", r"^remote\..*\.(url|gh-resolved)$"])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    // Exit 1 = no remote configured at all.
    output
        .status
        .success()
        .then(|| pick_github_repo(&String::from_utf8_lossy(&output.stdout)))
        .flatten()
}

// ── shaping ────────────────────────────────────────────────────────────────

/// Assigned rows, then exact-number rows, then the rest — deduped by number.
fn merge_issues(
    assigned: Vec<RestIssue>,
    exact: Vec<RestIssue>,
    rest: Vec<RestIssue>,
) -> Vec<GithubIssueSummary> {
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    let tagged = assigned
        .into_iter()
        .map(|issue| (issue, true))
        .chain(exact.into_iter().chain(rest).map(|issue| (issue, false)));
    for (issue, assigned) in tagged {
        if seen.insert(issue.number) {
            rows.push(issue.summary(assigned));
        }
    }
    rows
}

fn bounded(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push_str("… [truncated]");
    out
}

fn snapshot_from(
    repo: &str,
    issue: RestIssue,
    comments: Vec<RestComment>,
    total: usize,
) -> GithubIssueSnapshot {
    let total = total.max(comments.len());
    let mut kept = Vec::new();
    let mut used = 0usize;
    for comment in comments.into_iter().rev() {
        let body = bounded(
            comment.body.as_deref().unwrap_or_default(),
            COMMENT_MAX_CHARS,
        );
        let cost = body.chars().count();
        if used + cost > COMMENTS_BUDGET_CHARS {
            break;
        }
        used += cost;
        kept.push(GithubIssueComment {
            author: login(comment.user),
            created_at: comment.created_at,
            body,
        });
    }
    kept.reverse();
    GithubIssueSnapshot {
        repo: repo.to_string(),
        number: issue.number,
        title: issue.title,
        state: issue.state,
        url: issue.html_url,
        author: login(issue.user),
        labels: issue.labels.into_iter().map(|label| label.name).collect(),
        body: bounded(issue.body.as_deref().unwrap_or_default(), BODY_MAX_CHARS),
        omitted_comments: total - kept.len(),
        comments: kept,
    }
}

#[cfg(test)]
mod tests;
