//! Ephemeral OAuth handoff. Only the originating device owns the child and
//! credentials. A random attempt ID binds all subsequent requests; nothing is
//! written to chat history, logs, or the workspace document.
use super::*;
use cypher_harness::{CancellationToken, SlashUi};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStatus {
    pub attempt_id: String,
    pub phase: String,
    pub authorization_url: Option<String>,
    pub error: Option<String>,
}

struct Attempt {
    status: LoginStatus,
    dialog: Option<String>,
    authorization_url: Option<String>,
    responses: mpsc::Sender<(String, Value)>,
    cancel: CancellationToken,
}

/// One login per runtime: the adapter's loopback port and auth dump are shared.
#[derive(Default)]
pub struct Logins(Arc<Mutex<Option<Attempt>>>);

impl Drop for Logins {
    fn drop(&mut self) {
        if let Some(attempt) = self.0.lock().unwrap().as_ref() {
            attempt.cancel.cancel();
        }
    }
}

impl Logins {
    pub fn cancel_all(&self) {
        if let Some(a) = self.0.lock().unwrap().as_ref() {
            a.cancel.cancel();
        }
    }
    pub fn active(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|a| !terminal(&a.status.phase))
    }

    pub fn begin(
        &self,
        dir: PathBuf,
        name: String,
        harness: Arc<dyn Harness>,
    ) -> Result<LoginStatus, String> {
        let entry = read_mcp_root(&dir)["mcpServers"][&name].clone();
        if name.is_empty()
            || name.contains(char::is_whitespace)
            || entry.is_null()
            || auth_kind(&entry) != McpAuthKind::Oauth
            || entry["disabled"] == true
        {
            return Err("Select an enabled OAuth MCP server.".into());
        }
        let mut guard = self.0.lock().unwrap();
        if guard.as_ref().is_some_and(|a| !terminal(&a.status.phase)) {
            return Err("An MCP sign-in is already active on this device. Cancel it or wait for it to expire.".into());
        }
        let (requests, mut incoming) = mpsc::channel(4);
        let (responses, response_rx) = mpsc::channel(4);
        let cancel = CancellationToken::new();
        let status = LoginStatus {
            attempt_id: uuid::Uuid::new_v4().to_string(),
            phase: "starting".into(),
            authorization_url: None,
            error: None,
        };
        *guard = Some(Attempt {
            status: status.clone(),
            dialog: None,
            authorization_url: None,
            responses,
            cancel: cancel.clone(),
        });
        let shared = self.0.clone();
        let id = status.attempt_id.clone();
        tokio::spawn(async move {
            let dump = auth_dump_path(&dir);
            let _ = std::fs::remove_file(&dump);
            let ui = SlashUi {
                requests,
                responses: response_rx,
                cancel: cancel.clone(),
            };
            let command = format!("/mcp-auth {name}");
            let work = harness.run_slash_interactive(&command, ui);
            tokio::pin!(work);
            let deadline = tokio::time::sleep(std::time::Duration::from_secs(10 * 60));
            tokio::pin!(deadline);
            let mut expired = false;
            let mut input_open = true;
            let result = loop {
                tokio::select! {
                    result = &mut work => break result,
                    _ = &mut deadline, if !expired => { expired = true; cancel.cancel(); }
                    request = incoming.recv(), if input_open => {
                        let Some((dialog, payload)) = request else { input_open = false; continue; };
                        let url = authorization_url(&payload);
                        let mut guard = shared.lock().unwrap();
                        let Some(a) = guard.as_mut().filter(|a| a.status.attempt_id == id) else { cancel.cancel(); continue; };
                        if let Some(url) = url {
                            a.dialog = Some(dialog);
                            a.authorization_url = Some(url.clone());
                            a.status.authorization_url = Some(url);
                            a.status.phase = "awaiting_callback".into();
                        } else {
                            a.status.error = Some("The adapter did not provide a valid HTTPS authorization link.".into());
                            cancel.cancel();
                        }
                    }
                }
            };
            // Preserve existing registration/refresh credentials. Unlike the
            // old login path, starting a login does not implicitly sign out.
            let persisted = persist_auth_dump(&dir, &dump);
            let _ = std::fs::remove_file(&dump);
            let signed_in = result.is_ok()
                && !cancel.is_cancelled()
                && auth_status(&dir, &name, McpAuthKind::Oauth) == McpAuthStatus::SignedIn;
            let mut guard = shared.lock().unwrap();
            if let Some(a) = guard.as_mut().filter(|a| a.status.attempt_id == id) {
                a.dialog = None;
                a.authorization_url = None;
                a.status.authorization_url = None;
                if expired {
                    a.status.phase = "failed".into();
                    a.status.error = Some("MCP sign-in timed out. Start again.".into());
                } else if cancel.is_cancelled() {
                    a.status.phase = "cancelled".into();
                } else if result.is_ok() && persisted.is_ok() && signed_in {
                    a.status.phase = "succeeded".into();
                } else {
                    a.status.phase = "failed".into();
                    // Adapter errors may echo URLs/codes. Never return raw output.
                    a.status.error = Some("MCP sign-in failed. Check the server's OAuth client ID, scope and callback URI, then retry.".into());
                }
            }
        });
        Ok(status)
    }

    pub fn status(&self, id: &str) -> Result<LoginStatus, String> {
        let guard = self.0.lock().unwrap();
        Ok(attempt(&guard, id)?.status.clone())
    }

    pub fn respond(&self, id: &str, callback: &str) -> Result<LoginStatus, String> {
        let mut guard = self.0.lock().unwrap();
        let a = guard
            .as_mut()
            .filter(|a| a.status.attempt_id == id)
            .ok_or("MCP sign-in attempt not found. Start again.")?;
        if a.cancel.is_cancelled() || a.status.phase != "awaiting_callback" {
            return Err("This sign-in is not waiting for a callback.".into());
        }
        validate_callback(a.authorization_url.as_deref().unwrap_or_default(), callback)?;
        let dialog = a
            .dialog
            .as_ref()
            .ok_or("No pending callback dialog.")?
            .clone();
        a.responses
            .try_send((dialog, serde_json::json!({"value": callback.trim()})))
            .map_err(|_| "MCP sign-in is no longer accepting a callback.")?;
        a.dialog = None;
        a.status.authorization_url = None;
        a.status.phase = "completing".into();
        Ok(a.status.clone())
    }

    pub fn cancel(&self, id: &str) -> Result<LoginStatus, String> {
        let guard = self.0.lock().unwrap();
        let a = attempt(&guard, id)?;
        a.cancel.cancel();
        Ok(a.status.clone())
    }
}

