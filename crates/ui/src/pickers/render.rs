//! Rendering: the chips, the popovers and the `Render` impl.

use super::*;

impl Pickers {
    /// What the composer's context gauge shows: the selected session's latest
    /// context-window reading, and whether a click compacts it (the harness
    /// has `/compact` and no turn is running). `None` — no ring at all —
    /// until the host engine has a reading (new chats, hosts on an older
    /// version), and in a Side Chat. A remote host's reading can trail a
    /// running turn by up to the session row's 20s freshness write; it
    /// catches up when the turn settles.
    pub fn context_ring_reading(
        &self,
        cx: &App,
    ) -> Option<crate::composer::context_ring::RingReading> {
        if self.side_chat {
            return None;
        }
        let state = self.state.read(cx);
        let chat_id = state.selected_chat.as_deref()?;
        let usage = state.session_for(chat_id)?.context_usage?;
        let busy = matches!(
            state.indicator_for(chat_id, chrono::Utc::now()),
            crate::state::Indicator::Working | crate::state::Indicator::AwaitingInput
        );
        let compactable = matches!(self.effective_harness(cx), Some(HarnessId::Pi));
        Some(crate::composer::context_ring::RingReading {
            usage,
            compactable,
            busy,
        })
    }

    // Chip builder: every argument is one visual slot of the chip.
    #[allow(clippy::too_many_arguments)]
    fn trigger_chip(
        &self,
        kind: PickerKind,
        label: SharedString,
        set: bool,
        chip_icon: Option<(&'static str, Option<gpui::Hsla>)>,
        suffix: Option<(SharedString, Option<gpui::Hsla>)>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let id: &'static str = match kind {
            PickerKind::Branch => "picker-branch",
            PickerKind::Checkout => "picker-checkout",
            PickerKind::HarnessModel => "picker-model",
            PickerKind::Traits => "picker-traits",
            PickerKind::Space => "picker-space",
            PickerKind::Device => "picker-device",
        };
        let open = self.open_kind() == Some(kind);
        let fade = format!("{id}-{}", cx.entity_id());
        // Ghost pill (zeron composer/styles.tsx `pill`): `h-8 rounded-lg px-2.5
        // gap-1.5 text-[12px] font-medium text-muted-foreground`, icons size-4,
        // hover/open wash — no border, no caret; the actions row stays quiet.
        div()
            .id(id)
            .h(px(32.0))
            .max_w(px(208.0))
            // Shrinkable under row pressure — four footer chips share one
            // line; without min_w_0 they overflowed and painted overlapped.
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .px(px(10.0))
            .rounded(px(8.0))
            .text_size(px(12.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            // zeron composer/styles.tsx `pill`: `transition-colors` — the wash
            // and text brighten fade over 150ms.
            .text_color(motion::hover_blend(
                &fade,
                if set {
                    theme.text.opacity(0.9)
                } else {
                    theme.text_muted
                },
                theme.text,
            ))
            .bg(if open {
                theme.element_hover
            } else {
                motion::hover_blend(&fade, gpui::transparent_black(), theme.element_hover)
            })
            .on_hover(motion::hover_listener(fade.clone()))
            .cursor_pointer()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, _, _| {
                    this.open.note_trigger_press_matching(|open| *open == kind)
                }),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.toggle(kind, window, cx)))
            .when_some(chip_icon, |el, (path, tint)| {
                el.child(
                    crate::icons::icon(path)
                        .size(px(16.0))
                        .flex_none()
                        .text_color(tint.unwrap_or(theme.text_muted)),
                )
            })
            .child(div().min_w_0().truncate().child(label))
            // The effort half of the combined model+effort chip (and the space
            // chip's "@ device" tag): muted, no icon — one button, two tones.
            // `tint` overrides the muted tone (the offline warning).
            .when_some(suffix, |el, (suffix, tint)| {
                el.child(
                    div()
                        .flex_none()
                        .text_color(tint.unwrap_or(theme.text_muted.opacity(0.7)))
                        .child(suffix),
                )
            })
    }

    /// A footer-row trigger (t3code ghost `Button size="xs"`): leading icon,
    /// truncating label, trailing chevron — smaller and quieter than the
    /// in-pill chips.
    fn footer_chip(
        &self,
        kind: PickerKind,
        id: &'static str,
        icon_path: &'static str,
        label: SharedString,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let open = self.open_kind() == Some(kind);
        let fade = format!("{id}-{}", cx.entity_id());
        div()
            .id(id)
            .h(px(20.0))
            .max_w(px(280.0))
            // Shrinkable: the canvas's selectors share a narrow tile.
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .rounded(px(6.0))
            .text_size(px(12.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(motion::hover_blend(
                &fade,
                theme.text_muted.opacity(0.7),
                theme.text.opacity(0.8),
            ))
            .bg(if open {
                theme.element_hover
            } else {
                motion::hover_blend(&fade, gpui::transparent_black(), theme.element_hover)
            })
            .on_hover(motion::hover_listener(fade.clone()))
            .cursor_pointer()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, _, _| {
                    this.open.note_trigger_press_matching(|open| *open == kind)
                }),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.toggle(kind, window, cx)))
            .child(
                crate::icons::icon(icon_path)
                    .size(px(12.0))
                    .flex_none()
                    .text_color(theme.text_muted.opacity(0.7)),
            )
            .child(div().min_w_0().truncate().child(label))
            .child(
                crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                    .size(px(12.0))
                    .flex_none()
                    .text_color(theme.text_muted.opacity(0.5)),
            )
    }

    /// A read-only footer label (locked sessions — t3code's
    /// `resolveLockedWorkspaceLabel` span).
    fn footer_label(icon_path: &'static str, label: SharedString, theme: &Theme) -> gpui::Div {
        div()
            .h(px(20.0))
            // Four of these share one row now (device, project, checkout,
            // ref): cap each early and let them SHRINK (`min_w_0`) — without
            // it the clusters overflowed into each other and the labels
            // painted overlapped (user report).
            .max_w(px(160.0))
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .text_size(px(12.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme.text_muted.opacity(0.6))
            .child(
                crate::icons::icon(icon_path)
                    .size(px(12.0))
                    .text_color(theme.text_muted.opacity(0.6)),
            )
            .child(div().min_w_0().truncate().child(label))
    }

    /// The new-session canvas's target row — device + project selector chips
    /// under the canvas logo (their popovers anchor BELOW; the composer
    /// footer carries only checkout + ref now, and sessions show their
    /// target in the titlebar instead).
    pub fn render_target_selectors(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let closing = self.open.closing_since();
        let mut overlay: Option<(PickerKind, AnyElement)> = match self.mounted_kind() {
            Some(PickerKind::Space) => {
                let content = self.render_space_popover(cx);
                Some((PickerKind::Space, self.popover_frame(280.0, content, cx)))
            }
            Some(PickerKind::Device) => {
                let content = self.render_device_popover(cx);
                Some((PickerKind::Device, self.popover_frame(224.0, content, cx)))
            }
            _ => None,
        };
        let (device_label, project_label, offline) = {
            let state = self.state.read(cx);
            let device_id = state.effective_device_id();
            let device_label: SharedString = device_id
                .as_deref()
                .and_then(|id| state.device_name(id))
                .map(str::to_string)
                .unwrap_or_else(|| "This device".to_string())
                .into();
            let offline = device_id
                .as_deref()
                .is_some_and(|id| !state.device_online(id, chrono::Utc::now()));
            let quick_chat = state.selected_chat_row().map_or(
                state.selected_chat.is_none() && state.scratch_pending,
                |chat| chat.is_scratch(),
            );
            let project_label: SharedString = state
                .selected_space_row()
                .map(|s| s.display_name().to_string())
                .unwrap_or_else(|| {
                    if quick_chat {
                        "Quick chat".to_string()
                    } else {
                        "No project".to_string()
                    }
                })
                .into();
            (device_label, project_label, offline)
        };
        // A project window's canvas always targets its project (and that
        // project's host): the target reads, it doesn't pick.
        if self.state.read(cx).window_project().is_some() {
            return div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .child(
                    Self::footer_label(crate::icons::MONITOR, device_label, &theme)
                        .when(offline, |el| el.text_color(theme.warning.opacity(0.8))),
                )
                .child(Self::footer_label(
                    crate::icons::FOLDER,
                    project_label,
                    &theme,
                ))
                .into_any_element();
        }
        let device_chip = self
            .footer_chip(
                PickerKind::Device,
                "picker-device",
                crate::icons::MONITOR,
                device_label,
                &theme,
                cx,
            )
            .when(offline, |el| el.text_color(theme.warning.opacity(0.8)));
        let project_chip = self.footer_chip(
            PickerKind::Space,
            "picker-project",
            crate::icons::FOLDER,
            project_label,
            &theme,
            cx,
        );
        div()
            .max_w_full()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .child(attach_overlay_below(
                device_chip,
                &mut overlay,
                PickerKind::Device,
                "device-popover",
                closing,
            ))
            .child(attach_overlay_below(
                project_chip,
                &mut overlay,
                PickerKind::Space,
                "project-popover",
                closing,
            ))
            .into_any_element()
    }

    /// The composer footer row: checkout-kind + ref, LEFT-aligned, only when
    /// the picked (or session's) project has git. Device + project moved to
    /// the new-session canvas ([`Self::render_target_selectors`]); sessions
    /// name their target in the titlebar.
    pub fn render_footer(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = Theme::of(cx).clone();
        // A selected chat whose workspace row hasn't synced yet (the moment
        // right after send mints it) still renders the DRAFT footer — the
        // values are identical, so the toolbar never blinks through a
        // half-empty locked state.
        let (space, session) = {
            let state = self.state.read(cx);
            let space = state.selected_space_row().cloned();
            let session = state
                .selected_chat
                .as_ref()
                .and_then(|_| state.selected_chat_row().cloned());
            (space, session)
        };
        let row = || {
            // Symmetric: the container's 8px gap sits above the toolbar;
            // bleeding 8 of the container's 16px bottom padding (mb -8)
            // leaves 8 below — equal air on both sides of the row.
            // `w_full` is load-bearing: without it the canvas layout sizes
            // the row to CONTENT, and the left cluster's flex_1 (basis 0)
            // collapsed to zero width — both clusters painted from the same
            // origin, chips overlapping (user report).
            div()
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .px(px(10.0))
                .mb(px(-8.0))
        };

        if let Some(chat) = &session {
            // Sessions never move: read-only checkout-kind + ref labels,
            // LEFT-aligned, only when the session's project has git. The
            // target (project @ device) lives in the titlebar now.
            let space = space.as_ref().filter(|s| s.git_detected)?;
            let is_worktree = chat.cwd.as_deref().is_some_and(|cwd| cwd != space.path);
            let (icon_path, label) = if is_worktree {
                (crate::icons::FOLDER_WITH_FILES, "Worktree")
            } else {
                (crate::icons::FOLDER, "Local checkout")
            };
            // Mirrors the draft chips: checkout hugs the left edge, ref the
            // right.
            let left = div()
                .flex()
                .flex_row()
                .items_center()
                .min_w_0()
                .child(Self::footer_label(
                    icon_path,
                    SharedString::from(label),
                    &theme,
                ));
            let right = div()
                .flex()
                .flex_row()
                .items_center()
                .min_w_0()
                .child(Self::footer_label(
                    crate::icons::GIT_BRANCH,
                    chat.branch
                        .clone()
                        .map(SharedString::from)
                        .unwrap_or_else(|| SharedString::from("No ref")),
                    &theme,
                ));
            return Some(row().child(left).child(right).into_any_element());
        }

        // New-session canvas: checkout + ref only, LEFT-aligned (device +
        // project live under the canvas logo now).
        let git = space.as_ref().is_some_and(|s| s.git_detected);
        if !git {
            return None;
        }
        // Refs feed the draft labels — eager + idempotent.
        self.ensure_refs(false, cx);
        let closing = self.open.closing_since();
        let mut overlay: Option<(PickerKind, AnyElement)> = match self.mounted_kind() {
            Some(PickerKind::Branch) => {
                let content = self.render_branch_popover(cx);
                Some((PickerKind::Branch, self.popover_frame(320.0, content, cx)))
            }
            Some(PickerKind::Checkout) => {
                let content = self.render_checkout_popover(cx);
                Some((PickerKind::Checkout, self.popover_frame(224.0, content, cx)))
            }
            // Space/Device popovers mount on the canvas selectors
            // (`render_target_selectors`), not here.
            _ => None,
        };

        let ref_label = self.ref_label(cx);
        let ref_chip = self.footer_chip(
            PickerKind::Branch,
            "picker-branch",
            crate::icons::GIT_BRANCH,
            ref_label,
            &theme,
            cx,
        );
        // The pinned plan is authoritative for the kind icon too: a pinned
        // "Current checkout" reads as a bare folder even if a stale draft
        // `config.checkout` still says NewWorktree (same-project hover-add).
        let kind_icon = match self.pinned_plan(cx) {
            Some(CheckoutPlan::CurrentCheckout { .. }) => Self::checkout_kind_icon(true),
            Some(_) => Self::checkout_kind_icon(false),
            None => Self::checkout_kind_icon(
                self.config.checkout == CheckoutKind::Local
                    && self.selected_ref_worktree(cx).is_none(),
            ),
        };
        let kind_chip = self.footer_chip(
            PickerKind::Checkout,
            "picker-checkout",
            kind_icon,
            SharedString::from(self.checkout_label(cx)),
            &theme,
            cx,
        );
        // Checkout on the left edge, ref on the right — the row's
        // justify_between splits them (user request).
        let left = div()
            .flex()
            .flex_row()
            .items_center()
            .min_w_0()
            .child(attach_overlay(
                kind_chip,
                &mut overlay,
                PickerKind::Checkout,
                "checkout-popover",
                closing,
            ));
        let right = div()
            .flex()
            .flex_row()
            .items_center()
            .min_w_0()
            .child(attach_overlay_end(
                ref_chip,
                &mut overlay,
                PickerKind::Branch,
                "branch-popover",
                closing,
            ));
        Some(row().child(left).child(right).into_any_element())
    }

    fn popover_frame(&self, width: f32, content: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        popover::popover_card(&theme)
            .w(px(width))
            // zeron caps its tallest picker at min(640px, 75vh).
            .max_h(px(640.0))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_key_down(event, window, cx)
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close(cx)))
            .flex()
            .flex_col()
            .child(content)
            .into_any_element()
    }

    /// [`Self::popover_frame`] without the p-1 inset — the harness/model
    /// picker's rail + list panes bleed to the card edge (zeron
    /// harness-model-picker.tsx `className="w-80 p-0"`).
    fn popover_frame_flush(
        &self,
        width: f32,
        content: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        popover::popover_card_flush(&theme)
            .w(px(width))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_key_down(event, window, cx)
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close(cx)))
            .flex()
            .flex_col()
            .child(content)
            .into_any_element()
    }

    pub(super) fn search_box(&self, theme: &Theme) -> AnyElement {
        popover::search_input_frame(theme, self.search.clone().into_any_element())
            .into_any_element()
    }

    fn retry_row(
        &self,
        id: &'static str,
        message: &str,
        kind: PickerKind,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        popover::error_row(theme, message)
            .child(
                div()
                    .id(id)
                    .px(px(Theme::SPACE_SM))
                    .py(px(3.0))
                    .rounded(px(Theme::CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.border)
                    .text_color(theme.text)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.element_hover))
                    .on_click(cx.listener(move |this, _, _, cx| match kind {
                        PickerKind::Branch | PickerKind::Checkout => this.ensure_refs(true, cx),
                        PickerKind::HarnessModel | PickerKind::Traits => {
                            this.harnesses = Loadable::Idle;
                            this.model_generation = this.model_generation.wrapping_add(1);
                            this.models.clear();
                            this.ensure_harnesses(false, cx);
                        }
                        // Projects/devices load nothing; no retry surface exists.
                        PickerKind::Space | PickerKind::Device => {}
                    }))
                    .child(SharedString::from("Retry")),
            )
            .into_any_element()
    }

    pub(super) fn runtime_missing_row(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let target_device = self
            .space_target(cx)
            .or_else(|| state.local_device_id.clone());
        let Some(target_device) = target_device else {
            return popover::error_row(
                theme,
                "The engine is not connected. Reconnect before installing Pi Runtime.",
            )
            .into_any_element();
        };
        let remote = Some(&target_device) != state.local_device_id.as_ref();
        let label = state.device_name(&target_device).unwrap_or(&target_device);
        popover::error_row(theme, &runtime_install_guidance(label, remote))
            .child(
                popover::btn_primary(theme, "Open Agents settings")
                    .id("model-open-agents")
                    .debug_selector(|| "model-open-agents".into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close(cx);
                        cx.emit(PickerEvent::OpenAgentSettings {
                            target_device: target_device.clone(),
                        });
                    })),
            )
            .into_any_element()
    }

    /// The ref picker (t3code BranchToolbarBranchSelector): search on top,
    /// rows with right-aligned muted `current`/`worktree` tags, and a
    /// "Showing X of Y refs" footer when the list is capped.
    fn render_branch_popover(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        if self.state.read(cx).selected_space_row().is_none() {
            return div()
                .p(px(Theme::SPACE_SM))
                .text_size(px(12.0))
                .text_color(theme.text_faint)
                .child(SharedString::from("No project selected"))
                .into_any_element();
        }
        let rows = self.filtered_ref_rows(cx);
        let total = rows.len();
        let shown = total.min(MAX_REF_ROWS);
        // Existing session: the highlighted row is the SESSION's branch and a
        // pick switches the checkout (see `pick_ref`); a new chat highlights
        // the draft pick.
        let session_branch = self
            .state
            .read(cx)
            .selected_chat_row()
            .and_then(|c| c.branch.clone());
        let switching = self.switching.clone();
        let body: AnyElement =
            match &self.refs {
                Loadable::Loading | Loadable::Idle => {
                    popover::skeleton_rows("branch-skeleton", &theme, 4, cx.entity_id(), cx)
                }
                Loadable::Error(message) => {
                    let message = message.clone();
                    self.retry_row("branch-retry", &message, PickerKind::Branch, &theme, cx)
                }
                Loadable::Ready(_) if rows.is_empty() => div()
                    .p(px(Theme::SPACE_SM))
                    .text_size(px(12.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from("No refs found."))
                    .into_any_element(),
                Loadable::Ready(_) => {
                    let active = self.active;
                    let selected = session_branch.or_else(|| self.config.branch.clone());
                    div()
                        .id("branch-list")
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .max_h(px(224.0))
                        .overflow_y_scroll()
                        .children(rows.into_iter().take(MAX_REF_ROWS).enumerate().map(
                            |(ix, row)| {
                                let label: SharedString = row.name.clone().into();
                                let is_selected = selected.as_deref() == Some(row.name.as_str());
                                // Right-aligned muted tag (t3code `text-[10px]
                                // text-muted-foreground/45`): current beats worktree.
                                let tag: Option<&'static str> = if row.current {
                                    Some("current")
                                } else if row.worktree_path.is_some() {
                                    Some("worktree")
                                } else {
                                    None
                                };
                                let is_switching = switching.as_deref() == Some(row.name.as_str());
                                popover::menu_row_nav(
                                    &theme,
                                    is_selected,
                                    ix == active,
                                    format!("branch-row-{ix}"),
                                )
                                .id(("branch-row", ix))
                                .when(switching.is_some(), |el| el.opacity(0.55))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.pick_ref(row.clone(), cx);
                                }))
                                .child(div().flex_1().min_w_0().truncate().child(label))
                                .when(is_switching, |el| {
                                    el.child(
                                        div()
                                            .flex_none()
                                            .text_size(px(10.0))
                                            .text_color(theme.text_muted.opacity(0.6))
                                            .child(SharedString::from("switching…")),
                                    )
                                })
                                .when_some(tag, |el, tag| {
                                    el.child(
                                        div()
                                            .flex_none()
                                            .text_size(px(10.0))
                                            .text_color(theme.text_muted.opacity(0.45))
                                            .child(SharedString::from(tag)),
                                    )
                                })
                            },
                        ))
                        .into_any_element()
                }
            };
        let mut popover = div()
            .flex()
            .flex_col()
            .child(self.search_box(&theme))
            .child(body);
        // Mid-session switch failure (dirty tree, ref checked out elsewhere):
        // git's own message, under a hairline.
        if let Some(error) = &self.switch_error {
            popover = popover.child(
                popover::menu_section().child(
                    div()
                        .px(px(Theme::SPACE_SM))
                        .py(px(4.0))
                        .text_size(px(11.0))
                        .text_color(theme.danger.opacity(0.9))
                        .child(SharedString::from(error.clone())),
                ),
            );
        }
        if total > shown {
            popover = popover.child(
                popover::menu_section().child(
                    div()
                        .px(px(Theme::SPACE_SM))
                        .py(px(4.0))
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(format!(
                            "Showing {shown} of {total} refs"
                        ))),
                ),
            );
        }
        popover.into_any_element()
    }

    /// The checkout-kind dropdown (t3code BranchToolbarEnvModeSelector): two
    /// rows — "Current checkout"/"Current worktree" (local) and "New worktree".
    fn render_checkout_popover(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let has_worktree = self.selected_ref_worktree(cx).is_some();
        let local_label: &'static str = if has_worktree {
            "Current worktree"
        } else {
            "Current checkout"
        };
        let local_icon = if has_worktree {
            crate::icons::FOLDER_WITH_FILES
        } else {
            crate::icons::FOLDER
        };
        let options: [(CheckoutKind, &'static str, &'static str); 2] = [
            (CheckoutKind::Local, local_label, local_icon),
            (
                CheckoutKind::NewWorktree,
                "New worktree",
                crate::icons::FOLDER_WITH_FILES,
            ),
        ];
        let active = self.active;
        let current = self.config.checkout;
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .children(
                options
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (kind, label, icon_path))| {
                        let is_selected = current == kind;
                        popover::menu_row_nav(
                            &theme,
                            is_selected,
                            ix == active,
                            format!("checkout-row-{ix}"),
                        )
                        .id(("checkout-row", ix))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.pick_checkout(kind, cx);
                        }))
                        .child(
                            crate::icons::icon(icon_path)
                                .size(px(14.0))
                                .text_color(theme.text_muted),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(label)),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The model picker: an icons-only provider rail on the left (favorites
    /// star on top), a search box over that provider's models on the right.
    /// Rows are two lines — model name over the provider icon + name. Searching
    /// hides the rail and spans every provider.
    fn render_harness_model_popover(&mut self, cx: &mut Context<Self>) -> AnyElement {
        const HEIGHT: f32 = 346.0; // t3 max-h-86.5

        let theme = Theme::of(cx).clone();

        // Catalog-level loading/error take over the whole card.
        match &self.harnesses {
            Loadable::Loading | Loadable::Idle => {
                return div()
                    .h(px(HEIGHT))
                    .p(px(8.0))
                    .child(popover::skeleton_rows(
                        "harness-skeleton",
                        &theme,
                        4,
                        cx.entity_id(),
                        cx,
                    ))
                    .into_any_element();
            }
            Loadable::Error(message) => {
                let message = message.clone();
                return div()
                    .h(px(HEIGHT))
                    .p(px(8.0))
                    .child(self.retry_row(
                        "harness-retry",
                        &message,
                        PickerKind::HarnessModel,
                        &theme,
                        cx,
                    ))
                    .into_any_element();
            }
            Loadable::Ready(_) => {}
        }

        let locked = self.harness_locked(cx);
        let effective = self.effective_harness(cx);
        let model_scroll = self.model_scroll.clone();
        let query = self.search.read(cx).text().trim().to_string();
        let searching = !query.is_empty();
        let favorites_view = !searching && self.model_rail == ModelRail::Favorites;
        let provider_tabs = self.provider_tabs(cx);
        let viewed_provider = self.viewed_provider(cx);
        let rows = self.visible_model_rows(cx);
        let active = self.active;
        let selected_id = self.selected_model(cx).map(|m| m.id.clone());

        // ── rail: icons only — the favorites star, a divider, one brand
        //    icon per provider. Hidden while a search is live.
        let rail: Option<AnyElement> = (!searching).then(|| {
            let mut column = div()
                .w(px(44.0))
                .flex_none()
                .p(px(4.0))
                .flex()
                .flex_col()
                .gap(px(4.0));
            column = column.child(
                div()
                    .id("model-rail-favorites")
                    .relative()
                    .w(px(36.0))
                    .h(px(36.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(!favorites_view, |el| {
                        el.hover(|s| s.bg(crate::theme::ink(0.06)))
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model_rail = ModelRail::Favorites;
                        // Anchor on the selected row when it's starred, else
                        // the top — never a stray second highlight.
                        this.active = this.selected_model_index(cx);
                        this.model_scroll.set_offset(gpui::Point::default());
                        this.model_scroll.scroll_to_item(this.active);
                        cx.notify();
                    }))
                    .child(
                        crate::icons::icon(crate::icons::STAR_BOLD)
                            .size(px(17.0))
                            .text_color(if favorites_view {
                                theme.text
                            } else {
                                theme.text_muted.opacity(0.75)
                            }),
                    )
                    .when(favorites_view, |el| {
                        el.child(rail_indicator(picker_purple(&theme)))
                    }),
            );
            // Full-bleed divider, aligned with the search row's bottom
            // hairline (see the height math there) — one line across.
            column = column.child(
                div()
                    .h(px(1.0))
                    .mx(px(-4.0))
                    .my(px(1.0))
                    .bg(crate::theme::hairline(0.08)),
            );
            for (ix, provider) in provider_tabs.iter().enumerate() {
                let provider = provider.clone();
                let is_viewed =
                    !favorites_view && viewed_provider.as_deref() == Some(provider.as_str());
                let is_disabled = locked
                    && ((provider == "mock" && effective != Some(HarnessId::Mock))
                        || (provider != "mock" && effective == Some(HarnessId::Mock)));
                let (icon_path, tint) = provider_brand_icon(&provider);
                column = column.child(
                    div()
                        .id(("provider-tab", ix))
                        .aria_label(provider_display_name(&provider).to_string())
                        .relative()
                        .w(px(36.0))
                        .h(px(36.0))
                        .rounded(px(8.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(is_disabled, |el| el.opacity(0.35))
                        .when(!is_disabled, |el| el.cursor_pointer())
                        .when(!is_disabled && !is_viewed, |el| {
                            el.hover(|s| s.bg(crate::theme::ink(0.06)))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.pick_provider(provider.clone(), cx);
                        }))
                        .child(crate::icons::icon(icon_path).size(px(18.0)).text_color(
                            tint.unwrap_or(if is_viewed {
                                theme.text
                            } else {
                                theme.text_muted
                            }),
                        ))
                        .when(is_viewed, |el| {
                            el.child(rail_indicator(picker_purple(&theme)))
                        }),
                );
            }
            column.into_any_element()
        });

        // ── search row: icon + borderless input over a FULL-BLEED hairline
        //    (it meets the rail's divider at the same y, one line across the
        //    card — user request; no accent tint). Height matches the rail's
        //    star tab band exactly: 4px pad + 36px tab + 4px gap + 1px
        //    divider margin = the hairline at y 45–46, same as this row's
        //    inside-drawn bottom border at h 46.
        let search_row = div()
            .flex_none()
            .h(px(46.0))
            .px(px(10.0))
            .border_b_1()
            .border_color(crate::theme::hairline(0.08))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(
                crate::icons::icon(crate::icons::MAGNIFER)
                    .size(px(14.0))
                    .flex_none()
                    .text_color(theme.text_muted.opacity(0.7)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.0))
                    .child(self.search.clone()),
            );

        // ── model rows, flat — the scroll container's direct children so
        //    keyboard `scroll_to_item(active)` maps 1:1.
        let effective_models = effective.and_then(|h| self.models.get(&h));
        let list_children: Vec<AnyElement> = if !rows.is_empty() {
            rows.iter()
                .enumerate()
                .map(|(ix, row)| {
                    let is_selected = Some(row.harness) == effective
                        && selected_id.as_deref() == Some(row.model.id.as_str());
                    let is_active = ix == active;
                    let is_fav = self.defaults.is_favorite(row.harness, &row.model.id);
                    let (icon_path, tint) = provider_brand_icon(&row.provider_id);
                    let label: SharedString = row.model.label.clone().into();
                    let subline: SharedString = match &row.model.description {
                        Some(description) if !favorites_view && !searching => {
                            description.clone().into()
                        }
                        Some(description) => {
                            format!("{} · {description}", row.provider_title).into()
                        }
                        None => row.provider_title.clone(),
                    };
                    let harness = row.harness;
                    let star_model = row.model.id.clone();
                    let mut el = div()
                        .id(("model-row", ix))
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(8.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.0))
                        .cursor_pointer();
                    // ONE moving highlight (t3/Base-UI combobox): hovering
                    // moves the keyboard cursor instead of painting its own
                    // wash, so hover + arrow cursor can never wear two
                    // washes at once. Selection is the distinct stronger
                    // treatment (wash + ring).
                    if is_selected {
                        el = el
                            .bg(crate::theme::card_selected_bg())
                            .shadow(crate::theme::card_selected_shadows());
                    } else if is_active {
                        el = el.bg(crate::theme::ink(0.05));
                    }
                    el = el.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered && this.active != ix {
                            this.active = ix;
                            cx.notify();
                        }
                    }));
                    el = el
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.activate_model_index(ix, cx);
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .w_full()
                                        .truncate()
                                        .text_size(px(12.5))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme.text)
                                        .child(label),
                                )
                                .child(
                                    // Harness identity subline (t3
                                    // `showProvider`) — replaces the model
                                    // description.
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(6.0))
                                        .child(
                                            crate::icons::icon(icon_path)
                                                .size(px(11.0))
                                                .flex_none()
                                                .text_color(
                                                    tint.unwrap_or(theme.text_muted.opacity(0.7)),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(px(11.0))
                                                .text_color(theme.text_muted.opacity(0.7))
                                                .child(subline),
                                        ),
                                ),
                        );
                    if ix < 9 {
                        el = el.child(popover::kbd_hint(&theme, &format!("⌘{}", ix + 1)));
                    }
                    el = el.child(
                        div()
                            .id(("model-star", ix))
                            .flex_none()
                            .w(px(22.0))
                            .h(px(22.0))
                            .rounded(px(6.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| s.bg(crate::theme::ink(0.08)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_model_favorite(harness, &star_model, cx);
                            }))
                            .child(
                                crate::icons::icon(if is_fav {
                                    crate::icons::STAR_BOLD
                                } else {
                                    crate::icons::STAR
                                })
                                .size(px(13.0))
                                .text_color(if is_fav {
                                    theme.warning
                                } else {
                                    theme.text_muted.opacity(0.45)
                                }),
                            ),
                    );
                    el.into_any_element()
                })
                .collect()
        } else if searching {
            vec![empty_list_note(&theme, "No models found")]
        } else if favorites_view {
            vec![empty_list_note(
                &theme,
                "No starred models yet — hit a row's star",
            )]
        } else {
            match effective_models {
                Some(Loadable::Error(message)) => {
                    let message = message.clone();
                    if missing_pi_runtime(effective, &message) {
                        vec![self.runtime_missing_row(&theme, cx)]
                    } else {
                        vec![self.retry_row(
                            "model-retry",
                            &message,
                            PickerKind::HarnessModel,
                            &theme,
                            cx,
                        )]
                    }
                }
                Some(Loadable::Ready(models)) if models.is_empty() => {
                    vec![empty_list_note(
                        &theme,
                        "No models available — add a service in Settings → Providers",
                    )]
                }
                _ => vec![popover::skeleton_rows(
                    "model-skeleton",
                    &theme,
                    4,
                    cx.entity_id(),
                    cx,
                )],
            }
        };

        let pane = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            // A whisper of wash lifts the pane off the rail (t3
            // `bg-muted/40` + `border-l border-border/70`).
            .bg(crate::theme::ink(0.02))
            .when(rail.is_some(), |el| {
                el.border_l_1().border_color(crate::theme::hairline(0.07))
            })
            .child(search_row)
            .child(
                div().flex_1().min_h_0().py(px(6.0)).child(
                    div()
                        .id("model-menu-scroll")
                        .size_full()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .px(px(6.0))
                        .overflow_y_scroll()
                        .track_scroll(&model_scroll)
                        .children(list_children),
                ),
            );

        div()
            .h(px(HEIGHT))
            .flex()
            .flex_row()
            .items_stretch()
            .children(rail)
            .child(pane)
            .into_any_element()
    }

    /// The traits dropdown body (t3code TraitsPicker): the reasoning ladder
    /// plus every advertised model option as headed sections of menu ROWS —
    /// label, a "Default" badge on the section's default choice, and the
    /// trailing check on the selected row. Sections split by hairline
    /// separators. Selecting keeps the menu open for multi-adjust.
    fn render_traits_sections(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let Some(model) = self.selected_model(cx).cloned() else {
            return popover::skeleton_rows("traits-skeleton", &theme, 3, cx.entity_id(), cx);
        };
        let levels = self.trait_ladder(cx);
        // Display the effective level (draft pick or the chat's config), so
        // the ladder check mirrors the chip summary.
        let current = self.effective_reasoning(cx);

        let mut sections: Vec<AnyElement> = Vec::new();
        if !levels.is_empty() {
            let default_level = default_reasoning(&levels);
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    // 2px row gap — the menu-column rhythm everywhere else
                    // (model list, device switcher); without it adjacent
                    // hover/selected washes fuse into one blob (user report).
                    .gap(px(2.0))
                    .child(popover::menu_heading(&theme, "Reasoning"))
                    .children(levels.into_iter().enumerate().map(|(ix, level)| {
                        let is_active = current == Some(level);
                        let is_default = default_level == Some(level);
                        let mut row =
                            popover::menu_row(&theme, is_active, format!("trait-reasoning-{ix}"))
                                .id(("reasoning-row", ix))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.pick_reasoning(level, cx);
                                }))
                                .child(SharedString::from(reasoning_label(level)));
                        row = row.child(div().flex_1());
                        if is_default {
                            row = row.child(default_badge(&theme));
                        }
                        row
                    }))
                    .into_any_element(),
            );
        }

        let selections = self.explicit_options(cx);
        for (opt_ix, option) in model.options.iter().enumerate() {
            if !sections.is_empty() {
                sections.push(popover::menu_separator().into_any_element());
            }
            let selected_choice = selections
                .get(&option.id)
                .and_then(|v| v.as_str())
                .unwrap_or(&option.default_choice)
                .to_string();
            let option_id = option.id.clone();
            let default_choice = option.default_choice.clone();
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0)) // same rhythm as the Reasoning section above
                    .child(popover::menu_heading(&theme, &option.label))
                    .children(
                        option
                            .choices
                            .iter()
                            .enumerate()
                            .map(|(choice_ix, choice)| {
                                let is_active = selected_choice == choice.id;
                                let choice_id = choice.id.clone();
                                let option_id = option_id.clone();
                                let is_default = choice.id == default_choice;
                                let mut row = popover::menu_row(
                                    &theme,
                                    is_active,
                                    format!("trait-choice-{opt_ix}-{choice_ix}"),
                                )
                                .id(("trait-choice", opt_ix * 32 + choice_ix))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.pick_option(
                                        option_id.clone(),
                                        choice_id.clone(),
                                        is_default,
                                        cx,
                                    );
                                }))
                                .child(SharedString::from(choice.label.clone()));
                                row = row.child(div().flex_1());
                                if is_default {
                                    row = row.child(default_badge(&theme));
                                }
                                row
                            }),
                    )
                    .into_any_element(),
            );
        }

        div()
            .flex()
            .flex_col()
            .pb(px(2.0))
            .children(sections)
            .into_any_element()
    }
}

