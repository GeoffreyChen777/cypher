//! A tab's PTY session: spawning it, applying its output stream, and
//! coalescing keyboard input into writes.

use super::*;

enum StreamDisposition {
    Continue,
    Stop,
}

impl TerminalPanel {
    /// OpenTerminal, then pump SubscribeTerminal with reconnect backoff.
    pub(super) fn spawn_session(
        chat: String,
        key: u64,
        engine: EngineHandle,
        target: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let (cols, rows) = this
                .update(cx, |panel, _| {
                    panel
                        .tab_mut(&chat, key)
                        .map(|t| (t.emulator.cols() as u16, t.emulator.rows() as u16))
                        .unwrap_or((80, 24))
                })
                .unwrap_or((80, 24));

            let opened = engine
                .client()
                .call_as::<TerminalSession>(
                    methods::OPEN_TERMINAL,
                    with_target(
                        serde_json::json!({ "chatId": chat, "cols": cols, "rows": rows }),
                        &target,
                    ),
                )
                .await;
            let session = match opened {
                Ok(session) => session,
                Err(err) => {
                    tracing::warn!(error = %err, "OpenTerminal failed");
                    let _ = this.update(cx, |panel, cx| {
                        if let Some(tab) = panel.tab_mut(&chat, key) {
                            tab.emulator.feed(
                                format!("\x1b[31mfailed to open terminal: {err}\x1b[0m\r\n")
                                    .as_bytes(),
                            );
                            tab.exited = Some(-1);
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            let terminal_id = session.id.clone();
            let attached = this
                .update(cx, |panel, cx| {
                    if let Some(tab) = panel.tab_mut(&chat, key) {
                        tab.terminal_id = Some(terminal_id.clone());
                        cx.notify();
                        true
                    } else {
                        false
                    }
                })
                .unwrap_or(false);
            if !attached {
                // Tab was closed before the open completed — release the PTY.
                let _ = engine
                    .client()
                    .call(
                        methods::CLOSE_TERMINAL,
                        with_target(
                            serde_json::json!({ "terminalId": terminal_id }),
                            &target,
                        ),
                    )
                    .await;
                return;
            }

            let mut attempt: u32 = 0;
            loop {
                let Ok(after_seq) = this.update(cx, |panel, _| {
                    panel.tab_mut(&chat, key).map(|t| t.last_seq)
                }) else {
                    return; // entity released
                };
                let Some(after_seq) = after_seq else { return }; // tab closed

                let subscribed = engine
                    .client()
                    .subscribe(
                        methods::SUBSCRIBE_TERMINAL,
                        with_target(
                            serde_json::json!({ "terminalId": terminal_id, "afterSeq": after_seq }),
                            &target,
                        ),
                    )
                    .await;
                let mut rx = match subscribed {
                    Ok(rx) => rx,
                    Err(err) => {
                        tracing::debug!(error = %err, attempt, "SubscribeTerminal failed; backing off");
                        cx.background_executor()
                            .timer(Duration::from_millis(backoff_ms(attempt)))
                            .await;
                        attempt = attempt.saturating_add(1);
                        continue;
                    }
                };

                while let Some(value) = rx.recv().await {
                    let event: TerminalEvent = match serde_json::from_value(value) {
                        Ok(event) => event,
                        Err(err) => {
                            tracing::warn!(error = %err, "terminal: malformed stream frame");
                            continue;
                        }
                    };
                    attempt = 0;
                    let outcome = this.update(cx, |panel, cx| {
                        panel.apply_stream_event(&chat, key, &engine, event, cx)
                    });
                    match outcome {
                        Ok(StreamDisposition::Continue) => {}
                        Ok(StreamDisposition::Stop) => return,
                        Err(_) => return,
                    }
                }

                // Stream dropped without an exit — reconnect from afterSeq.
                let done = this
                    .update(cx, |panel, _| {
                        panel.tab_mut(&chat, key).map(|t| t.exited.is_some()).unwrap_or(true)
                    })
                    .unwrap_or(true);
                if done {
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(backoff_ms(attempt)))
                    .await;
                attempt = attempt.saturating_add(1);
            }
        })
    }

    fn apply_stream_event(
        &mut self,
        chat: &str,
        key: u64,
        engine: &EngineHandle,
        event: TerminalEvent,
        cx: &mut Context<Self>,
    ) -> StreamDisposition {
        let target = self.chat_target(chat, cx);
        // New output can shift/overwrite the selected text: the terminal pill
        // would quote something that no longer matches the screen. Only for
        // the selected chat's ACTIVE tab in this panel — a background tab's
        // (or another chat's) output must never dismiss anything — and only
        // the OFFER pill, never an open editor/draft. (Checked before the
        // `tab` borrow below.)
        if matches!(
            event,
            TerminalEvent::Data { .. } | TerminalEvent::Exit { .. }
        ) && self.state.read(cx).selected_chat.as_deref() == Some(chat)
            && self
                .chats
                .get(chat)
                .and_then(|tabs| tabs.tabs.get(tabs.active))
                .is_some_and(|t| t.key == key)
        {
            self.dismiss_offer_popup(cx);
        }
        let Some(tab) = self.tab_mut(chat, key) else {
            return StreamDisposition::Stop;
        };
        match event {
            TerminalEvent::Data { seq, data } => {
                tab.last_seq = seq;
                let responses = tab.emulator.feed(&decode_base64(&data));
                if !responses.is_empty()
                    && let Some(id) = tab.terminal_id.clone()
                {
                    // Query responses (DSR etc.) go straight back, no coalescing.
                    let engine = engine.clone();
                    let data = encode_base64(&responses);
                    cx.spawn(async move |_, _| {
                        let _ = engine
                            .client()
                            .call(
                                methods::WRITE_TERMINAL,
                                with_target(
                                    serde_json::json!({ "terminalId": id, "data": data }),
                                    &target,
                                ),
                            )
                            .await;
                    })
                    .detach();
                }
                cx.notify();
                StreamDisposition::Continue
            }
            TerminalEvent::Exit { seq, exit_code, .. } => {
                tab.last_seq = seq;
                tab.exited = Some(exit_code);
                tab.emulator.feed(&exit_message(exit_code));
                cx.notify();
                StreamDisposition::Stop
            }
        }
    }

    // ---- input ----

    /// Queue keyboard bytes on the active tab (12 ms coalescing window).
    pub(super) fn queue_input(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        let Some(chat) = self.selected_chat(cx) else {
            return;
        };
        let Some(tabs) = self.chats.get_mut(&chat) else {
            return;
        };
        let active = tabs.active;
        let Some(tab) = tabs.tabs.get_mut(active) else {
            return;
        };
        if tab.exited.is_some() {
            return;
        }
        // A keypress while scrolled back snaps to the live bottom (xterm).
        if tab.emulator.display_offset() > 0 {
            tab.emulator.scroll_to_bottom();
        }
        let key = tab.key;
        if tab.coalescer.push(bytes) {
            tab.flush_task = Some(Self::schedule_flush(chat, key, cx));
        }
    }

    fn schedule_flush(chat: String, key: u64, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(COALESCE_MS))
                .await;
            let _ = this.update(cx, |panel, cx| panel.flush_input(chat, key, cx));
        })
    }

    fn flush_input(&mut self, chat: String, key: u64, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let target = self.chat_target(&chat, cx);
        let Some(tab) = self.tab_mut(&chat, key) else {
            return;
        };
        if tab.coalescer.is_empty() {
            return;
        }
        let Some(id) = tab.terminal_id.clone() else {
            // OpenTerminal still in flight — keep the buffer, retry shortly.
            if tab.exited.is_none() {
                tab.flush_task = Some(Self::schedule_flush(chat, key, cx));
            }
            return;
        };
        let data = encode_base64(&tab.coalescer.take());
        cx.spawn(async move |_, _| {
            let _ = engine
                .client()
                .call(
                    methods::WRITE_TERMINAL,
                    with_target(
                        serde_json::json!({ "terminalId": id, "data": data }),
                        &target,
                    ),
                )
                .await;
        })
        .detach();
    }
}
