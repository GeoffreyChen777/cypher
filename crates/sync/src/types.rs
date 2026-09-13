//! Shared client-side sync types: the error surface, the per-dial URL/token
//! seam, and the stats snapshot behind `SyncStatus` / `cypher sync`.
//!
//! These lived in the legacy s2 room client (`room.rs`) until the chat2
//! cutover retired it; the registry and chat2 clients keep speaking the same
//! vocabulary.

use futures::future::BoxFuture;
pub const EXPECTED_USER_HEADER: &str = "x-cypher-expected-user";

/// Frozen expected identity, independent of the provider's refreshed token.
/// The Worker checks this before selecting/creating any conversation object.
pub struct AccountUrl {
    inner: std::sync::Arc<dyn UrlProvider>,
    user: String,
}
impl AccountUrl {
    pub fn new(inner: std::sync::Arc<dyn UrlProvider>, user: impl Into<String>) -> Self {
        Self {
            inner,
            user: user.into(),
        }
    }
}
impl UrlProvider for AccountUrl {
    fn url(&self) -> BoxFuture<'static, Result<String, SyncError>> {
        self.inner.url()
    }
    fn request(&self) -> BoxFuture<'static, Result<crate::ConnectionRequest, SyncError>> {
        let future = self.inner.request();
        let user = self.user.clone();
        Box::pin(async move {
            if user.is_empty() || user.len() > 256 || user.chars().any(char::is_control) {
                return Err(SyncError::Protocol("invalid_expected_user".into()));
            }
            let mut request = future.await?;
            request.headers_mut().insert(
                EXPECTED_USER_HEADER,
                user.parse()
                    .map_err(|_| SyncError::Protocol("invalid_expected_user".into()))?,
            );
            Ok(request)
        })
    }
}

/// Errors surfaced by the sync clients.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SyncError {
    #[error("websocket: {0}")]
    WebSocket(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("join refused: {0}")]
    JoinRefused(String),
    #[error("loro: {0}")]
    Loro(String),
    #[error("auth: {0}")]
    Auth(String),
    #[error("client is shut down")]
    Closed,
}

/// Per-dial WebSocket URL provider — consulted before EVERY connection attempt,
/// including background reconnects, so a short-lived auth token embedded in the
/// URL (`?token=…`) is re-read fresh rather than frozen at first connect.
/// Return [`SyncError::Auth`] when no valid credential is available (signed
/// out); the reconnect loop backs off and retries.
pub trait UrlProvider: Send + Sync + 'static {
    fn url(&self) -> BoxFuture<'static, Result<String, SyncError>>;
    /// v3 handshakes keep credentials in headers. The default is deliberately
    /// credential-free; old query-token URLs are not a v3 fallback.
    fn request(
        &self,
    ) -> BoxFuture<
        'static,
        Result<tokio_tungstenite::tungstenite::handshake::client::Request, SyncError>,
    > {
        let future = self.url();
        Box::pin(async move {
            use tokio_tungstenite::tungstenite::client::IntoClientRequest;
            let request = future
                .await?
                .into_client_request()
                .map_err(|_| SyncError::Protocol("invalid_url".into()))?;
            if request.uri().query().is_some()
                || request
                    .uri()
                    .authority()
                    .is_some_and(|a| a.as_str().contains('@'))
            {
                return Err(SyncError::Protocol("url_credentials_not_supported".into()));
            }
            Ok(request)
        })
    }
}

