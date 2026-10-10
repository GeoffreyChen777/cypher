//! Plain-HTTPS registry transport (offline pull/push beside the room socket).

use std::sync::Arc;

use cypher_sync::{RegistryTransport, SyncError};

use crate::host::doc_host::EdgeConfig;

pub(super) async fn token_revoked(token: &Option<Arc<dyn cypher_rpc::TokenSource>>) -> bool {
    match token {
        Some(token) => token.token().await.is_none(),
        // Fixed test/dev URLs have no revocable credential source.
        None => false,
    }
}

/// Plain-HTTPS registry pull/push. This intentionally owns the EdgeConfig
/// instead of deriving requests from the WebSocket URL: HTTP credentials must
/// stay in `Authorization: Bearer`, never leak into query strings or logs.
pub(super) struct EdgeRegistryTransport {
    pub(super) http: reqwest::Client,
    pub(super) edge: EdgeConfig,
    pub(super) org_id: String,
}

impl EdgeRegistryTransport {
    fn endpoint(&self, leaf: &str) -> String {
        format!(
            "{}/registry/{}/{leaf}",
            self.edge.url.trim_end_matches('/'),
            self.org_id
        )
    }
}

impl RegistryTransport for EdgeRegistryTransport {
    fn fetch(&self, since: u64) -> futures::future::BoxFuture<'static, Result<String, SyncError>> {
        let http = self.http.clone();
        let edge = self.edge.clone();
        let url = self.endpoint("rows");
        let device = edge.device_id.clone();
        Box::pin(async move {
            let bearer = edge
                .bearer()
                .await
                .ok_or_else(|| SyncError::Auth("signed out".into()))?;
            let response = http
                .get(url)
                .query(&[
                    ("since", since.to_string()),
                    ("device", device),
                    ("beat", "1".to_string()),
                ])
                .bearer_auth(bearer)
                .send()
                .await
                .map_err(|err| SyncError::WebSocket(err.to_string()))?;
            if !response.status().is_success() {
                return Err(SyncError::Protocol(format!(
                    "registry pull HTTP {}",
                    response.status()
                )));
            }
            response
                .text()
                .await
                .map_err(|err| SyncError::WebSocket(err.to_string()))
        })
    }

    fn push(&self, body: String) -> futures::future::BoxFuture<'static, Result<String, SyncError>> {
        let http = self.http.clone();
        let edge = self.edge.clone();
        let url = self.endpoint("push");
        let device = edge.device_id.clone();
        Box::pin(async move {
            let bearer = edge
                .bearer()
                .await
                .ok_or_else(|| SyncError::Auth("signed out".into()))?;
            let response = http
                .post(url)
                .query(&[("device", device)])
                .header("content-type", "application/json")
                .bearer_auth(bearer)
                .body(body)
                .send()
                .await
                .map_err(|err| SyncError::WebSocket(err.to_string()))?;
            if !response.status().is_success() {
                return Err(SyncError::Protocol(format!(
                    "registry push HTTP {}",
                    response.status()
                )));
            }
            response
                .text()
                .await
                .map_err(|err| SyncError::WebSocket(err.to_string()))
        })
    }
}
