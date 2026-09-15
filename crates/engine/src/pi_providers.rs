//! Device-scoped provider settings, executed by the pinned Runtime SDK.
//! Credentials are passed only through a child's stdin, never its command line.

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cypher_harness::CancellationToken;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::pi_runtime::PiRuntimePaths;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PiProviderInfo {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    pub base_url: String,
    pub provider_type: String,
    pub credential_saved: bool,
    pub state: String,
    pub model_count: usize,
    pub checked_at: Option<i64>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PiProvidersSnapshot {
    pub providers: Vec<PiProviderInfo>,
}

// Intentionally no Debug: this request can contain a credential.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveProvider {
    pub id: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub edit: bool,
}

/// Serialize local read-modify-write requests across RPC connections.
static OPERATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const HELPER_SOURCE: &str = include_str!("../../../dist/pi-runtime/provider-service.mjs");

fn helper_path(paths: &PiRuntimePaths) -> Result<std::path::PathBuf, String> {
    let bundled = paths.current.join("provider-service.mjs");
    let node = paths.current.join("bin/node");
    if bundled.is_file()
        && std::fs::read(&node)
            .ok()
            .is_some_and(|bytes| bytes.starts_with(b"#!"))
    {
        return Ok(bundled);
    }
    std::fs::create_dir_all(&paths.agent_dir).map_err(|err| err.to_string())?;
    let dest = paths.agent_dir.join(".cypher-provider-service.mjs");
    if std::fs::read_to_string(&dest).ok().as_deref() != Some(HELPER_SOURCE) {
        std::fs::write(&dest, HELPER_SOURCE).map_err(|err| err.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600));
        }
    }
    Ok(dest)
}

pub async fn request(
    paths: &PiRuntimePaths,
    action: &str,
    mut params: serde_json::Value,
) -> Result<PiProvidersSnapshot, String> {
    let _guard = OPERATION.lock().await;
    let helper = helper_path(paths)?;
    if !paths.installed() || !helper.is_file() {
        return Err(
            "Install or update Pi Runtime in Settings → Agents to manage providers.".into(),
        );
    }
    params["action"] = action.into();
    let bytes = serde_json::to_vec(&params).map_err(|_| "Invalid provider request.")?;
    if bytes.len() > 65536 {
        return Err("Provider request is too large.".into());
    }
    let mut child = tokio::process::Command::new(paths.current.join("bin/node"))
        .arg(helper)
        .env("PI_CODING_AGENT_DIR", &paths.agent_dir)
        .env("PI_PACKAGE_DIR", &paths.package_dir)
        .env_remove("NODE_OPTIONS")
        .current_dir(&paths.agent_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "Could not start Pi Runtime's provider service.")?;
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        let mut stdin = child
            .stdin
            .take()
            .ok_or("Provider service stdin unavailable.")?;
        stdin
            .write_all(&bytes)
            .await
            .map_err(|_| "Provider service disconnected.")?;
        drop(stdin);
        let output = child
            .wait_with_output()
            .await
            .map_err(|_| "Provider service disconnected.")?;
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|_| "Invalid provider service response.")?;
        if value["ok"] != true {
            return Err(value["error"]
                .as_str()
                .unwrap_or("Provider operation failed.")
                .to_string());
        }
        serde_json::from_value(value["data"].clone())
            .map_err(|_| "Invalid provider snapshot.".to_string())
    })
    .await;
    result.map_err(|_| "Provider operation timed out.".to_string())?
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStatus {
    pub attempt_id: String,
    pub provider_id: String,
    pub phase: String,
    pub authorization_url: Option<String>,
    pub user_code: Option<String>,
    pub instructions: Option<String>,
    pub error: Option<String>,
}

struct LoginAttempt {
    status: LoginStatus,
    prompt_id: Option<String>,
    stdin: mpsc::Sender<String>,
    cancel: CancellationToken,
}

/// One subscription sign-in per runtime. Credentials stay on this device.
#[derive(Default)]
pub struct Logins(Arc<Mutex<Option<LoginAttempt>>>);

impl Drop for Logins {
    fn drop(&mut self) {
        if let Some(attempt) = self.0.lock().unwrap().as_ref() {
            attempt.cancel.cancel();
        }
    }
}

fn login_terminal(phase: &str) -> bool {
    matches!(phase, "succeeded" | "failed" | "cancelled")
}

impl Logins {
    pub fn cancel_all(&self) {
        if let Some(attempt) = self.0.lock().unwrap().as_ref() {
            attempt.cancel.cancel();
        }
    }

