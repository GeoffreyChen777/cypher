//! The engine's client for the Edge's `/bots/:id/*` routes
//! (docs/design/agent-bots.md §11.3), behind a trait so tests can stand in
//! a fake Edge.

use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;

use cypher_proto::{
    AckRequest, AnswerRequest, AskRequest, AskSent, ConnectRequest, Connected, InboxPage,
    InviteCreated, InviteRequest, PublishRequest, StateRequest, WireError,
};

use crate::host::doc_host::EdgeConfig;

/// What every network tool and RPC says without an Edge.
pub const SIGNED_OUT: &str = "The bot network needs you signed in to Cypher.";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// A refused or failed Edge call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetError {
    /// The HTTP status; `None` when the request never got an answer.
    pub status: Option<u16>,
    /// The Edge's error code (`rate_limited`, `invite_invalid`, …), or
    /// `transport` / `signed_out`.
    pub code: String,
    pub message: String,
}

impl NetError {
    pub fn new(status: Option<u16>, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn signed_out() -> Self {
        Self::new(None, "signed_out", SIGNED_OUT)
    }

    pub fn transport(message: impl std::fmt::Display) -> Self {
        Self::new(
            None,
            "transport",
            format!("The Cypher Edge could not be reached ({message})."),
        )
    }

    /// The Edge answered that the thing is gone or already settled, so a
    /// retry can never succeed.
    pub fn is_final(&self) -> bool {
        matches!(self.status, Some(404 | 409 | 410))
    }
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for NetError {}

/// The `/bots/:id/*` routes, `room` being `:id`. Every call acts as the
/// signed-in user; the Edge refuses rooms they don't own.
#[async_trait]
pub trait BotEdge: Send + Sync + 'static {
    async fn publish(&self, room: &str, request: &PublishRequest) -> Result<(), NetError>;
    async fn inbox(&self, room: &str) -> Result<InboxPage, NetError>;
    async fn create_invite(
        &self,
        room: &str,
        request: &InviteRequest,
    ) -> Result<InviteCreated, NetError>;
    async fn revoke_invite(&self, room: &str, invite_id: &str) -> Result<(), NetError>;
    async fn connect(&self, room: &str, request: &ConnectRequest) -> Result<Connected, NetError>;
    async fn revoke_connection(&self, room: &str, connection_id: &str) -> Result<(), NetError>;
    async fn ask(&self, room: &str, request: &AskRequest) -> Result<AskSent, NetError>;
    async fn answer(&self, room: &str, request: &AnswerRequest) -> Result<(), NetError>;
    async fn state(&self, room: &str, request: &StateRequest) -> Result<(), NetError>;
    async fn ack(&self, room: &str, request: &AckRequest) -> Result<(), NetError>;
}

/// [`BotEdge`] over HTTP, with a fresh bearer per request (token refreshes
/// apply without a restart, like every other Edge call).
pub struct HttpBotEdge {
    http: reqwest::Client,
    edge: EdgeConfig,
}

impl HttpBotEdge {
    pub fn new(edge: EdgeConfig) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            edge,
        }
    }

    async fn send<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        room: &str,
        path: &str,
        body: Option<String>,
    ) -> Result<T, NetError> {
        let bearer = self.edge.bearer().await.ok_or_else(NetError::signed_out)?;
        let url = format!("{}/bots/{room}/{path}", self.edge.url.trim_end_matches('/'));
        let mut request = self.http.request(method, &url).bearer_auth(bearer);
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body);
        }
        let response = request.send().await.map_err(NetError::transport)?;
        let status = response.status();
        let text = response.text().await.map_err(NetError::transport)?;
        if !status.is_success() {
            let error = serde_json::from_str::<WireError>(&text).ok();
            let code = error
                .as_ref()
                .map_or("failed".to_string(), |e| e.error.clone());
            let message = error
                .and_then(|e| e.message)
                .unwrap_or_else(|| format!("The Cypher Edge refused the request ({status})."));
            return Err(NetError::new(Some(status.as_u16()), code, message));
        }
        let text = if text.trim().is_empty() {
            "null"
        } else {
            &text
        };
        serde_json::from_str(text).map_err(|err| {
            NetError::new(
                Some(status.as_u16()),
                "bad_reply",
                format!("The Cypher Edge sent a reply this engine can't read ({err})."),
            )
        })
    }
}

/// The routes answer `{ok: true}` where there is nothing else to say.
#[derive(serde::Deserialize)]
struct Ignored {}

fn json<T: Serialize>(value: &T) -> Option<String> {
    serde_json::to_string(value).ok()
}

#[async_trait]
impl BotEdge for HttpBotEdge {
    async fn publish(&self, room: &str, request: &PublishRequest) -> Result<(), NetError> {
        self.send::<Ignored>(reqwest::Method::POST, room, "publish", json(request))
            .await
            .map(drop)
    }

    async fn inbox(&self, room: &str) -> Result<InboxPage, NetError> {
        self.send(reqwest::Method::GET, room, "inbox", None).await
    }

    async fn create_invite(
        &self,
        room: &str,
        request: &InviteRequest,
    ) -> Result<InviteCreated, NetError> {
        self.send(reqwest::Method::POST, room, "invites", json(request))
            .await
    }

    async fn revoke_invite(&self, room: &str, invite_id: &str) -> Result<(), NetError> {
        let path = format!("invites/{invite_id}");
        self.send::<Ignored>(reqwest::Method::DELETE, room, &path, None)
            .await
            .map(drop)
    }

    async fn connect(&self, room: &str, request: &ConnectRequest) -> Result<Connected, NetError> {
        self.send(reqwest::Method::POST, room, "connect", json(request))
            .await
    }

    async fn revoke_connection(&self, room: &str, connection_id: &str) -> Result<(), NetError> {
        let path = format!("connections/{connection_id}");
        self.send::<Ignored>(reqwest::Method::DELETE, room, &path, None)
            .await
            .map(drop)
    }

    async fn ask(&self, room: &str, request: &AskRequest) -> Result<AskSent, NetError> {
        self.send(reqwest::Method::POST, room, "ask", json(request))
            .await
    }

    async fn answer(&self, room: &str, request: &AnswerRequest) -> Result<(), NetError> {
        self.send::<Ignored>(reqwest::Method::POST, room, "answer", json(request))
            .await
            .map(drop)
    }

    async fn state(&self, room: &str, request: &StateRequest) -> Result<(), NetError> {
        self.send::<Ignored>(reqwest::Method::POST, room, "state", json(request))
            .await
            .map(drop)
    }

    async fn ack(&self, room: &str, request: &AckRequest) -> Result<(), NetError> {
        self.send::<Ignored>(reqwest::Method::POST, room, "ack", json(request))
            .await
            .map(drop)
    }
}