fn attempt<'a>(guard: &'a Option<Attempt>, id: &str) -> Result<&'a Attempt, String> {
    guard
        .as_ref()
        .filter(|a| a.status.attempt_id == id)
        .ok_or_else(|| "MCP sign-in attempt not found. Start again.".into())
}

fn terminal(phase: &str) -> bool {
    matches!(phase, "succeeded" | "failed" | "cancelled")
}

fn authorization_url(payload: &Value) -> Option<String> {
    let title = payload.get("title")?.as_str()?;
    if title.len() > 32_768 {
        return None;
    }
    title.lines().map(str::trim).find_map(|line| {
        let url = reqwest::Url::parse(line).ok()?;
        (url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url
                .query_pairs()
                .any(|(k, v)| k == "state" && !v.is_empty())
            && url.query_pairs().any(|(k, _)| k == "redirect_uri"))
        .then(|| line.to_owned())
    })
}

fn validate_callback(auth: &str, callback: &str) -> Result<(), String> {
    let invalid = || {
        "Paste the full callback URL for this sign-in attempt (including state and code)."
            .to_string()
    };
    if callback.len() > 16_384 || callback.contains(['\r', '\n', '\0']) {
        return Err(invalid());
    }
    let auth = reqwest::Url::parse(auth).map_err(|_| invalid())?;
    let callback = reqwest::Url::parse(callback.trim()).map_err(|_| invalid())?;
    let query = |url: &reqwest::Url, key: &str| -> Option<String> {
        let values: Vec<_> = url
            .query_pairs()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
            .collect();
        (values.len() == 1)
            .then(|| values[0].clone())
            .filter(|v| !v.is_empty())
    };
    let redirect = query(&auth, "redirect_uri").ok_or_else(invalid)?;
    let redirect = reqwest::Url::parse(&redirect).map_err(|_| invalid())?;
    if callback.scheme() != redirect.scheme()
        || callback.host_str() != redirect.host_str()
        || callback.port_or_known_default() != redirect.port_or_known_default()
        || callback.path() != redirect.path()
        || !callback.username().is_empty()
        || callback.password().is_some()
        || callback.fragment().is_some()
        || query(&auth, "state").is_none()
        || query(&callback, "state") != query(&auth, "state")
        || (query(&callback, "code").is_none() && query(&callback, "error").is_none())
    {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const AUTH: &str = "https://auth.example/authorize?state=attempt&redirect_uri=http%3A%2F%2Flocalhost%3A8976%2Fcallback";
    #[test]
    fn extracts_only_standalone_https_authorization_link() {
        assert_eq!(authorization_url(&serde_json::json!({"title": format!("Complete OAuth\n\u{1b}]8;;{AUTH}\u{7}link\n{AUTH}\nPaste callback")})).as_deref(), Some(AUTH));
        assert!(authorization_url(&serde_json::json!({"title":"javascript:alert(1)"})).is_none());
    }
    #[test]
    fn callback_is_bound_to_state_and_redirect() {
        assert!(
            validate_callback(
                AUTH,
                "http://localhost:8976/callback?state=attempt&code=secret"
            )
            .is_ok()
        );
        for callback in [
            "http://localhost:8976/callback?state=other&code=secret",
            "http://evil.example:8976/callback?state=attempt&code=secret",
            "http://localhost:8977/callback?state=attempt&code=secret",
            "http://localhost:8976/wrong?state=attempt&code=secret",
            "http://localhost:8976/callback?state=attempt&state=other&code=secret",
            "http://localhost:8976/callback?state=attempt",
        ] {
            assert!(validate_callback(AUTH, callback).is_err());
        }
    }

    #[test]
    fn attempts_reject_cross_attempt_duplicate_and_cancelled_callbacks() {
        let logins = Logins::default();
        let (responses, mut rx) = mpsc::channel(4);
        *logins.0.lock().unwrap() = Some(Attempt {
            status: LoginStatus {
                attempt_id: "first".into(),
                phase: "awaiting_callback".into(),
                authorization_url: Some(AUTH.into()),
                error: None,
            },
            dialog: Some("dialog-one".into()),
            authorization_url: Some(AUTH.into()),
            responses,
            cancel: CancellationToken::new(),
        });
        let callback = "http://localhost:8976/callback?state=attempt&code=secret";
        assert!(logins.active());
        assert!(logins.status("other").is_err());
        assert!(logins.cancel("other").is_err());
        assert!(logins.respond("other", callback).is_err());
        assert!(rx.try_recv().is_err());
        assert_eq!(
            logins.respond("first", callback).unwrap().phase,
            "completing"
        );
        let (dialog, payload) = rx.try_recv().unwrap();
        assert_eq!(dialog, "dialog-one");
        assert_eq!(payload["value"], callback);
        assert!(logins.respond("first", callback).is_err());
        let serialized = serde_json::to_string(&logins.status("first").unwrap()).unwrap();
        assert!(!serialized.contains("secret"));
        logins.cancel("first").unwrap();
        assert!(logins.respond("first", callback).is_err());
    }
}