    pub fn active(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|attempt| !login_terminal(&attempt.status.phase))
    }

    pub fn begin(&self, paths: &PiRuntimePaths, provider_id: &str) -> Result<LoginStatus, String> {
        if provider_id != "anthropic" && provider_id != "openai-codex" {
            return Err(
                "Sign in is only available for Claude Pro/Max and ChatGPT Plus/Pro.".into(),
            );
        }
        if !paths.installed() {
            return Err(
                "Install or update Pi Runtime in Settings → Agents to manage providers.".into(),
            );
        }
        let mut guard = self.0.lock().unwrap();
        if guard
            .as_ref()
            .is_some_and(|attempt| !login_terminal(&attempt.status.phase))
        {
            return Err(
                "A provider sign-in is already active on this device. Cancel it or wait for it to finish.".into(),
            );
        }
        let helper = helper_path(paths)?;
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(4);
        let cancel = CancellationToken::new();
        let status = LoginStatus {
            attempt_id: uuid::Uuid::new_v4().to_string(),
            provider_id: provider_id.to_string(),
            phase: "starting".into(),
            authorization_url: None,
            user_code: None,
            instructions: None,
            error: None,
        };
        *guard = Some(LoginAttempt {
            status: status.clone(),
            prompt_id: None,
            stdin: stdin_tx,
            cancel: cancel.clone(),
        });
        let shared = self.0.clone();
        let id = status.attempt_id.clone();
        let provider = provider_id.to_string();
        let node = paths.current.join("bin/node");
        let agent = paths.agent_dir.clone();
        let pkg = paths.package_dir.clone();
        tokio::spawn(async move {
            let mut child = match tokio::process::Command::new(node)
                .arg(helper)
                .arg("--oauth")
                .env("PI_CODING_AGENT_DIR", &agent)
                .env("PI_PACKAGE_DIR", &pkg)
                .env_remove("NODE_OPTIONS")
                .current_dir(&agent)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
            {
                Ok(child) => child,
                Err(_) => {
                    fail(
                        &shared,
                        &id,
                        "Could not start Pi Runtime's provider service.",
                    );
                    return;
                }
            };
            let Some(mut stdin) = child.stdin.take() else {
                fail(&shared, &id, "Provider service stdin unavailable.");
                return;
            };
            let Some(stdout) = child.stdout.take() else {
                fail(&shared, &id, "Provider service stdout unavailable.");
                return;
            };
            let start =
                serde_json::json!({"action":"oauth_login","id":provider}).to_string() + "\n";
            if stdin.write_all(start.as_bytes()).await.is_err() {
                fail(&shared, &id, "Provider service disconnected.");
                return;
            }
            let mut lines = BufReader::new(stdout).lines();
            let deadline = tokio::time::sleep(Duration::from_secs(10 * 60));
            tokio::pin!(deadline);
            let mut expired = false;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        let _ = stdin.write_all(b"{\"type\":\"cancel\"}\n").await;
                        let _ = child.start_kill();
                        break;
                    }
                    _ = &mut deadline, if !expired => {
                        expired = true;
                        cancel.cancel();
                    }
                    line = stdin_rx.recv() => {
                        let Some(line) = line else { break; };
                        if stdin.write_all(line.as_bytes()).await.is_err() { break; }
                    }
                    line = lines.next_line() => {
                        let Ok(Some(line)) = line else { break; };
                        apply_helper_line(&shared, &id, &line);
                        if shared.lock().unwrap().as_ref().is_some_and(|a| a.status.attempt_id == id && login_terminal(&a.status.phase)) {
                            let _ = child.start_kill();
                            break;
                        }
                    }
                }
            }
            let mut guard = shared.lock().unwrap();
            if let Some(attempt) = guard.as_mut().filter(|a| a.status.attempt_id == id)
                && !login_terminal(&attempt.status.phase)
            {
                attempt.status.phase = if expired {
                    "failed"
                } else if cancel.is_cancelled() {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
                if expired {
                    attempt.status.error = Some("Sign-in timed out. Start again.".into());
                } else if attempt.status.phase == "failed" {
                    attempt.status.error = Some("Sign-in failed. Start again.".into());
                }
            }
        });
        Ok(status)
    }

    pub fn status(&self, id: &str) -> Result<LoginStatus, String> {
        let guard = self.0.lock().unwrap();
        Ok(login_attempt(&guard, id)?.status.clone())
    }

    pub fn respond(&self, id: &str, callback: &str) -> Result<LoginStatus, String> {
        let mut guard = self.0.lock().unwrap();
        let attempt = guard
            .as_mut()
            .filter(|attempt| attempt.status.attempt_id == id)
            .ok_or("Provider sign-in attempt not found. Start again.")?;
        if attempt.cancel.is_cancelled() || attempt.status.phase != "awaiting_callback" {
            return Err("This sign-in is not waiting for a callback.".into());
        }
        let callback = callback.trim();
        if callback.is_empty() || callback.len() > 8192 || callback.contains('\0') {
            return Err("Paste the full callback URL or authorization code.".into());
        }
        let prompt = attempt
            .prompt_id
            .clone()
            .ok_or("No pending callback prompt.")?;
        let payload =
            serde_json::json!({"type":"response","id":prompt,"value":callback}).to_string() + "\n";
        attempt
            .stdin
            .try_send(payload)
            .map_err(|_| "Provider sign-in is no longer accepting a callback.")?;
        attempt.prompt_id = None;
        attempt.status.phase = "completing".into();
        Ok(attempt.status.clone())
    }

    pub fn cancel(&self, id: &str) -> Result<LoginStatus, String> {
        let guard = self.0.lock().unwrap();
        let attempt = login_attempt(&guard, id)?;
        attempt.cancel.cancel();
        Ok(attempt.status.clone())
    }
}