/// Fixed scoped test/dev connection. Intentionally has no Debug implementation.
pub struct AuthenticatedUrl {
    url: String,
    bearer: String,
}
impl AuthenticatedUrl {
    pub fn new(url: impl Into<String>, bearer: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            bearer: bearer.into(),
        }
    }
    pub fn for_account(self, user: impl Into<String>) -> AccountUrl {
        AccountUrl::new(std::sync::Arc::new(self), user)
    }
}
impl UrlProvider for AuthenticatedUrl {
    fn url(&self) -> BoxFuture<'static, Result<String, SyncError>> {
        let url = self.url.clone();
        Box::pin(async move { Ok(url) })
    }
    fn request(
        &self,
    ) -> BoxFuture<
        'static,
        Result<tokio_tungstenite::tungstenite::handshake::client::Request, SyncError>,
    > {
        let url = self.url.clone();
        let bearer = self.bearer.clone();
        Box::pin(async move {
            use tokio_tungstenite::tungstenite::{
                client::IntoClientRequest,
                http::{HeaderValue, header::AUTHORIZATION},
            };
            let mut request = url
                .into_client_request()
                .map_err(|_| SyncError::Protocol("invalid_url".into()))?;
            if request.uri().query().is_some()
                || request
                    .uri()
                    .authority()
                    .is_some_and(|a| a.as_str().contains('@'))
            {
                return Err(SyncError::Protocol("url_credentials_not_supported".into()));
            }
            request.headers_mut().insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {bearer}"))
                    .map_err(|_| SyncError::Auth("invalid_bearer".into()))?,
            );
            Ok(request)
        })
    }
}

/// Fixed URL (dev bearers and tests — tokens that never expire).
pub struct StaticUrl(pub String);

impl UrlProvider for StaticUrl {
    fn url(&self) -> BoxFuture<'static, Result<String, SyncError>> {
        let url = self.0.clone();
        Box::pin(async move { Ok(url) })
    }
}

#[cfg(test)]
mod account_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Rotating(AtomicUsize);
    impl UrlProvider for Rotating {
        fn url(&self) -> BoxFuture<'static, Result<String, SyncError>> {
            Box::pin(async { Ok("wss://edge.test/sync3/org/chats/chat/ws".into()) })
        }
        fn request(&self) -> BoxFuture<'static, Result<crate::ConnectionRequest, SyncError>> {
            let bearer = if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                "old-token"
            } else {
                "new-other-account-token"
            };
            AuthenticatedUrl::new("wss://edge.test/sync3/org/chats/chat/ws", bearer).request()
        }
    }
    #[tokio::test]
    async fn refreshed_credentials_cannot_change_captured_expected_identity() {
        let url = AccountUrl::new(Arc::new(Rotating(AtomicUsize::new(0))), "captured-user");
        let first = url.request().await.unwrap();
        let second = url.request().await.unwrap();
        assert_ne!(
            first.headers()["authorization"],
            second.headers()["authorization"]
        );
        assert_eq!(first.headers()[EXPECTED_USER_HEADER], "captured-user");
        assert_eq!(second.headers()[EXPECTED_USER_HEADER], "captured-user");
        assert!(second.uri().query().is_none());
    }
    #[tokio::test]
    async fn invalid_captured_identity_never_becomes_a_request_header() {
        let url = AccountUrl::new(
            Arc::new(StaticUrl("wss://edge.test".into())),
            "user\r\ninjected:true",
        );
        assert!(url.request().await.is_err());
    }
}

/// Live sync introspection for one room — the data behind the engine's
/// `SyncStatus` RPC and `cypher sync`. Every 2026-08 incident was debugged
/// blind because none of this was observable at runtime.
#[derive(Debug, Clone, Default)]
pub struct RoomStatsSnapshot {
    /// A join is currently established.
    pub connected: bool,
    /// A server state response has been received (WS hello or HTTPS pull).
    /// This is distinct from `connected`: HTTPS pull is allowed to converge
    /// while the WebSocket is still unavailable.
    pub server_known: bool,
    /// Epoch ms of the last SERVER-PUSHED frame (broadcast, backfill,
    /// join answer) — 0 = never. The deaf-socket tell: fresh acks + stale
    /// pushes.
    pub last_pushed_ms: i64,
    /// Epoch ms of the last ack for our own writes — 0 = never.
    pub last_ack_ms: i64,
    /// Mid-session rejoins (reconnect resyncs, stale-peer, full resyncs).
    pub rejoins: u64,
    /// Liveness probes sent (background cadence + on-demand hints).
    pub probes: u64,
    /// Full-snapshot resyncs requested after failed imports.
    pub full_resyncs: u64,
    /// Sessions lost (transport drops, deadlines, requested redials).
    pub disconnects: u64,
    /// Our writes the server REJECTED (InvalidUpdate/PermissionDenied acks).
    /// Nonzero while `last_ack_ms` goes stale is the latched-session tell.
    pub rejected: u64,
}
