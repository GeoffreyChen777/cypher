//! Auth — the engine owns the WorkOS session for its device (ARCHITECTURE §5).
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
//!   signed in; the bearer IS the configured user id.
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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::EngineError;
use crate::util::lock;

mod loopback;
mod session_store;
mod workos;

use loopback::loopback_loop;
use session_store::{AccessEntry, StoredSession, write_private};
pub(crate) use session_store::{consume_sync_rejoin, mark_sync_rejoin};
use workos::{ExchangeOutcome, SignInResult, jwt_claims, new_pkce_verifier, pkce_s256_challenge};

const SIGN_IN_TTL: Duration = Duration::from_secs(15 * 60);
/// Refresh when the cached token has less than this much life left.
const TOKEN_SLACK: Duration = Duration::from_secs(30);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Wire types (AuthRpc)
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

#[derive(Clone)]
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
    /// A secret bearer is not a user ID. Only compiled into explicit Dev builds.
    #[cfg(feature = "development")]
    pub dev_access_token: Option<String>,
    /// Loopback callback port; `None` = ephemeral.
    pub callback_port: Option<u16>,
}

impl std::fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthConfig")
            .field("edge_url", &self.edge_url)
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

impl AuthConfig {
    pub fn new(edge_url: impl Into<String>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            edge_url: edge_url.into(),
            data_dir: data_dir.into(),
            workos_client_id: None,
            workos_api_base: "https://api.workos.com".into(),
            dev_user_id: "dev-user".into(),
            #[cfg(feature = "development")]
            dev_access_token: None,
            callback_port: None,
        }
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
    /// Shared with the workspace host so the viewport's periodic activity
    /// refresh rides the presence beat instead of spending a request.
    viewport_activity: crate::host::viewport_activity::ViewportActivity,
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
    /// Inspect cached account state without refreshing credentials or rewriting
    /// the session. Diagnostics may run while the engine owns the session lock.
    pub fn saved_state(data_dir: &std::path::Path) -> Option<AuthState> {
        let raw = std::fs::read(data_dir.join("session.json")).ok()?;
        let mut session: StoredSession = serde_json::from_slice(&raw).ok()?;
        session.user.avatar_url = sanitize_avatar_url(session.user.avatar_url);
        Some(state_for(session.user, session.org_id))
    }

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
                if let Ok(bytes) = serde_json::to_vec(&session)
                    && let Err(err) = write_private(&session_file, &bytes)
                {
                    tracing::warn!(error = %err, "auth: failed to persist cleaned session");
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
                viewport_activity: Default::default(),
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
    /// The slot the viewport's activity report waits in until the next
    /// presence beat carries it.
    pub fn viewport_activity(&self) -> crate::host::viewport_activity::ViewportActivity {
        self.inner.viewport_activity.clone()
    }

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
            #[cfg(feature = "development")]
            if let Some(token) = &self.inner.config.dev_access_token {
                return Some(token.clone());
            }
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

    /// Is the viewport reporting as the identity this engine is signed in as?
    /// A UI whose account view is stale must not report activity into the
    /// account that replaced it, whichever path the report takes.
    pub fn notification_identity_matches(&self, expected_user: &str, expected_org: &str) -> bool {
        let state = self.state();
        state.user().map(|u| u.id.as_str()) == Some(expected_user)
            && state.org_id() == Some(expected_org)
            && expected_org
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }

    /// Desktop viewport activity, never engine liveness. Bind the request to
    /// the identity the UI observed; a login/organization switch fails closed.
    pub async fn report_notification_activity(
        &self,
        expected_user: &str,
        expected_org: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, EngineError> {
        let token = self
            .access_token()
            .await
            .ok_or_else(|| EngineError::Other("not signed in".into()))?;
        if !self.notification_identity_matches(expected_user, expected_org) {
            return Err(EngineError::Other("notification identity changed".into()));
        }
        let url = format!(
            "{}/registry/{expected_org}/notifications/activity",
            self.inner.config.edge_url.trim_end_matches('/')
        );
        let response = self
            .inner
            .http
            .post(url)
            .bearer_auth(token)
            .timeout(std::time::Duration::from_secs(5))
            .json(&body)
            .send()
            .await
            .map_err(|_| EngineError::Other("notification activity unavailable".into()))?;
        if !response.status().is_success() {
            return Err(EngineError::Other(
                "notification activity unavailable".into(),
            ));
        }
        response
            .json()
            .await
            .map_err(|_| EngineError::Other("invalid notification activity response".into()))
    }

    /// Publish an execution-owner session transition. The Edge registry uses
    /// this structured event for notifications because chat2 transcript rows
    /// are intentionally opaque and the registry mirror is asynchronous.
    pub async fn report_notification_event(
        &self,
        expected_user: &str,
        expected_org: &str,
        session: &cypher_proto::Session,
    ) -> Result<serde_json::Value, EngineError> {
        let token = self
            .access_token()
            .await
            .ok_or_else(|| EngineError::Other("not signed in".into()))?;
        let state = self.state();
        if state.user().map(|u| u.id.as_str()) != Some(expected_user)
            || state.org_id() != Some(expected_org)
        {
            return Err(EngineError::Other("notification identity changed".into()));
        }
        let url = format!(
            "{}/registry/{expected_org}/notifications/event",
            self.inner.config.edge_url.trim_end_matches('/')
        );
        self.inner
            .http
            .post(url)
            .bearer_auth(token)
            .timeout(std::time::Duration::from_secs(5))
            .json(&serde_json::json!({
                "chatId": session.chat_id,
                "deviceId": session.device_id,
                "status": session.status,
                "startedAt": session.started_at.map(|x| x.timestamp_millis()),
                "updatedAt": session.updated_at.timestamp_millis(),
                "subagents": session.subagents.iter().take(32).map(|run| serde_json::json!({
                    "mode": run.mode,
                    "status": run.status,
                    "updatedAt": run.updated_at,
                })).collect::<Vec<_>>(),
            }))
            .send()
            .await
            .map_err(|_| EngineError::Other("notification event unavailable".into()))?
            .error_for_status()
            .map_err(|_| EngineError::Other("notification event rejected".into()))?
            .json()
            .await
            .map_err(|_| EngineError::Other("invalid notification event response".into()))
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
// URL encoding
// ---------------------------------------------------------------------------

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

#[cfg(test)]
mod tests;
