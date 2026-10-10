//! Slash commands.

use super::*;

impl Composer {
    fn command_target(&self, cx: &App) -> Option<String> {
        if let ComposerTransport::SideChat(side) = &self.transport {
            return Some(side.target_device_id.clone());
        }
        let state = self.state.read(cx);
        state
            .selected_chat_row()
            .map(|chat| chat.device_id.clone())
            .or_else(|| {
                state
                    .selected_space_row()
                    .map(|space| space.device_id.clone())
            })
            .or_else(|| state.local_device_id.clone())
    }

    fn sync_slash_owner(&mut self, cx: &App) {
        let owner = self.command_target(cx);
        if self.slash_owner == owner {
            return;
        }
        self.slash_owner = owner;
        self.slash_generation = self.slash_generation.wrapping_add(1);
        self.slash_cache.clear();
        self.slash_prefetch = None;
        self.slash_task = None;
        self.slash.request = self.slash.request.wrapping_add(1);
        self.slash.harness = None;
        self.slash.menu = Default::default();
        self.slash.loading = false;
        self.slash.error = None;
    }

    /// Warm [`Self::slash_cache`] without opening the popup, so the first `/`
    /// is not a cold `pi --mode rpc` spawn.
    pub(super) fn prefetch_slash_commands(&mut self, cx: &mut Context<Self>) {
        if !crate::prefs::slash_commands::any_shown_in_app(cx) {
            return;
        }
        self.sync_slash_owner(cx);
        let Some(harness) = self.pickers.read(cx).resolved(cx).harness else {
            return;
        };
        if self.slash_cache.contains_key(&harness) || self.slash_prefetch.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let target = self.command_target(cx);
        let generation = self.slash_generation;
        self.slash_prefetch = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::json!({ "harness": harness });
            if let (Some(target), Some(object)) = (&target, params.as_object_mut()) {
                object.insert("targetDeviceId".into(), target.clone().into());
            }
            let result = engine.client().call(methods::LIST_COMMANDS, params).await;
            this.update(cx, |composer, cx| {
                if composer.command_target(cx) != target || composer.slash_generation != generation
                {
                    return;
                }
                composer.slash_prefetch = None;
                if let Ok(value) = result
                    && let Ok(commands) = serde_json::from_value::<Vec<SlashCommand>>(value)
                {
                    composer.slash_cache.insert(harness, commands);
                    if composer.slash.token.is_some() {
                        composer.slash.loading = false;
                        composer.refilter_slash(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The `/` token under the cursor, and the command whose choices it picks
    /// from: the command list while a name is typed, a command's choices
    /// once a command that has them is followed by a space. The command list
    /// always offers the composer's own actions (Attach files), so `/` opens
    /// it even with no command turned on in Settings.
    fn slash_target(
        &self,
        text: &str,
        cursor: usize,
        harness: Option<HarnessId>,
        cx: &App,
    ) -> (Option<MentionToken>, Option<String>) {
        if let Some(token) = slash_token(text, cursor) {
            return (Some(token), None);
        }
        let Some(choice) = crate::composer::slash_menu::choice_token(text, cursor) else {
            return (None, None);
        };
        let offered = harness
            .and_then(|harness| self.slash_cache.get(&harness))
            .is_some_and(|commands| commands.iter().any(|c| c.name == choice.command));
        if !offered
            || crate::composer::slash_menu::choices(&choice.command).is_empty()
            || !crate::prefs::slash_commands::shows_in_app(cx, &choice.command)
        {
            return (None, None);
        }
        (
            Some(MentionToken {
                range: choice.range,
                query: choice.query,
            }),
            Some(choice.command),
        )
    }

    /// Ask the chat's host what the Pi plugins' switches are, for the menu's
    /// badges. Only worth asking when a command they belong to is shown; a
    /// host too old to answer just leaves the badges off.
    fn fetch_slash_modes(&mut self, cx: &mut Context<Self>) {
        let pi = self
            .pickers
            .read(cx)
            .resolved(cx)
            .harness
            .unwrap_or(HarnessId::Pi)
            == HarnessId::Pi;
        let relevant = ["fast", "scripts", "orchestrate", "goal"]
            .iter()
            .any(|name| crate::prefs::slash_commands::shows_in_app(cx, name));
        if !pi || !relevant || !matches!(self.transport, ComposerTransport::Main) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let chat = self.state.read(cx).selected_chat.clone();
        let target = self.command_target(cx);
        self.slash_modes_request = self.slash_modes_request.wrapping_add(1);
        let request = self.slash_modes_request;
        self.slash_modes_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::json!({ "chatId": chat });
            if let (Some(target), Some(object)) = (&target, params.as_object_mut()) {
                object.insert("targetDeviceId".into(), target.clone().into());
            }
            let result = engine
                .client()
                .call(methods::PI_SESSION_MODES, params)
                .await;
            this.update(cx, |composer, cx| {
                if composer.slash_modes_request != request {
                    return;
                }
                match result.map(serde_json::from_value::<PiSessionModes>) {
                    Ok(Ok(modes)) => composer.slash_modes = Some((chat, modes)),
                    Ok(Err(err)) => tracing::debug!(%err, "Pi session modes decode failed"),
                    Err(err) => tracing::debug!(%err, "Pi session modes unavailable"),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Track the `/` token on every edit: open/refresh the popup, fetch the
    /// harness's command list on first open, filter locally per keystroke.
    pub(super) fn update_slash(&mut self, text: &str, cursor: usize, cx: &mut Context<Self>) {
        self.sync_slash_owner(cx);
        let harness = self.pickers.read(cx).resolved(cx).harness;
        let (token, parent) = self.slash_target(text, cursor, harness, cx);
        let still_dismissed = token.as_ref().is_some_and(|token| {
            self.slash.dismissed.as_ref().is_some_and(|(range, value)| {
                token.range == *range && text.get(range.clone()) == Some(value.as_str())
            })
        });
        if still_dismissed {
            self.slash.token = None;
            self.sync_mention_controls(cx);
            return;
        }
        self.slash.dismissed = None;
        let harness_changed = self.slash.harness != harness;
        if token == self.slash.token && parent == self.slash.parent && !harness_changed {
            self.refilter_slash(cx);
            return;
        }
        let opened = self.slash.token.is_none() && token.is_some();
        self.slash.token = token.clone();
        self.slash.parent = parent;
        self.slash_scroll.set_offset(Point::default());
        self.slash.harness = harness;
        self.slash.error = None;
        if token.is_none() {
            self.slash.active = None;
            self.sync_mention_controls(cx);
            return;
        }
        if opened {
            self.fetch_slash_modes(cx);
        }
        // No resolved harness (catalog still loading): the actions only, no
        // fetch. Nor is the agent's list worth a fetch (a cold Pi spawn) when
        // none of its commands is turned on: the menu has only the actions.
        let Some(harness) = harness.filter(|_| crate::prefs::slash_commands::any_shown_in_app(cx))
        else {
            self.slash.loading = false;
            self.refilter_slash(cx);
            return;
        };
        if self.slash_cache.contains_key(&harness) {
            self.slash.loading = false;
            self.refilter_slash(cx);
            return;
        }
        if self.slash_prefetch.is_some() {
            self.slash.loading = true;
            self.refilter_slash(cx);
            return;
        }
        // First open for this harness: one ListCommands, targeted like file
        // search (the chat/space host device owns the agent binary).
        self.slash.request = self.slash.request.wrapping_add(1);
        self.slash.loading = true;
        self.refilter_slash(cx);
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.slash.loading = false;
            return;
        };
        let target = self.command_target(cx);
        let request = self.slash.request;
        self.slash_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::json!({ "harness": harness });
            if let (Some(target), Some(object)) = (&target, params.as_object_mut()) {
                object.insert("targetDeviceId".into(), target.clone().into());
            }
            let result = engine.client().call(methods::LIST_COMMANDS, params).await;
            this.update(cx, |composer, cx| {
                if composer.slash.request != request || composer.command_target(cx) != target {
                    return;
                }
                composer.slash.loading = false;
                match result {
                    Ok(value) => match serde_json::from_value::<Vec<SlashCommand>>(value) {
                        Ok(commands) => {
                            composer.slash_cache.insert(harness, commands);
                        }
                        Err(err) => tracing::warn!(%err, "slash command decode failed"),
                    },
                    Err(err) => {
                        tracing::debug!(%err, "slash command discovery failed");
                        composer.slash.error = Some(slash_error_message(&err));
                    }
                }
                composer.refilter_slash(cx);
            })
            .ok();
        }));
        cx.notify();
    }

    /// Re-rank the cached list for the current query (pure local filter).
    fn refilter_slash(&mut self, cx: &mut Context<Self>) {
        let query = self
            .slash
            .token
            .as_ref()
            .map(|t| t.query.clone())
            .unwrap_or_default();
        let menu = match &self.slash.parent {
            Some(parent) => crate::composer::slash_menu::choice_level(parent, &query),
            None => {
                let commands = self
                    .slash
                    .harness
                    .and_then(|h| self.slash_cache.get(&h))
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                crate::composer::slash_menu::command_level(
                    &crate::composer::slash_menu::Action::ALL,
                    commands,
                    |name| crate::prefs::slash_commands::shows_in_app(cx, name),
                    &query,
                )
            }
        };
        if self.slash.parent.is_some() && menu.selectable.is_empty() {
            // Typing an argument no choice starts with (a goal's text): the
            // menu gets out of the way until the word matches one again.
            self.slash.token = None;
            self.slash.parent = None;
            self.slash.menu = Default::default();
            self.slash.active = None;
        } else {
            self.slash.active = menu.preferred;
            self.slash.menu = menu;
        }
        self.sync_mention_controls(cx);
        cx.notify();
    }

    pub(super) fn move_slash(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.slash.active = crate::kit::popover::menu_step(
            self.slash.active,
            self.slash.menu.selectable.len(),
            delta,
        );
        // Keep the keyboard-highlighted row in view. Every menu row, group
        // headings included, is a direct child of the scroll container.
        if let Some(row) = self
            .slash
            .active
            .and_then(|active| self.slash.menu.selectable.get(active))
        {
            self.slash_scroll.scroll_to_item(*row);
        }
        self.sync_mention_controls(cx);
        cx.notify();
    }

    pub(super) fn dismiss_slash(&mut self, cx: &mut Context<Self>) {
        let dismissed = self.slash.token.as_ref().and_then(|token| {
            self.input
                .read(cx)
                .text()
                .get(token.range.clone())
                .map(|text| (token.range.clone(), text.to_string()))
        });
        self.reset_slash(dismissed, cx);
        cx.notify();
    }

    /// Type the highlighted row into the prompt. The input adds a space after
    /// it, so a command with choices opens them on the edit that follows
    /// ([`Self::update_slash`]), and a typed choice closes the menu.
    pub(super) fn accept_slash(&mut self, cx: &mut Context<Self>) {
        use crate::composer::slash_menu::Row;
        let Some(token) = self.slash.token.clone() else {
            return;
        };
        let Some(row) = self
            .slash
            .active
            .and_then(|active| self.slash.menu.selectable.get(active))
            .and_then(|&row| self.slash.menu.rows.get(row))
            .cloned()
        else {
            return;
        };
        let replacement = match row {
            Row::Action(crate::composer::slash_menu::Action::Attach) => {
                // The action types nothing: drop the `/…` that summoned it.
                self.input
                    .update(cx, |input, cx| input.remove_plain_token(token.range, cx));
                self.reset_slash(None, cx);
                self.open_file_picker(cx);
                cx.notify();
                return;
            }
            Row::Command(ix) => {
                let Some(command) = self
                    .slash
                    .harness
                    .and_then(|h| self.slash_cache.get(&h))
                    .and_then(|commands| commands.get(ix))
                else {
                    return;
                };
                format!("/{}", command.name)
            }
            Row::Choice(choice) => choice.value.to_string(),
            Row::Header(_) => return,
        };
        self.input.update(cx, |input, cx| {
            input.replace_plain_token(token.range, &replacement, cx)
        });
        self.reset_slash(None, cx);
        cx.notify();
    }

    /// Tear down the slash completion (mirrors [`Self::reset_mention`]).
    pub(super) fn reset_slash(
        &mut self,
        dismissed: Option<(Range<usize>, String)>,
        cx: &mut Context<Self>,
    ) {
        let request = self.slash.request.wrapping_add(1);
        self.slash_task = None;
        self.slash = SlashState {
            request,
            dismissed,
            harness: self.slash.harness,
            ..SlashState::default()
        };
        self.sync_mention_controls(cx);
    }

    pub(super) fn render_slash_popup(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        use crate::composer::slash_menu::{self, Row};
        let token = self.slash.token.as_ref()?;
        let commands = self
            .slash
            .harness
            .and_then(|h| self.slash_cache.get(&h))
            .map(Vec::as_slice)
            .unwrap_or_default();
        // What the badges can say about the chat this prompt goes to. A Side
        // Chat's composer has no Pi session switches of its own here.
        let main = matches!(self.transport, ComposerTransport::Main);
        let (chat, context, running_subagents) = {
            let state = self.state.read(cx);
            let chat = state.selected_chat.clone();
            let session = chat
                .as_deref()
                .and_then(|id| state.session_for(id))
                .filter(|_| main);
            let running = session.map_or(0, |session| {
                session
                    .subagents
                    .iter()
                    .filter(|run| run.status == cypher_proto::SubagentRunStatus::Running)
                    .count()
            });
            (
                chat,
                session.and_then(|session| session.context_usage),
                running,
            )
        };
        let modes = self
            .slash_modes
            .as_ref()
            .filter(|(for_chat, _)| main && *for_chat == chat)
            .map(|(_, modes)| modes);
        let facts = slash_menu::Facts {
            modes,
            context,
            running_subagents,
        };
        let mut card = crate::kit::popover::popover_card(theme)
            .w(px(420.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_slash(cx)));
        // The rows come first (the composer's actions are there even while
        // the agent's list loads or fails), then a line for that list.
        let status: Option<gpui::AnyElement> = if self.slash.loading && commands.is_empty() {
            Some(
                crate::kit::popover::skeleton_rows("slash-loading", theme, 2, cx.entity_id(), cx)
                    .into_any_element(),
            )
        } else if let Some(error) = self.slash.error.clone() {
            Some(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(px(12.0))
                    .text_color(theme.danger_muted)
                    .child(error)
                    .into_any_element(),
            )
        } else if self.slash.menu.selectable.is_empty() {
            Some(
                div()
                    .px(px(12.0))
                    .py(px(10.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child("No matching commands")
                    .into_any_element(),
            )
        } else {
            None
        };
        if !self.slash.menu.rows.is_empty() {
            let parent = self.slash.parent.clone();
            let chevron_column = self.slash.menu.rows.iter().any(|row| match row {
                Row::Command(ix) => commands
                    .get(*ix)
                    .is_some_and(|command| !slash_menu::choices(&command.name).is_empty()),
                _ => false,
            });
            // A command's choices: which command, and what is in effect now.
            if let Some(parent) = &parent {
                let summary = slash_menu::choice_summary(parent, &facts);
                card = card
                    .child(
                        div()
                            .px(px(8.0))
                            .pt(px(4.0))
                            .pb(px(2.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .text_size(px(12.0))
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(SharedString::from(format!("/{parent}"))),
                            )
                            .children(summary.map(|summary| {
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(summary))
                            })),
                    )
                    .child(crate::kit::popover::menu_separator());
            }
            let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(self.slash.menu.rows.len());
            for (row_ix, row) in self.slash.menu.rows.iter().enumerate() {
                let position = self
                    .slash
                    .menu
                    .selectable
                    .iter()
                    .position(|&selectable| selectable == row_ix);
                let selected = position.is_some() && self.slash.active == position;
                let accept = position.map(|position| {
                    cx.listener(move |this, _, _, cx| {
                        this.slash.active = Some(position);
                        this.accept_slash(cx);
                    })
                });
                let line = div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(8.0));
                let row = match row {
                    Row::Header(title) => {
                        rows.push(
                            crate::kit::popover::menu_heading(theme, title).into_any_element(),
                        );
                        continue;
                    }
                    Row::Action(action) => line
                        .child(
                            crate::kit::icons::icon(action.icon())
                                .size(px(14.0))
                                .text_color(theme.text_muted),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(12.5))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(SharedString::from(action.label())),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .overflow_hidden()
                                .truncate()
                                .text_size(px(12.0))
                                .text_color(theme.text_muted.opacity(0.65))
                                .child(SharedString::from(action.description())),
                        )
                        .when(chevron_column, |line| {
                            line.child(div().size(px(12.0)).flex_none())
                        }),
                    Row::Command(ix) => {
                        let Some(command) = commands.get(*ix) else {
                            continue;
                        };
                        let mut description = command.description.clone();
                        if let Some(hint) = &command.input_hint {
                            if description.is_empty() {
                                description = format!("<{hint}>");
                            } else {
                                description = format!("{description} · <{hint}>");
                            }
                        }
                        let badge = slash_menu::command_badge(&command.name, &facts);
                        let has_choices = !slash_menu::choices(&command.name).is_empty();
                        line.child(
                            crate::kit::icons::icon(crate::prefs::slash_commands::icon(
                                &command.name,
                            ))
                            .size(px(14.0))
                            .text_color(theme.text_muted),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(12.5))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(SharedString::from(format!("/{}", command.name))),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .overflow_hidden()
                                .truncate()
                                .text_size(px(12.0))
                                // A shade under the menu's other muted text: the tone Settings →
                                // Commands gives the same descriptions.
                                .text_color(theme.text_muted.opacity(0.65))
                                .child(SharedString::from(description)),
                        )
                        .children(badge.map(|badge| slash_badge(theme, badge)))
                        // The chevron has a column of its own on every row
                        // once any command has choices, so the badges end at
                        // one edge whether a chevron follows them or not.
                        .when(chevron_column, |line| {
                            line.child(
                                div()
                                    .size(px(12.0))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(has_choices, |slot| {
                                        slot.child(
                                            crate::kit::icons::icon(
                                                crate::kit::icons::ALT_ARROW_RIGHT,
                                            )
                                            .size(px(12.0))
                                            .text_color(theme.text_muted.opacity(0.7)),
                                        )
                                    }),
                            )
                        })
                    }
                    Row::Choice(choice) => {
                        let in_effect = parent.as_deref().is_some_and(|parent| {
                            slash_menu::choice_in_effect(parent, choice, &facts)
                        });
                        line.child(
                            // The check marks the choice in effect; the slot
                            // keeps every value aligned either way.
                            div()
                                .size(px(14.0))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .when(in_effect, |slot| {
                                    slot.child(
                                        crate::kit::icons::icon(crate::kit::icons::CHECK)
                                            .size(px(12.0))
                                            .text_color(theme.success),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(12.5))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(SharedString::from(choice.value)),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .overflow_hidden()
                                .truncate()
                                .text_size(px(12.0))
                                .text_color(theme.text_muted.opacity(0.65))
                                .child(SharedString::from(choice.description)),
                        )
                    }
                };
                rows.push(
                    crate::kit::popover::menu_row(
                        theme,
                        selected,
                        format!("slash-result-{row_ix}"),
                    )
                    .id(("slash-result", row_ix))
                    .when_some(accept, |el, accept| el.on_click(accept))
                    .child(row)
                    .into_any_element(),
                );
            }
            // The scroll container owns the height cap; every row (headings
            // included) is its direct child so `scroll_to_item` maps 1:1 to
            // `menu.rows` (the pickers' model-menu pattern). Without it the
            // list hard-clips at the card's max height and neither wheel nor
            // keys can scroll.
            card = card.child(
                div()
                    .id("slash-menu-scroll")
                    .max_h(px(320.0))
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.slash_scroll)
                    .children(rows),
            );
        }
        card = card.children(status);
        let anchor = self
            .input
            .read(cx)
            .visible_point_for_index(token.range.start)?;
        Some(crate::kit::popover::anchored_menu_above_at(
            "slash-popup",
            anchor,
            card.into_any_element(),
            None,
        ))
    }
}
