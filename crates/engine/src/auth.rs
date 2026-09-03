//! Auth — the engine owns the WorkOS session for its device (feature-inventory §3.7,
//! ARCHITECTURE §5). Port of zeron's `apps/backend/src/auth.ts`.
//!
//! The engine is a public client: it builds the AuthKit authorize URL itself but
//! delegates the secret-bearing **code exchange** and **refresh** to the edge Worker
//! (`/auth/exchange`, `/auth/refresh` — the WorkOS API key lives only there).
//! Every authorization attempt draws a fresh RFC 7636 PKCE verifier (stored with
//! the pending `state`), publishes its S256 `code_challenge`, and presents the
//! verifier exactly once at the exchange; cancellation erases it.
//!
//! Two modes:
//! - **Dev** (no WorkOS client id configured, or the edge reports `auth: "dev"`): always
//!   signed in; the bearer IS the configured user id (current M2/M3 behavior).
//! - **WorkOS**: authorization-code flow. Headed devices use a loopback callback server
//!   on an ephemeral port; headless devices use the paste-code flow (the redirect is the
//!   edge's hosted `/auth/cli/callback` page, which shows `state.code` to paste back via
//!   stdin or the `CompleteSignIn` RPC). The refresh token is persisted 0600 in the data
//!   dir; access tokens are cached with dual-clock expiry (monotonic AND wall, whichever
//!   aged more — see [`AccessEntry`]) and refreshed on demand plus by a background loop,
//!   so the device-room relay and room clients always dial with a live `?token=`, even
//!   on the first redial after a laptop wakes from sleep. Org onboarding: an org-less session is `NeedsOrganization`; `SelectOrg`
//!   runs an org-scoped refresh and the state follows the returned token's `org_id`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

use crate::EngineError;

const SIGN_IN_TTL: Duration = Duration::from_secs(15 * 60);
/// Refresh when the cached token has less than this much life left.
const TOKEN_SLACK: Duration = Duration::from_secs(30);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// Wire types (feature-inventory §2 AuthRpc)
// ---------------------------------------------------------------------------

/// Wire-identical to [`cypher_proto::UserProfile`]: camelCase on the wire and
/// in `session.json` (`avatarUrl`), with `id`/`email`/`name` unchanged by the
/// rename. An old session.json written before the avatar field stays readable
/// via `#[serde(default)]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthUser {
    pub id: String,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// GitHub/WorkOS profile picture (edge's `profilePictureUrl`), when the
    /// identity provider returned one. Load failure falls back to the
    /// initial-letter avatar in the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgMembership {
    pub id: String,
    pub organization_id: String,
    pub name: String,
}

/// AuthStatus stream payload (`SignedOut | NeedsOrganization{user} |
/// SignedIn{user, orgId?}`). Serializes as the canonical [`cypher_proto::AuthState`]
/// wire shape (`{"state": "signedIn", …}`) so every client parses one form.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthState {
    SignedOut,
    NeedsOrganization {
        user: AuthUser,
    },
    SignedIn {
        user: AuthUser,
        org_id: Option<String>,
    },
}

impl AuthState {
    pub fn is_signed_in(&self) -> bool {
        matches!(self, AuthState::SignedIn { .. })
    }

    pub fn org_id(&self) -> Option<&str> {
        match self {
            AuthState::SignedIn { org_id, .. } => org_id.as_deref(),
            _ => None,
        }
    }

    pub fn user(&self) -> Option<&AuthUser> {
        match self {
            AuthState::SignedIn { user, .. } | AuthState::NeedsOrganization { user } => Some(user),
            AuthState::SignedOut => None,
        }
    }

    /// The proto wire twin — the one shape the engine emits over AuthStatus.
    pub fn to_proto(&self) -> cypher_proto::AuthState {
        let profile = |user: &AuthUser| cypher_proto::UserProfile {
            id: user.id.clone(),
            email: user.email.clone(),
            name: user.name.clone(),
            avatar_url: user.avatar_url.clone(),
        };
        match self {
            AuthState::SignedOut => cypher_proto::AuthState::SignedOut,
            AuthState::NeedsOrganization { user } => cypher_proto::AuthState::NeedsOrganization {
                user: profile(user),
            },
            AuthState::SignedIn { user, org_id } => cypher_proto::AuthState::SignedIn {
                user: profile(user),
                org_id: org_id.clone(),
            },
        }
    }
}

impl Serialize for AuthState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_proto().serialize(serializer)
    }
}

// ---------------------------------------------------------------------------
// Config + construction
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Edge base URL (`/auth/*` routes).
    pub edge_url: String,
    /// Data dir for the persisted session (`session.json`, 0600).
    pub data_dir: PathBuf,
    /// WorkOS client id; `None` = dev mode.
    pub workos_client_id: Option<String>,
    /// WorkOS API base (authorize URL host).
    pub workos_api_base: String,
    /// Dev-mode bearer/user id (mirrors the old `ZERON_EDGE_TOKEN` behavior).
    pub dev_user_id: String,
    /// Loopback callback port; `None` = ephemeral.
    pub callback_port: Option<u16>,
}

impl AuthConfig {
    pub fn new(edge_url: impl Into<String>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            edge_url: edge_url.into(),
            data_dir: data_dir.into(),
            workos_client_id: None,
            workos_api_base: "https://api.workos.com".into(),
            dev_user_id: "dev-user".into(),
            callback_port: None,
        }
    }
}

/// The persisted session (refresh token + user + last org scope).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSession {
    refresh_token: String,
    user: AuthUser,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    org_id: Option<String>,
}

/// Access-token cache. Expiry ages the token's own lifetime (`exp - iat`) by
/// BOTH clocks, pessimistically. Monotonic alone (`Instant`) freezes across
/// system sleep (macOS `mach_absolute_time` and Linux `CLOCK_MONOTONIC` both
/// exclude suspend), so a laptop waking from hours of sleep presented a
/// wall-expired token that still read "fresh" — every room/relay redial got a
/// 401 with the same stale bearer and sync never recovered (user report).
/// Wall clock alone breaks under skewed device clocks (`exp` vs local time);
/// the elapsed-since-issue reading is skew-immune, and a BACKWARD wall step
/// (NTP correction) degrades harmlessly to the monotonic reading.
struct AccessEntry {
    token: String,
    ttl: Duration,
    got_at: Instant,
    got_wall: std::time::SystemTime,
}

impl AccessEntry {
    fn fresh(token: String) -> Self {
        let ttl = jwt_claims(&token)
            .and_then(|c| match (c.exp, c.iat) {
                (Some(exp), Some(iat)) if exp > iat => {
                    Some(Duration::from_secs((exp - iat) as u64))
                }
                _ => None,
            })
            .unwrap_or(Duration::from_secs(240));
        Self {
            token,
            ttl,
            got_at: Instant::now(),
            got_wall: std::time::SystemTime::now(),
        }
    }

    fn remaining(&self) -> Duration {
        let monotonic = self.got_at.elapsed();
        let wall = std::time::SystemTime::now()
            .duration_since(self.got_wall)
            .unwrap_or(Duration::ZERO);
        self.ttl.saturating_sub(monotonic.max(wall))
    }
}

struct AuthInner {
    config: AuthConfig,
    /// `Some(client_id)` = WorkOS mode; `None` = dev mode.
    workos: Option<String>,
    /// Whether construction loaded a parseable WorkOS session. This is an
    /// immutable startup fact: refresh or sign-out must not rewrite it.
    loaded_workos_session: bool,
    http: reqwest::Client,
    state_tx: watch::Sender<AuthState>,
    token_tx: watch::Sender<u64>,
    stored: Mutex<Option<StoredSession>>,
    access: Mutex<Option<AccessEntry>>,
    /// Pending OAuth states plus the cancellation generation that fences code
    /// exchanges already in flight when sign-out occurs.
    sign_in: Mutex<SignInLifecycle>,
    /// Single-flight refresh: WorkOS refresh tokens are single-use (rotated per
    /// exchange); two concurrent refreshes would race and could revoke the session.
    refresh_gate: tokio::sync::Mutex<()>,
    /// Loopback callback listener port, bound lazily on the first headed sign-in.
    loopback: tokio::sync::Mutex<Option<u16>>,
}

/// A pending authorization attempt: the RFC 7636 PKCE verifier bound to the
/// OAuth `state`, plus when it was started (TTL is [`SIGN_IN_TTL`]). The
/// verifier is consumed exactly once together with the state — the same
/// `take_pending` call removes both, so a replayed callback can never reuse a
/// verifier and a canceled sign-in can never exchange with one.
struct PendingSignIn {
    verifier: String,
    at: Instant,
}

#[derive(Default)]
struct SignInLifecycle {
    generation: u64,
    /// `state` → the pending attempt it fences.
    pending: HashMap<String, PendingSignIn>,
    /// `state` → a WorkOS authentication paused for email verification.
    email_verification: HashMap<String, PendingEmailVerification>,
}

struct PendingEmailVerification {
    generation: u64,
    pending_authentication_token: String,
    email: Option<String>,
    at: Instant,
}

/// The auth service — cheap to clone by `Arc`.
#[derive(Clone)]
pub struct Auth {
    inner: Arc<AuthInner>,
}

