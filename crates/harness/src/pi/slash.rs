//! Extension slash commands run outside a chat (Settings → MCP sign-in)
//! through a short-lived pi child.

use super::*;

impl PiHarness {
    /// Run `/command` through a short-lived `pi --mode rpc` child so command
    /// handlers (MCP OAuth, etc.) execute inside Pi, the same path as the TUI.
    pub(super) async fn run_slash_command(&self, prompt: &str) -> Result<String, HarnessError> {
        self.run_slash_command_ui(prompt, None).await
    }

    pub(super) async fn run_slash_command_ui(
        &self,
        prompt: &str,
        mut ui: Option<crate::SlashUi>,
    ) -> Result<String, HarnessError> {
        // Same command path as the TUI (`pi --mode rpc` + `/mcp login`).
        let mut cmd = self.spawn_command(None, &RunHostContext::default(), None)?;
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if ui.is_some() {
                Stdio::null()
            } else {
                Stdio::inherit()
            })
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled("pi".into())
            } else {
                HarnessError::Io(e)
            }
        })?;
        let (client, mut incoming) = match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => PiClient::new(stdin, stdout),
            _ => {
                shutdown_child(&mut child, self.kill_grace).await;
                return Err(HarnessError::Protocol("pi child has no stdio".into()));
            }
        };
        let mut params = Map::new();
        params.insert("message".into(), Value::String(prompt.to_owned()));
        let prompt_client = client.clone();
        let mut prompt_fut = Box::pin(async move { prompt_client.request("prompt", params).await });
        let mut prompt_done = false;
        let mut output = String::new();
        let mut error: Option<String> = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15 * 60);
        let cancel = ui.as_ref().map(|ui| ui.cancel.clone()).unwrap_or_default();
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    error = Some("MCP sign-in cancelled.".into());
                    break;
                }
                response = async {
                    match &mut ui {
                        Some(ui) => ui.responses.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match response {
                        Some((id, payload)) => client.respond_ui(&id, payload),
                        None => { error = Some("MCP sign-in closed.".into()); break; }
                    }
                }
                res = &mut prompt_fut, if !prompt_done => {
                    prompt_done = true;
                    match res {
                        Ok(_) => {}
                        Err(err) => {
                            error = Some(err.to_string());
                            break;
                        }
                    }
                }
                inc = incoming.recv() => match inc {
                    Some(Incoming::UiRequest { id, method, payload }) => {
                        match method.as_str() {
                            "notify" => {
                                let message = payload
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                let is_error = payload
                                    .get("notifyType")
                                    .and_then(Value::as_str)
                                    == Some("error");
                                if is_error {
                                    error = Some(message.to_owned());
                                } else if !message.is_empty() {
                                    if !output.is_empty() {
                                        output.push('\n');
                                    }
                                    output.push_str(message);
                                    // `/mcp login` announces the authorization
                                    // link in a notify, before its input dialog.
                                    if let Some(ui) = &ui {
                                        let _ = ui.requests.try_send((id, payload));
                                    }
                                }
                            }
                            // Do NOT cancel input/select: `/mcp login` races
                            // `ui.input` (paste callback URL) against the
                            // localhost OAuth callback. Cancelling input
                            // wins that race and aborts sign-in. Leave the
                            // dialog unanswered; the callback completes it.
                            "select" | "input" | "editor" | "confirm" => {
                                if let Some(ui) = &ui
                                    && (method != "input" || ui.requests.try_send((id, payload)).is_err()) {
                                        error = Some("Unsupported MCP sign-in dialog.".into());
                                        break;
                                    }
                            }
                            _ => {}
                        }
                    }
                    Some(Incoming::Event(_) | Incoming::Response { .. }) => {}
                    Some(Incoming::Eof) | None => break,
                },
                _ = tokio::time::sleep_until(deadline) => {
                    error = Some("The MCP sign-in timed out.".into());
                    break;
                }
            }
            if prompt_done {
                break;
            }
        }
        shutdown_child(&mut child, self.kill_grace).await;
        match error {
            Some(message) if !message.is_empty() => Err(HarnessError::Protocol(message)),
            _ => Ok(output),
        }
    }
}
