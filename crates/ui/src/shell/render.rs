//! Render pieces: titlebar, sidebar, settings navigation, and the shell's
//! `Render` impl.

use super::*;

impl Shell {
    /// Evaluate a width tween at "now" (manual drive — see [`WidthTween`]).
    /// Mid-flight: eased 200ms lerp, and `motion_active` is flagged so render
    /// schedules the next animation frame. Finished, stale, absent, or under
    /// reduced motion: exactly `target`.
    pub(super) fn eval_tween(&self, tween: Option<WidthTween>, target: f32) -> f32 {
        let Some(WidthTween { from, to, started }) = tween else {
            return target;
        };
        if self.motion.reduced_motion {
            return target;
        }
        let total = RESIZE.total();
        let raw = started.elapsed().as_secs_f32() / total.as_secs_f32();
        if raw >= 1.0 {
            return target;
        }
        self.motion.active.set(true);
        motion::lerp(from, to, RESIZE.progress(raw))
    }

    /// Animated width container: tweens 200ms ease-out on collapse/expand, and
    /// clips a fixed-width inner so content never reflows mid-transition.
    fn pane_container(
        &self,
        tween: Option<WidthTween>,
        target: f32,
        inner: AnyElement,
    ) -> AnyElement {
        div()
            .h_full()
            .flex_none()
            .overflow_hidden()
            .w(px(self.eval_tween(tween, target)))
            .child(inner)
            .into_any_element()
    }