impl Auth {
    /// Build from config: dev mode unless a WorkOS client id is configured.
    pub fn new(config: AuthConfig) -> Self {
        let workos = config
            .workos_client_id
            .clone()
            .filter(|s| !s.trim().is_empty());
        let session_file = config.data_dir.join("session.json");
        let stored: Option<StoredSession> = if workos.is_some() {
            std::fs::read_to_string(&session_file)
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
        } else {
            None
        };
        // Sanitize the loaded session's avatar exactly once, BEFORE it feeds
        // either the initial state or `inner.stored`: an unsanitized value kept
        // in memory could be re-emitted by a later org-changing refresh. Persist
        // the cleaned session too, so disk and memory agree from the first load.
        let stored = stored.map(|mut session| {
            let cleaned = sanitize_avatar_url(session.user.avatar_url.clone());
            if session.user.avatar_url != cleaned {
                session.user.avatar_url = cleaned;
                if let Ok(bytes) = serde_json::to_vec(&session) {
                    if let Err(err) = write_private(&session_file, &bytes) {
                        tracing::warn!(error = %err, "auth: failed to persist cleaned session");
                    }
                }
            }
            session
        });
        let initial = match (&workos, &stored) {
            (None, _) => AuthState::SignedIn {
                user: AuthUser {
                    id: config.dev_user_id.clone(),
                    email: config.dev_user_id.clone(),
                    name: None,
                    avatar_url: None,
                },
                org_id: None,
            },
            (Some(_), Some(session)) => state_for(session.user.clone(), session.org_id.clone()),
            (Some(_), None) => AuthState::SignedOut,
        };
        let loaded_workos_session = workos.is_some() && stored.is_some();
        let (state_tx, _) = watch::channel(initial);
        let (token_tx, _) = watch::channel(0);
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            inner: Arc::new(AuthInner {
                config,
                workos,
                loaded_workos_session,
                http,
                state_tx,
                token_tx,
                stored: Mutex::new(stored),
                access: Mutex::new(None),
                sign_in: Mutex::new(SignInLifecycle::default()),
                refresh_gate: tokio::sync::Mutex::new(()),
                loopback: tokio::sync::Mutex::new(None),
            }),
        }
    }

    /// Like [`Auth::new`], but additionally probes `{edge}/health`: an edge running in
    /// dev auth mode forces dev mode even when a client id is configured (matching the
    /// edge's "bearer = user id" verification).
    pub async fn detect(mut config: AuthConfig) -> Self {
        if config.workos_client_id.is_some() {
            #[derive(Deserialize)]
            struct Health {
                auth: Option<String>,
            }
            let url = format!("{}/health", config.edge_url.trim_end_matches('/'));
            let probe = async {
                reqwest::Client::new()
                    .get(&url)
                    .timeout(Duration::from_secs(3))
                    .send()
                    .await
                    .ok()?
                    .json::<Health>()
                    .await
                    .ok()
            };
            if let Some(health) = probe.await
                && health.auth.as_deref() == Some("dev")
            {
                tracing::info!("auth: edge is in dev mode — using dev bearer");
                config.workos_client_id = None;
            }
        }
        Self::new(config)
    }

    pub fn workos_enabled(&self) -> bool {
        self.inner.workos.is_some()
    }

    /// True when construction loaded a parseable persisted WorkOS session.
    /// The value stays true even if a later refresh revokes that session.
    pub fn loaded_workos_session(&self) -> bool {
        self.inner.loaded_workos_session
    }

    /// Live auth status (current value + changes).
    pub fn watch_state(&self) -> watch::Receiver<AuthState> {
        self.inner.state_tx.subscribe()
    }

    pub fn state(&self) -> AuthState {
        self.inner.state_tx.borrow().clone()
    }

    /// The signed-in user id — the identity that scopes workspace rooms
    /// (`ws3/{orgId}/{userId}`) and local storage (`orgs/{org}/{user}/`).
    /// Dev mode mirrors the edge's dev-bearer parsing (`user@org` → `user`,
    /// a bare token IS the user id). `None` = signed out (WorkOS only).
    pub fn user_id(&self) -> Option<String> {
        if self.inner.workos.is_none() {
            let dev = &self.inner.config.dev_user_id;
            return Some(dev.split('@').next().unwrap_or(dev).to_string());
        }
        self.state().user().map(|u| u.id.clone())
    }

    /// Current bearer for edge rooms / the device relay — `None` when signed out.
    /// Dev mode: the configured user id. WorkOS: cached access token, refreshed when
    /// it has under 30s left.
    pub async fn access_token(&self) -> Option<String> {
        if self.inner.workos.is_none() {
            return Some(self.inner.config.dev_user_id.clone());
        }
        if let Some(entry) = &*lock(&self.inner.access)
            && entry.remaining() > TOKEN_SLACK
        {
            return Some(entry.token.clone());
        }
        match self.refresh(None).await {
            Ok(token) => token,
            Err(err) => {
                tracing::warn!(error = %err, "auth: refresh failed");
                None
            }
        }
    }

    /// Sleep-until-near-expiry refresh loop so long-lived dials (relay, rooms) always
    /// have a live token to present on reconnect. No-op task in dev mode.
    pub fn spawn_refresh_loop(&self) -> tokio::task::JoinHandle<()> {
        let auth = self.clone();
        tokio::spawn(async move {
            if auth.inner.workos.is_none() {
                return;
            }
            let mut state_rx = auth.watch_state();
            let mut wake = cypher_sync::wake::subscribe();
            // Exponential backoff for failed refreshes: a transient edge/WorkOS
            // outage must never turn into a tight retry loop. A session is only
            // revoked by an explicit permanent rejection, which signs out and
            // parks this loop on the state channel at the top.
            const BACKOFF_MIN: Duration = Duration::from_secs(5);
            const BACKOFF_MAX: Duration = Duration::from_secs(300);
            let mut backoff = BACKOFF_MIN;
            loop {
                if !state_rx.borrow().is_signed_in() {
                    if state_rx.changed().await.is_err() {
                        return;
                    }
                    continue;
                }
                let remaining = lock(&auth.inner.access)
                    .as_ref()
                    .map(AccessEntry::remaining)
                    .unwrap_or(Duration::ZERO);
                let wait = remaining.saturating_sub(Duration::from_secs(60));
                if wait > Duration::ZERO {
                    // Re-evaluate at least once a minute rather than parking
                    // on one long timer: tokio timers ride the monotonic
                    // clock, which excludes system suspend — a laptop waking
                    // from sleep would otherwise wait the WHOLE original
                    // duration again before noticing the (wall-expired) token.
                    let wait = wait.min(Duration::from_secs(60));
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => { continue; }
                        changed = state_rx.changed() => {
                            if changed.is_err() { return; }
                            continue;
                        }
                        // Wake: the cached token is almost certainly
                        // wall-expired — refresh NOW so the reconnecting
                        // rooms/relays dial with live credentials instead of
                        // discovering staleness one 401 at a time.
                        _ = wake.recv() => {}
                    }
                }
                if let Err(err) = auth.refresh(None).await {
                    tracing::warn!(
                        error = %err,
                        backoff_s = backoff.as_secs(),
                        "auth: background refresh failed; backing off"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                } else {
                    backoff = BACKOFF_MIN;
                }
            }
        })
    }

    // -- sign-in flows ------------------------------------------------------

    /// Begin a headed sign-in: returns the AuthKit authorize URL redirecting to our
    /// loopback callback server (bound lazily on an ephemeral port).
    pub async fn start_sign_in(&self) -> Result<String, EngineError> {
        if self.inner.workos.is_none() {
            return Ok(String::new()); // dev mode: nothing to do (TS parity)
        }
        let port = self.ensure_loopback().await?;
        Ok(self.begin_sign_in(&format!("http://127.0.0.1:{port}/callback")))
    }

    /// Begin a headless sign-in: the redirect is the edge's hosted paste-code page —
    /// nothing ever redirects to this machine, so the browser can be anywhere.
    pub fn start_headless_sign_in(&self) -> String {
        if self.inner.workos.is_none() {
            return String::new();
        }
        let edge = self.inner.config.edge_url.trim_end_matches('/');
        self.begin_sign_in(&format!("{edge}/auth/cli/callback"))
    }

    /// Finish a headless sign-in with the pasted `state.code` string. The state half
    /// must match a sign-in started HERE (same CSRF discipline as the loopback flow).
    pub async fn complete_sign_in(&self, pasted: &str) -> Result<(), EngineError> {
        if self.inner.workos.is_none() {
            return Ok(());
        }
        let trimmed = pasted.trim();
        let (state, code) = trimmed.split_once('.').unwrap_or(("", ""));
        if state.is_empty() || code.is_empty() {
            return Err(EngineError::Other(
                "invalid or expired sign-in code — start sign-in again and paste the full code"
                    .into(),
            ));
        }
        let Some((generation, verifier)) = self.take_pending(state) else {
            return Err(EngineError::Other(
                "invalid or expired sign-in code — start sign-in again and paste the full code"
                    .into(),
            ));
        };
        match self.exchange_code(code, &verifier).await? {
            ExchangeOutcome::Complete(result) => self.finish_sign_in(result, generation),
            ExchangeOutcome::EmailVerificationRequired {
                pending_authentication_token,
                email,
            } => {
                self.store_email_verification(
                    state,
                    generation,
                    pending_authentication_token,
                    email,
                );
                Err(EngineError::Other(
                    "email verification required — enter the six-digit code from your email".into(),
                ))
            }
        }
    }

    /// Whether a headless or headed sign-in is waiting for WorkOS's emailed
    /// verification code. This is intentionally a boolean so the pending
    /// authentication token never crosses an RPC or log boundary.
    pub fn email_verification_pending(&self) -> bool {
        let mut sign_in = lock(&self.inner.sign_in);
        let now = Instant::now();
        sign_in
            .email_verification
            .retain(|_, pending| now.duration_since(pending.at) < SIGN_IN_TTL);
        !sign_in.email_verification.is_empty()
    }

    /// Complete the one pending headless email-verification continuation.
    /// The token is kept in memory only and is removed after a successful
    /// exchange (or sign-out); a bad code leaves it available for retry.
    pub async fn complete_email_verification(&self, code: &str) -> Result<(), EngineError> {
        let code = code.trim();
        if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
            return Err(EngineError::Other(
                "email verification code must be six digits".into(),
            ));
        }
        let (state, generation, token) = {
            let mut sign_in = lock(&self.inner.sign_in);
            let now = Instant::now();
            sign_in
                .email_verification
                .retain(|_, pending| now.duration_since(pending.at) < SIGN_IN_TTL);
            let Some((state, pending)) = sign_in.email_verification.iter().next() else {
                return Err(EngineError::Other(
                    "no email verification is pending — start sign-in again".into(),
                ));
            };
            (
                state.clone(),
                pending.generation,
                pending.pending_authentication_token.clone(),
            )
        };
        let result = self.exchange_email_verification(&token, code).await?;
        self.finish_sign_in(result, generation)?;
        self.clear_email_verification(&state);
        Ok(())
    }

    pub fn sign_out(&self) {
        let mut sign_in = lock(&self.inner.sign_in);
        sign_in.generation = sign_in.generation.wrapping_add(1);
        sign_in.pending.clear();
        sign_in.email_verification.clear();
        *lock(&self.inner.stored) = None;
        *lock(&self.inner.access) = None;
        self.persist::<&StoredSession>(None);
        self.inner.state_tx.send_replace(AuthState::SignedOut);
        self.inner
            .token_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }

    // -- organizations ------------------------------------------------------

    pub async fn list_orgs(&self) -> Result<Vec<OrgMembership>, EngineError> {
        if self.inner.workos.is_none() {
            return Ok(Vec::new());
        }
        #[derive(Deserialize)]
        struct Orgs {
            #[serde(default)]
            orgs: Vec<OrgMembership>,
        }
        let body: Orgs = self
            .authed_json(reqwest::Method::GET, "/auth/orgs", None)
            .await?;
        Ok(body.orgs)
    }

    /// Create an org (the edge makes us its first admin member) and scope to it.
    pub async fn create_org(&self, name: &str) -> Result<(), EngineError> {
        if self.inner.workos.is_none() {
            return Ok(());
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Created {
            organization_id: String,
        }
        let created: Created = self
            .authed_json(
                reqwest::Method::POST,
                "/auth/orgs",
                Some(serde_json::json!({ "name": name })),
            )
            .await?;
        self.select_org(&created.organization_id).await
    }

    /// Scope the session to an org: one refresh with `organizationId`; the state follows
    /// the returned token's `org_id` claim.
    pub async fn select_org(&self, organization_id: &str) -> Result<(), EngineError> {
        if self.inner.workos.is_none() {
            return Ok(());
        }
        let token = self.refresh(Some(organization_id)).await?;
        let scoped = token
            .as_deref()
            .and_then(jwt_claims)
            .and_then(|c| c.org_id)
            .is_some_and(|org| org == organization_id);
        if !scoped {
            return Err(EngineError::Other(
                "could not switch to that workspace — you may no longer be a member".into(),
            ));
        }
        Ok(())
    }

    // -- internals ----------------------------------------------------------

    fn begin_sign_in(&self, redirect_uri: &str) -> String {
        let state = uuid::Uuid::new_v4().to_string();
        // RFC 7636 §4: every authorization attempt draws a fresh CSPRNG
        // verifier and sends its S256 challenge up front; the verifier itself
        // never leaves this device until the code exchange.
        let verifier = new_pkce_verifier();
        let challenge = pkce_s256_challenge(&verifier);
        {
            let mut sign_in = lock(&self.inner.sign_in);
            let cutoff = Instant::now();
            sign_in
                .pending
                .retain(|_, pending| cutoff.duration_since(pending.at) < SIGN_IN_TTL);
            sign_in.pending.insert(
                state.clone(),
                PendingSignIn {
                    verifier,
                    at: cutoff,
                },
            );
        }
        let client_id = self.inner.workos.clone().unwrap_or_default();
        // GitHub-only: pin `provider` to the exact `GitHubOAuth` so the app
        // flow can never fall back to AuthKit's email/SSO screen. Callback and
        // PKCE are unaffected by the provider pin; an unverified GitHub email
        // may continue through the local email-verification form below.
        format!(
            "{}/user_management/authorize?response_type=code&client_id={}&redirect_uri={}&provider=GitHubOAuth&state={}&code_challenge={}&code_challenge_method=S256",
            self.inner.config.workos_api_base.trim_end_matches('/'),
            url_encode(&client_id),
            url_encode(redirect_uri),
            state,
            challenge
        )
    }

    /// Consume a pending sign-in state (and its PKCE verifier) and capture the
    /// cancellation generation. `None` means unknown/expired (CSRF check). The
    /// verifier leaves with the state: the same state can never be exchanged
    /// twice, and an unmatched verifier is never recoverable.
    fn take_pending(&self, state: &str) -> Option<(u64, String)> {
        let mut sign_in = lock(&self.inner.sign_in);
        let now = Instant::now();
        sign_in
            .pending
            .retain(|_, pending| now.duration_since(pending.at) < SIGN_IN_TTL);
        let pending = sign_in.pending.remove(state)?;
        Some((sign_in.generation, pending.verifier))
    }

    fn store_email_verification(
        &self,
        state: &str,
        generation: u64,
        pending_authentication_token: String,
        email: Option<String>,
    ) {
        lock(&self.inner.sign_in).email_verification.insert(
            state.to_string(),
            PendingEmailVerification {
                generation,
                pending_authentication_token,
                email,
                at: Instant::now(),
            },
        );
    }

    fn pending_email_verification(&self, state: &str) -> Option<(u64, String, Option<String>)> {
        let mut sign_in = lock(&self.inner.sign_in);
        let now = Instant::now();
        sign_in
            .email_verification
            .retain(|_, pending| now.duration_since(pending.at) < SIGN_IN_TTL);
        let pending = sign_in.email_verification.get(state)?;
        Some((
            pending.generation,
            pending.pending_authentication_token.clone(),
            pending.email.clone(),
        ))
    }

    fn clear_email_verification(&self, state: &str) {
        lock(&self.inner.sign_in).email_verification.remove(state);
    }

    async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
    ) -> Result<ExchangeOutcome, EngineError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WireUser {
            id: String,
            email: String,
            #[serde(default)]
            first_name: Option<String>,
            #[serde(default)]
            last_name: Option<String>,
            #[serde(default)]
            profile_picture_url: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Exchange {
            user: WireUser,
            access_token: String,
            refresh_token: String,
        }
        let url = format!(
            "{}/auth/exchange",
            self.inner.config.edge_url.trim_end_matches('/')
        );
        let res = self
            .inner
            .http
            .post(&url)
            .json(&serde_json::json!({
                "code": code,
                // RFC 7636 §4.5: the verifier is presented exactly once, at
                // the exchange, to the edge that saw the challenge.
                "codeVerifier": verifier
            }))
            .send()
            .await
            .map_err(|e| EngineError::Other(format!("the edge is unreachable: {e}")))?;
        if res.status() == reqwest::StatusCode::CONFLICT {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Verification {
                code: String,
                pending_authentication_token: String,
                #[serde(default)]
                email: Option<String>,
            }
            let body: Verification = res
                .json()
                .await
                .map_err(|e| EngineError::Other(format!("malformed verification response: {e}")))?;
            if body.code == "email_verification_required"
                && !body.pending_authentication_token.is_empty()
            {
                return Ok(ExchangeOutcome::EmailVerificationRequired {
                    pending_authentication_token: body.pending_authentication_token,
                    email: body.email,
                });
            }
            return Err(EngineError::Other(
                "sign-in returned an invalid email-verification response".into(),
            ));
        }
        if !res.status().is_success() {
            return Err(EngineError::Other(format!(
                "sign-in failed during token exchange ({}) — the code may have expired; start again",
                res.status().as_u16()
            )));
        }
        let body: Exchange = res
            .json()
            .await
            .map_err(|e| EngineError::Other(format!("malformed exchange response: {e}")))?;
        let name = [body.user.first_name, body.user.last_name]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(ExchangeOutcome::Complete(SignInResult {
            user: AuthUser {
                id: body.user.id,
                email: body.user.email,
                name: (!name.is_empty()).then_some(name),
                avatar_url: sanitize_avatar_url(body.user.profile_picture_url),
            },
            access_token: body.access_token,
            refresh_token: body.refresh_token,
        }))
    }

    async fn exchange_email_verification(
        &self,
        pending_authentication_token: &str,
        code: &str,
    ) -> Result<SignInResult, EngineError> {
        let url = format!(
            "{}/auth/verify-email",
            self.inner.config.edge_url.trim_end_matches('/')
        );
        let res = self
            .inner
            .http
            .post(&url)
            .json(&serde_json::json!({
                "pendingAuthenticationToken": pending_authentication_token,
                "code": code
            }))
            .send()
            .await
            .map_err(|e| EngineError::Other(format!("the edge is unreachable: {e}")))?;
        if !res.status().is_success() {
            return Err(EngineError::Other(format!(
                "email verification failed ({}) — check the code and try again",
                res.status().as_u16()
            )));
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WireUser {
            id: String,
            email: String,
            #[serde(default)]
            first_name: Option<String>,
            #[serde(default)]
            last_name: Option<String>,
            #[serde(default)]
            profile_picture_url: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Exchange {
            user: WireUser,
            access_token: String,
            refresh_token: String,
        }
        let body: Exchange = res
            .json()
            .await
            .map_err(|e| EngineError::Other(format!("malformed verification response: {e}")))?;
        let name = [body.user.first_name, body.user.last_name]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(SignInResult {
            user: AuthUser {
                id: body.user.id,
                email: body.user.email,
                name: (!name.is_empty()).then_some(name),
                avatar_url: sanitize_avatar_url(body.user.profile_picture_url),
            },
            access_token: body.access_token,
            refresh_token: body.refresh_token,
        })
    }

    fn finish_sign_in(&self, result: SignInResult, generation: u64) -> Result<(), EngineError> {
        // Serialize the final commit with sign-out. A callback can consume its
        // OAuth state and spend time exchanging the code; if cancellation wins
        // during that await, its old generation must never restore credentials.
        let sign_in = lock(&self.inner.sign_in);
        if sign_in.generation != generation {
            return Err(EngineError::Other(
                "sign-in was canceled — start again from Cypher".into(),
            ));
        }
        let org_id = jwt_claims(&result.access_token).and_then(|c| c.org_id);
        *lock(&self.inner.access) = Some(AccessEntry::fresh(result.access_token));
        let session = StoredSession {
            refresh_token: result.refresh_token,
            user: result.user.clone(),
            org_id: org_id.clone(),
        };
        self.persist(Some(&session));
        mark_sync_rejoin(&self.inner.config.data_dir);
        *lock(&self.inner.stored) = Some(session);
        tracing::info!(email = %result.user.email, org = org_id.as_deref().unwrap_or("<none>"),
            "auth: signed in");
        self.inner
            .state_tx
            .send_replace(state_for(result.user, org_id));
        self.inner
            .token_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
        Ok(())
    }

    /// Refresh the session (single-flight). `organization_id` migrates the WorkOS
    /// session to that org; routine refreshes keep the current scope. Returns the new
    /// access token, `None` when signed out / the refresh could not run.
    async fn refresh(&self, organization_id: Option<&str>) -> Result<Option<String>, EngineError> {
        let _gate = self.inner.refresh_gate.lock().await;
        // Re-check under the gate: the refresh we queued behind may have done the work.
        if organization_id.is_none()
            && let Some(entry) = &*lock(&self.inner.access)
            && entry.remaining() > TOKEN_SLACK
        {
            return Ok(Some(entry.token.clone()));
        }
        let Some(refresh_token) = lock(&self.inner.stored)
            .as_ref()
            .map(|s| s.refresh_token.clone())
        else {
            return Ok(None);
        };
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct RefreshBody<'a> {
            refresh_token: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            organization_id: Option<&'a str>,
        }
        let url = format!(
            "{}/auth/refresh",
            self.inner.config.edge_url.trim_end_matches('/')
        );
        let res = self
            .inner
            .http
            .post(&url)
            .json(&RefreshBody {
                refresh_token: &refresh_token,
                organization_id,
            })
            .send()
            .await;
        let res = match res {
            Ok(res) => res,
            Err(err) => {
                // Network failure is transient: keep the session, but surface the
                // error so the background loop applies its retry delay.
                return Err(EngineError::Other(format!(
                    "could not reach the edge during refresh: {err}"
                )));
            }
        };
        let status = res.status().as_u16();
        if !res.status().is_success() {
            // Permanent rejection requires BOTH an HTTP 401 AND a stable machine
            // `code` that means the refresh token itself is dead (revoked session,
            // deleted user) — it can NEVER succeed again, so degrade to SignedOut
            // and every downstream retry loop quiets down. Everything else — 429
            // rate limits, 502/503 upstream/network failures, or even a 401 whose
            // body carries no recognized code — is transient: the session survives
            // and the caller backs off and retries. This applies to org-switch
            // refreshes too: an `invalid_grant` is a dead refresh token no matter
            // what scope the attempt carried, while a "not a member" rejection
            // surfaces its own (non-permanent) code and stays an error.
            let code = res
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_owned));
            if status == 401 && code.as_deref().is_some_and(is_permanent_refresh_rejection) {
                tracing::warn!(
                    status,
                    code = code.as_deref().unwrap_or(""),
                    "auth: refresh rejected — session revoked; signing out"
                );
                self.sign_out();
                return Ok(None);
            }
            return Err(EngineError::Other(format!("refresh failed ({status})")));
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RefreshUser {
            #[serde(default)]
            profile_picture_url: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Tokens {
            access_token: String,
            refresh_token: String,
            /// Absent = the edge predates avatar sync (preserve the stored
            /// avatar); present = replace/clear from `profilePictureUrl`.
            #[serde(default)]
            user: Option<RefreshUser>,
        }
        let tokens: Tokens = res
            .json()
            .await
            .map_err(|e| EngineError::Other(format!("malformed refresh response: {e}")))?;
        let org_id = jwt_claims(&tokens.access_token).and_then(|c| c.org_id);
        let entry = AccessEntry::fresh(tokens.access_token.clone());
        tracing::info!(ttl_s = entry.ttl.as_secs(), "auth: access token refreshed");
        *lock(&self.inner.access) = Some(entry);
        // `None` (the refresh carried no user) → PRESERVE the stored avatar;
        // `Some(None)` (user present, no/sanitized-away picture) → CLEAR it.
        let avatar_update: Option<Option<String>> = tokens
            .user
            .as_ref()
            .map(|u| sanitize_avatar_url(u.profile_picture_url.clone()));
        let (user, org_changed, avatar_changed) = {
            let mut stored = lock(&self.inner.stored);
            match stored.as_mut() {
                Some(session) => {
                    let changed = session.org_id != org_id;
                    session.refresh_token = tokens.refresh_token;
                    session.org_id = org_id.clone();
                    let mut avatar_changed = false;
                    if let Some(avatar) = avatar_update
                        && session.user.avatar_url != avatar
                    {
                        session.user.avatar_url = avatar;
                        avatar_changed = true;
                    }
                    (session.user.clone(), changed, avatar_changed)
                }
                None => return Ok(None), // signed out mid-refresh
            }
        };
        self.persist(lock(&self.inner.stored).as_ref());
        // Emit when the profile (avatar) or scope changed so already-signed-in
        // surfaces update without a re-login.
        if org_changed || avatar_changed {
            self.inner.state_tx.send_replace(state_for(user, org_id));
        }
        self.inner
            .token_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
        Ok(Some(tokens.access_token))
    }

    fn session_file(&self) -> PathBuf {
        self.inner.config.data_dir.join("session.json")
    }

    /// Persist (0600) or remove the stored session. Never panics: a disk error degrades
    /// to a logged warning, not a crash mid-refresh.
    fn persist<S: std::borrow::Borrow<StoredSession>>(&self, session: Option<S>) {
        let path = self.session_file();
        let outcome = match session {
            Some(session) => serde_json::to_vec(session.borrow())
                .map_err(std::io::Error::other)
                .and_then(|bytes| write_private(&path, &bytes)),
            None => match std::fs::remove_file(&path) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            },
        };
        if let Err(err) = outcome {
            tracing::warn!(error = %err, "auth: failed to persist session");
        }
    }

    async fn authed_json<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T, EngineError> {
        let token = self
            .access_token()
            .await
            .ok_or_else(|| EngineError::Other("not signed in".into()))?;
        let url = format!(
            "{}{}",
            self.inner.config.edge_url.trim_end_matches('/'),
            path
        );
        let mut req = self.inner.http.request(method, &url).bearer_auth(token);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req
            .send()
            .await
            .map_err(|e| EngineError::Other(format!("the edge is unreachable: {e}")))?;
        if !res.status().is_success() {
            return Err(EngineError::Other(format!(
                "workspace request failed ({})",
                res.status().as_u16()
            )));
        }
        res.json::<T>()
            .await
            .map_err(|e| EngineError::Other(format!("malformed response: {e}")))
    }

    // -- loopback callback server ------------------------------------------

    /// Bind the loopback callback listener (idempotent); returns its port.
    async fn ensure_loopback(&self) -> Result<u16, EngineError> {
        let mut slot = self.inner.loopback.lock().await;
        if let Some(port) = *slot {
            return Ok(port);
        }
        let requested = self.inner.config.callback_port.unwrap_or(0);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", requested))
            .await
            .map_err(|e| EngineError::Other(format!("sign-in callback bind failed: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| EngineError::Other(format!("sign-in callback addr: {e}")))?
            .port();
        *slot = Some(port);
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(loopback_loop(listener, weak));
        tracing::info!(port, "auth: sign-in callback listening");
        Ok(port)
    }
}

struct SignInResult {
    user: AuthUser,
    access_token: String,
    refresh_token: String,
}

enum ExchangeOutcome {
    Complete(SignInResult),
    EmailVerificationRequired {
        pending_authentication_token: String,
        email: Option<String>,
    },
}

fn state_for(user: AuthUser, org_id: Option<String>) -> AuthState {
    // Every user must belong to an organization before the product opens up; an org-less
    // session is `NeedsOrganization`, which the UI gates on.
    match org_id {
        Some(org_id) => AuthState::SignedIn {
            user,
            org_id: Some(org_id),
        },
        None => AuthState::NeedsOrganization { user },
    }
}

/// Sanitize an avatar URL from ANY ingress (loaded session, exchange,
/// refresh) with REAL URL parsing (`reqwest::Url`, the `url` crate): the
/// scheme must be `https`, there must be a host, there must be no embedded
/// credentials (username/password), and the string is bounded to 2048 chars
/// AND bytes. Everything else — malformed URLs, bad ports, whitespace,
/// control chars, non-HTTPS schemes — → `None`. A stored value is never
/// trusted as-is; every boundary re-validates, so a non-URL can never be
/// interpreted as a local file and nothing can exceed the renderer's bound.
fn sanitize_avatar_url(value: Option<String>) -> Option<String> {
    let url = value?;
    if url.is_empty() || url.len() > 2048 || url.chars().count() > 2048 {
        return None;
    }
    // The parser percent-encodes whitespace/control chars rather than failing;
    // the raw string must never carry them (a stored avatar URL is fetched
    // verbatim).
    if url.chars().any(char::is_whitespace) || url.bytes().any(|b| b < 0x20) {
        return None;
    }
    let parsed = reqwest::Url::parse(&url).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    // `host_str()` requires a real host and rejects empty/malformed ones;
    // userinfo (username/password) must never ride along.
    if parsed.host_str()?.is_empty() || !parsed.username().is_empty() || parsed.password().is_some()
    {
        return None;
    }
    // The WHATWG parser "ignores slashes" right after `https://` —
    // `https:///a.png` silently normalizes to host `a.png` with an EMPTY
    // authority in the input (and `https:////a.png` similarly). A real
    // avatar URL names its host explicitly, so reject that shape.
    let rest = url.get(url.find(':')? + 1..)?.strip_prefix("//")?;
    if rest.is_empty() || rest.starts_with('/') {
        return None;
    }
    Some(url)
}

/// The relay/room token seam: `Auth` IS a [`cypher_rpc::TokenSource`], so the host relay
/// and link cache always dial with a fresh bearer after refreshes.
#[async_trait::async_trait]
impl cypher_rpc::TokenSource for Auth {
    async fn token(&self) -> Option<String> {
        if self.inner.workos.is_some() && !self.state().is_signed_in() {
            return None;
        }
        self.access_token().await
    }

    fn subscribe(&self) -> Option<watch::Receiver<u64>> {
        Some(self.inner.token_tx.subscribe())
    }
}

// ---------------------------------------------------------------------------
// Loopback HTTP (hand-rolled: no HTTP server dependency in the engine)
// ---------------------------------------------------------------------------

async fn loopback_loop(listener: tokio::net::TcpListener, inner: Weak<AuthInner>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let Some(inner) = inner.upgrade() else { break };
        tokio::spawn(async move {
            if let Err(err) = handle_loopback_conn(stream, Auth { inner }).await {
                tracing::debug!(error = %err, "auth: callback connection failed");
            }
        });
    }
}

async fn handle_loopback_conn(
    mut stream: tokio::net::TcpStream,
    auth: Auth,
) -> Result<(), std::io::Error> {
    // Read the request head (bounded; verification submissions also carry a
    // small URL-encoded body).
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "header read"))??;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16 * 1024 {
            break;
        }
    }
    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|offset| offset + 4)
        .unwrap_or(buf.len());
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let (method, target) = {
        let request_line = head.lines().next().unwrap_or_default();
        (
            request_line
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_owned(),
            request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_owned(),
        )
    };
    let content_length = head
        .lines()
        .find_map(|line| {
            line.strip_prefix("Content-Length:")
                .or_else(|| line.strip_prefix("content-length:"))
        })
        .and_then(|length| length.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > 16 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "request body too large",
        ));
    }
    let body_start = header_end;
    while buf.len() < body_start + content_length {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "body read"))??;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let form_body = String::from_utf8_lossy(
        &buf[body_start..buf.len().min(body_start.saturating_add(content_length))],
    )
    .into_owned();
    let (path, query) = target
        .as_str()
        .split_once('?')
        .unwrap_or((target.as_str(), ""));

    let query_params: HashMap<String, String> = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), url_decode(v)))
        .collect();
    let form_params: HashMap<String, String> = form_body
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), url_decode(v)))
        .collect();
    let invalid_callback = || {
        (
            "400 Bad Request",
            page("Invalid or expired sign-in link. Start again from Cypher."),
        )
    };
    let (status, body) = match path {
        "/callback" => {
            let code = query_params.get("code");
            let state = query_params.get("state");
            match (code, state) {
                (Some(code), Some(state)) => match auth.take_pending(state) {
                    Some((generation, verifier)) => {
                        match auth.exchange_code(code, &verifier).await {
                            Ok(ExchangeOutcome::Complete(result)) => {
                                match auth.finish_sign_in(result, generation) {
                                    Ok(()) => (
                                        "200 OK",
                                        page(
                                            "Signed in. You can close this tab and return to Cypher.",
                                        ),
                                    ),
                                    Err(err) => {
                                        tracing::info!(
                                            error = %err,
                                            "auth: discarded canceled callback exchange"
                                        );
                                        (
                                            "409 Conflict",
                                            page(
                                                "This sign-in was canceled. Start again from Cypher if you still want to enable sync.",
                                            ),
                                        )
                                    }
                                }
                            }
                            Ok(ExchangeOutcome::EmailVerificationRequired {
                                pending_authentication_token,
                                email,
                            }) => {
                                auth.store_email_verification(
                                    state,
                                    generation,
                                    pending_authentication_token,
                                    email.clone(),
                                );
                                ("200 OK", verification_page(state, email.as_deref(), None))
                            }
                            Err(err) => {
                                tracing::warn!(
                                    error = %err,
                                    "auth: loopback code exchange failed"
                                );
                                (
                                    "502 Bad Gateway",
                                    page(
                                        "Sign-in failed during token exchange — check the Cypher logs.",
                                    ),
                                )
                            }
                        }
                    }
                    None => invalid_callback(),
                },
                _ => invalid_callback(),
            }
        }
        "/verify" => {
            let params = if method == "POST" {
                &form_params
            } else {
                &query_params
            };
            let state = params.get("state");
            let code = params.get("code");
            match (state, code) {
                (Some(state), Some(code))
                    if code.len() == 6 && code.chars().all(|c| c.is_ascii_digit()) =>
                {
                    match auth.pending_email_verification(state) {
                        Some((generation, token, email)) => {
                            match auth.exchange_email_verification(&token, code).await {
                                Ok(result) => match auth.finish_sign_in(result, generation) {
                                    Ok(()) => {
                                        auth.clear_email_verification(state);
                                        (
                                            "200 OK",
                                            page(
                                                "Email verified and signed in. You can close this tab and return to Cypher.",
                                            ),
                                        )
                                    }
                                    Err(err) => {
                                        auth.clear_email_verification(state);
                                        tracing::info!(
                                            error = %err,
                                            "auth: discarded canceled email verification"
                                        );
                                        (
                                            "409 Conflict",
                                            page(
                                                "This sign-in was canceled. Start again from Cypher if you still want to enable sync.",
                                            ),
                                        )
                                    }
                                },
                                Err(err) => {
                                    tracing::warn!(
                                        error = %err,
                                        "auth: email verification failed"
                                    );
                                    (
                                        "400 Bad Request",
                                        verification_page(
                                            state,
                                            email.as_deref(),
                                            Some(
                                                "That code was not accepted. Check the email and try again.",
                                            ),
                                        ),
                                    )
                                }
                            }
                        }
                        None => invalid_callback(),
                    }
                }
                (Some(state), _) if auth.pending_email_verification(state).is_some() => {
                    let email = auth
                        .pending_email_verification(state)
                        .and_then(|(_, _, email)| email);
                    (
                        "400 Bad Request",
                        verification_page(
                            state,
                            email.as_deref(),
                            Some("Enter the six-digit code from your email."),
                        ),
                    )
                }
                _ => invalid_callback(),
            }
        }
        _ => ("404 Not Found", page("Not found.")),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// Shared shell for every loopback page — mirrors the edge's hosted
/// paste-code page (dark, centered card, system fonts) so the browser side of
/// sign-in looks like one product regardless of which server rendered it.
/// Inline styles only: these pages must never fetch external assets.
fn page_shell(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\
<html lang='en'>\
<head>\
<meta charset='utf-8'>\
<meta name='viewport' content='width=device-width, initial-scale=1'>\
<meta name='referrer' content='no-referrer'>\
<meta name='robots' content='noindex'>\
<title>{title}</title>\
<style>\
  body {{ margin: 0; min-height: 100vh; display: grid; place-items: center;\
         background: #0a0a0a; color: #ededed;\
         font: 15px/1.6 ui-sans-serif, system-ui, sans-serif; }}\
  main {{ max-width: 24rem; padding: 2.5rem 2rem; text-align: center; }}\
  .mark {{ width: 44px; height: 44px; margin: 0 auto 1.25rem; border-radius: 12px;\
         background: #ededed; color: #0a0a0a; display: grid; place-items: center;\
         font: 700 20px ui-sans-serif, system-ui, sans-serif; }}\
  h1 {{ font-size: 1.05rem; font-weight: 600; margin: 0 0 0.75rem; }}\
  p {{ color: #a1a1a1; margin: 0.25rem 0; }}\
  p.error {{ color: #f87171; }}\
  strong {{ color: #ededed; font-weight: 600; }}\
  form {{ margin-top: 1.25rem; }}\
  input[name=code] {{ display: block; width: 100%; box-sizing: border-box;\
         margin: 0 0 0.75rem; padding: 0.8rem 1rem; text-align: center;\
         background: #171717; border: 1px solid #2e2e2e; border-radius: 10px;\
         color: #ededed; font: 600 22px/1.4 ui-monospace, monospace;\
         letter-spacing: 0.45em; text-indent: 0.45em; outline: none; }}\
  input[name=code]:focus {{ border-color: #ededed; }}\
  button {{ width: 100%; padding: 0.7rem 1rem; border-radius: 10px; border: none;\
         background: #ededed; color: #0a0a0a; cursor: pointer;\
         font: 600 14px ui-sans-serif, system-ui, sans-serif; }}\
  button:hover {{ background: #ffffff; }}\
  .hint {{ margin-top: 1.25rem; font-size: 13px; }}\
</style>\
</head>\
<body><main><div class='mark' aria-hidden='true'>C</div>{body}</main></body>\
</html>"
    )
}

fn page(message: &str) -> String {
    // Split "Title. Rest of the message." into heading + body when possible.
    let (title, rest) = message.split_once(". ").unwrap_or((message, ""));
    let title = title.trim_end_matches('.');
    let rest = if rest.is_empty() {
        String::new()
    } else {
        format!("<p>{}</p>", escape_html(rest))
    };
    page_shell(
        &escape_html(title),
        &format!("<h1>{}</h1>{rest}", escape_html(title)),
    )
}

fn verification_page(state: &str, email: Option<&str>, error: Option<&str>) -> String {
    let email = email
        .map(escape_html)
        .unwrap_or_else(|| "your email".into());
    let error = error
        .map(|message| format!("<p class='error'>{}</p>", escape_html(message)))
        .unwrap_or_default();
    page_shell(
        "Verify your email",
        &format!(
            "<h1>Verify your email</h1>\
<p>We sent a six-digit code to <strong>{email}</strong>.</p>\
{error}\
<form action='/verify' method='post'>\
<input type='hidden' name='state' value='{}'>\
<input name='code' inputmode='numeric' autocomplete='one-time-code' \
pattern='[0-9]{{6}}' maxlength='6' placeholder='000000' required autofocus \
aria-label='Verification code'>\
<button type='submit'>Verify and sign in</button>\
</form>\
<p class='hint'>The code expires in a few minutes. Keep this tab open until you're signed in.</p>",
            escape_html(state)
        ),
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

// ---------------------------------------------------------------------------
// PKCE (RFC 7636) — verifier/challenge generation
// ---------------------------------------------------------------------------

/// A fresh RFC 7636 §4.1 verifier: 43–128 characters of the unreserved URL-safe
/// alphabet. 32 CSPRNG bytes encode to 43 base64url chars (no padding), so the
/// output is both in range and free of any characters needing URL escaping.
fn new_pkce_verifier() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS CSPRNG must be available");
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    URL_SAFE_NO_PAD.encode(bytes)
}

/// RFC 7636 §4.2 S256 challenge: `base64url(sha256(verifier))` without padding.
fn pkce_s256_challenge(verifier: &str) -> String {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use sha2::{Digest, Sha256};
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

// ---------------------------------------------------------------------------
// Small utilities (JWT claims, base64url, URL encoding, 0600 writes)
// ---------------------------------------------------------------------------

/// The stable machine codes that mean the refresh token is permanently dead.
/// The edge only ever emits `invalid_grant` (WorkOS's explicit credential
/// rejection, surfaced on `/auth/refresh`); `refresh_token_invalid` is accepted
/// as a conservative alias. Any other code — or a body with no code at all — is
/// treated as retryable: a transient hiccup must never clear a session.
fn is_permanent_refresh_rejection(code: &str) -> bool {
    matches!(code, "invalid_grant" | "refresh_token_invalid")
}

const SYNC_REJOIN_MARKER: &str = "sync-rejoin";

/// Written on a successful sign-in so the next synced runtime may revive a
/// tombstoned device row (the user is explicitly re-pairing this machine).
pub(crate) fn mark_sync_rejoin(data_dir: &Path) {
    if let Err(err) = std::fs::write(data_dir.join(SYNC_REJOIN_MARKER), b"1") {
        tracing::warn!(error = %err, "auth: failed to write sync-rejoin marker");
    }
}

/// Consume the one-shot rejoin marker. `true` = this boot should announce
/// even if the registry still has a tombstone for this device.
pub(crate) fn consume_sync_rejoin(data_dir: &Path) -> bool {
    let path = data_dir.join(SYNC_REJOIN_MARKER);
    match std::fs::remove_file(&path) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => {
            tracing::warn!(error = %err, "auth: failed to consume sync-rejoin marker");
            false
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct JwtClaims {
    #[serde(default)]
    exp: Option<i64>,
    #[serde(default)]
    iat: Option<i64>,
    #[serde(default)]
    org_id: Option<String>,
}

/// Decode (without verifying — the edge verifies) the JWT payload claims. Total: a
/// malformed token yields `None`, never a panic.
fn jwt_claims(token: &str) -> Option<JwtClaims> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    serde_json::from_slice(&bytes).ok()
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4 + 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn url_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Write a file readable only by the owner (0600). On non-unix targets a plain write.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        // An existing file keeps its old mode through OpenOptions — enforce 0600 anyway.
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        file.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trips_jwt_payload() {
        let payload = br#"{"exp":100,"iat":40,"org_id":"org_1"}"#;
        // Standard base64url without padding (as JWTs use).
        let encoded = {
            const ALPHABET: &[u8] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for chunk in payload.chunks(3) {
                let b = [
                    chunk[0],
                    *chunk.get(1).unwrap_or(&0),
                    *chunk.get(2).unwrap_or(&0),
                ];
                let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
                out.push(ALPHABET[(n >> 18) as usize & 63] as char);
                out.push(ALPHABET[(n >> 12) as usize & 63] as char);
                if chunk.len() > 1 {
                    out.push(ALPHABET[(n >> 6) as usize & 63] as char);
                }
                if chunk.len() > 2 {
                    out.push(ALPHABET[n as usize & 63] as char);
                }
            }
            out
        };
        assert_eq!(
            base64url_decode(&encoded).as_deref(),
            Some(payload.as_slice())
        );
        let token = format!("h.{encoded}.sig");
        let claims = jwt_claims(&token).expect("claims decode");
        assert_eq!(claims.exp, Some(100));
        assert_eq!(claims.iat, Some(40));
        assert_eq!(claims.org_id.as_deref(), Some("org_1"));
    }

    #[test]
    fn url_coding_round_trips() {
        let raw = "http://127.0.0.1:1234/callback?x=a b&y=%";
        assert_eq!(url_decode(&url_encode(raw)), raw);
        assert_eq!(url_encode("a b"), "a%20b");
    }

    // -- Avatar URL sanitization (every ingress) -----------------------

    #[test]
    fn sanitize_avatar_url_accepts_only_safe_https_urls() {
        let good = "https://avatars.example.com/a.png";
        assert_eq!(
            sanitize_avatar_url(Some(good.into())).as_deref(),
            Some(good)
        );
        // Query strings and paths are fine.
        assert_eq!(
            sanitize_avatar_url(Some("https://avatars.example.com/a.png?v=1&s=2".into()))
                .as_deref(),
            Some("https://avatars.example.com/a.png?v=1&s=2")
        );
        // Exactly 2048 chars is allowed.
        let at_cap = format!("https://h/{}", "a".repeat(2048 - "https://h/".len()));
        assert!(sanitize_avatar_url(Some(at_cap)).is_some());
    }

    #[test]
    fn sanitize_avatar_url_rejects_non_https_hostless_and_oversized() {
        for bad in [
            None,
            Some(String::new()),
            // Non-HTTPS schemes (including file/javascript interpretations).
            Some("http://avatars.example.com/a.png".into()),
            Some("file:///etc/passwd".into()),
            Some("javascript:alert(1)".into()),
            Some("ftp://x/a.png".into()),
            // Hostless.
            Some("https://".into()),
            Some("https:///a.png".into()),
            Some("https://?x=1".into()),
            // Embedded credentials (userinfo) must not ride along.
            Some("https://user@host/a.png".into()),
            Some("https://user:pass@host/a.png".into()),
            // Malformed ports / hosts that a hand-rolled prefix check would
            // accept but real URL parsing rejects.
            Some("https://host:99999/a.png".into()),
            Some("https://host:abc/a.png".into()),
            Some("https://:443/a.png".into()),
            Some("https://host:443:444/a.png".into()),
            // Whitespace / control characters (would break URI parsing).
            Some("https://host/a b.png".into()),
            Some("https://host/\n".into()),
            // Oversized: 2048 chars is the cap.
            Some(format!("https://host/{}", "a".repeat(2048))),
        ] {
            assert_eq!(
                sanitize_avatar_url(bad.clone()),
                None,
                "{bad:?} must be rejected"
            );
        }
    }

    // -- PKCE (RFC 7636) ------------------------------------------------

    /// Parse `k=v&k2=v2` query params with the production url-decoder.
    fn query_params(url: &str) -> HashMap<String, String> {
        url.split_once('?')
            .map(|(_, q)| q)
            .unwrap_or_default()
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), url_decode(v)))
            .collect()
    }

    #[test]
    fn pkce_verifier_is_random_url_safe_and_in_range() {
        let a = new_pkce_verifier();
        let b = new_pkce_verifier();
        for v in [&a, &b] {
            assert!(
                (43..=128).contains(&v.len()),
                "verifier must be 43-128 chars, got {}",
                v.len()
            );
            assert!(
                v.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~')),
                "verifier must be URL-safe unreserved: {v}"
            );
        }
        // Two draws must never collide: a CSPRNG, not a counter/timestamp.
        assert_ne!(a, b);
    }

    #[test]
    fn pkce_s256_challenge_matches_independent_sha256() {
        use sha2::Digest as _;
        let verifier = new_pkce_verifier();
        let challenge = pkce_s256_challenge(&verifier);
        // Independent recomputation, exactly as the WorkOS authorize endpoint
        // will: base64url(sha256(verifier)), no padding.
        let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
        hasher.update(verifier.as_bytes());
        let digest = hasher.finalize();
        use base64::Engine as _;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        assert_eq!(challenge, URL_SAFE_NO_PAD.encode(digest));
        assert_eq!(challenge.len(), 43);
    }

    #[test]
    fn authorize_url_carries_pkce_challenge_and_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = AuthConfig::new("http://edge.test", dir.path());
        config.workos_client_id = Some("client_test".into());
        let auth = Auth::new(config);
        let url = auth.start_headless_sign_in();
        assert!(url.starts_with("https://api.workos.com/user_management/authorize?"));
        let params = query_params(&url);
        assert_eq!(
            params.get("response_type").map(String::as_str),
            Some("code")
        );
        assert_eq!(
            params.get("client_id").map(String::as_str),
            Some("client_test")
        );
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        let state = params.get("state").expect("state present");
        let challenge = params.get("code_challenge").expect("challenge present");
        // The URL challenge is exactly the S256 challenge of the verifier
        // bound to that same state (proving they travel together).
        let (generation, verifier) = auth.take_pending(state).expect("pending state exists");
        assert_eq!(challenge, &pkce_s256_challenge(&verifier));
        let _ = generation;
    }

    #[test]
    fn pending_state_is_consumed_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = AuthConfig::new("http://edge.test", dir.path());
        config.workos_client_id = Some("client_test".into());
        let auth = Auth::new(config);
        let url = auth.start_headless_sign_in();
        let state = query_params(&url).remove("state").expect("state present");

        let first = auth.take_pending(&state);
        assert!(first.is_some(), "first take yields state+verifier");
        // The same state is dead now — a replayed callback can never exchange.
        assert_eq!(auth.take_pending(&state), None);
        // Unknown states were never pending.
        assert_eq!(auth.take_pending("state-that-never-existed"), None);
    }

    #[test]
    fn expired_pending_state_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = AuthConfig::new("http://edge.test", dir.path());
        config.workos_client_id = Some("client_test".into());
        let auth = Auth::new(config);
        // Backdate a pending attempt past the TTL (the test module sees the
        // private lifecycle, so it can simulate the clock).
        let mut sign_in = lock(&auth.inner.sign_in);
        sign_in.pending.insert(
            "stale-state".into(),
            PendingSignIn {
                verifier: new_pkce_verifier(),
                at: Instant::now() - SIGN_IN_TTL - Duration::from_secs(1),
            },
        );
        drop(sign_in);
        assert_eq!(auth.take_pending("stale-state"), None);
    }

    #[test]
    fn sign_out_erases_pending_verifiers() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = AuthConfig::new("http://edge.test", dir.path());
        config.workos_client_id = Some("client_test".into());
        let auth = Auth::new(config);
        let url = auth.start_headless_sign_in();
        let state = query_params(&url).remove("state").expect("state present");
        auth.sign_out();
        // Cancellation fenced the attempt: state AND verifier are gone, so a
        // late callback cannot exchange.
        assert_eq!(auth.take_pending(&state), None);
        assert!(lock(&auth.inner.sign_in).pending.is_empty());
    }

    // -- Wire-level PKCE (mock edge, real loopback listener) -------------

    /// A one-shot HTTP "edge": captures the first request body and answers
    /// with the given JSON exchange payload. Returns `(base_url, body_rx)`.
    async fn mock_edge(
        json_body: &'static str,
    ) -> (String, tokio::sync::oneshot::Receiver<String>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 2048];
            let head_end = loop {
                let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
                    .await
                    .unwrap()
                    .unwrap();
                assert!(n > 0, "edge: client hung up before the request body");
                buf.extend_from_slice(&chunk[..n]);
                if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]);
            let content_length: usize = head
                .lines()
                .find_map(|line| {
                    let (k, v) = line.split_once(':')?;
                    (k.trim().eq_ignore_ascii_case("content-length"))
                        .then(|| v.trim().parse().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while buf.len() < head_end + content_length {
                let n = stream.read(&mut chunk).await.unwrap();
                assert!(n > 0, "edge: client hung up mid-body");
                buf.extend_from_slice(&chunk[..n]);
            }
            let body =
                String::from_utf8_lossy(&buf[head_end..head_end + content_length]).into_owned();
            let _ = tx.send(body);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json_body}",
                json_body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        (format!("http://127.0.0.1:{port}"), rx)
    }

    /// `Auth` in WorkOS mode against a mock edge, with a scratch data dir.
    fn workos_auth(edge_url: &str) -> (Auth, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = AuthConfig::new(edge_url, dir.path());
        config.workos_client_id = Some("client_test".into());
        (Auth::new(config), dir)
    }

    /// `workos_auth` plus a persisted WorkOS session (refresh token + user +
    /// org), the state a signed-in device has on disk.
    fn workos_auth_with_session(edge_url: &str) -> (Auth, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let session = StoredSession {
            refresh_token: "rt-1".into(),
            user: AuthUser {
                id: "u1".into(),
                email: "a@b.c".into(),
                name: None,
                avatar_url: None,
            },
            org_id: Some("org_1".into()),
        };
        std::fs::write(
            dir.path().join("session.json"),
            serde_json::to_vec(&session).unwrap(),
        )
        .unwrap();
        let auth = Auth::new(config_with_edge(edge_url, dir.path()));
        assert!(auth.state().is_signed_in());
        (auth, dir)
    }

    fn config_with_edge(edge_url: &str, data_dir: &std::path::Path) -> AuthConfig {
        let mut config = AuthConfig::new(edge_url, data_dir);
        config.workos_client_id = Some("client_test".into());
        config
    }

    /// A one-shot HTTP "edge" answering with an arbitrary status line + body
    /// (drains the request first so the client's write completes).
    async fn mock_edge_status(status: &'static str, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 2048];
            let head_end = loop {
                let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
                    .await
                    .unwrap()
                    .unwrap();
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]);
            let content_length: usize = head
                .lines()
                .find_map(|line| {
                    let (k, v) = line.split_once(':')?;
                    (k.trim().eq_ignore_ascii_case("content-length"))
                        .then(|| v.trim().parse().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while buf.len() < head_end + content_length {
                let Ok(n) = stream.read(&mut chunk).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        format!("http://127.0.0.1:{port}")
    }

    /// A one-shot HTTP "edge" that accepts the connection and closes without
    /// answering — a transport failure (dropped connection).
    async fn mock_edge_dropped() -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            drop(stream);
        });
        format!("http://127.0.0.1:{port}")
    }

    const EXCHANGE_OK: &str = r#"{"user":{"id":"u1","email":"a@b.c","firstName":"Ann","lastName":"X","profilePictureUrl":"https://avatars.example/a.png"},"accessToken":"at","refreshToken":"rt"}"#;

    /// Drive the headed flow end-to-end: authorize URL carries the S256
    /// challenge; the loopback callback exchanges the code and the captured
    /// body proves the verifier for that exact challenge is presented exactly
    /// once — a replayed callback is rejected 400.
    #[tokio::test]
    async fn loopback_exchange_presents_verifier_for_its_challenge_once() {
        let (edge, body_rx) = mock_edge(EXCHANGE_OK).await;
        let (auth, _dir) = workos_auth(&edge);

        let url = auth.start_sign_in().await.expect("sign-in URL");
        let params = query_params(&url);
        let state = params.get("state").expect("state").clone();
        let challenge = params.get("code_challenge").expect("challenge").clone();
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        let callback = params
            .get("redirect_uri")
            .expect("loopback redirect")
            .clone();
        let port: u16 = callback
            .split('/')
            .nth(2)
            .and_then(|host| host.rsplit_once(':').map(|(_, p)| p.parse().ok()).flatten())
            .expect("loopback port");

        async fn callback_get(port: u16, code: &str, state: &str) -> String {
            let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            stream
                .write_all(
                    format!(
                        "GET /callback?code={code}&state={state} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut resp = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = stream.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                resp.extend_from_slice(&chunk[..n]);
            }
            String::from_utf8_lossy(&resp).into_owned()
        }

        let first = callback_get(port, "auth-code-1", &state).await;
        assert!(first.contains("200 OK"), "first callback: {first}");

        let body = tokio::time::timeout(Duration::from_secs(5), body_rx)
            .await
            .expect("exchange reached the edge")
            .expect("edge captured body");
        let sent: serde_json::Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(sent["code"], "auth-code-1");
        let verifier = sent["codeVerifier"].as_str().expect("codeVerifier present");
        // The verifier that left this device is the one whose S256 hash was
        // published in the authorize URL — nothing else could pass WorkOS.
        assert_eq!(challenge, pkce_s256_challenge(verifier));

        // Replay the same callback: the state+verifier pair was consumed.
        let second = callback_get(port, "auth-code-1", &state).await;
        assert!(second.contains("400 Bad Request"), "replay: {second}");
    }

    /// The headless paste-code path: `complete_sign_in` consumes the pending
    /// state+verifier, exchanges with `codeVerifier`, and a second paste of
    /// the same code is rejected without touching the edge again.
    #[tokio::test]
    async fn headless_exchange_consumes_verifier_exactly_once() {
        let (edge, body_rx) = mock_edge(EXCHANGE_OK).await;
        let (auth, _dir) = workos_auth(&edge);

        let url = auth.start_headless_sign_in();
        let params = query_params(&url);
        let state = params.get("state").expect("state").clone();
        let challenge = params.get("code_challenge").expect("challenge").clone();
        let pasted = format!("{state}.paste-code-1");
        auth.complete_sign_in(&pasted)
            .await
            .expect("first paste signs in");
        // The GitHub/WorkOS profile picture rides the exchange and lands in
        // the signed-in user profile (for the sidebar avatar).
        assert_eq!(
            auth.state().user().and_then(|u| u.avatar_url.as_deref()),
            Some("https://avatars.example/a.png")
        );

        let body = tokio::time::timeout(Duration::from_secs(5), body_rx)
            .await
            .expect("exchange reached the edge")
            .expect("edge captured body");
        let sent: serde_json::Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(sent["code"], "paste-code-1");
        let verifier = sent["codeVerifier"].as_str().expect("codeVerifier present");
        assert_eq!(challenge, pkce_s256_challenge(verifier));

        // The verifier was single-use: a replayed paste must fail without a
        // second edge round trip (take_pending already returned None).
        assert!(auth.complete_sign_in(&pasted).await.is_err());
    }

    #[test]
    fn auth_state_serializes_as_proto_shape() {
        let user = AuthUser {
            id: "u1".into(),
            email: "u@x".into(),
            name: None,
            avatar_url: None,
        };
        let signed_in = AuthState::SignedIn {
            user: user.clone(),
            org_id: Some("org_1".into()),
        };
        let value = serde_json::to_value(&signed_in).expect("json");
        assert_eq!(
            value,
            serde_json::json!({
                "state": "signedIn",
                "user": {"id": "u1", "email": "u@x", "name": null},
                "orgId": "org_1",
            })
        );
        // The proto type itself round-trips the emitted value.
        let parsed: cypher_proto::AuthState = serde_json::from_value(value).expect("proto parse");
        assert!(matches!(parsed, cypher_proto::AuthState::SignedIn { .. }));
        assert_eq!(
            serde_json::to_value(AuthState::SignedOut).expect("json"),
            serde_json::json!({"state": "signedOut"})
        );
        assert_eq!(
            serde_json::to_value(AuthState::NeedsOrganization { user }).expect("json"),
            serde_json::json!({
                "state": "needsOrganization",
                "user": {"id": "u1", "email": "u@x", "name": null},
            })
        );
    }

    // -- Refresh error semantics (Phase C) --------------------------------
    //
    // A transient WorkOS/edge failure must NEVER revoke a signed-in device:
    // only an explicit permanent credential rejection (401 + `invalid_grant`)
    // clears the session. 429, 5xx, malformed bodies, and dropped connections
    // all preserve the stored session and surface an error to retry.

    /// The stored session still on disk, for asserting preservation.
    fn stored_session(dir: &tempfile::TempDir) -> StoredSession {
        let bytes = std::fs::read(dir.path().join("session.json")).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn refresh_invalid_grant_signs_out_and_removes_session() {
        let edge = mock_edge_status(
            "401 Unauthorized",
            r#"{"error":"invalid_grant","code":"invalid_grant","retryable":false}"#,
        )
        .await;
        let (auth, dir) = workos_auth_with_session(&edge);

        let result = auth.refresh(None).await;
        // A permanent rejection resolves as signed-out (not an error): the
        // session can never recover, so the caller stops retrying.
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
        assert!(matches!(auth.state(), AuthState::SignedOut));
        assert!(
            !dir.path().join("session.json").exists(),
            "stored session removed on permanent rejection"
        );
    }

    #[tokio::test]
    async fn refresh_429_preserves_session() {
        let edge = mock_edge_status(
            "429 Too Many Requests",
            r#"{"error":"rate limited","code":"rate_limited","retryable":true}"#,
        )
        .await;
        let (auth, dir) = workos_auth_with_session(&edge);

        let result = auth.refresh(None).await;
        assert!(
            result.is_err(),
            "transient failure surfaces as a retryable error"
        );
        assert!(auth.state().is_signed_in(), "session stays signed in");
        assert_eq!(stored_session(&dir).refresh_token, "rt-1");
    }

    #[tokio::test]
    async fn refresh_5xx_preserves_session() {
        for (status, body) in [
            (
                "500 Internal Server Error",
                r#"{"code":"upstream","retryable":true}"#,
            ),
            (
                "503 Service Unavailable",
                r#"{"code":"upstream","retryable":true}"#,
            ),
        ] {
            let edge = mock_edge_status(status, body).await;
            let (auth, dir) = workos_auth_with_session(&edge);
            let result = auth.refresh(None).await;
            assert!(result.is_err(), "{status} surfaces as an error");
            assert!(auth.state().is_signed_in(), "{status} keeps the session");
            assert_eq!(stored_session(&dir).refresh_token, "rt-1");
        }
    }

    #[tokio::test]
    async fn refresh_malformed_gateway_body_preserves_session() {
        // A 200 with a non-JSON body: the gateway is broken, not the session.
        let edge = mock_edge_status("200 OK", "<html>oops</html>").await;
        let (auth, dir) = workos_auth_with_session(&edge);

        let result = auth.refresh(None).await;
        assert!(result.is_err(), "malformed response surfaces as an error");
        assert!(
            auth.state().is_signed_in(),
            "malformed response keeps the session"
        );
        assert_eq!(stored_session(&dir).refresh_token, "rt-1");
    }

    #[tokio::test]
    async fn refresh_dropped_connection_preserves_session() {
        let edge = mock_edge_dropped().await;
        let (auth, dir) = workos_auth_with_session(&edge);

        let result = auth.refresh(None).await;
        assert!(result.is_err(), "transport failure surfaces as an error");
        assert!(
            auth.state().is_signed_in(),
            "transport failure keeps the session"
        );
        assert_eq!(stored_session(&dir).refresh_token, "rt-1");
    }

    #[tokio::test]
    async fn refresh_401_without_machine_code_preserves_session() {
        // A bare 401 with no recognized `code` is ambiguous, not a confirmed
        // credential rejection — keep the session (conservative direction).
        let edge = mock_edge_status("401 Unauthorized", r#"{"error":"nope"}"#).await;
        let (auth, dir) = workos_auth_with_session(&edge);

        let result = auth.refresh(None).await;
        assert!(result.is_err());
        assert!(
            auth.state().is_signed_in(),
            "ambiguous 401 keeps the session"
        );
        assert_eq!(stored_session(&dir).refresh_token, "rt-1");
    }

    #[tokio::test]
    async fn exchange_failure_surfaces_without_persisting_session() {
        let edge = mock_edge_status(
            "401 Unauthorized",
            r#"{"error":"invalid_grant","code":"invalid_grant","retryable":false}"#,
        )
        .await;
        let (auth, dir) = workos_auth(&edge);
        let url = auth.start_headless_sign_in();
        let state = query_params(&url).remove("state").expect("state present");

        let err = auth
            .complete_sign_in(&format!("{state}.expired-code"))
            .await;
        assert!(err.is_err(), "exchange rejection surfaces as an error");
        assert!(matches!(auth.state(), AuthState::SignedOut));
        assert!(
            !dir.path().join("session.json").exists(),
            "a failed exchange never persists a session"
        );
    }

    // -- Avatar refresh semantics ---------------------------------------
    //
    // The refresh response carries optional nested user metadata. `user`
    // ABSENT (old edge / omitted) → preserve the stored avatar; `user`
    // PRESENT → replace or clear from its sanitized `profilePictureUrl`.
    // Changes emit on the state channel so already-signed-in surfaces update
    // without a re-login.

    /// `workos_auth_with_session` with a persisted avatar on the stored user
    /// (so state and disk agree at construction, like a real loaded session).
    fn workos_auth_with_avatar(
        edge_url: &str,
        avatar_url: Option<&str>,
    ) -> (Auth, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let session = StoredSession {
            refresh_token: "rt-1".into(),
            user: AuthUser {
                id: "u1".into(),
                email: "a@b.c".into(),
                name: None,
                avatar_url: avatar_url.map(str::to_string),
            },
            org_id: Some("org_1".into()),
        };
        std::fs::write(
            dir.path().join("session.json"),
            serde_json::to_vec(&session).unwrap(),
        )
        .unwrap();
        let auth = Auth::new(config_with_edge(edge_url, dir.path()));
        assert!(auth.state().is_signed_in());
        (auth, dir)
    }

    #[tokio::test]
    async fn refresh_with_user_metadata_replaces_the_avatar_and_emits() {
        let edge = mock_edge_status(
            "200 OK",
            r#"{"user":{"id":"u1","email":"a@b.c","firstName":"Ann","lastName":"X","profilePictureUrl":"https://avatars.example.com/new.png"},"accessToken":"h.eyJvcmdfaWQiOiJvcmdfMSJ9.sig","refreshToken":"rt2"}"#,
        )
        .await;
        let (auth, dir) =
            workos_auth_with_avatar(&edge, Some("https://avatars.example.com/old.png"));
        let mut state_rx = auth.watch_state();

        let result = auth.refresh(None).await;
        assert!(result.is_ok());
        assert_eq!(
            auth.state().user().and_then(|u| u.avatar_url.as_deref()),
            Some("https://avatars.example.com/new.png")
        );
        // Persisted too — the avatar survives a restart.
        assert_eq!(
            stored_session(&dir).user.avatar_url.as_deref(),
            Some("https://avatars.example.com/new.png")
        );
        // The state channel emitted the updated profile (no re-login).
        assert!(state_rx.changed().await.is_ok());
        assert!(matches!(auth.state(), AuthState::SignedIn { .. }));
    }

    #[tokio::test]
    async fn refresh_without_user_metadata_preserves_the_stored_avatar() {
        let edge = mock_edge_status(
            "200 OK",
            r#"{"accessToken":"h.eyJvcmdfaWQiOiJvcmdfMSJ9.sig","refreshToken":"rt2"}"#,
        )
        .await;
        let (auth, dir) =
            workos_auth_with_avatar(&edge, Some("https://avatars.example.com/keep.png"));
        let mut state_rx = auth.watch_state();

        let result = auth.refresh(None).await;
        assert!(result.is_ok());
        // Old edge / omitted user: the stored avatar survives on state AND disk.
        assert_eq!(
            auth.state().user().and_then(|u| u.avatar_url.as_deref()),
            Some("https://avatars.example.com/keep.png")
        );
        assert_eq!(
            stored_session(&dir).user.avatar_url.as_deref(),
            Some("https://avatars.example.com/keep.png")
        );
        // Nothing changed → the state channel does NOT re-emit.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), state_rx.changed())
                .await
                .is_err(),
            "no avatar/org change must not emit"
        );
    }

    #[tokio::test]
    async fn refresh_with_explicit_null_clears_the_avatar() {
        let edge = mock_edge_status(
            "200 OK",
            r#"{"user":{"id":"u1","email":"a@b.c","firstName":"Ann","lastName":"X","profilePictureUrl":null},"accessToken":"h.eyJvcmdfaWQiOiJvcmdfMSJ9.sig","refreshToken":"rt2"}"#,
        )
        .await;
        let (auth, dir) =
            workos_auth_with_avatar(&edge, Some("https://avatars.example.com/gone.png"));

        let result = auth.refresh(None).await;
        assert!(result.is_ok());
        // The user is present with no picture: an explicit clear.
        assert_eq!(
            auth.state().user().and_then(|u| u.avatar_url.as_deref()),
            None
        );
        assert_eq!(stored_session(&dir).user.avatar_url, None);
    }

    // -- Loaded-session avatar boundary --------------------------------
    //
    // session.json serializes the user camelCase (`avatarUrl`). A valid URL
    // is parsed and lands in state AND inner.stored (persisted too); an
    // unsafe URL is parsed, sanitized to None everywhere, and rewritten out
    // of the file — so a later org-changing refresh can never re-emit it.

    #[test]
    fn loaded_session_parses_and_persists_a_valid_camelcase_avatar() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("session.json"),
            r#"{"refreshToken":"rt-1","user":{"id":"u1","email":"a@b.c","avatarUrl":"https://avatars.example.com/a.png"},"orgId":"org_1"}"#,
        )
        .unwrap();
        let auth = Auth::new(config_with_edge("http://edge.test", dir.path()));
        // Initial state carries the avatar.
        assert_eq!(
            auth.state().user().and_then(|u| u.avatar_url.as_deref()),
            Some("https://avatars.example.com/a.png")
        );
        // `inner.stored` holds the SAME clean value (nothing re-emittable).
        let stored = stored_session(&dir);
        assert_eq!(
            stored.user.avatar_url.as_deref(),
            Some("https://avatars.example.com/a.png")
        );
        // session.json serializes camelCase — never snake_case.
        let raw = std::fs::read_to_string(dir.path().join("session.json")).unwrap();
        assert!(
            raw.contains("avatarUrl"),
            "session.json must use avatarUrl: {raw}"
        );
        assert!(
            !raw.contains("avatar_url"),
            "session.json must not use avatar_url: {raw}"
        );
    }

    #[test]
    fn loaded_session_parses_then_sanitizes_an_unsafe_avatar_everywhere() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("session.json"),
            r#"{"refreshToken":"rt-1","user":{"id":"u1","email":"a@b.c","avatarUrl":"http://evil.example/a.png"},"orgId":"org_1"}"#,
        )
        .unwrap();
        let auth = Auth::new(config_with_edge("http://edge.test", dir.path()));
        // The unsafe URL IS parsed (it was really read in), then sanitized to
        // None everywhere: initial state, inner.stored, and the persisted file.
        assert_eq!(
            auth.state().user().and_then(|u| u.avatar_url.as_deref()),
            None
        );
        assert_eq!(stored_session(&dir).user.avatar_url, None);
        let raw = std::fs::read_to_string(dir.path().join("session.json")).unwrap();
        assert!(
            !raw.contains("evil.example"),
            "unsafe URL must be rewritten out: {raw}"
        );
        assert!(
            !raw.contains("avatarUrl"),
            "cleaned session omits the avatar: {raw}"
        );
    }

    /// Gap-2 regression: an unsafe avatar must never survive in `inner.stored`
    /// and get re-emitted by a later org-changing refresh.
    #[tokio::test]
    async fn unsafe_loaded_avatar_never_reemits_after_org_changing_refresh() {
        let edge = mock_edge_status(
            "200 OK",
            r#"{"accessToken":"h.eyJvcmdfaWQiOiJvcmdfMiJ9.sig","refreshToken":"rt2"}"#,
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("session.json"),
            r#"{"refreshToken":"rt-1","user":{"id":"u1","email":"a@b.c","avatarUrl":"http://evil.example/a.png"},"orgId":"org_1"}"#,
        )
        .unwrap();
        let auth = Auth::new(config_with_edge(&edge, dir.path()));
        assert_eq!(
            auth.state().user().and_then(|u| u.avatar_url.as_deref()),
            None,
            "unsafe avatar sanitized at load"
        );
        // A refresh that changes org (org_1 → org_2) emits a new SignedIn
        // state built from `inner.stored` — which must be the sanitized user.
        let result = auth.refresh(None).await;
        assert!(result.is_ok());
        assert_eq!(auth.state().org_id(), Some("org_2"));
        assert_eq!(
            auth.state().user().and_then(|u| u.avatar_url.as_deref()),
            None,
            "unsafe avatar must not re-emit through an org-changing refresh"
        );
    }
}