fn login_attempt<'a>(
    guard: &'a Option<LoginAttempt>,
    id: &str,
) -> Result<&'a LoginAttempt, String> {
    guard
        .as_ref()
        .filter(|attempt| attempt.status.attempt_id == id)
        .ok_or_else(|| "Provider sign-in attempt not found. Start again.".into())
}

fn fail(shared: &Arc<Mutex<Option<LoginAttempt>>>, id: &str, error: &str) {
    if let Some(attempt) = shared
        .lock()
        .unwrap()
        .as_mut()
        .filter(|a| a.status.attempt_id == id)
    {
        attempt.status.phase = "failed".into();
        attempt.status.error = Some(error.into());
    }
}

fn apply_helper_line(shared: &Arc<Mutex<Option<LoginAttempt>>>, id: &str, line: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    let mut guard = shared.lock().unwrap();
    let Some(attempt) = guard.as_mut().filter(|a| a.status.attempt_id == id) else {
        return;
    };
    match value["type"].as_str() {
        Some("event") => match value["event"]["type"].as_str() {
            Some("auth_url") => {
                attempt.status.authorization_url =
                    value["event"]["url"].as_str().map(str::to_string);
                attempt.status.instructions =
                    value["event"]["instructions"].as_str().map(str::to_string);
                if attempt.prompt_id.is_some() {
                    attempt.status.phase = "awaiting_callback".into();
                }
            }
            Some("device_code") => {
                attempt.status.authorization_url = value["event"]["verificationUri"]
                    .as_str()
                    .map(str::to_string);
                attempt.status.user_code = value["event"]["userCode"].as_str().map(str::to_string);
                attempt.status.phase = "waiting".into();
                attempt.status.instructions =
                    Some("Open the link, enter the code, then wait here.".into());
            }
            Some("progress") => {
                attempt.status.instructions =
                    value["event"]["message"].as_str().map(str::to_string);
            }
            _ => {}
        },
        Some("prompt") => {
            attempt.prompt_id = value["id"].as_str().map(str::to_string);
            attempt.status.phase = "awaiting_callback".into();
            if let Some(message) = value["prompt"]["message"].as_str() {
                attempt.status.instructions = Some(message.to_string());
            }
        }
        Some("done") => {
            attempt.status.phase = "succeeded".into();
            attempt.status.error = None;
            attempt.prompt_id = None;
        }
        Some("error") => {
            attempt.status.phase = "failed".into();
            attempt.status.error = Some(
                value["error"]
                    .as_str()
                    .unwrap_or("Sign-in failed. Start again.")
                    .to_string(),
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &std::path::Path) -> PiRuntimePaths {
        let current = root.join("current");
        PiRuntimePaths {
            root: root.into(),
            executable: current.join("bin/pi"),
            npm_executable: current.join("bin/npm"),
            package_dir: current.join("pi"),
            agent_dir: root.join("agent"),
            current,
        }
    }

    #[tokio::test]
    async fn missing_runtime_never_falls_back_to_system_pi() {
        let dir = tempfile::tempdir().unwrap();
        let error = request(&paths(dir.path()), "list", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(error.contains("Install or update Pi Runtime"));
    }

    #[test]
    fn save_request_rejects_unknown_fields() {
        assert!(
            serde_json::from_value::<SaveProvider>(serde_json::json!({
                "id": "test", "baseUrl": "https://example.com", "authPath": "/elsewhere"
            }))
            .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn private_stdin_transport_and_safe_response() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        for directory in [
            paths.current.join("bin"),
            paths.package_dir.clone(),
            paths.agent_dir.clone(),
        ] {
            std::fs::create_dir_all(directory).unwrap();
        }
        for path in [
            paths.executable.clone(),
            paths.package_dir.join("package.json"),
            paths.current.join("runtime.json"),
            paths.current.join("provider-service.mjs"),
        ] {
            std::fs::write(path, "{}").unwrap();
        }
        let node = paths.current.join("bin/node");
        std::fs::write(
            &node,
            r#"#!/bin/sh
test "$#" = 1 || exit 1
test "$PI_CODING_AGENT_DIR" -ef . || exit 1
cat >/dev/null
echo 'sensitive dependency diagnostic' >&2
echo '{"ok":true,"data":{"providers":[]}}'
"#,
        )
        .unwrap();
        std::fs::set_permissions(node, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = request(
            &paths,
            "save",
            serde_json::json!({
                "id": "test", "apiKey": "fixture-secret"
            }),
        )
        .await
        .unwrap();
        assert!(result.providers.is_empty());
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("fixture-secret")
        );
    }
}