    /// The animated spacer clearing the macOS traffic lights ahead of a
    /// titlebar control cluster. Fullscreen toggles tween the cluster start
    /// over 200ms ease-out ([`RESIZE`]; reduced motion snaps).
    /// `None` off macOS — no phantom flex child.
    fn titlebar_spacer(&self, container_pad: f32) -> Option<AnyElement> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let fullscreen = self.titlebar.fullscreen.unwrap_or(false);
        // The tween runs in cluster-start coordinates; the spacer is that
        // minus the container's own padding.
        let start = self.eval_tween(
            self.motion.titlebar_tween,
            titlebar_cluster_start(fullscreen),
        );
        let width = (start - container_pad).max(0.0);
        Some(div().flex_none().h_full().w(px(width)).into_any_element())
    }

    /// The header's content row with the animated left inset — the native port
    /// of zeron __root.tsx `transition-[padding-left] duration-200 ease-out` +
    /// `style={{ paddingLeft: headerInset }}`: on sidebar toggles (and macOS
    /// fullscreen flips) the SAME element's padding tweens, so the title
    /// glides to its new x-position. Route changes SNAP: the tween is killed
    /// by every route transition (zeron remounts the keyed header variants —
    /// instant swap, zero horizontal motion).
    /// Where unified-titlebar content (tabs / the settings label) starts: past
    /// the traffic lights + control cluster, riding the fullscreen inset tween.
    pub(super) fn title_bar_content_start(&self) -> f32 {
        let fullscreen = self.titlebar.fullscreen.unwrap_or(false);
        let is_macos = cfg!(target_os = "macos");
        let cluster = self.eval_tween(
            self.motion.titlebar_tween,
            cluster_buttons_start(is_macos, fullscreen),
        );
        cluster + CLUSTER_BUTTONS_WIDTH + 10.0
    }

    /// The unified window titlebar. Chat: only a drag strip over the
    /// SIDEBAR column's top band — the workspace tiles' own headers are the
    /// drag regions over the rest (a full-width strip would cover their
    /// tabs). Settings: the full-width band. The traffic lights and control
    /// cluster overlay its left end either way.
    fn render_title_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        match self.route {
            Route::Chat => {
                let sidebar_now = self.eval_tween(self.motion.sidebar_tween, self.sidebar_target());
                let bar = div()
                    .h(px(Theme::TITLEBAR_HEIGHT))
                    .w(px(sidebar_now))
                    .flex_none();
                self.titlebar_drag_region("chat-titlebar", bar, cx)
                    .into_any_element()
            }
            Route::Settings(_) => {
                let inner = div()
                    .size_full()
                    .flex()
                    .items_center()
                    .pt(px(Theme::TITLEBAR_TOP_PAD))
                    .pl(px(self.title_bar_content_start()))
                    .pr(px(titlebar_right_padding(
                        cfg!(target_os = "windows"),
                        Theme::SPACE_LG,
                    )));
                let bar = div().h(px(Theme::TITLEBAR_HEIGHT)).flex_none().child(inner);
                self.titlebar_drag_region("settings-header-titlebar", bar, cx)
                    .into_any_element()
            }
        }
    }

    /// Make a titlebar strip drag the window — zed's platform-titlebar
    /// pattern (zeron's `.drag` region): mark it a [`WindowControlArea::Drag`]
    /// (macOS app-owned titlebar), hand the drag to the compositor once the
    /// pointer moves with the button down, and double-click zooms.
    pub(super) fn titlebar_drag_region(
        &self,
        id: impl Into<gpui::ElementId>,
        el: gpui::Div,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        el.id(id)
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down_out(cx.listener(|this, _, _, _| this.titlebar.should_move = false))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.titlebar.should_move = false),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.titlebar.should_move = true),
            )
            // Hand the drag to the compositor only while the button is
            // actually held (`pressed_button` guard): on macOS
            // `start_window_move` runs AppKit's NATIVE drag session
            // (`performWindowDragWithEvent:`), and AppKit resolves a quick
            // second click inside that session as a titlebar double-click —
            // system zoom — natively, beyond gpui's reach. Without the guard a
            // stale `titlebar_should_move` (armed by a down whose bubble was
            // later stopped) would start that session from a mere hover move
            // between the two clicks of a double-click.
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, window, _| {
                    if this.titlebar.should_move && event.pressed_button == Some(MouseButton::Left)
                    {
                        this.titlebar.should_move = false;
                        window.start_window_move();
                    }
                }),
            )
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    if cfg!(target_os = "macos") {
                        // Native titlebar double-click action (zoom/minimize
                        // per system preference).
                        window.titlebar_double_click();
                    } else {
                        window.zoom_window();
                    }
                }
            })
    }

    /// The ONE top-left window-control cluster (sidebar toggle + back/forward —
    /// zeron window-controls.tsx): rendered once, in a paint-only overlay layer
    /// pinned at the window's top-left, ABOVE the sidebar and headers. The
    /// sidebar width animates *beneath* it, so the buttons keep their element
    /// identity and never move or remount on collapse/expand; only the
    /// fullscreen traffic-light inset tweens (the animated spacer). The
    /// container has no id/listeners — everything between the buttons falls
    /// through to the titlebar drag strips below.
    fn render_titlebar_cluster(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let can_back = self.nav.can_back();
        let can_forward = self.nav.can_forward();
        // The new-session + joins the cluster while the sidebar is collapsed
        // (fading on the sidebar width tween) — INSIDE the cluster row so it
        // shares the buttons' exact size and 2px rhythm; a separate mount in
        // the title row sat 10px off the cluster and read misaligned (user
        // report).
        let plus_alpha = self.titlebar_plus_alpha();
        let show_plus = matches!(self.route, Route::Chat) && plus_alpha > 0.01;
        div()
            .absolute()
            .top_0()
            .left_0()
            .h(px(Theme::TITLEBAR_HEIGHT))
            .flex()
            .flex_row()
            .items_center()
            .pt(px(Theme::TITLEBAR_TOP_PAD))
            .gap(px(2.0))
            .px(px(10.0))
            .children(self.titlebar_spacer(12.0))
            .child(window_control_button(
                "toggle-sidebar",
                icons::SIDEBAR_MINIMALISTIC_LEFT,
                &theme,
                cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)),
            ))
            .child(nav_history_button(
                "nav-back",
                icons::ARROW_LEFT,
                can_back,
                &theme,
                cx.listener(|this, _, _, cx| this.navigate_back(cx)),
            ))
            .child(nav_history_button(
                "nav-forward",
                icons::ARROW_RIGHT,
                can_forward,
                &theme,
                cx.listener(|this, _, _, cx| this.navigate_forward(cx)),
            ))
            .children(show_plus.then(|| {
                div()
                    .flex_none()
                    .opacity(plus_alpha)
                    .child(window_control_button(
                        "titlebar-new-session",
                        icons::PLUS,
                        &theme,
                        cx.listener(|this, _, _, cx| this.open_new_session(cx)),
                    ))
            }))
            // Workspace layout presets (Chat route only).
            .when(matches!(self.route, Route::Chat), |el| {
                el.child(self.render_layout_button(&theme, cx))
            })
            .into_any_element()
    }

    /// How present the titlebar's new-session + is: 0 with the sidebar open
    /// (the + lives in the sidebar header), 1 fully collapsed, riding the
    /// sidebar width tween in between.
    pub(super) fn titlebar_plus_alpha(&self) -> f32 {
        let sidebar_now = self.eval_tween(self.motion.sidebar_tween, self.sidebar_target());
        let open_width = self.settings.sidebar_width.max(1.0);
        (1.0 - sidebar_now / open_width).clamp(0.0, 1.0)
    }

    /// Native Windows caption controls integrated into Cypher's unified
    /// titlebar. `WindowControlArea` maps these hit targets to HTMINBUTTON,
    /// HTMAXBUTTON, and HTCLOSE, so Windows owns their behavior (including
    /// Snap Layouts) while GPUI renders the system Segoe caption glyphs.
    fn render_windows_caption_controls(&self, window: &Window, cx: &App) -> Option<AnyElement> {
        if !cfg!(target_os = "windows") {
            return None;
        }

        let theme = Theme::of(cx);
        let (maximize_id, maximize_glyph) = if window.is_maximized() {
            ("window-restore", "\u{e923}")
        } else {
            ("window-maximize", "\u{e922}")
        };
        Some(
            div()
                .id("windows-window-controls")
                .absolute()
                .top_0()
                .right_0()
                .h(px(Theme::TITLEBAR_HEIGHT))
                .flex()
                .flex_row()
                .font_family("Segoe Fluent Icons")
                .child(windows_caption_button(
                    "window-minimize",
                    "\u{e921}",
                    WindowControlArea::Min,
                    theme,
                    false,
                ))
                .child(windows_caption_button(
                    maximize_id,
                    maximize_glyph,
                    WindowControlArea::Max,
                    theme,
                    false,
                ))
                .child(windows_caption_button(
                    "window-close",
                    "\u{e8bb}",
                    WindowControlArea::Close,
                    theme,
                    true,
                ))
                .into_any_element(),
        )
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = crate::appearance::surface_style::theme(
            crate::appearance::surface_style::Region::Sidebar,
            cx,
        );
        let inner: AnyElement = match self.route {
            Route::Settings(section) => self.render_settings_nav(section, &theme, cx),
            Route::Chat => self.render_chat_sidebar(&theme, cx),
        };
        let target = self.sidebar_target();
        // Transparent — the sidebar sits directly on the frost shell; the main
        // card's gutter, tone, and shadow provide the separation without a
        // vertical divider. The content row spans the full window height (the
        // titlebar overlays it), so the column pads itself below the chrome.
        self.pane_container(
            self.motion.sidebar_tween,
            target,
            div()
                .h_full()
                .pt(px(Theme::TITLEBAR_HEIGHT))
                .child(inner)
                .into_any_element(),
        )
    }

    /// Settings navigation has a pinned device selector, scrollable scope
    /// groups, and a pinned Back row. Only Device settings follow the selector;
    /// client preferences and workspace management keep their own scope.
    fn render_settings_nav(
        &mut self,
        section: SettingsSection,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let section_icon = |item: SettingsSection| match item {
            SettingsSection::Devices => icons::MONITOR,
            SettingsSection::Harnesses => icons::WIDGET,
            SettingsSection::Providers => icons::KEY_MINIMALISTIC,
            SettingsSection::Titles => icons::TUNING,
            SettingsSection::Commands => icons::COMMAND,
            SettingsSection::Mcp => icons::GLOBAL,
            SettingsSection::Subagents => icons::CHECKLIST,
            SettingsSection::Github => icons::GITHUB_MARK,
            SettingsSection::Appearance => icons::TUNING,
            SettingsSection::Notifications => icons::BELL,
            SettingsSection::Shortcuts => icons::KEYBOARD,
            SettingsSection::Archived => icons::ARCHIVE_MINIMALISTIC,
        };
        let mut groups = div()
            .id("settings-nav-groups")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(8.0))
            .pb(px(16.0));
        for (index, (title, sections)) in SettingsSection::NAV_GROUPS.into_iter().enumerate() {
            let rows = sections
                .iter()
                .copied()
                .map(|item| {
                    let selected = item == section;
                    div()
                        .id(SharedString::from(format!("settings-nav-{}", item.label())))
                        .role(gpui::Role::Button)
                        .aria_label(item.label())
                        .tab_index(0)
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .min_h(px(32.0))
                        .rounded(px(8.0))
                        .px(px(8.0))
                        .py(px(6.0))
                        .text_size(px(13.0))
                        .when(selected, |el| {
                            el.bg(crate::appearance::surface_style::sidebar_selected(theme))
                                .font_weight(gpui::FontWeight::MEDIUM)
                        })
                        .text_color(if selected {
                            theme.text
                        } else {
                            theme.text_muted
                        })
                        .cursor_pointer()
                        .hover(|s| {
                            s.bg(if selected {
                                crate::appearance::surface_style::sidebar_selected(theme)
                            } else {
                                crate::appearance::surface_style::sidebar_hover(theme)
                            })
                            .text_color(theme.text)
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.open_settings(item, cx)))
                        .child(
                            icon(section_icon(item))
                                .size(px(16.0))
                                .text_color(if selected {
                                    theme.text
                                } else {
                                    theme.text_muted
                                }),
                        )
                        .child(SharedString::from(item.label()))
                        .into_any_element()
                })
                .collect::<Vec<_>>();
            groups = groups.child(
                div()
                    .id(("settings-nav-group", index))
                    .flex()
                    .flex_col()
                    .when(index > 0, |el| {
                        el.mt(px(12.0))
                            .pt(px(12.0))
                            .border_t_1()
                            .border_color(theme.border)
                    })
                    .child(
                        div()
                            .px(px(8.0))
                            .pt(px(4.0))
                            .pb(px(8.0))
                            .text_size(px(11.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text_muted)
                            .child(title),
                    )
                    .child(div().flex().flex_col().gap(px(2.0)).children(rows)),
            );
        }
        // Follow the user-resized sidebar instead of imposing a header width.
        div()
            .w(px(self.settings.sidebar_width))
            .h_full()
            .min_h_0()
            .key_context("SettingsNavigation")
            .tab_group()
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "tab" {
                    if event.keystroke.modifiers.shift {
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(px(12.0))
                    .pt(px(12.0))
                    .pb(px(16.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(
                        div()
                            .px(px(4.0))
                            .text_size(px(13.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(SharedString::from("Settings")),
                    )
                    .child(self.pages.target.clone()),
            )
            .child(groups)
            // Neither the device selector nor Back scroll with the sections.
            .child(
                div()
                    .flex_none()
                    .px(px(8.0))
                    .py(px(12.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .id("settings-back")
                            .role(gpui::Role::Button)
                            .aria_label("Back to chats")
                            .tab_index(0)
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .rounded(px(8.0))
                            .px(px(Theme::SPACE_SM))
                            .py(px(6.0))
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .cursor_pointer()
                            .hover(|s| {
                                s.bg(crate::appearance::surface_style::sidebar_hover(theme))
                                    .text_color(theme.text)
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.close_settings(cx)))
                            .child(
                                // AltArrowLeft chevron (zeron settings-sidebar.tsx),
                                // not the straight history arrow.
                                icon(icons::ALT_ARROW_LEFT)
                                    .size(px(16.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(SharedString::from("Back")),
                    ),
            )
            .into_any_element()
    }

    /// One compact session row: agent mark + title on the left, status corner
    /// on the right (mini spinner while working, amber question mark while the
    /// run waits on an answer, emerald check for unseen finished turns,
    /// relative time otherwise). The row is inset from the project-card
    /// edge — or, when `nested` under a checkout section, from that
    /// section's rail; click selects and right-click opens the context
    /// menu. The branch is NOT repeated per row — it lives in the
    /// branch/worktree group header above (see [`spaces`](crate::shell::spaces)).
    /// `harness` is `None` when the sidebar hides agent marks (one runtime
    /// throughout): the title then starts in line with the project title.
    /// `Some(None)` keeps the mark's slot empty so titles stay aligned.
    pub(super) fn render_chat_row(
        &self,
        row: ChatRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ChatRow {
            id,
            title,
            time_ago,
            harness,
            status,
            selected,
            pinned,
            nested,
        } = row;
        let corner = chat_row_corner(&id, time_ago, status, theme, cx);
        let (hover, text) = (
            crate::appearance::surface_style::sidebar_hover(theme),
            theme.text,
        );
        let selected_wash = crate::appearance::surface_style::sidebar_selected(theme);
        let subline = theme.text_muted.opacity(0.5);
        let select_id = id.clone();
        let menu_id = id.clone();
        let drag = workspace_view::TabDrag::new(
            crate::workspace::TabKey::session(id.clone()),
            title.clone(),
            harness.flatten(),
        );
        // Hover fades over transition-colors (zeron session-row.tsx) — both
        // the wash and the title brighten ride the same 150ms blend.
        let fade_key = format!("chat-row-{id}");
        let rest_bg = if selected {
            selected_wash
        } else {
            crate::kit::theme::wash(0.0)
        };
        // A selected row must NOT drift toward the hover wash: in dark the two
        // fills are identical so the blend is a no-op, but light's hover sits
        // below its near-opaque selected fill, and blending toward it visibly
        // dimmed the active row under the pointer (user report).
        let hover_bg = if selected { selected_wash } else { hover };
        let rest_text = if selected { text } else { text.opacity(0.8) };
        div()
            .id(SharedString::from(format!("chat-{id}")))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .rounded(px(8.0))
            .mx(px(6.0))
            .mb(px(2.0))
            // Sessions are children of the project header: keep the selected
            // wash inset 6px, then indent the content so the agent mark lands
            // beneath the project title rather than at the card's left edge.
            // Under a checkout section the rail already carries the indent.
            // Without an agent mark the title itself takes the mark's place,
            // landing on the project title's 33px column either way.
            .pl(px(match (nested, harness.is_some()) {
                (false, true) => 20.0,
                (false, false) => 27.0,
                (true, true) => 8.0,
                (true, false) => 11.0,
            }))
            .pr(px(8.0))
            .py(px(6.0))
            .text_color(motion::hover_blend(&fade_key, rest_text, text))
            .bg(motion::hover_blend(&fade_key, rest_bg, hover_bg))
            // No selection ring (user request) — the wash alone marks the
            // active row.
            .on_hover(motion::hover_listener(fade_key.clone()))
            .cursor_pointer()
            // ⌘-click (Ctrl elsewhere) opens it in a new split to the right.
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                let split = event.modifiers().secondary();
                this.open_chat_with(select_id.clone(), split, cx);
            }))
            // Drag into the workspace: onto a tab bar or tile centre (join)
            // or a tile edge (split). A plain click still opens it.
            .on_drag(drag, workspace_view::TabDrag::ghost)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.menus.chat.open((menu_id.clone(), event.position));
                    cx.notify();
                }),
            )
            // Agent identity + title. The project card already owns host and
            // project identity, so sessions only repeat what distinguishes
            // one run from another.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .when_some(harness, |el, harness| {
                        el.child(match harness.map(crate::pickers::harness_brand_icon) {
                            Some((path, tint)) => icon(path)
                                .size(px(14.0))
                                .flex_none()
                                .text_color(tint.unwrap_or(subline).opacity(0.82))
                                .into_any_element(),
                            None => div().size(px(14.0)).flex_none().into_any_element(),
                        })
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .line_height(px(16.0))
                            .child(title),
                    )
                    .when(pinned, |el| {
                        el.child(
                            icon(icons::PIN)
                                .size(px(11.0))
                                .flex_none()
                                .text_color(subline),
                        )
                    }),
            )
            // Status corner: spinner / unread check / relative time.
            .child(div().text_color(subline).child(corner))
            .into_any_element()
    }

    /// Chat-mode sidebar: Cypher / Add project header, project-grouped session
    /// cards (every host together), the notice strip, and the UserMenu.
    fn render_chat_sidebar(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (user, workspace_scope) = {
            let state = self.state.read(cx);
            (state.auth_user().cloned(), state.workspace_scope)
        };

        // Keyed rows: (stable key, estimated height, element) — the key + height
        // list drives the resort FLIP diff below (attention-bucket
        // promotions glide; cleared rows just go).
        let keyed: Vec<(String, f32, AnyElement)> = self.render_active_rows(theme, cx);

        // Resort glide (View Transitions parity): when the ORDER of a live
        // list changes (new activity resort, grouping flip), surviving rows
        // glide from their old y to the new one — layout is already at the new
        // position; the offset is a paint-only relative inset animated to 0
        // over 260ms cubic-bezier(0.22,1,0.36,1). New rows fade in; removals
        // just go (matching the original). First fill and chat switches (which
        // don't reorder) never animate.
        let order: Vec<(String, f32)> = keyed.iter().map(|(k, h, _)| (k.clone(), *h)).collect();
        if self.sidebar.prev_order != order {
            if !self.sidebar.prev_order.is_empty() {
                let offsets = resort_offsets(&self.sidebar.prev_order, &order, GROUP_CARD_GAP);
                let prev_keys: std::collections::HashSet<&str> = self
                    .sidebar
                    .prev_order
                    .iter()
                    .map(|(k, _)| k.as_str())
                    .collect();
                let new_keys: std::collections::HashSet<String> = order
                    .iter()
                    .filter(|(k, _)| !prev_keys.contains(k.as_str()))
                    .map(|(k, _)| k.clone())
                    .collect();
                if !offsets.is_empty() || !new_keys.is_empty() {
                    self.sidebar.resort_epoch += 1;
                    self.sidebar.resort = offsets;
                    self.sidebar.new_keys = new_keys;
                }
            }
            self.sidebar.prev_order = order;
        }
        let epoch = self.sidebar.resort_epoch;
        let list_items: Vec<AnyElement> = keyed
            .into_iter()
            .map(|(key, _, element)| {
                if let Some(dy) = self.sidebar.resort.get(&key).copied() {
                    let id = SharedString::from(format!("resort-{epoch}-{key}"));
                    div()
                        .child(element)
                        .with_animation(id, RESORT.animation(), move |el, t| {
                            el.relative().top(px(dy * (1.0 - t)))
                        })
                        .into_any_element()
                } else if self.sidebar.new_keys.contains(&key) {
                    let id = SharedString::from(format!("row-in-{epoch}-{key}"));
                    motion::fade_quick(id, div().child(element)).into_any_element()
                } else {
                    element
                }
            })
            .collect();

        let (user_line, trigger_subline, menu_identity): (
            SharedString,
            Option<SharedString>,
            SharedString,
        ) = match workspace_scope {
            Some(WorkspaceScope::Local) => {
                let line = if matches!(self.sync.flow, SyncFlow::RestartPending { .. }) {
                    "Sync ready after restart"
                } else {
                    "Local only"
                };
                (line.into(), None, "Stored on this device".into())
            }
            Some(WorkspaceScope::Development) => (
                "Development".into(),
                Some("Local development runtime".into()),
                "Authentication disabled".into(),
            ),
            Some(WorkspaceScope::Synced) | None => {
                let line: SharedString = user
                    .as_ref()
                    .map(|u| u.name.clone().unwrap_or_else(|| u.email.clone()).into())
                    .unwrap_or_else(|| SharedString::from("Not signed in"));
                let email = user
                    .as_ref()
                    .map(|u| SharedString::from(u.email.clone()))
                    .unwrap_or_else(|| line.clone());
                (line, None, email)
            }
        };
        let avatar_url = user
            .as_ref()
            .and_then(|u| u.avatar_url.clone())
            .map(SharedString::from);
        let user_menu = self.render_user_menu(
            user_line.clone(),
            trigger_subline,
            menu_identity,
            avatar_url,
            theme,
            cx,
        );

        // The fixed product header + Add project action lives ABOVE the scroll
        // region (it must stay reachable no matter how long the card list gets).
        let actions = self.render_sidebar_header(theme, cx);

        div()
            .w(px(self.settings.sidebar_width))
            .h_full()
            .flex()
            .flex_col()
            // (No titlebar strip: the unified window titlebar spans the whole
            // window above this column.)
            .child(actions)
            // The project-grouped card list scrolls inside an EdgeFade scope —
            // a true per-glyph gradient at active overflow edges. Glass-safe
            // (no painted overlay can fade content over see-through blur) and
            // equivalent on opaque themes: alpha→0 reveals the surface tone
            // underneath, same as the gradient overlays it replaced. Overflow
            // is read at PAINT time via the scroll handle — render-time gating
            // rode the previous frame's offset, so the last frame of a content
            // shrink (row archived while scrolled) left a phantom fade stuck
            // over an unscrollable list (user report).
            .child(
                crate::kit::edge_fade::edge_faded(
                    SIDEBAR_GLASS_FADE_BAND,
                    true,
                    true,
                    div().relative().flex_1().min_h_0().child(
                        div()
                            .id("sidebar-lists")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.sidebar.scroll)
                            .px(px(Theme::SPACE_SM))
                            .flex()
                            .flex_col()
                            // No "Sessions" header (user request) — the list
                            // is the whole column; a little air stands in.
                            .pt(px(4.0))
                            .child(if !list_items.is_empty() {
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(GROUP_CARD_GAP))
                                    .pb(px(Theme::SPACE_SM))
                                    .children(list_items)
                                    .into_any_element()
                            } else {
                                div()
                                    .px(px(Theme::SPACE_SM))
                                    .pb(px(Theme::SPACE_SM))
                                    .text_size(px(12.0))
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from("No projects yet"))
                                    .into_any_element()
                            }),
                    ),
                )
                .fade_overflow_y(&self.sidebar.scroll),
            )
            // Update strip (above the user menu; below the lists). App-wide
            // chrome — the main window's alone.
            .when_some(
                self.render_update_strip(theme, cx)
                    .filter(|_| !self.is_project_window()),
                |el, strip| el.child(strip),
            )
            .when_some(
                self.render_pi_update_strip(theme, cx)
                    .filter(|_| !self.is_project_window()),
                |el, strip| el.child(strip),
            )
            // Inline mutation-failure notice.
            .when_some(self.sidebar.notice.clone(), |el, notice| {
                el.child(
                    div()
                        .id("sidebar-notice")
                        .mx(px(Theme::SPACE_SM))
                        .mb(px(Theme::SPACE_SM))
                        .px(px(Theme::SPACE_SM))
                        .py(px(4.0))
                        .rounded(px(Theme::CONTROL_RADIUS))
                        .border_1()
                        .border_color(theme.danger)
                        .text_size(px(11.0))
                        .text_color(theme.danger)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.sidebar.notice = None;
                            cx.notify();
                        }))
                        .child(notice),
                )
            })
            .when(!self.is_project_window(), |el| {
                el.child(div().p(px(Theme::SPACE_SM)).flex_none().child(user_menu))
            })
            .into_any_element()
    }
}

