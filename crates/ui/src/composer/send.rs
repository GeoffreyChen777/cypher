//! State observation, sending, steering, interrupting and retrying.

use gpui::{AsyncApp, WeakEntity};

use super::*;

impl Composer {
    pub(super) fn on_state_changed(&mut self, cx: &mut Context<Self>) {
        let (key, pending) = {
            let s = self.state.read(cx);
            (
                s.selected_chat.clone().unwrap_or_default(),
                pending_input_request(&s.transcript),
            )
        };

        // Draft swap on chat navigation — the input entity itself survives.
        if key != self.current_key {
            let old_text = self.input.read(cx).text().to_string();
            if old_text.is_empty() {
                self.drafts.remove(&self.current_key);
            } else {
                self.drafts.insert(self.current_key.clone(), old_text);
            }
            let draft = self.drafts.get(&key).cloned().unwrap_or_default();
            self.current_key = key;
            self.failure = None;
            self.wizard = None;
            // Comments are current-chat-only: switching chats discards them
            // (and the inspector/edit state with them).
            self.comments.clear();
            self.comment_edit = None;
            if self.comments_popup.begin_close() {
                crate::kit::popover::reap_popup(cx, |this: &mut Self| &mut this.comments_popup);
            }
            // Attachments stay stashed under their chat key (the map swap IS
            // the navigation); only the transient chrome resets.
            self.preview = None;
            self.reset_mention(None, cx);
            self.reset_issue(None, cx);
            // Route changes snap: a mode difference between the
            // old and new session's composer must not glide across
            // navigation. Killing the in-flight morph here isn't enough —
            // the nav-driven flip only commits AFTER the swapped draft has
            // been re-measured, one or two renders later, so the whole
            // window snaps (see ROUTE_SNAP_MS).
            self.flip_morph = None;
            self.last_rendered_height = 0.0;
            self.route_snap_until = Some(Instant::now() + Duration::from_millis(ROUTE_SNAP_MS));
            self.input.update(cx, |input, cx| input.set_text(draft, cx));
        }

        // Question panel lifecycle (wizard state cached per request id).
        match pending {
            Some((request_id, questions)) if !self.answered_requests.contains(&request_id) => {
                let same = self
                    .wizard
                    .as_ref()
                    .is_some_and(|w| w.request_id == request_id);
                if !same {
                    self.reset_mention(None, cx);
                    self.reset_issue(None, cx);
                    let slash = {
                        let state = self.state.read(cx);
                        state
                            .pending_echoes()
                            .iter()
                            .rev()
                            .chain(state.transcript.iter().rev())
                            .find(|e| e.role == MessageRole::User)
                            .and_then(|e| {
                                let text: String = e
                                    .parts
                                    .iter()
                                    .filter_map(|p| match p {
                                        MessagePart::Text { text, .. } => Some(text.as_str()),
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                slash_command_label(&text).map(str::to_string)
                            })
                    };
                    let pick_only = questions.first().is_some_and(wizard_pick_only);
                    let input_header = questions.first().map(|q| q.header.clone());
                    let mut wizard = Wizard::new(request_id, questions);
                    if let Some(cmd) = slash {
                        wizard = wizard.for_slash(cmd);
                    }
                    // Straight behind an answer this is stage two of a
                    // two-stage question (pi-ask-user's optional comment):
                    // swap it in, don't fade it in over the composer that
                    // just came back.
                    if handoff_quiet(self.answered_at, Instant::now()) {
                        wizard = wizard.quietly();
                    }
                    self.wizard = Some(wizard);
                    self.advance_task = None;
                    // The composer is unmounting now, so this is the safe
                    // moment to arm its next entrance: a card the user
                    // cancels or the turn supersedes brings it back with the
                    // ordinary fade; answering re-arms the instant swap.
                    self.input_swap_instant = false;
                    self.input.update(cx, |input, cx| {
                        let placeholder = if pick_only {
                            ""
                        } else if input_header.as_deref() == Some("Optional comment") {
                            "Optional comment (press Enter to skip)…"
                        } else if input_header.as_deref() == Some("Custom answer") {
                            "Type your answer…"
                        } else {
                            "Type your own answer, or pick an option above"
                        };
                        input.set_placeholder(placeholder, cx)
                    });
                }
            }
            _ => {
                if let Some(wizard) = self.wizard.as_ref() {
                    // LATCH (original composer.tsx `inputLatch`): a transient
                    // fold/sync blip — or a steer appended behind the
                    // streaming entry — must not unmount the panel and lose
                    // the user's picks. Release only on explicit resolution
                    // (here or on another device) or when a NON-EMPTY
                    // transcript shows the question superseded (a newer
                    // assistant entry took over). Never on run death: the
                    // question stays answerable until answered — the engine
                    // delivers a dead run's answer as a resumed turn.
                    //
                    // An ANSWERED card never reaches here — `wizard_finish`
                    // retires it on the click itself (user requirement: the
                    // panel must go the moment you answer, not one engine
                    // round trip later).
                    let transcript = self.state.read(cx).transcript.clone();
                    let released = input_request_resolved(&transcript, &wizard.request_id)
                        || (!transcript.is_empty()
                            && !self.answered_requests.contains(&wizard.request_id));
                    if released {
                        self.wizard = None;
                        self.advance_task = None;
                        self.input
                            .update(cx, |input, cx| input.set_placeholder("Do anything…", cx));
                    }
                }
            }
        }
        if self.slash.token.is_some() {
            let (text, cursor) = {
                let input = self.input.read(cx);
                (input.text().to_string(), input.cursor_offset())
            };
            self.update_slash(&text, cursor, cx);
        }
        self.prefetch_slash_commands(cx);
        cx.notify();
    }

    fn run_live(&self, cx: &App) -> bool {
        let s = self.state.read(cx);
        let Some(chat_id) = s.selected_chat.as_deref() else {
            return false;
        };
        matches!(
            s.indicator_for(chat_id, chrono::Utc::now()),
            Indicator::Working | Indicator::AwaitingInput
        )
    }

    /// New-chat sends need a project: with none picked (empty device, or a
    /// selection healed away) the send button dims and submit is a no-op —
    /// project-less `~`-cwd sessions are no longer mintable from the canvas.
    /// A quick chat is the deliberate exception (its folder is minted on
    /// send). Existing chats carry their own project, so they always send.
    pub(super) fn send_blocked(&self, cx: &App) -> bool {
        if matches!(self.transport, ComposerTransport::Main)
            && self
                .pickers
                .read(cx)
                .resolved(cx)
                .harness
                .unwrap_or(HarnessId::Pi)
                == HarnessId::Pi
            && crate::prefs::slash_commands::command_intent(self.input.read(cx).text()).is_some()
        {
            return false;
        }
        let state = self.state.read(cx);
        state.selected_chat.is_none()
            && state.selected_space_row().is_none()
            && !state.scratch_pending
    }

    pub(super) fn button_mode(&self, cx: &App) -> SendButtonMode {
        // Attachments and transcript comments are independently sendable.
        // Comment-only live turns must show Steer rather than Stop.
        let has_content = has_send_content(
            !self.input.read(cx).text().trim().is_empty(),
            !self.staged().is_empty(),
            !self.comments.is_empty(),
        );
        send_button_mode(self.run_live(cx), has_content)
    }

    pub(super) fn on_submit(&mut self, cx: &mut Context<Self>) {
        if self.wizard.is_some() {
            // Enter inside the panel's free-text input submits the page — it
            // must never fall through and send a chat message under a panel
            // that is still on screen. `wizard_advance` folds the typed text
            // in on the way. Nothing to send yet: stay (an empty answer
            // would reach the agent as a dismissal).
            if self.wizard_ready(cx) {
                self.wizard_advance(cx);
            }
            return;
        }
        let text = self.input.read(cx).text().trim().to_string();
        if matches!(self.transport, ComposerTransport::Main)
            && self
                .pickers
                .read(cx)
                .resolved(cx)
                .harness
                .unwrap_or(HarnessId::Pi)
                == HarnessId::Pi
            && let Some(intent) = crate::prefs::slash_commands::command_intent(&text)
        {
            let state = self.state.read(cx);
            let target = state
                .selected_chat_row()
                .map(|c| c.device_id.clone())
                .or_else(|| state.selected_space_row().map(|s| s.device_id.clone()))
                .or_else(|| state.local_device_id.clone());
            self.input.update(cx, |input, cx| input.set_text("", cx));
            self.slash = SlashState::default();
            cx.emit(ComposerEvent::OpenProviders {
                intent,
                target_device: target,
            });
            return;
        }
        match self.button_mode(cx) {
            SendButtonMode::Stop => self.interrupt(cx),
            _ if !has_send_content(
                !text.is_empty(),
                !self.staged().is_empty(),
                !self.comments.is_empty(),
            ) => {}
            _ if self.send_blocked(cx) => {}
            SendButtonMode::Send => self.send(text, false, cx),
            SendButtonMode::Steer => self.send(text, true, cx),
        }
    }

    /// Queue a Run (or Steer) doc command with an optimistic echo. New chats
    /// thread the picked config in: worktree creation (when the isolated toggle
    /// is on), `Mutate createChat` with the `ChatConfig` + cwd, and the model /
    /// reasoning / options on the Run request itself.
    fn send(&mut self, text: String, steer: bool, cx: &mut Context<Self>) {
        self.send_with(text, steer, SendDraft::Take, cx);
    }

    /// Compact the selected session (the context ring's click): `/compact`
    /// rides an ordinary Run — every harness that offers the ring
    /// understands it — but the user's draft, attachments and comments stay
    /// put. Never mid-run (a steered `/compact` would reach the model as
    /// text) and never over an in-flight send (replacing `send_task` would
    /// cancel it).
    pub(super) fn compact_context(&mut self, cx: &mut Context<Self>) {
        if self.sending
            || self.run_live(cx)
            || !matches!(self.transport, ComposerTransport::Main)
            || self.state.read(cx).selected_chat.is_none()
        {
            return;
        }
        self.send_with("/compact".into(), false, SendDraft::Keep, cx);
    }

    /// The checks that block a send before anything is staged or cleared:
    /// on `Err` the draft, comments and attachments all stay put.
    fn validate_send(&self, text: &str, take_draft: bool, cx: &App) -> Result<(), SharedString> {
        if !text.trim_start().starts_with('/')
            && let Some(id) = self.pickers.read(cx).unavailable_pi_model(cx)
        {
            return Err(format!(
                "Model \"{id}\" is no longer available. Reconnect its provider in Settings → Providers, or choose another model."
            ).into());
        }
        // Annotated slash-command sends are blocked with an inline composer
        // error — comments and session references ride the NEXT NORMAL
        // Run/Steer only, and a slash command would be misrouted by the
        // harness's command interception. Everything (draft + comments) is
        // preserved. Comments never exist in a side chat (no annotation
        // surface) and session references are a main-surface feature, so the
        // block is main-only.
        if matches!(self.transport, ComposerTransport::Main) {
            if take_draft && block_slash_with_comments(!self.comments.is_empty(), text) {
                return Err(
                    "Comments can't be sent with a slash command — remove the /command or the comments first."
                        .into(),
                );
            }
            if block_slash_with_session_refs(text) {
                return Err(
                    "Session references can't be sent with a slash command — remove the /command or the references first."
                        .into(),
                );
            }
            // Send-time cap: pasted/private markup bypasses the picker's
            // insert-time guard, so a raw prompt with more than
            // MAX_SESSION_REFS DISTINCT refs is rejected before anything is
            // cleared (draft/comments/attachments all preserved).
            if session_send_cap_exceeded(text) {
                return Err("Up to 3 session references per message — remove one first.".into());
            }
            if block_slash_with_issue_refs(text) {
                return Err(
                    "Issue and pull request references can't be sent with a slash command — remove the /command or the references first."
                        .into(),
                );
            }
            if issue_refs(text).len() > MAX_ISSUE_REFS {
                return Err(
                    "Up to 3 issue or pull request references per message — remove one first."
                        .into(),
                );
            }
        }
        Ok(())
    }

    fn send_with(&mut self, text: String, steer: bool, draft: SendDraft, cx: &mut Context<Self>) {
        let take_draft = matches!(draft, SendDraft::Take);
        if let Err(message) = self.validate_send(&text, take_draft, cx) {
            self.failure = Some(message);
            cx.notify();
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.failure = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let target = match self.resolve_send_target(cx) {
            Ok(target) => target,
            Err(message) => {
                self.failure = Some(message);
                cx.notify();
                return;
            }
        };
        let refs = match self.snapshot_send_refs(&text, &target, cx) {
            Ok(refs) => refs,
            Err(message) => {
                self.failure = Some(message);
                cx.notify();
                return;
            }
        };
        let taken = self.begin_optimistic_send(&text, steer, take_draft, &target, cx);

        let steer_cmd = steer && !target.is_new;
        // Transport identity rides the async block (the side-chat branch
        // dispatches through `SEND_SIDE_CHAT`); the inherited sandbox comes
        // from the fork's synthetic row (the parent's config).
        let transport = self.transport.clone();
        let inherited_sandbox = self
            .state
            .read(cx)
            .selected_chat_row()
            .and_then(|c| c.config.as_ref())
            .map(|c| c.sandbox)
            .unwrap_or(SandboxLevel::WorkspaceWrite);
        let job = SendJob {
            engine,
            target,
            refs,
            text,
            steer_cmd,
            transport,
            inherited_sandbox,
            taken,
        };
        self.send_task = Some(cx.spawn(async move |this, cx| {
            let result = run_send(&job, &this, cx).await;
            this.update(cx, |composer, cx| {
                composer.sending = false;
                // The send has left the streaming stage on EVERY path (sealed,
                // failed mid-upload, or never uploaded at all) — retire the
                // "Uploading n%" trailer so the working spinner goes back to
                // narrating the run instead of a finished upload forever.
                composer.state.update(cx, |s, cx| {
                    s.end_upload_progress(&job.target.chat_id);
                    cx.notify();
                });
                if let Err(message) = result {
                    composer.on_send_failed(job, message, take_draft, cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Resolve where a send goes — chat id, checkout plan, model config,
    /// devices and project — NOW, so the async block needs no picker or
    /// state access.
    fn resolve_send_target(&self, cx: &App) -> Result<SendTarget, SharedString> {
        // Chat id: existing selection, or client-minted for the new-chat canvas
        // (the chat then appears from the doc host once the doc materializes).
        let (chat_id, is_new) = match self.state.read(cx).selected_chat.clone() {
            Some(id) => (id, false),
            None => (uuid::Uuid::new_v4().to_string(), true),
        };
        // Where the new session runs (Current checkout / reuse an existing
        // worktree / fresh worktree off the picked base) — resolved NOW so
        // the async block needs no picker access.
        let plan = self.pickers.read(cx).checkout_plan(cx);
        // New-worktree sends need a base ref: with refs never loaded the old
        // path silently fell back to the repo folder. Block the send visibly
        // instead — a request for a new worktree must never silently run in
        // the base checkout.
        if is_new
            && matches!(
                &plan,
                crate::pickers::CheckoutPlan::NewWorktree { base: None }
            )
        {
            return Err("Choose a base ref first — New worktree can't start without one.".into());
        }
        // Fully-resolved model/reasoning/options — concrete values (chat config
        // or defaults), so the engine never has to guess a "default".
        let resolved = self.pickers.read(cx).resolved(cx);
        let existing_cwd = self
            .state
            .read(cx)
            .selected_chat_row()
            .and_then(|c| c.cwd.clone());
        // The PROJECT fixes the new chat's device + base folder — sessions are
        // minted onto the project's device, not necessarily this one. With no
        // project ("Don't work in a project") the composer's device pick is
        // the host and the session runs from `~` there.
        let space = self.state.read(cx).selected_space_row().cloned();
        // A quick chat: the host mints a scratch folder for this chat id
        // before the row is created, and the session runs there.
        let scratch = is_new && space.is_none() && self.state.read(cx).scratch_pending;
        let local_device_id = self.state.read(cx).local_device_id.clone();
        let target_device_id = self.state.read(cx).effective_device_id();
        let device_id = if is_new {
            target_device_id
                .clone()
                .unwrap_or_else(|| "local".to_string())
        } else {
            self.state
                .read(cx)
                .selected_chat_row()
                .map(|c| c.device_id.clone())
                .or_else(|| local_device_id.clone())
                .unwrap_or_else(|| "local".to_string())
        };
        // Uploads/read-backs target the chat's HOST device (forwardable RPCs);
        // for a new chat that's the target device (None when it's local).
        let host_device_id = if is_new {
            target_device_id
                .clone()
                .filter(|id| local_device_id.as_deref() != Some(id.as_str()))
        } else {
            self.state
                .read(cx)
                .selected_chat_row()
                .map(|c| c.device_id.clone())
        };
        let space_id = space.as_ref().map(|s| s.id.clone());
        let space_path = space.as_ref().map(|s| s.path.clone());
        Ok(SendTarget {
            chat_id,
            is_new,
            plan,
            resolved,
            existing_cwd,
            scratch,
            local_device_id,
            device_id,
            host_device_id,
            space_id,
            space_path,
        })
    }

    /// Snapshot the issue and session references the prompt carries, and
    /// reject the ones that can never resolve before anything is staged.
    fn snapshot_send_refs(
        &self,
        text: &str,
        target: &SendTarget,
        cx: &App,
    ) -> Result<SendRefs, SharedString> {
        // Issue references (main transport only): each is snapshotted at send
        // time through the chat's host device's `gh`, before anything is
        // created or queued, so a failed lookup leaves nothing behind. A new
        // worktree for a session started from an issue is named after the
        // first one.
        let issues: Vec<IssueRef> = if matches!(self.transport, ComposerTransport::Main) {
            issue_refs(text)
        } else {
            Vec::new()
        };
        let worktree_hint = issues.first().map(issue_worktree_hint);
        // Session references (main transport only): the distinct chat ids the
        // prompt references, in mention order. The referenced rows are
        // resolved against the synced chats snapshot at send time inside the
        // async block; any missing/unreachable/malformed/timeout ref fails
        // the send visibly (the existing failure path restores everything).
        let session_ids: Vec<String> = if matches!(self.transport, ComposerTransport::Main) {
            session_ref_chat_ids(text)
        } else {
            Vec::new()
        };
        // Snapshot the chats rows now (the async block can't read state):
        // referenced ids are resolved against this snapshot at send time, and
        // an id with no row fails the send visibly.
        let session_chats: Vec<Chat> = if session_ids.is_empty() {
            Vec::new()
        } else {
            self.state.read(cx).chats.clone()
        };
        // Host presence for the referenced sessions, snapshotted with the
        // rows: an offline host is read from this device's synced replica
        // instead of waiting out a relay read that can't succeed.
        let offline_hosts: HashSet<String> = {
            let state = self.state.read(cx);
            let now = chrono::Utc::now();
            session_ids
                .iter()
                .filter_map(|id| session_chats.iter().find(|c| &c.id == id))
                .filter(|chat| !state.device_online(&chat.device_id, now))
                .map(|chat| chat.device_id.clone())
                .collect()
        };
        // Authoritative send validation against the snapshot: the current
        // chat and temporary child chats are rejected synchronously (before
        // anything is staged/cleared), with an actionable error and every
        // draft/comment/attachment preserved. Sessions from other projects
        // and other devices are legitimate references. Unknown ids still
        // fail later in async loading.
        if let Some(message) = session_refs_authoritative_error(
            &session_ids,
            (!target.is_new).then_some(target.chat_id.as_str()),
            &session_chats,
        ) {
            return Err(message.into());
        }
        Ok(SendRefs {
            issues,
            worktree_hint,
            session_ids,
            session_chats,
            offline_hosts,
        })
    }

    /// Take the draft and show the send at once: the attachment strip and
    /// input empty, the optimistic echo lands, comments clear, and `Sent` is
    /// emitted — all before the RPCs start.
    fn begin_optimistic_send(
        &mut self,
        text: &str,
        steer: bool,
        take_draft: bool,
        target: &SendTarget,
        cx: &mut Context<Self>,
    ) -> TakenDraft {
        let chat_id = &target.chat_id;
        let is_new = target.is_new;
        // Snapshot-and-clear NOW (use-attachments.ts takeAttachments): the
        // strip empties the instant you hit send; a failure hands the files
        // back into the chat's stash.
        let staged = if take_draft {
            self.preview = None;
            self.attachments
                .remove(&self.current_key)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let message_id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().timestamp_millis();

        // The echo carries synthetic attachment refs from the first frame, so
        // photos render while the send is still pending instead of waiting for
        // the upload to finish (real paths replace them in the post-upload
        // refresh). The refs resolve instantly: the staged bytes are seeded
        // into the transcript cache under every device key the transcript
        // consults, and the synthetic paths never persist — the queued command
        // and the doc entry are built from `with_attachments` on real paths.
        let echo_paths: Vec<String> = staged
            .iter()
            .map(|att| format!("pending/{}/{}", att.id, att.name))
            .collect();
        let echo_text = attachments::with_attachments(text, &echo_paths);
        seed_echo_images(
            &staged,
            &echo_paths,
            &target.device_id,
            target.local_device_id.as_deref(),
        );

        // Optimistic echo (client-minted id doubles as the persisted message id,
        // so the doc frame dedups it away). It shows the comments this send
        // takes, as the host's entry will.
        let echo_comments: Vec<cypher_doc::MessageComment> = if take_draft {
            self.comments
                .iter()
                .map(DraftComment::message_comment)
                .collect()
        } else {
            Vec::new()
        };
        let echo = echo_entry(&message_id, echo_text, created_at, echo_comments.clone());
        // Label the echo "Steer" now; the ledger's Steer command confirms it
        // once synced (Side Chat has no steer verb).
        let marks_steer = steer && !is_new && matches!(self.transport, ComposerTransport::Main);
        self.state.update(cx, |s, cx| {
            if is_new {
                s.select_chat(Some(chat_id.clone()), cx);
            }
            if marks_steer {
                s.mark_steer(&message_id);
            }
            s.push_echo(chat_id, echo);
            // Working overlay until the host executes the queued command —
            // without it a remote send flashed Completed (and could ring the
            // done-chime) in the queue→drain→sync gap.
            s.begin_pending_send(chat_id, &message_id, chrono::Utc::now());
            cx.notify();
        });

        if take_draft {
            self.input.update(cx, |input, cx| input.set_text("", cx));
            self.drafts.remove(&self.current_key);
        }
        self.failure = None;
        self.sending = true;
        // Comments: snapshot for the command + failure restore, then clear
        // OPTIMISTICALLY (the indicator hides the instant you send). They
        // ride ONLY a normal Run/Steer — RespondInput/interrupt never carry
        // them. Acceptance clears permanently; a queue failure restores them.
        let sent_comments = if take_draft {
            let sent = std::mem::take(&mut self.comments);
            self.comment_edit = None;
            if self.comments_popup.begin_close() {
                crate::kit::popover::reap_popup(cx, |this: &mut Self| &mut this.comments_popup);
            }
            sent
        } else {
            Vec::new()
        };
        cx.emit(ComposerEvent::Sent {
            chat_id: chat_id.clone(),
            message_id: message_id.clone(),
        });
        cx.notify();
        TakenDraft {
            staged,
            message_id,
            created_at,
            echo_comments,
            sent_comments,
        }
    }

    /// The send failed: red banner, echo removed, prompt back in the draft,
    /// staged files back in the chat's stash, comments restored.
    fn on_send_failed(
        &mut self,
        job: SendJob,
        message: String,
        take_draft: bool,
        cx: &mut Context<Self>,
    ) {
        let SendJob {
            target,
            text: restore_text,
            taken,
            ..
        } = job;
        let (chat_id, message_id) = (target.chat_id.as_str(), taken.message_id.as_str());
        let (staged, sent_comments) = (&taken.staged[..], &taken.sent_comments[..]);
        self.failure = Some(message.into());
        self.state.update(cx, |s, cx| {
            s.remove_echo(chat_id, message_id);
            s.end_pending_send(chat_id, message_id);
            cx.notify();
        });
        // A kept draft never left the input — nothing to restore.
        if take_draft {
            self.input
                .update(cx, |input, cx| input.set_text(restore_text, cx));
        }
        if !staged.is_empty() {
            // Merge by id (stashAttachments): files the user staged
            // while the send was in flight survive the hand-back.
            let slot = self.attachments.entry(chat_id.to_string()).or_default();
            let mut merged = staged.to_vec();
            merged.extend(
                slot.drain(..)
                    .filter(|e| !staged.iter().any(|f| f.id == e.id)),
            );
            *slot = merged;
        }
        // Restore the comments taken at send: the snapshot first,
        // then any added DURING the in-flight send (deduped by id)
        // — order is preserved. Only if still in the same chat.
        // Side chats never hold comments (defensive guard).
        if matches!(self.transport, ComposerTransport::Main)
            && self.state.read(cx).selected_chat.as_deref() == Some(chat_id)
        {
            self.comments =
                merge_restored_comments(sent_comments.to_vec(), std::mem::take(&mut self.comments));
        }
    }

    pub(super) fn interrupt(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        match self.transport.clone() {
            ComposerTransport::Main => {
                let Some(chat_id) = self.state.read(cx).selected_chat.clone() else {
                    return;
                };
                let params = serde_json::json!({
                    "chatId": chat_id,
                    "command": { "kind": "interrupt" },
                });
                self.send_task = Some(cx.spawn(async move |this, cx| {
                    let result = engine.client().call(methods::QUEUE_COMMAND, params).await;
                    if let Err(err) = result {
                        this.update(cx, |composer, cx| {
                            composer.failure = Some(format!("Stop failed: {err}").into());
                            cx.notify();
                        })
                        .ok();
                    }
                }));
            }
            ComposerTransport::SideChat(side) => {
                let mut params = serde_json::Map::new();
                params.insert(
                    "sideChatId".into(),
                    serde_json::Value::String(side.side_chat_id.clone()),
                );
                side.with_target(&mut params, self.state.read(cx).local_device_id.as_deref());
                self.send_task = Some(cx.spawn(async move |this, cx| {
                    let result = engine
                        .client()
                        .call(
                            methods::INTERRUPT_SIDE_CHAT,
                            serde_json::Value::Object(params),
                        )
                        .await;
                    if let Err(err) = result {
                        this.update(cx, |composer, cx| {
                            composer.failure = Some(format!("Stop failed: {err}").into());
                            cx.notify();
                        })
                        .ok();
                    }
                }));
            }
        }
    }

    /// Retry one failed durable Run/Steer attempt. The command watch will
    /// repaint the card as Retrying once the new pending attempt lands.
    pub(super) fn retry_failed_command(&mut self, command_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.failure = Some("Engine not connected".into());
            cx.notify();
            return;
        };
        let Some(chat_id) = self.state.read(cx).selected_chat.clone() else {
            return;
        };
        self.send_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::RETRY_COMMAND,
                    serde_json::json!({
                        "chatId": chat_id,
                        "commandId": command_id,
                    }),
                )
                .await;
            if let Err(err) = result {
                this.update(cx, |composer, cx| {
                    composer.failure = Some(format!("Retry failed: {err}").into());
                    cx.notify();
                })
                .ok();
            }
        }));
    }
}

/// Where a send goes, resolved before anything is staged.
struct SendTarget {
    chat_id: String,
    is_new: bool,
    plan: crate::pickers::CheckoutPlan,
    resolved: crate::pickers::ResolvedRunConfig,
    existing_cwd: Option<String>,
    scratch: bool,
    local_device_id: Option<String>,
    device_id: String,
    host_device_id: Option<String>,
    space_id: Option<String>,
    space_path: Option<String>,
}

/// The references a prompt carries, snapshotted at send time (the async
/// block can't read state).
struct SendRefs {
    issues: Vec<IssueRef>,
    worktree_hint: Option<String>,
    session_ids: Vec<String>,
    session_chats: Vec<Chat>,
    offline_hosts: HashSet<String>,
}

/// What the optimistic send took from the composer: the staged files and
/// comments to restore on failure, and the echo's identity.
struct TakenDraft {
    staged: Vec<StagedAttachment>,
    message_id: String,
    created_at: i64,
    echo_comments: Vec<cypher_doc::MessageComment>,
    sent_comments: Vec<DraftComment>,
}

/// Everything the async half of a send owns.
struct SendJob {
    engine: EngineHandle,
    target: SendTarget,
    refs: SendRefs,
    text: String,
    steer_cmd: bool,
    transport: ComposerTransport,
    inherited_sandbox: SandboxLevel,
    taken: TakenDraft,
}

/// A send's working directory: the run's cwd, the chat row's cwd, the
/// branch the footer names and the worktree the host should create.
struct SendCwd {
    cwd: String,
    worktree_cwd: Option<String>,
    chat_branch: Option<String>,
    run_worktree: Option<cypher_proto::WorktreeSpec>,
}

/// The async half of a send: snapshot references, resolve the working
/// directory, create the chat, upload (pre-queue paths), then queue or send.
async fn run_send(
    job: &SendJob,
    this: &WeakEntity<Composer>,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let SendJob {
        engine,
        target,
        refs,
        text,
        steer_cmd,
        transport,
        ..
    } = job;
    let staged = &job.taken.staged;
    let is_new = target.is_new;
    let issue_snapshots = fetch_issue_snapshots(
        engine,
        cx.background_executor(),
        target.host_device_id.as_deref(),
        &refs.issues,
    )
    .await?;
    let SendCwd {
        cwd,
        worktree_cwd,
        chat_branch,
        run_worktree,
    } = resolve_send_cwd(job, cx.background_executor()).await?;

    // Best-effort Mutate createChat with the picked config: the
    // engine resolves device + cwd from the PROJECT row when one
    // is picked; project-less chats name the host device outright
    // (idempotent; the doc host would materialize the chat on
    // first command anyway, so failures are non-fatal).
    if is_new {
        let mutate = create_chat_mutation(
            &target.chat_id,
            target.space_id.as_deref(),
            &target.device_id,
            worktree_cwd.as_deref(),
            chat_branch.as_deref(),
            &target.resolved,
        );
        if let Err(err) = engine.client().call(methods::MUTATE, mutate).await {
            tracing::warn!(error = %err, "CreateChat mutate unavailable; doc host will materialize the chat");
        }
    }

    // Queue-first sends (a Main-transport Run with staged
    // attachments) queue the durable command BEFORE any bytes
    // upload: a lost relay frame can't wedge the send and the
    // user's Run intent survives any upload hiccup. Those uploads
    // happen after the queue in the Main branch below. Steer
    // (refs ride the prompt text only — no separate attachments
    // field) and Side Chat (non-durable RPC) keep the pre-upload
    // path. Attachment-less sends have nothing to upload here.
    let queue_first_upload =
        matches!(transport, ComposerTransport::Main) && !steer_cmd && !staged.is_empty();
    let mut content = text.clone();
    let mut attachment_paths: Vec<String> = Vec::new();
    if !staged.is_empty() && !queue_first_upload {
        attachment_paths = pre_upload_attachments(job, cx).await?;
        content = attachments::with_attachments(text, &attachment_paths);
        // Refresh the echo in place with the attachment refs
        // (same id, same clock — the bubble grows its thumbnails
        // without flickering).
        refresh_echo(job, content.clone(), this, cx);
    }

    // Build the effective prompt only after attachment refs have
    // been added to the visible content. This is essential for an
    // annotated Steer (which has no separate attachments field)
    // and keeps Run's harness prompt identical to the visible
    // request transport apart from the annotations.
    //
    // Session references: each referenced transcript is loaded at
    // send time (WatchDocMessages reset, bounded) and composed as
    // UNTRUSTED REFERENCE CONTEXT — background only — ahead of
    // the comments block and the visible request. A missing/
    // unreachable/malformed/timed-out ref fails the send visibly
    // (never silently omitted) and the failure path restores the
    // draft, attachments, and comments.
    let session_contexts = load_session_contexts(
        engine,
        cx.background_executor(),
        target.local_device_id.as_deref(),
        &refs.session_ids,
        &refs.session_chats,
        &refs.offline_hosts,
    )
    .await?;
    let sent_comments = &job.taken.sent_comments;
    let agent_prompt =
        if sent_comments.is_empty() && session_contexts.is_empty() && issue_snapshots.is_empty() {
            None
        } else {
            Some(serialize_reference_prompt(
                &session_contexts,
                &issue_snapshots,
                sent_comments,
                &content,
            ))
        };

    let message_id = &job.taken.message_id;
    match transport {
        ComposerTransport::Main => {
            if *steer_cmd {
                // Steer: the pre-upload path already embedded the
                // attachment refs in `content` (Steer carries no
                // separate attachments field).
                let command = SessionCommandPayload::Steer {
                    prompt: content.clone(),
                    message_id: Some(message_id.clone()),
                    agent_prompt: agent_prompt.clone(),
                };
                queue_command(engine, &target.chat_id, &command).await?;
            } else if queue_first_upload {
                queue_first_run(job, content, cwd, run_worktree, agent_prompt, this, cx).await?;
            } else {
                // Plain Run (no staged attachments): prompt is the
                // bare text, no pending ids.
                let command = run_command(
                    job,
                    content.clone(),
                    cwd,
                    attachment_paths,
                    Vec::new(),
                    run_worktree,
                    agent_prompt.clone(),
                );
                queue_command(engine, &target.chat_id, &command).await?;
            }
        }
        ComposerTransport::SideChat(side) => {
            send_side_chat(job, side, content, cwd, attachment_paths).await?;
        }
    }
    Ok(())
}

/// Resolve the working directory: existing chats keep theirs; new chats run
/// per the checkout plan (t3code env-mode): the space's folder as-is, an
/// EXISTING worktree of the picked ref (a plain cwd override — multiple
/// sessions share one worktree), or a fresh isolated worktree the HOST
/// creates off the picked base ref at command-drain time (the durable
/// WorktreeSpec — never a pre-queue RPC).
async fn resolve_send_cwd(job: &SendJob, executor: &BackgroundExecutor) -> Result<SendCwd, String> {
    let target = &job.target;
    let is_new = target.is_new;
    let mut cwd = if is_new {
        // Project-less sessions run from the host's home dir —
        // "~" is expanded on the host when the run spawns.
        target.space_path.clone().or_else(|| Some("~".to_string()))
    } else {
        target.existing_cwd.clone()
    }
    .unwrap_or_else(|| ".".to_string());
    let mut worktree_cwd: Option<String> = None;
    if target.scratch {
        // The folder must exist on the HOST before the row names
        // it as cwd. Bounded: a lost relay frame fails the send
        // visibly instead of wedging it on "Sending…".
        let path = create_scratch_dir(
            &job.engine,
            executor,
            target.host_device_id.as_deref(),
            &target.chat_id,
        )
        .await?;
        cwd = path.clone();
        worktree_cwd = Some(path);
    }
    // Fresh-worktree plans ride the QUEUED Run command (a
    // WorktreeSpec the HOST materializes at drain time) instead of
    // a blocking CreateWorktree relay RPC here: the RPC had no
    // timeout, so a lost relay frame wedged the send on "Sending…"
    // forever while the session ran remotely anyway.
    // The picked ref rides createChat so the session footer names
    // it from the first frame (it read "Select ref" until the
    // host's diff reconciler got around to stamping the branch).
    let (chat_branch, run_worktree) = if is_new {
        plan_checkout(
            &target.plan,
            target.space_path.as_deref(),
            job.refs.worktree_hint.clone(),
            &mut cwd,
            &mut worktree_cwd,
        )
    } else {
        (None, None)
    };
    Ok(SendCwd {
        cwd,
        worktree_cwd,
        chat_branch,
        run_worktree,
    })
}

/// The legacy pre-upload path (Steer / Side Chat): upload every staged file
/// before the send and seed the transcript cache; returns the final paths.
async fn pre_upload_attachments(job: &SendJob, cx: &mut AsyncApp) -> Result<Vec<String>, String> {
    let target = &job.target;
    let staged = &job.taken.staged;
    let mut attachment_paths: Vec<String> = Vec::new();
    for att in staged {
        // Legacy pre-upload path (Steer / Side Chat): the
        // upload id is internal-only (no durable command
        // references it) and no chat seal is needed.
        let upload_id = uuid::Uuid::new_v4().to_string();
        match attachments::upload_attachment(
            &job.engine,
            cx.background_executor(),
            target.host_device_id.as_deref(),
            att,
            &upload_id,
            None,
            None,
        )
        .await
        {
            Ok(path) => attachment_paths.push(path),
            Err(err) => {
                tracing::warn!(name = %att.name, error = %err, "attachment upload failed");
                return Err(
                    "Couldn't upload the attachment — the device may be offline.".to_string(),
                );
            }
        }
    }
    // Seed the transcript cache from local bytes so the sent
    // bubble's thumbnails never round-trip (seedTranscript-
    // Attachment in the original send path).
    let seed_device = target
        .host_device_id
        .clone()
        .unwrap_or_else(|| target.device_id.clone());
    seed_echo_images(
        staged,
        &attachment_paths,
        &seed_device,
        Some(&target.device_id),
    );
    Ok(attachment_paths)
}

/// Replace the optimistic echo with one carrying `content` (same id, same
/// clock).
fn refresh_echo(job: &SendJob, content: String, this: &WeakEntity<Composer>, cx: &mut AsyncApp) {
    let refreshed = echo_entry(
        &job.taken.message_id,
        content,
        job.taken.created_at,
        job.taken.echo_comments.clone(),
    );
    let echo_chat_id = job.target.chat_id.clone();
    let message_id = &job.taken.message_id;
    this.update(cx, |composer, cx| {
        composer.state.update(cx, |s, cx| {
            s.remove_echo(&echo_chat_id, message_id);
            s.push_echo(&echo_chat_id, refreshed);
            cx.notify();
        });
    })
    .ok();
}

/// Queue-first Run: the durable command carries the Run intent + PENDING
/// attachment descriptors (never the bytes, never pending refs in the
/// transcript — the prompt is the bare text). The host holds the command at
/// WaitForAttachments until every upload is sealed, then resolves the ids to
/// final paths and appends the refs trailer.
async fn queue_first_run(
    job: &SendJob,
    content: String,
    cwd: String,
    run_worktree: Option<cypher_proto::WorktreeSpec>,
    agent_prompt: Option<String>,
    this: &WeakEntity<Composer>,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let SendJob {
        engine,
        target,
        text,
        ..
    } = job;
    let staged = &job.taken.staged;
    let chat_id = &target.chat_id;
    let device_id = &target.device_id;
    let host_device_id = &target.host_device_id;
    let mut attachment_paths: Vec<String> = Vec::new();
    let pending_attachments: Vec<cypher_proto::PendingAttachment> = staged
        .iter()
        .map(|att| cypher_proto::PendingAttachment {
            upload_id: uuid::Uuid::new_v4().to_string(),
            file_name: att.name.clone(),
        })
        .collect();
    let command = run_command(
        job,
        content.clone(),
        cwd,
        Vec::new(),
        pending_attachments.clone(),
        run_worktree,
        agent_prompt.clone(),
    );
    // Queue FIRST — durable by construction. A queue
    // failure returns Err and the outer failure path
    // restores the draft/stash/comments (nothing was
    // uploaded yet).
    queue_command(engine, chat_id, &command).await?;
    let progress = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let total_bytes = staged
        .iter()
        .map(|attachment| attachment.bytes().len() as u64)
        .sum();
    let progress_for_state = progress.clone();
    let progress_chat_id = chat_id.clone();
    this.update(cx, |composer, cx| {
        composer.state.update(cx, |state, cx| {
            state.begin_upload_progress(&progress_chat_id, total_bytes, progress_for_state);
            cx.notify();
        });
    })
    .ok();
    // The command is durable — now stream the bytes.
    // Each UploadCommit seals against this chat so the
    // host's drain releases the Run. An upload failure
    // does NOT delete the durable command: the host's
    // attachment grace window eventually expires it
    // and the user can Retry. (The optimistic echo
    // stays, seeded from local bytes; once the host
    // rejects, the ledger's failed row takes over.)
    for (att, pending) in staged.iter().zip(&pending_attachments) {
        match attachments::upload_attachment(
            engine,
            cx.background_executor(),
            host_device_id.as_deref(),
            att,
            &pending.upload_id,
            Some(chat_id),
            Some(progress.clone()),
        )
        .await
        {
            Ok(path) => attachment_paths.push(path),
            Err(err) => {
                tracing::warn!(
                    name = %att.name,
                    error = %err,
                    "post-queue attachment upload failed"
                );
                this.update(cx, |composer, cx| {
                    composer.sending = false;
                    composer.failure = Some(
                        "Attachments couldn't finish uploading — the message stays queued but the host will fail it unless the upload completes. Retry once the device is reachable."
                            .into(),
                    );
                    cx.notify();
                })
                .ok();
                return Ok(());
            }
        }
    }
    // Seed the transcript cache from local bytes and
    // refresh the echo with the REAL refs (the queued
    // command's bare prompt is replaced in the doc by
    // the host's entry carrying the trailer).
    let seed_device = host_device_id.clone().unwrap_or_else(|| device_id.clone());
    seed_echo_images(staged, &attachment_paths, &seed_device, Some(device_id));
    refresh_echo(
        job,
        attachments::with_attachments(text, &attachment_paths),
        this,
        cx,
    );
    Ok(())
}

/// Send AND live steer both ride `SendSideChat` with the RunRequest +
/// messageId (the engine resumes the same temporary chat; there is no
/// separate steer verb). The inherited sandbox (parent config) is threaded
/// through instead of the main surface's hardcoded default.
async fn send_side_chat(
    job: &SendJob,
    side: &ComposerSideChat,
    content: String,
    cwd: String,
    attachment_paths: Vec<String>,
) -> Result<(), String> {
    let resolved = &job.target.resolved;
    let request = ComposerSideChat::run_request(
        content.clone(),
        cwd,
        resolved.harness,
        resolved.model.clone(),
        resolved.reasoning,
        resolved.model_options.clone(),
        job.inherited_sandbox,
        attachment_paths,
    );
    let mut params = serde_json::Map::new();
    params.insert(
        "sideChatId".into(),
        serde_json::Value::String(side.side_chat_id.clone()),
    );
    params.insert(
        "request".into(),
        serde_json::to_value(&request).map_err(|e| format!("Send failed: {e}"))?,
    );
    params.insert(
        "messageId".into(),
        serde_json::Value::String(job.taken.message_id.clone()),
    );
    side.with_target(&mut params, job.target.local_device_id.as_deref());
    job.engine
        .client()
        .call(methods::SEND_SIDE_CHAT, serde_json::Value::Object(params))
        .await
        .map_err(|e| format!("Send failed: {e}"))?;
    Ok(())
}

/// The optimistic user entry for a send (the client-minted id doubles as the
/// persisted message id, so the doc frame dedups it away).
fn echo_entry(
    message_id: &str,
    text: String,
    created_at: i64,
    comments: Vec<cypher_doc::MessageComment>,
) -> SessionMessageEntry {
    SessionMessageEntry {
        id: message_id.to_string(),
        role: cypher_doc::MessageRole::User,
        parts: vec![MessagePart::Text {
            id: "t0".into(),
            text,
            agent_text: None,
        }],
        created_at,
        device_id: "local".into(),
        status: None,
        continuation_of: None,
        completed_at: None,
        comments,
        models: Vec::new(),
    }
}

/// Seed the transcript cache with the staged images under `primary`, and
/// under `secondary` too when it names another device, so the sent bubble's
/// thumbnails never round-trip. Plain files render as tiles from the path
/// alone — nothing to seed.
fn seed_echo_images(
    staged: &[StagedAttachment],
    paths: &[String],
    primary: &str,
    secondary: Option<&str>,
) {
    for (path, att) in paths.iter().zip(staged) {
        let Some(image) = att.image() else { continue };
        attachments::seed_attachment(primary, path, &att.name, image.clone());
        if let Some(secondary) = secondary
            && secondary != primary
        {
            attachments::seed_attachment(secondary, path, &att.name, image.clone());
        }
    }
}

/// Address a forwardable RPC at the chat's host device (`None` = local).
fn insert_target_device(params: &mut serde_json::Value, host: Option<&str>) {
    if let (Some(host), Some(object)) = (host, params.as_object_mut()) {
        object.insert(
            "targetDeviceId".into(),
            serde_json::Value::String(host.to_string()),
        );
    }
}

/// Snapshot each referenced issue through the host device's `gh`, bounded per
/// lookup; any failure fails the send before anything is created or queued.
async fn fetch_issue_snapshots(
    engine: &EngineHandle,
    executor: &BackgroundExecutor,
    host: Option<&str>,
    issues: &[IssueRef],
) -> Result<Vec<cypher_proto::GithubIssueSnapshot>, String> {
    let mut issue_snapshots: Vec<cypher_proto::GithubIssueSnapshot> =
        Vec::with_capacity(issues.len());
    for issue in issues {
        let reference = format!(
            "{} {}#{}",
            github_kind_noun(issue.pull),
            issue.repo,
            issue.number
        );
        let mut params = serde_json::json!({
            "repo": issue.repo,
            "number": issue.number,
        });
        insert_target_device(&mut params, host);
        let call = engine.client().call(methods::GET_GITHUB_ISSUE, params);
        let deadline = executor.timer(ISSUE_LOAD_TIMEOUT);
        futures::pin_mut!(call);
        futures::pin_mut!(deadline);
        let value = match futures::future::select(call, deadline).await {
            futures::future::Either::Left((Ok(value), _)) => value,
            futures::future::Either::Left((Err(err), _)) => {
                return Err(format!("Couldn't load GitHub {reference}: {err}"));
            }
            futures::future::Either::Right(_) => {
                return Err(format!("Loading GitHub {reference} timed out."));
            }
        };
        let mut snapshot: cypher_proto::GithubIssueSnapshot = serde_json::from_value(value)
            .map_err(|_| format!("The device returned an unreadable snapshot of {reference}."))?;
        // Older engines don't say which kind the number is.
        if issue.pull {
            snapshot.kind = cypher_proto::GithubIssueKind::PullRequest;
        }
        issue_snapshots.push(snapshot);
    }
    Ok(issue_snapshots)
}

/// Have the host mint this chat's scratch folder; returns its path.
async fn create_scratch_dir(
    engine: &EngineHandle,
    executor: &BackgroundExecutor,
    host: Option<&str>,
    chat_id: &str,
) -> Result<String, String> {
    let mut params = serde_json::json!({ "chatId": chat_id });
    insert_target_device(&mut params, host);
    let deadline = executor.timer(std::time::Duration::from_secs(20));
    let call = engine.client().call(methods::CREATE_SCRATCH_DIR, params);
    futures::pin_mut!(call);
    futures::pin_mut!(deadline);
    match futures::future::select(call, deadline).await {
        futures::future::Either::Left((Ok(value), _)) => value["path"]
            .as_str()
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .ok_or_else(|| "The device returned no scratch folder.".to_string()),
        futures::future::Either::Left((Err(err), _)) => {
            Err(format!("Could not create the scratch folder: {err}"))
        }
        futures::future::Either::Right(_) => Err("Creating the scratch folder timed out.".into()),
    }
}

/// Apply a new chat's checkout plan to its working directory; returns the
/// branch the footer names and the worktree the host should create.
fn plan_checkout(
    plan: &crate::pickers::CheckoutPlan,
    space_path: Option<&str>,
    worktree_hint: Option<String>,
    cwd: &mut String,
    worktree_cwd: &mut Option<String>,
) -> (Option<String>, Option<cypher_proto::WorktreeSpec>) {
    match plan {
        crate::pickers::CheckoutPlan::CurrentCheckout { branch } => (branch.clone(), None),
        crate::pickers::CheckoutPlan::ReuseWorktree { path, branch } => {
            *cwd = path.clone();
            *worktree_cwd = Some(path.clone());
            (branch.clone(), None)
        }
        crate::pickers::CheckoutPlan::NewWorktree { base } => {
            // Footer shows the base until the host stamps the actual
            // cypher/<name> branch post-creation. cwd stays the repo folder
            // — the compatible initial cwd for an old host that doesn't know
            // the spec (it degrades to the main checkout instead of failing
            // the run).
            let worktree = match (space_path, base) {
                (Some(repo_path), Some(base)) => Some(cypher_proto::WorktreeSpec {
                    repo_path: repo_path.to_string(),
                    base_ref: base.clone(),
                    name_hint: worktree_hint,
                }),
                _ => None,
            };
            (base.clone(), worktree)
        }
    }
}

/// The `createChat` mutation for a new chat: the engine resolves device + cwd
/// from the PROJECT row when one is picked; project-less chats name the host
/// device outright.
fn create_chat_mutation(
    chat_id: &str,
    space_id: Option<&str>,
    device_id: &str,
    worktree_cwd: Option<&str>,
    branch: Option<&str>,
    resolved: &crate::pickers::ResolvedRunConfig,
) -> serde_json::Value {
    let mut mutate = serde_json::json!({
        "op": "createChat",
        "chatId": chat_id,
    });
    if let Some(object) = mutate.as_object_mut() {
        match space_id {
            Some(space_id) => {
                object.insert(
                    "spaceId".into(),
                    serde_json::Value::String(space_id.to_string()),
                );
            }
            None => {
                object.insert(
                    "deviceId".into(),
                    serde_json::Value::String(device_id.to_string()),
                );
            }
        }
        if let Some(worktree_cwd) = worktree_cwd {
            object.insert(
                "cwd".into(),
                serde_json::Value::String(worktree_cwd.to_string()),
            );
        }
        if let Some(branch) = branch {
            object.insert(
                "branch".into(),
                serde_json::Value::String(branch.to_string()),
            );
        }
        if let Some(config) = resolved.chat_config()
            && let Ok(config) = serde_json::to_value(&config)
        {
            object.insert("config".into(), config);
        }
    }
    mutate
}

/// Load each referenced session's transcript (bounded) as untrusted
/// reference context; a missing, unreachable, malformed or timed-out
/// reference fails the send rather than being silently omitted.
async fn load_session_contexts(
    engine: &EngineHandle,
    executor: &BackgroundExecutor,
    local_device_id: Option<&str>,
    chat_ids: &[String],
    chats: &[Chat],
    offline_hosts: &HashSet<String>,
) -> Result<Vec<SessionReference>, String> {
    let mut contexts = Vec::with_capacity(chat_ids.len());
    for chat_id in chat_ids {
        let Some(chat) = chats.iter().find(|c| &c.id == chat_id) else {
            return Err(
                "A referenced session no longer exists — remove the @session reference and try again."
                    .to_string(),
            );
        };
        let entries = read_session_reset(
            engine,
            executor,
            local_device_id,
            chat_id,
            &chat.device_id,
            !offline_hosts.contains(&chat.device_id),
        )
        .await?;
        // Strip attachment refs BEFORE the safe visible-content
        // policy so absolute attachment paths never leak.
        let stripped: Vec<SessionMessageEntry> =
            entries.iter().map(strip_attachment_trailer).collect();
        let context =
            cypher_engine::bounded_transcript_context(&stripped, None).unwrap_or_default();
        contexts.push(SessionReference {
            title: session_display_title(chat),
            context,
        });
    }
    Ok(contexts)
}

/// A main-surface Run of the send's message with its resolved model config.
fn run_command(
    job: &SendJob,
    prompt: String,
    cwd: String,
    attachments: Vec<String>,
    pending_attachments: Vec<cypher_proto::PendingAttachment>,
    worktree: Option<cypher_proto::WorktreeSpec>,
    agent_prompt: Option<String>,
) -> SessionCommandPayload {
    let resolved = &job.target.resolved;
    let message_id = &job.taken.message_id;
    SessionCommandPayload::Run {
        request: RunRequest {
            prompt,
            harness: resolved.harness,
            model: resolved.model.clone(),
            reasoning: resolved.reasoning,
            model_options: resolved.model_options.clone(),
            cwd,
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: false,
            resume: None,
            attachments,
            pending_attachments,
            worktree,
        },
        message_id: message_id.to_string(),
        agent_prompt,
    }
}

/// Queue a durable command on `chat_id`.
async fn queue_command(
    engine: &EngineHandle,
    chat_id: &str,
    command: &SessionCommandPayload,
) -> Result<(), String> {
    let command = serde_json::to_value(command).map_err(|e| format!("Send failed: {e}"))?;
    let params = serde_json::json!({
        "chatId": chat_id,
        "command": command
    });
    engine
        .client()
        .call(methods::QUEUE_COMMAND, params)
        .await
        .map_err(|e| format!("Send failed: {e}"))?;
    Ok(())
}