/// The "Default" marker beside a section's default choice: a ghost badge —
/// bare muted text, no border or fill (user request; t3code draws an outline
/// pill here).
fn default_badge(theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .text_size(px(10.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text_muted.opacity(0.6))
        .child(SharedString::from("Default"))
}

/// Brand mark + optional tint for a harness (the Claude mark keeps its brand
/// orange even on the monochrome surface; the mock harness scripts
/// Claude-flavoured runs, so it wears the Claude mark).
/// The 3px bar marking the selected rail tab (t3 ModelPickerSidebar
/// `SELECTED_INDICATOR_CLASS`, `rounded-l-full`): LEFT half-capsule only —
/// the flat right edge presses against the rail/pane border it hugs.
fn rail_indicator(tint: gpui::Hsla) -> gpui::Div {
    div()
        .absolute()
        .right(px(-4.0))
        .top(px(8.0))
        .w(px(3.0))
        .h(px(20.0))
        .rounded_tl(px(3.0))
        .rounded_bl(px(3.0))
        .bg(tint)
}

/// The picker's selection purple — the app's violet identity, intentionally
/// independent of the emerald inline-code palette. It is NOT the indigo
/// `accent`: that bar read blue against the glass. Violet-400 on dark,
/// violet-600 on light (AA against white).
fn picker_purple(theme: &Theme) -> gpui::Hsla {
    match theme.appearance {
        crate::theme::Appearance::Dark => crate::theme::oklch(0.702, 0.183, 293.541),
        crate::theme::Appearance::Light => crate::theme::oklch(0.541, 0.281, 293.009),
    }
}

/// Centered muted note filling an empty model list ("No models found").
fn empty_list_note(theme: &Theme, copy: &str) -> AnyElement {
    div()
        .px(px(8.0))
        .py(px(24.0))
        .text_size(px(12.0))
        .text_color(theme.text_muted.opacity(0.6))
        .text_center()
        .child(SharedString::from(copy.to_string()))
        .into_any_element()
}

/// Attach the (single) open popover overlay to its trigger chip.
fn attach_overlay(
    chip: gpui::Stateful<gpui::Div>,
    overlay: &mut Option<(PickerKind, AnyElement)>,
    kind: PickerKind,
    id: &'static str,
    closing: Option<std::time::Instant>,
) -> gpui::Stateful<gpui::Div> {
    if overlay.as_ref().is_some_and(|(k, _)| *k == kind)
        && let Some((_, element)) = overlay.take()
    {
        return chip.child(popover::anchored_menu_above(id, element, closing));
    }
    chip
}

/// [`attach_overlay`] opening DOWNWARD — the canvas target selectors sit
/// mid-screen, so their menus drop below the chips.
fn attach_overlay_below(
    chip: gpui::Stateful<gpui::Div>,
    overlay: &mut Option<(PickerKind, AnyElement)>,
    kind: PickerKind,
    id: &'static str,
    closing: Option<std::time::Instant>,
) -> gpui::Stateful<gpui::Div> {
    if overlay.as_ref().is_some_and(|(k, _)| *k == kind)
        && let Some((_, element)) = overlay.take()
    {
        return chip.child(popover::anchored_menu_below(id, element, closing));
    }
    chip
}

/// [`attach_overlay`] with the menu RIGHT-ALIGNED to the trigger (t3code
/// `align="end"` — right-edge triggers like the ref picker open leftward).
fn attach_overlay_end(
    chip: gpui::Stateful<gpui::Div>,
    overlay: &mut Option<(PickerKind, AnyElement)>,
    kind: PickerKind,
    id: &'static str,
    closing: Option<std::time::Instant>,
) -> gpui::Stateful<gpui::Div> {
    if overlay.as_ref().is_some_and(|(k, _)| *k == kind)
        && let Some((_, element)) = overlay.take()
    {
        return chip
            .relative()
            .child(popover::anchored_menu_above_end(id, element, closing));
    }
    chip
}

impl Render for Pickers {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        // Eager-load the harness catalog + every offered harness's models so
        // the chip reads "Fable 5" (a concrete pick) before any popover
        // opens, and rail switches inside the picker are instant.
        self.ensure_harnesses(false, cx);
        self.prefetch_models(cx);
        // A popover opened without going through `toggle` still gets its
        // loads kicked here (all ensure_* are idempotent).
        if matches!(
            self.open_kind(),
            Some(PickerKind::Branch) | Some(PickerKind::Checkout)
        ) && matches!(self.refs, Loadable::Idle)
        {
            self.ensure_refs(false, cx);
        }
        // Chip shows the model's display name alone (zeron `modelText`); the
        // harness reads from the brand mark beside it. Never "Default model":
        // before the catalog lands the remembered label (or the configured id)
        // names the pick; the loaded list then resolves it to a concrete row.
        let model_label: SharedString = {
            let loaded = self.selected_model(cx).map(|m| m.label.clone());
            let label = loaded.or_else(|| {
                let remembered = self
                    .effective_harness(cx)
                    .and_then(|h| self.defaults.model_for(h));
                match self.effective_model_id(cx) {
                    Some(id) => Some(
                        remembered
                            .filter(|m| m.id == id)
                            .map(|m| m.label.clone())
                            .or_else(|| self.defaults.label_for(id).map(str::to_string))
                            .unwrap_or_else(|| id.to_string()),
                    ),
                    None => remembered.map(|m| m.label.clone()),
                }
            });
            label.map(SharedString::from).unwrap_or_default()
        };
        let harness_icon: (&'static str, Option<gpui::Hsla>) = {
            let from_model = self.selected_model(cx).and_then(|model| {
                let harness = self.effective_harness(cx)?;
                Some(provider_brand_icon(&model_provider_id(harness, &model.id)))
            });
            from_model
                .or_else(|| self.viewed_provider(cx).map(|id| provider_brand_icon(&id)))
                .unwrap_or((
                    crate::icons::CLAUDE_MARK,
                    Some(crate::icons::claude_brand()),
                ))
        };
        let explicit_options = self.explicit_options(cx);
        let traits_set = traits_summary(
            self.selected_model(cx),
            self.effective_reasoning(cx),
            &explicit_options,
        );
        let traits_active = traits_customized(
            self.selected_model(cx),
            self.effective_reasoning(cx),
            &self.trait_ladder(cx),
            &explicit_options,
        );
        let traits_label: SharedString = traits_set
            .clone()
            .map(SharedString::from)
            .unwrap_or_else(|| SharedString::from("Traits"));

        // Render the open popover's body first (mutable borrow), then the
        // chips. Branch/Checkout render in the composer FOOTER row (see
        // `render_footer`), not here.
        let closing = self.open.closing_since();
        let mut overlay: Option<(PickerKind, AnyElement)> = match self.mounted_kind() {
            // Footer-row pickers — their popovers mount down there.
            Some(PickerKind::Branch)
            | Some(PickerKind::Checkout)
            | Some(PickerKind::Space)
            | Some(PickerKind::Device) => None,
            Some(PickerKind::HarnessModel) => {
                let content = self.render_harness_model_popover(cx);
                Some((
                    PickerKind::HarnessModel,
                    // t3 ModelPickerContent `max-w-90` — 360px.
                    self.popover_frame_flush(360.0, content, cx),
                ))
            }
            Some(PickerKind::Traits) => {
                let content = div()
                    .p(px(4.0))
                    .child(self.render_traits_sections(cx))
                    .into_any_element();
                Some((
                    PickerKind::Traits,
                    self.popover_frame_flush(240.0, content, cx),
                ))
            }
            None => None,
        };

        // Left cluster: empty — the device/project pickers live in the
        // composer FOOTER row alongside checkout + ref.
        // Right cluster: agent+model and traits — the composer appends
        // attach + send after this element (zeron composer-actions.tsx
        // arrangement).
        let left = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .gap(px(4.0));
        // Model chip (brand icon + model name) beside a separate Traits chip
        // (t3code TraitsPicker arrangement): the trigger label is the joined
        // effective summary ("High · 1M · Fast", "Agent · Balance") so the
        // run's traits read without opening; it brightens only when something
        // departs from its default. No chip at all when the model has neither
        // a ladder nor options — a dead trigger reads as broken.
        let model_chip = self.trigger_chip(
            PickerKind::HarnessModel,
            model_label,
            true,
            Some(harness_icon),
            None,
            &theme,
            cx,
        );
        let has_traits = !self.narrow && !self.trait_ladder(cx).is_empty()
            || self
                .selected_model(cx)
                .is_some_and(|m| !m.options.is_empty());
        let traits_chip = has_traits.then(|| {
            self.trigger_chip(
                PickerKind::Traits,
                traits_label,
                traits_active,
                None,
                None,
                &theme,
                cx,
            )
        });
        // Shrinkable, end-aligned: in a narrow tile the chips ellipsize
        // (they are `min_w_0`) instead of painting over the send button.
        let right = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .flex_1()
            .min_w_0()
            .gap(px(4.0))
            // End-anchored: the menu's right edge sits flush with the chip's
            // right edge (user request), same as the footer's ref popover.
            .child(attach_overlay_end(
                model_chip,
                &mut overlay,
                PickerKind::HarnessModel,
                "model-popover",
                closing,
            ))
            .children(traits_chip.map(|chip| {
                attach_overlay_end(
                    chip,
                    &mut overlay,
                    PickerKind::Traits,
                    "traits-popover",
                    closing,
                )
            }));
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(Theme::SPACE_SM))
            .child(left)
            .child(right)
    }
}
