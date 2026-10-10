//! App and Pi runtime updates: the update strips and their actions.

use super::*;

impl Shell {
    /// Update strip: shown above the user menu whenever the engine's
    /// UpdateStatus stream reports a newer release. On a macOS bundle install
    /// it drives the whole flow — click to download, then click to restart into
    /// the staged bundle. Elsewhere (managed/source installs) it is advisory
    /// (`cypher update`); click dismisses it for that version.
    pub(super) fn render_update_strip(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let mac_app = matches!(self.install, cypher_update::InstallKind::MacApp { .. });
        let view = update_strip_view(
            self.state.read(cx).update.as_ref(),
            cypher_update::current_version(),
            self.update_dismissed.as_deref(),
            mac_app,
            &self.update_flow,
        )?;
        let UpdateStripView {
            label,
            clickable,
            failed,
        } = view;
        let tone = if failed { theme.danger } else { theme.accent };
        // Dark-purple GLASS tint (user request), not the 400-level accent as
        // a fill: deep pigment at partial alpha tints the blur showing
        // through instead of compositing into the slab that a bright indigo
        // fill produced (earlier user report). Light chrome gets a lavender
        // accent wash instead — dark purple under indigo-600 text goes muddy.
        let (chip_bg, chip_bg_hover) = if failed {
            (theme.danger.opacity(0.14), theme.danger.opacity(0.22))
        } else {
            match theme.appearance {
                crate::theme::Appearance::Dark => {
                    let purple = crate::theme::oklch(0.35, 0.12, 277.0);
                    (purple.opacity(0.45), purple.opacity(0.60))
                }
                crate::theme::Appearance::Light => {
                    (theme.accent.opacity(0.10), theme.accent.opacity(0.16))
                }
            }
        };

        let mut strip = div()
            .id("update-strip")
            .mx(px(Theme::SPACE_SM))
            // No bottom margin: the user-menu block below carries its own
            // SPACE_SM padding — doubling it read as a hole (user report).
            .px(px(Theme::SPACE_SM))
            .py(px(6.0))
            .rounded(px(Theme::CONTROL_RADIUS))
            .bg(chip_bg)
            .flex()
            .flex_row()
            .items_center()
            .text_size(px(11.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(tone)
            .child(div().flex_1().min_w_0().child(label));
        if clickable {
            strip = strip
                .cursor_pointer()
                .hover(move |s| s.bg(chip_bg_hover))
                .on_click(cx.listener(move |this, _, _, cx| this.on_update_strip_click(cx)));
        }
        Some(strip.into_any_element())
    }

    /// Pi CLI + extension update notification. The engine checks npm shortly
    /// after boot and every six hours; one click delegates the actual update
    /// to `pi update --all`, then the engine hot-reloads the Pi runtime.
    pub(super) fn render_pi_update_strip(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let view =
            pi_update_strip_view(self.state.read(cx).pi_update.as_ref(), self.pi_update_busy)?;
        let PiUpdateStripView {
            label,
            clickable,
            failed,
        } = view;
        let tone = if failed { theme.danger } else { theme.accent };
        let (chip_bg, chip_bg_hover) = if failed {
            (theme.danger.opacity(0.14), theme.danger.opacity(0.22))
        } else {
            match theme.appearance {
                crate::theme::Appearance::Dark => {
                    let purple = crate::theme::oklch(0.35, 0.12, 277.0);
                    (purple.opacity(0.45), purple.opacity(0.60))
                }
                crate::theme::Appearance::Light => {
                    (theme.accent.opacity(0.10), theme.accent.opacity(0.16))
                }
            }
        };
        let mut strip = div()
            .id("pi-update-strip")
            .mx(px(Theme::SPACE_SM))
            .mt(px(6.0))
            .px(px(Theme::SPACE_SM))
            .py(px(6.0))
            .rounded(px(Theme::CONTROL_RADIUS))
            .bg(chip_bg)
            .flex()
            .flex_row()
            .items_center()
            .text_size(px(11.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(tone)
            .child(div().flex_1().min_w_0().child(label));
        if clickable {
            strip = strip
                .cursor_pointer()
                .hover(move |s| s.bg(chip_bg_hover))
                .on_click(cx.listener(|this, _, _, cx| this.begin_pi_update(cx)));
        }
        Some(strip.into_any_element())
    }

    fn begin_pi_update(&mut self, cx: &mut Context<Self>) {
        if self.pi_update_busy {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.pi_update_busy = true;
        let state = self.state.clone();
        self.pi_update_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::APPLY_PI_UPDATES, serde_json::json!({}))
                .await;
            if let Ok(value) = &result {
                match serde_json::from_value::<cypher_engine::pi::packages::PiUpdateStatus>(
                    value.clone(),
                ) {
                    Ok(status) => {
                        state.update(cx, |state, cx| {
                            state.apply_pi_update(status);
                            cx.notify();
                        });
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "malformed ApplyPiUpdates reply");
                    }
                }
            }
            this.update(cx, |shell, cx| {
                shell.pi_update_busy = false;
                if let Err(err) = result {
                    shell.sidebar_notice = Some(format!("Pi update failed: {err}").into());
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Idle → download; Ready → swap + relaunch; Failed → retry; advisory
    /// installs → dismiss for this version.
    fn on_update_strip_click(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.install, cypher_update::InstallKind::MacApp { .. }) {
            self.update_dismissed = self
                .state
                .read(cx)
                .update
                .as_ref()
                .and_then(|s| s.latest_version.clone());
            cx.notify();
            return;
        }
        match std::mem::replace(&mut self.update_flow, UpdateFlow::Idle) {
            UpdateFlow::Idle | UpdateFlow::Failed(_) => self.begin_update_download(cx),
            UpdateFlow::Downloading => self.update_flow = UpdateFlow::Downloading,
            UpdateFlow::Ready(staged) => self.apply_staged_update(staged, cx),
        }
    }

    /// Fetch the manifest and stage the new Cypher desktop bundle under the data dir
    /// (tokio — reqwest); the strip flips to "restart to apply" when done.
    fn begin_update_download(&mut self, cx: &mut Context<Self>) {
        let edge_url = self.boot.edge_url.clone();
        let data_dir = self.data_dir.clone();
        self.update_flow = UpdateFlow::Downloading;
        let download = Tokio::spawn(cx, async move {
            let manifest = cypher_update::fetch_latest(&edge_url).await?;
            cypher_update::stage_mac_app(&edge_url, &manifest, &data_dir).await
        });
        self.update_task = Some(cx.spawn(async move |this, cx| {
            let outcome = match download.await {
                Ok(Ok(staged)) => Ok(staged),
                Ok(Err(err)) => Err(format!("{err:#}")),
                Err(join_err) => Err(join_err.to_string()),
            };
            this.update(cx, |shell, cx| {
                shell.update_flow = match outcome {
                    Ok(staged) => UpdateFlow::Ready(staged),
                    Err(message) => {
                        tracing::warn!(%message, "update download failed");
                        UpdateFlow::Failed(message.into())
                    }
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Swap the staged bundle over the installed one, arm the detached
    /// relauncher, and quit — the relauncher `open`s the new bundle once this
    /// process (and its engine lock / IPC socket) is gone.
    fn apply_staged_update(&mut self, staged: PathBuf, cx: &mut Context<Self>) {
        let cypher_update::InstallKind::MacApp { bundle } = self.install.clone() else {
            return;
        };
        match cypher_update::apply_mac_app(&staged, &bundle) {
            Ok(()) => {
                cypher_update::relaunch_app_after_exit(&bundle);
                cx.quit();
            }
            Err(err) => {
                tracing::error!(error = %err, "update apply failed");
                self.update_flow = UpdateFlow::Failed(format!("{err:#}").into());
                cx.notify();
            }
        }
    }

    pub(super) fn open_about(&mut self, cx: &mut Context<Self>) {
        let check = about_check_from_status(
            self.state.read(cx).update.as_ref(),
            cypher_update::current_version(),
        );
        self.about = Some(AboutDialog {
            check,
            runtime_checking: false,
        });
        cx.notify();
    }

    pub(super) fn begin_update_check(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.about.as_ref().map(|about| &about.check),
            Some(AboutCheck::Checking)
        ) {
            return;
        }
        let check = AboutCheck::Checking;
        if let Some(about) = &mut self.about {
            about.check = check;
            about.runtime_checking = true;
        } else {
            self.about = Some(AboutDialog {
                check,
                runtime_checking: true,
            });
        }
        self.begin_runtime_update_check(cx);
        let engine = self.state.read(cx).engine().cloned();
        let edge_url = self.boot.edge_url.clone();
        let state = self.state.clone();
        self.about_task = Some(cx.spawn(async move |this, cx| {
            let status = if let Some(engine) = engine {
                match engine
                    .client()
                    .call(methods::CHECK_UPDATE, serde_json::json!({}))
                    .await
                {
                    Ok(value) => serde_json::from_value::<cypher_update::UpdateStatus>(value).ok(),
                    Err(_) => None,
                }
            } else {
                None
            };
            let status =
                match status {
                    Some(status) => status,
                    None => match Tokio::spawn(cx, async move {
                        cypher_update::fetch_latest(&edge_url).await
                    })
                    .await
                    {
                        Ok(Ok(manifest)) => cypher_update::UpdateStatus {
                            current_version: cypher_update::current_version().into(),
                            update_available: cypher_update::version_newer(
                                &manifest.version,
                                cypher_update::current_version(),
                            ),
                            latest_version: Some(manifest.version),
                            checked_at: Some(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as i64)
                                    .unwrap_or(0),
                            ),
                            error: None,
                            relaunch_pending: false,
                        },
                        Ok(Err(err)) => cypher_update::UpdateStatus {
                            current_version: cypher_update::current_version().into(),
                            latest_version: None,
                            update_available: false,
                            checked_at: None,
                            error: Some(format!("{err:#}")),
                            relaunch_pending: false,
                        },
                        Err(err) => cypher_update::UpdateStatus {
                            current_version: cypher_update::current_version().into(),
                            latest_version: None,
                            update_available: false,
                            checked_at: None,
                            error: Some(err.to_string()),
                            relaunch_pending: false,
                        },
                    },
                };
            state.update(cx, |state, cx| {
                state.apply_update(status.clone());
                cx.notify();
            });
            this.update(cx, |shell, cx| {
                if let Some(about) = &mut shell.about {
                    about.check =
                        about_check_from_status(Some(&status), cypher_update::current_version());
                    if matches!(about.check, AboutCheck::Idle) {
                        about.check = AboutCheck::Current;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The Runtime half of a manual "Check for Updates": one on-demand sweep
    /// of the Runtime manifest, on its own task so a multi-minute bundle
    /// download never holds up the application check's answer.
    fn begin_runtime_update_check(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            if let Some(about) = &mut self.about {
                about.runtime_checking = false;
            }
            return;
        };
        let state = self.state.clone();
        self.about_runtime_task = Some(cx.spawn(async move |this, cx| {
            let reply = engine
                .client()
                .call(methods::CHECK_PI_UPDATE, serde_json::json!({}))
                .await;
            match reply {
                Ok(value) => {
                    match serde_json::from_value::<cypher_engine::pi::packages::PiUpdateStatus>(
                        value,
                    ) {
                        Ok(status) => {
                            state.update(cx, |state, cx| {
                                state.apply_pi_update(status);
                                cx.notify();
                            });
                        }
                        Err(err) => tracing::warn!(error = %err, "malformed CheckPiUpdate reply"),
                    }
                }
                // The check keeps running in the engine; the live
                // PiUpdateStatus watch stays the display authority.
                Err(err) => tracing::warn!(error = %err, "Pi Runtime update check failed"),
            }
            this.update(cx, |shell, cx| {
                if let Some(about) = &mut shell.about {
                    about.runtime_checking = false;
                }
                cx.notify();
            })
            .ok();
        }));
    }
}
