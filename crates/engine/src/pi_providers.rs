//! Device-scoped provider settings, executed by the pinned Runtime SDK.
//! Credentials are passed only through a child's stdin, never its command line.

use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::pi_runtime::PiRuntimePaths;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PiProviderInfo {
    pub id: String,
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

pub async fn request(
    paths: &PiRuntimePaths,
    action: &str,
    mut params: serde_json::Value,
) -> Result<PiProvidersSnapshot, String> {
    let _guard = OPERATION.lock().await;
    let helper = paths.current.join("provider-service.mjs");
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