/// A chat row's status corner. It shares the relative-time slot so the
/// compact row's width stays stable: spinner while working, amber question
/// mark while the agent waits on the user, emerald check for an unseen
/// finished turn ("ready for you"), time otherwise. The pulse clock drives
/// the spinner while it stays mounted.
fn chat_row_corner(
    id: &str,
    time_ago: SharedString,
    status: ChatIndicator,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> AnyElement {
    match status {
        ChatIndicator::Working => div()
            .flex_none()
            .child(loaders::mini_gradient_spinner(
                format!("chat-working-{id}"),
                2.0,
                cx.entity_id(),
                cx,
            ))
            .into_any_element(),
        // The turn is parked on a question, so the spinner has stopped —
        // without a corner of its own the row fell back to the relative
        // time and read exactly like an idle session (user report). Amber
        // is the tone the theme reserves for awaiting-input; the glyph is
        // slightly larger than the check because it carries inner detail.
        ChatIndicator::AwaitingInput => icon(icons::QUESTION_CIRCLE)
            .size(px(12.0))
            .flex_none()
            .text_color(theme.warning)
            .into_any_element(),
        ChatIndicator::Completed => icon(icons::CHECK)
            .size(px(11.0))
            .flex_none()
            .text_color(theme.success.opacity(0.9))
            .into_any_element(),
        _ => div()
            .flex_none()
            .text_size(px(10.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .child(time_ago)
            .into_any_element(),
    }
}

/// One sidebar session row's content and state (see
/// [`Shell::render_chat_row`]).
pub(super) struct ChatRow {
    pub id: String,
    pub title: SharedString,
    pub time_ago: SharedString,
    pub harness: Option<Option<cypher_proto::HarnessId>>,
    pub status: ChatIndicator,
    pub selected: bool,
    pub pinned: bool,
    pub nested: bool,
}

/// The sign-in gate's faint grid backdrop (zeron styles.css `.bg-grid`):
/// 44px hairlines at white 3.5%, with the radial mask approximated by edge
/// gradients back into the page background (gpui has no mask-image).
pub(super) fn grid_backdrop(theme: &Theme) -> AnyElement {
    let line = crate::kit::theme::hairline(0.035);
    let bg = theme.bg;
    const STEP: f32 = 44.0;
    const SPAN: f32 = 2640.0;
    let verticals = (1..(SPAN / STEP) as usize).map(|i| {
        div()
            .absolute()
            .left(px(i as f32 * STEP))
            .top_0()
            .bottom_0()
            .w(px(1.0))
            .bg(line)
    });
    let horizontals = (1..((SPAN * 0.75) / STEP) as usize).map(|i| {
        div()
            .absolute()
            .top(px(i as f32 * STEP))
            .left_0()
            .right_0()
            .h(px(1.0))
            .bg(line)
    });
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .children(verticals)
        .children(horizontals)
        // Mask approximation: fade the grid back into the background toward
        // the window edges (the original masks to an ellipse at 50% / 40%).
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(120.0))
                .bg(gpui::linear_gradient(
                    180.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(px(260.0))
                .bg(gpui::linear_gradient(
                    0.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(px(200.0))
                .bg(gpui::linear_gradient(
                    90.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(200.0))
                .bg(gpui::linear_gradient(
                    270.0,
                    gpui::linear_color_stop(bg, 0.0),
                    gpui::linear_color_stop(bg.opacity(0.0), 1.0),
                )),
        )
        .into_any_element()
}

/// A size-6 icon button for the titlebar strip (zeron window-controls.tsx:
/// `grid size-6 place-items-center rounded-md text-muted-foreground`).
pub(super) fn window_control_button(
    id: &'static str,
    icon_path: &'static str,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let muted = theme.text_muted;
    let fade_key = format!("window-control-{id}");
    div()
        .id(id)
        .size(px(24.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        // zeron window-controls.tsx: `transition-colors` — the wash fades.
        .bg(motion::hover_blend(
            &fade_key,
            theme.glass_hover().opacity(0.0),
            theme.glass_hover(),
        ))
        .on_hover(motion::hover_listener(fade_key))
        // Buttons in/over a titlebar drag strip must be EXCLUDED from the
        // strip's event surface entirely. `.occlude()` (gpui
        // `HitboxBehavior::BlockMouse`) makes the window hit-test STOP at the
        // button, so every `is_hovered`-guarded strip listener — the
        // mouse-down that arms the drag, the mouse-move that hands AppKit a
        // native drag session (`performWindowDragWithEvent:`, whose second
        // quick click zooms NATIVELY on macOS), and the `click_count == 2`
        // zoom handler — never fires with the pointer over a button. It also
        // removes the button's rect from the native Drag control-area
        // hit-test on Windows/Linux. The click-level stop_propagation is
        // zed's ButtonLike belt on top. Double-click on EMPTY strip space
        // still zooms — nothing occludes it there.
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        .child(icon(icon_path).size(px(16.0)).text_color(muted))
}

const WINDOWS_CAPTION_BUTTON_WIDTH: f32 = 36.0;
const WINDOWS_CAPTION_WIDTH: f32 = WINDOWS_CAPTION_BUTTON_WIDTH * 3.0;

pub(super) fn titlebar_right_padding(is_windows: bool, base: f32) -> f32 {
    base + if is_windows {
        WINDOWS_CAPTION_WIDTH
    } else {
        0.0
    }
}

/// A Windows-owned caption target using the same system glyphs and native
/// non-client hit-test areas as GPUI/Zed's platform titlebar.
fn windows_caption_button(
    id: &'static str,
    glyph: &'static str,
    area: WindowControlArea,
    theme: &Theme,
    close: bool,
) -> impl IntoElement {
    let (hover_bg, hover_fg, active_bg, active_fg) = if close {
        let red: gpui::Hsla = gpui::rgb(0xe81123).into();
        (
            red,
            gpui::white(),
            red.opacity(0.8),
            gpui::white().opacity(0.8),
        )
    } else {
        (
            theme.glass_hover(),
            theme.text,
            theme.glass_hover().opacity(0.7),
            theme.text,
        )
    };
    div()
        .id(id)
        .w(px(WINDOWS_CAPTION_BUTTON_WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(10.0))
        .text_color(theme.text)
        .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
        .active(move |style| style.bg(active_bg).text_color(active_fg))
        .occlude()
        .window_control_area(area)
        .child(glyph)
}

/// A titlebar history button (zeron window-controls.tsx): enabled it is a
/// normal window-control button; disabled it dims to 35% opacity and ignores
/// the pointer (`disabled:pointer-events-none disabled:opacity-35`).
fn nav_history_button(
    id: &'static str,
    icon_path: &'static str,
    enabled: bool,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    if !enabled {
        return div()
            .size(px(24.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            // Even disabled it reads as a control — occlude so double-clicks
            // on it don't fall through to the titlebar strip's zoom handler.
            .occlude()
            .child(
                icon(icon_path)
                    .size(px(16.0))
                    .text_color(theme.text_muted.opacity(0.35)),
            )
            .into_any_element();
    }
    window_control_button(id, icon_path, theme, on_click).into_any_element()
}

/// A size-7 icon button for the main-panel header (zeron __root.tsx:
/// `grid size-7 place-items-center rounded-md text-muted-foreground`).
pub(super) fn header_icon_button(
    id: impl Into<gpui::ElementId>,
    icon_path: &'static str,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let id = id.into();
    let muted = theme.text_muted;
    let fade_key = format!("header-icon-{id}");
    div()
        .id(id)
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        // zeron __root.tsx header buttons: `transition-colors`.
        .bg(motion::hover_blend(
            &fade_key,
            crate::kit::theme::wash(0.0),
            crate::kit::theme::wash(0.11),
        ))
        .on_hover(motion::hover_listener(fade_key))
        // Same occlusion + click-swallowing as [`window_control_button`]: this
        // button sits inside a tile header's titlebar drag region, so its
        // rect must be carved out of the strip's drag/double-click surface.
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        .child(icon(icon_path).size(px(16.0)).text_color(muted))
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "dev-capture")]
        if !self.is_project_window() {
            crate::shell::dev_capture::start_once(window.window_handle(), cx);
        }
        let scroll_activity = self.sample_notification_activity(window, cx);
        // A sidebar chat selection can leave settings without close_settings.
        // Do not retain a hidden credential field in the cached page entity.
        if self.route != Route::Settings(SettingsSection::Providers)
            && let Some(page) = &self.pages.providers
        {
            page.update(cx, |page, cx| page.dismiss(cx));
        }
        let theme = Theme::of(cx);
        // The shell tone (zeron `.frost`): the surface the sidebar sits on and
        // the main panel floats over as an inset rounded card. On macOS the
        // window background is the blurred desktop (lib.rs `Blurred`), so the
        // frost paints translucent — the sidebar and card margins read as
        // glass while the opaque card keeps text off it.
        let (frost, text, font) = (theme.glass(), theme.text, theme.font_sans.clone());
        let (workspace_scope, auth) = {
            let state = self.state.read(cx);
            (state.workspace_scope, state.auth.clone())
        };
        self.sync.flow = sync_flow_after_auth(self.sync.flow, workspace_scope, auth.as_ref());
        let restart_required = self.sync.flow == SyncFlow::SignedOutRestartRequired;
        let gate = self
            .dev
            .gate
            .clone()
            .unwrap_or_else(|| self.state.read(cx).gate());

        self.track_fullscreen(window, cx);
        self.route_focus(&gate, restart_required, window, cx);

        let root = div()
            .id("shell-root")
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .bg(frost)
            .text_color(text)
            .font_family(font)
            .text_size(px(14.0))
            .child(scroll_activity)
            .capture_any_mouse_down(cx.listener(|this, _, _, cx| {
                if this
                    .attention
                    .notification_activity
                    .interact(std::time::Instant::now())
                {
                    cx.notify();
                }
            }))
            .capture_key_down(cx.listener(|this, _, _, cx| {
                if this
                    .attention
                    .notification_activity
                    .interact(std::time::Instant::now())
                {
                    cx.notify();
                }
            }))
            .track_focus(&self.focus.root);
        let root = self.bind_shell_actions(root, cx);

        let render_gate = if restart_required {
            GatePhase::Loading
        } else {
            gate.clone()
        };
        let root = match &render_gate {
            GatePhase::Ready => {
                self.on_ready_frame(window, cx);
                root.child(self.render_ready_page(window, cx))
            }
            GatePhase::Loading => root, // splash overlay covers boot
            GatePhase::OrgGate => {
                let card = self.render_org_gate(cx);
                root.child(card)
            }
            phase @ (GatePhase::Failed(_) | GatePhase::SignIn) => {
                let card = self.render_gate_card(phase, cx);
                root.child(card)
            }
        };
        let root = if restart_required {
            let restart = self.render_signed_out_restart(cx);
            root.child(restart)
        } else {
            root
        };

        // A manually-driven tween is mid-flight: keep frames coming (the same
        // scheduling `with_animation` would have requested). Hover color fades
        // ride the same clock; their once-per-frame tick lives here (this is
        // the window's root render — it runs exactly once per frame).
        if self.motion.active.get() | motion::hover_fades_active() {
            window.request_animation_frame();
        }

        // Boot splash overlay: visible → crossfades out on Ready → removed.
        let root = match self.splash {
            SplashPhase::Visible => {
                let theme = Theme::of(cx).clone();
                root.child(loaders::splash_overlay(&theme, false))
            }
            SplashPhase::FadingOut => {
                let theme = Theme::of(cx).clone();
                root.child(loaders::splash_overlay(&theme, true))
            }
            SplashPhase::Gone => root,
        };

        // Caption controls are shell-level chrome, not Ready-page content:
        // keep them above the splash and every auth/org/error gate as well as
        // the full application. Gate pages also need a native drag surface
        // because they do not render the unified tabs/settings titlebar.
        let root = if (!restart_required && matches!(gate, GatePhase::Ready))
            || !cfg!(target_os = "windows")
        {
            root
        } else {
            root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(Theme::TITLEBAR_HEIGHT))
                    .window_control_area(WindowControlArea::Drag),
            )
        };
        root.children(self.render_windows_caption_controls(window, cx))
    }
}

impl Shell {
    /// Feed this frame's foreground/selection into the notification activity
    /// sampler; returns the scroll observer that marks interaction.
    fn sample_notification_activity(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let foreground = window.is_window_active();
        let selected = matches!(self.route, Route::Chat)
            .then(|| self.state.read(cx).selected_chat.clone())
            .flatten();
        if let Some(activity) = self.attention.notification_activity.sample(
            foreground,
            selected,
            std::time::Instant::now(),
        ) {
            self.state.update(cx, |state, cx| {
                state.report_notification_activity(activity, cx)
            });
        }
        let weak = cx.entity().downgrade();
        crate::shell::notification_activity::scroll_observer(move |cx| {
            let _ = weak.update(cx, |shell, cx| {
                if shell
                    .attention
                    .notification_activity
                    .interact(std::time::Instant::now())
                {
                    cx.notify();
                }
            });
        })
    }

    /// Track fullscreen (the titlebar cluster tween) and reset this pass's
    /// manual tween bookkeeping.
    fn track_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Fullscreen hides the macOS traffic lights — reflow the control
        // cluster with a 200ms ease-out tween. A fullscreen transition
        // resizes the window, which re-renders us, so polling here is exact.
        let fullscreen = window.is_fullscreen();
        if self.titlebar.fullscreen != Some(fullscreen) {
            if self.titlebar.fullscreen.is_some() && cfg!(target_os = "macos") {
                self.motion.titlebar_tween = Some(WidthTween::new(
                    titlebar_cluster_start(!fullscreen),
                    titlebar_cluster_start(fullscreen),
                ));
            }
            self.titlebar.fullscreen = Some(fullscreen);
        }
        // Manual tween drive bookkeeping for this pass (see [`WidthTween`]).
        self.motion.reduced_motion = motion::reduced_motion(cx);
        self.motion.active.set(false);
    }

    /// Keep keyboard focus somewhere that dispatches: the focused tile's
    /// composer on Chat, a blur elsewhere, and never a find field that is no
    /// longer rendered.
    fn route_focus(
        &mut self,
        gate: &GatePhase,
        restart_required: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Keyboard shortcuts (mod-s/b/j) dispatch through the window focus
        // chain — with nothing focused they go dead. Land initial focus on the
        // composer, and whenever focus is lost with no successor (e.g. the
        // focused element unmounted), route it back there.
        // The landing spot is the FOCUSED tile's composer (the root when
        // that tile is empty, so window shortcuts keep dispatching).
        if self.focus.sub.is_none() {
            self.focus.sub = Some(cx.on_focus_lost(window, |this: &mut Shell, window, cx| {
                match this.route {
                    Route::Chat if !this.showing_setup() => this.focus_landing(window, cx),
                    // No composer here — clear the stale handle so `focused()`
                    // reads None (the render hook below re-lands focus when the
                    // route returns to Chat; a lingering unmounted handle would
                    // otherwise dead-end keyboard dispatch for good).
                    Route::Chat | Route::Settings(_) => window.blur(),
                }
            }));
        }
        let chat_ready = !restart_required
            && matches!(gate, GatePhase::Ready)
            && matches!(self.route, Route::Chat)
            && !self.showing_setup();
        if chat_ready
            && (window.focused(cx).is_none() || std::mem::take(&mut self.tiles.focus_pending))
        {
            self.focus_landing(window, cx);
        }
        // The find bar can close out from under the keyboard — selecting
        // another chat closes find inside the transcript, which unmounts the
        // field without ever firing a focus-lost event. Focus would then sit
        // on an element that no longer renders and every key would dead-end.
        let stranded: Vec<session::SlotId> = self
            .tiles
            .slots
            .iter()
            .filter(|(_, slot)| {
                !slot.transcript.read(cx).find_open()
                    && slot.find_input.focus_handle(cx).is_focused(window)
            })
            .map(|(sid, _)| *sid)
            .collect();
        for sid in stranded {
            if let Some(slot) = self.tiles.slots.get_mut(&sid) {
                slot.find_focus_pending = false;
            }
            match self.route {
                Route::Chat if !self.showing_setup() => self.focus_landing(window, cx),
                Route::Chat | Route::Settings(_) => window.blur(),
            }
        }
    }

    /// The root's drag handlers, window actions and the surface-copy key.
    fn bind_shell_actions(
        &self,
        root: gpui::Stateful<gpui::Div>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        root.on_drag_move(cx.listener(Self::on_sidebar_drag))
            .on_drag_move(cx.listener(Self::on_dock_drag))
            .on_drag_move(cx.listener(Self::on_terminal_drag))
            .on_drag_move(cx.listener(Self::on_split_drag))
            // The panel shortcuts are chat-scoped chrome: in Settings they are
            // no-ops (zeron __root.tsx gates the hotkey on `!isSettings`, and
            // the terminal panel is only mounted on session routes). The
            // sidebar toggle stays live everywhere, as in the original.
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| {
                if matches!(this.route, Route::Chat)
                    && let Some(sid) = this.focused_slot()
                {
                    this.toggle_terminal(sid, window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            // New session works from anywhere — `open_new_session` routes back
            // to chat itself, so Settings is not a dead spot.
            .on_action(cx.listener(|this, _: &NewSession, _, cx| this.open_new_session(cx)))
            // Chat-scoped, unlike new-session — `cycle_session` holds the guard
            // and says why.
            .on_action(cx.listener(|this, _: &NextSession, _, cx| this.cycle_session(true, cx)))
            .on_action(cx.listener(|this, _: &PrevSession, _, cx| this.cycle_session(false, cx)))
            .on_action(cx.listener(|this, _: &ToggleChanges, _, cx| {
                if matches!(this.route, Route::Chat)
                    && let Some(sid) = this.focused_slot()
                {
                    this.toggle_dock(sid, cx)
                }
            }))
            // Workspace layout (customizable — see `apply_keymap`).
            .on_action(cx.listener(|this, _: &SplitRight, _, cx| {
                this.split_focused(crate::workspace::Edge::Right, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitDown, _, cx| {
                this.split_focused(crate::workspace::Edge::Bottom, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusLeft, _, cx| {
                this.focus_neighbour(crate::workspace::Edge::Left, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusRight, _, cx| {
                this.focus_neighbour(crate::workspace::Edge::Right, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusUp, _, cx| {
                this.focus_neighbour(crate::workspace::Edge::Top, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusDown, _, cx| {
                this.focus_neighbour(crate::workspace::Edge::Bottom, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseTab, _, cx| this.close_focused_tab(cx)))
            .on_action(cx.listener(|this, _: &ToggleZoom, _, cx| this.toggle_zoom_focused(cx)))
            .on_action(cx.listener(|this, _: &FocusTile1, _, cx| this.focus_tile(0, cx)))
            .on_action(cx.listener(|this, _: &FocusTile2, _, cx| this.focus_tile(1, cx)))
            .on_action(cx.listener(|this, _: &FocusTile3, _, cx| this.focus_tile(2, cx)))
            .on_action(cx.listener(|this, _: &FocusTile4, _, cx| this.focus_tile(3, cx)))
            .on_action(cx.listener(|this, _: &FocusTile5, _, cx| this.focus_tile(4, cx)))
            .on_action(cx.listener(|this, _: &FocusTile6, _, cx| this.focus_tile(5, cx)))
            .on_action(cx.listener(|this, _: &FocusTile7, _, cx| this.focus_tile(6, cx)))
            .on_action(cx.listener(|this, _: &FocusTile8, _, cx| this.focus_tile(7, cx)))
            .on_action(cx.listener(|this, _: &FocusTile9, _, cx| this.focus_tile(8, cx)))
            .map(|root| {
                use crate::workspace::Preset;
                macro_rules! layout {
                    ($root:expr, $($action:ident => $preset:ident),* $(,)?) => {
                        $root$(.on_action(cx.listener(|this, _: &$action, _, cx| {
                            this.apply_layout_from_menu(Preset::$preset, cx)
                        })))*
                    };
                }
                layout!(
                    root,
                    LayoutSingle => Single,
                    LayoutColumns2 => Columns2,
                    LayoutRows2 => Rows2,
                    LayoutColumns3 => Columns3,
                    LayoutRows3 => Rows3,
                    LayoutGrid2x2 => Grid2x2,
                    LayoutGrid3x3 => Grid3x3,
                    LayoutTwoStackedPlusOne => TwoStackedPlusOne,
                    LayoutOnePlusTwoStacked => OnePlusTwoStacked,
                )
            })
            .on_action(
                cx.listener(|this, _: &crate::shell::menus::OpenSettings, _, cx| {
                    this.open_settings(SettingsSection::Harnesses, cx);
                }),
            )
            // About / updates / adding projects are app-wide: a project
            // window hands them to the main window.
            .on_action(cx.listener(|this, _: &crate::shell::menus::About, _, cx| {
                if this.is_project_window() {
                    this.forward_to_main(cx, |main, cx| main.open_about(cx));
                    return;
                }
                this.open_about(cx);
            }))
            .on_action(
                cx.listener(|this, _: &crate::shell::menus::CheckForUpdates, _, cx| {
                    if this.is_project_window() {
                        this.forward_to_main(cx, |main, cx| main.begin_update_check(cx));
                        return;
                    }
                    this.begin_update_check(cx);
                }),
            )
            .on_action(cx.listener(|this, _: &AddSpacePalette, _, cx| {
                if this.is_project_window() {
                    this.forward_to_main(cx, |main, cx| {
                        if main.dialogs.add_space.is_none() {
                            main.open_add_space(cx);
                        }
                    });
                } else if this.dialogs.add_space.is_some() {
                    this.dialogs.add_space = None;
                    cx.notify();
                } else {
                    this.open_add_space(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FindInChat, _, cx| {
                if let Some(sid) = this.focused_slot() {
                    this.open_find(sid, cx)
                }
            }))
            // Transcript/diff text takes no focus: a drag there leaves focus
            // on this root, outside every input's Copy binding. Edit → Copy
            // dispatches the action here; ⌘C arrives as a raw key (a global
            // binding would pre-empt the terminal's own raw ⌘C copy).
            .on_action(|_: &crate::widgets::text_input::Copy, _, cx| {
                copy_surface_selection(cx);
            })
            .on_key_down(|event: &gpui::KeyDownEvent, _, cx| {
                let ks = &event.keystroke;
                let m = &ks.modifiers;
                if ks.key == "c"
                    && (m.platform || m.control)
                    && !(m.shift || m.alt || m.function)
                    && copy_surface_selection(cx)
                {
                    cx.stop_propagation();
                }
            })
    }

    /// Per-frame bookkeeping while the app is Ready: sync liveness and update
    /// checks on window activation, seen-marking visible sessions, and the
    /// capture knob that opens the model menu.
    fn on_ready_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Focus is a sync signal: on the rising edge of window
        // activation, nudge every open room to verify liveness, so a
        // broadcast-deaf socket (accepted writes, pongs, nothing
        // delivered) heals within seconds of the user looking at the
        // app rather than waiting out the background probe cadence.
        let window_active = window.is_window_active();
        if window_active && !self.attention.was_window_active {
            self.state.update(cx, |s, cx| s.probe_sync(cx));
            // Platforms release independently, so the build you want
            // may have shipped while you were away. The engine rate
            // limits this, so the rising edge is safe to forward every
            // time; it wakes the checker and never blocks on the
            // network.
            if let Some(engine) = self.state.read(cx).engine().cloned() {
                cx.background_spawn(async move {
                    let _ = engine
                        .client()
                        .call(methods::UPDATE_ON_ACTIVATION, serde_json::json!({}))
                        .await;
                })
                .detach();
            }
        }
        self.attention.was_window_active = window_active;
        // A run finishing while you're LOOKING at the session must not
        // badge "completed" until you leave and return — mark it seen
        // live while the window is active (idempotent guard inside;
        // one extra frame settles it).
        // Every VISIBLE tile's session counts.
        if window_active {
            let unseen_visible: Vec<String> = {
                let s = self.state.read(cx);
                self.workspace
                    .visible_tabs()
                    .into_iter()
                    .filter_map(|tab| tab.chat_id())
                    .filter(|id| s.chats.iter().any(|c| c.id == *id && c.unseen()))
                    .map(str::to_string)
                    .collect()
            };
            for chat_id in unseen_visible {
                self.state
                    .update(cx, |s, cx| s.mark_chat_seen(&chat_id, cx));
            }
        }
        // Capture knob: `CYPHER_OPEN_DIALOG=model` pops the combined
        // harness/model menu (needs `window`, so it fires here rather
        // than in `on_state_changed`).
        if self.dev.open_dialog.as_deref() == Some("model")
            && let Some(composer) = self
                .focused_slot()
                .and_then(|sid| self.tiles.slots.get(&sid))
                .map(|slot| slot.composer.clone())
        {
            self.dev.open_dialog = None;
            composer.update(cx, |c, cx| c.debug_open_model_menu(window, cx));
        }
    }

    /// The Ready page: sidebar | workspace (or settings), the titlebar over
    /// it, and the overlays, as one keyed entrance.
    fn render_ready_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let sidebar = self.render_sidebar(cx);
        let sidebar_handle = self.resize_handle(
            "sidebar-resize",
            || SidebarResize,
            |shell, _| shell.settings.sidebar_width = SIDEBAR_DEFAULT,
            cx,
        );
        // Chat: the workspace of session tiles; Settings: the section
        // outlet. The per-session state stays intact for the return
        // trip.
        let main = match self.route {
            Route::Chat => self.render_workspace(window, cx),
            Route::Settings(section) => self.render_settings_main(section, cx),
        };
        let overlays = self.render_overlays(window.viewport_size(), window, cx);
        // The whole app page is one keyed `animate-in` entrance (zeron
        // App.tsx `<div key={phase} className="animate-in h-full">`):
        // arriving from the splash or any gate fades the page in; the
        // splash-out crossfades over it on boot.
        // The sidebar resize handle FLOATS over the sidebar/workspace
        // seam (zero layout width) so the sidebar's right gutter stays
        // exactly as wide as its left one — a 5px flex child here read
        // as lopsided spacing.
        let sidebar_seam = div()
            .w(px(0.0))
            .h_full()
            .flex_none()
            .relative()
            .child(sidebar_handle.absolute().top_0().bottom_0().left(px(-2.0)));
        let title_bar = self.render_title_bar(cx);
        // Two columns: sidebar | workspace (or settings). The content
        // row spans the FULL window height — the titlebar overlays it
        // (glass, no fill); the sidebar pads itself down, and the
        // top-row tiles' headers sit in the titlebar band.
        let page = div()
            .size_full()
            .relative()
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_row()
                    .child(sidebar)
                    .child(sidebar_seam)
                    .child(main),
            )
            .child(div().absolute().top_0().left_0().right_0().child(title_bar))
            .child(self.render_titlebar_cluster(cx))
            .children(overlays);
        motion::fade_in("phase-app", page).into_any_element()
    }
}
