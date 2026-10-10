//! The sidebar's project cards: group keys and collapse state, the active
//! rows, card headers (project and branch) and the card glyph.

use super::*;

impl Shell {
    /// Deterministic disclosure identity for a project card — the same
    /// `g:{key}` the FLIP resort diff keys cards by, so one key drives both
    /// the collapse state and the resort baseline. Local Shell UI state only.
    fn project_group_key(card_key: &str) -> String {
        format!("g:{card_key}")
    }

    /// Deterministic disclosure identity for a branch/worktree group, scoped
    /// under its project card. `worktree` + `label` + the worktree's NORMALIZED
    /// path is the same identity [`group_chats`] keys groups by, so a group
    /// that reappears after status churn or a reorder keeps its collapsed
    /// state — and two same-label detached worktrees stay distinct. The
    /// normalization makes an equivalent `/wt` and `/wt/` one stable key.
    pub(super) fn branch_group_key(
        card_key: &str,
        worktree: bool,
        label: &str,
        worktree_path: Option<&str>,
    ) -> String {
        format!(
            "g:{card_key}/b:{worktree}:{label}:{}",
            worktree_path.map(normalize_worktree_path).unwrap_or("")
        )
    }

    /// Is this disclosure group (project card or branch/worktree group)
    /// currently collapsed?
    fn sidebar_group_collapsed(&self, key: &str) -> bool {
        self.sidebar.collapsed.contains(key)
    }

    /// Toggle a disclosure group (project card or branch/worktree group).
    /// Collapse state is local Shell UI state — never persisted or synced;
    /// everything starts expanded.
    fn toggle_sidebar_group(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.sidebar.collapsed.remove(&key) {
            self.sidebar.collapsed.insert(key);
        }
        cx.notify();
    }

    /// The sidebar's project-grouped list: one group per `Space`
    /// (every host together, empty spaces included) plus synthetic
    /// No-project / Unavailable-project groups; the selected session's
    /// project floats as a card. Ordering comes from
    /// [`AppState::sidebar_groups`]; cards are keyed for the FLIP resort
    /// glide (a group's height is an estimate — header + visible rows). The
    /// state's refs borrow `cx`, so the cards are snapshotted into owned form
    /// first (the same clone-per-row cost the pre-grouping sidebar paid).
    pub(in crate::shell) fn render_active_rows(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<(String, f32, AnyElement)> {
        let now = Utc::now();
        let selected = self.state.read(cx).selected_chat.clone();
        let cards: Vec<GroupCard> = {
            let view = self.sidebar_view();
            let state = self.state.read(cx);
            let groups = state.sidebar_groups_with(now, &view);
            // Host names only earn header space when the list spans several
            // machines; a merged Quick chats card counts each session's host.
            let hosts: HashSet<&str> = groups
                .iter()
                .flat_map(|g| {
                    std::iter::once(g.device_id.as_str())
                        .chain(g.chats.iter().map(|(_, c)| c.device_id.as_str()))
                })
                .collect();
            let show_device = hosts.len() > 1;
            // Config-less rows never had a mark, so they don't count as a
            // runtime of their own.
            let harnesses: HashSet<cypher_proto::HarnessId> = groups
                .iter()
                .flat_map(|g| g.chats.iter())
                .filter_map(|(_, c)| c.config.as_ref().map(|c| c.harness))
                .collect();
            let show_harness = harnesses.len() > 1;
            groups
                .into_iter()
                .map(|g| {
                    let chats: Vec<(ChatIndicator, Chat)> = g
                        .chats
                        .into_iter()
                        .map(|(status, chat)| (status, chat.clone()))
                        .collect();
                    let groups = if g.kind == SidebarGroupKind::Scratch {
                        group_chats_by_device(chats, |id| {
                            state
                                .device_name(id)
                                .unwrap_or("Unknown device")
                                .to_string()
                        })
                    } else {
                        group_chats(chats, g.path.as_deref())
                    };
                    GroupCard {
                        key: g.key,
                        kind: g.kind,
                        pinned: g.pinned,
                        icon: g.icon,
                        color: g.color,
                        title: g.title,
                        device: g.device,
                        show_device,
                        show_harness,
                        offline: g.offline,
                        space_id: g.space_id.map(str::to_string),
                        groups,
                    }
                })
                .collect()
        };
        // Only the project holding the selected session floats as a card; the
        // rest are bare groups with the same layout. A focus move crossfades
        // the card plate over — never on first paint.
        let focused = selected.as_deref().and_then(|id| {
            cards
                .iter()
                .find(|card| card.contains_chat(id))
                .map(|card| card.key.clone())
        });
        if focused != self.sidebar.focused_card {
            let previous = std::mem::replace(&mut self.sidebar.focused_card, focused);
            if !self.sidebar.prev_order.is_empty() {
                self.sidebar.focus_from = previous;
                self.sidebar.focus_tween = Some(WidthTween::new(0.0, 1.0));
            }
        }
        let focus_t = self.eval_tween(self.sidebar.focus_tween, 1.0);
        cards
            .into_iter()
            .map(|card| {
                let key = card.key.clone();
                let lift = if self.sidebar.focused_card.as_ref() == Some(&key) {
                    focus_t
                } else if self.sidebar.focus_from.as_ref() == Some(&key) {
                    1.0 - focus_t
                } else {
                    0.0
                };
                // A card's height is an estimate for the FLIP resort glide:
                // header + one branch-group header per group + its rows — but
                // only for what's currently visible. A collapsed project is
                // header only; a collapsed branch group keeps its header and
                // drops its rows.
                let project_key = Self::project_group_key(&key);
                // A lone ordinary checkout has no section header: rows sit
                // directly under the card header.
                let height = super::GROUP_CARD_HEADER_HEIGHT
                    + if self.sidebar_group_collapsed(&project_key) || card.chat_count() == 0 {
                        0.0
                    } else if card.inline_groups() {
                        super::GROUP_CARD_BODY_PADDING
                            + card.chat_count() as f32 * super::CHAT_ROW_HEIGHT
                    } else {
                        super::GROUP_CARD_BODY_PADDING
                            + card.groups.iter().fold(0.0_f32, |acc, g| {
                                let group_key = Self::branch_group_key(
                                    &key,
                                    g.worktree,
                                    &g.label,
                                    g.worktree_path.as_deref(),
                                );
                                acc + super::BRANCH_GROUP_HEADER_HEIGHT
                                    + if self.sidebar_group_collapsed(&group_key) {
                                        0.0
                                    } else {
                                        g.chats.len() as f32 * super::CHAT_ROW_HEIGHT
                                    }
                            })
                    };
                let element = self.render_group_card(&card, &selected, now, lift, theme, cx);
                (format!("g:{key}"), height, element)
            })
            .collect()
    }

    /// One project group (12px radius, clipped). Every project has the same
    /// layout, and `lift` only adds the card plate: 1 is the opaque floating
    /// card (`theme.surface`, subtle shadow + hairline ring) of the project
    /// holding the selected session, 0 a bare group on the sidebar frost.
    /// The header owns the prominent project label, a hover-revealed target
    /// machine and a presence dot. Real space headers host the rename/remove
    /// context menu on right-click; synthetic cards have no menu. Below the
    /// header, chats are grouped by checkout: when there is more than one, a
    /// small section label introduces each group and a hairline rail runs
    /// down its left edge, tying the indented session rows to it; a lone
    /// ordinary checkout skips both and lists its sessions directly.
    fn render_group_card(
        &self,
        group: &GroupCard,
        selected: &Option<String>,
        now: chrono::DateTime<Utc>,
        lift: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let project_key = Self::project_group_key(&group.key);
        let project_collapsed = self.sidebar_group_collapsed(&project_key);
        let header = self.render_group_header(group, lift, theme, cx);
        // Rows are the visible branch/worktree group headers and their session
        // rows: a collapsed project hides every group, a collapsed branch
        // group keeps its header and drops its rows. An inline card (one
        // ordinary checkout) skips the header, and with it any stale branch
        // collapse that would otherwise hide rows with no way back.
        let flat = group.inline_groups();
        let rows: Vec<AnyElement> = if project_collapsed {
            Vec::new()
        } else {
            group
                .groups
                .iter()
                .flat_map(|chat_group| {
                    let group_key = Self::branch_group_key(
                        &group.key,
                        chat_group.worktree,
                        &chat_group.label,
                        chat_group.worktree_path.as_deref(),
                    );
                    let group_collapsed = !flat && self.sidebar_group_collapsed(&group_key);
                    let mut elements: Vec<AnyElement> =
                        Vec::with_capacity(1 + chat_group.chats.len());
                    if !flat {
                        elements.push(self.render_branch_group_header(
                            &group_key,
                            chat_group,
                            group_collapsed,
                            group.space_id.as_deref(),
                            theme,
                            cx,
                        ));
                    }
                    if !group_collapsed {
                        let chat_rows = chat_group.chats.iter().map(|(status, chat)| {
                            let time_ago: SharedString = format_time_ago(
                                chat.last_message_at.unwrap_or(chat.created_at),
                                now,
                            )
                            .into();
                            let is_selected = selected.as_deref() == Some(chat.id.as_str());
                            let harness = group
                                .show_harness
                                .then(|| chat.config.as_ref().map(|c| c.harness));
                            self.render_chat_row(
                                ChatRow {
                                    id: chat.id.clone(),
                                    title: transcript::single_line(
                                        &chat.title.clone().unwrap_or_else(|| "New session".into()),
                                    )
                                    .into(),
                                    time_ago,
                                    harness,
                                    status: *status,
                                    selected: is_selected,
                                    pinned: chat.pinned,
                                    nested: !flat,
                                },
                                theme,
                                cx,
                            )
                        });
                        if flat {
                            elements.extend(chat_rows);
                        } else {
                            // The rail sits under the section icon's centre
                            // (10px inset + half the 11px glyph), so the
                            // sessions read as hanging off their checkout.
                            elements.push(
                                div()
                                    .ml(px(15.0))
                                    .border_l_1()
                                    .border_color(crate::kit::theme::hairline(0.09))
                                    .flex()
                                    .flex_col()
                                    .children(chat_rows)
                                    .into_any_element(),
                            );
                        }
                    }
                    elements
                })
                .collect()
        };
        let has_body = !rows.is_empty();
        div()
            .rounded(px(12.0))
            .bg(theme.sidebar_card_fill(lift))
            .shadow(theme.sidebar_card_shadows_at(lift))
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(header)
            .children(rows)
            .when(has_body, |el| el.pb(px(super::GROUP_CARD_BODY_PADDING)))
            .into_any_element()
    }

    /// One branch/worktree section label between the project and its sessions
    /// (shown only when a project spans more than one checkout). It is a
    /// small, muted caption on a short row — a level below the session
    /// titles it introduces, so the three tiers read project → checkout →
    /// session at a glance. Its icon sits on the card's 10px column, directly
    /// above the rail that runs down the group's rows. The disclosure chevron
    /// only surfaces on hover, except on a collapsed group, which keeps the
    /// closed chevron and its hidden sessions' [`StatusSummary`]; clicking
    /// the row hides/shows the group's sessions.
    fn render_branch_group_header(
        &self,
        key: &str,
        group: &ChatGroup,
        collapsed: bool,
        space_id: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let toggle_key = key.to_string();
        // The row's hover group: hovering anywhere on the header reveals its
        // chevron and trailing plus.
        let hover_key = format!("branch-add-hover-{key}");
        let hover_t = motion::hover_t(&hover_key);
        let worktree = group.worktree;
        let worktree_path = group.worktree_path.clone();
        let branch = group.branch.clone();
        let caption = theme.text_muted.opacity(0.6);
        // A collapsed group hides its rows' status corners: their summary
        // rides ahead of the chevron instead.
        let status = collapsed
            .then(|| {
                render_status_summary(
                    &format!("branch-{key}"),
                    status_summary(group.chats.iter().map(|(status, _)| status)),
                    theme,
                    cx,
                )
            })
            .flatten();
        let chevron = div()
            .flex_none()
            .size(px(10.0))
            .opacity(if collapsed { 1.0 } else { hover_t })
            .child(
                icon(if collapsed {
                    icons::ALT_ARROW_RIGHT
                } else {
                    icons::ALT_ARROW_DOWN
                })
                .size(px(9.0))
                .text_color(theme.text_muted.opacity(0.55)),
            );
        let mut header = div()
            .id(SharedString::from(format!("branch-hdr-{key}")))
            .h(px(super::BRANCH_GROUP_HEADER_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .cursor_pointer()
            .on_hover(motion::hover_listener(hover_key.clone()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_sidebar_group(toggle_key.clone(), cx);
            }))
            .mx(px(10.0))
            .text_size(px(11.0))
            .line_height(px(13.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(caption)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(5.0))
                    .child(
                        icon(group.icon)
                            .size(px(11.0))
                            .flex_none()
                            .text_color(caption),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(group.label.clone())),
                    ),
            )
            .children(status)
            .child(chevron);
        // Real-space groups get the trailing add plus at the far tail (after
        // the disclosure chevron), opening a canvas targeted at THIS checkout
        // — the worktree's exact path (authoritative without ListRefs) or the
        // project-root checkout with the real optional branch metadata.
        if let Some(space_id) = space_id {
            let space_id = space_id.to_string();
            header = header.child(hover_add_plus(
                format!("branch-add-{key}"),
                &hover_key,
                6.0,
                theme,
                cx,
                move |this, _, _, cx| {
                    let plan = if worktree {
                        crate::pickers::CheckoutPlan::ReuseWorktree {
                            path: worktree_path.clone().unwrap_or_default(),
                            branch: branch.clone(),
                        }
                    } else {
                        crate::pickers::CheckoutPlan::CurrentCheckout {
                            branch: branch.clone(),
                        }
                    };
                    this.open_new_session_for(space_id.clone(), plan, cx);
                },
            ));
        }
        header.into_any_element()
    }

    /// A project card's single-line header: folder icon + prominent project
    /// name on the left — semibold (the strongest type in the sidebar) on
    /// the focused card, medium on bare groups so a column of headers stays
    /// light while still out-ranking the session titles; dimming them instead read as disabled and blurred the
    /// no-sessions dim below. The name is followed by the branch when the
    /// card's lone checkout is inlined — then the target-machine name, only
    /// while the host can't be reached a slashed-cloud glyph, and on a
    /// collapsed card the hidden sessions' [`StatusSummary`] at the far
    /// right. The machine name only appears when the sidebar spans several
    /// hosts, and then only on hover unless the host is offline; a project
    /// with no sessions dims its title. The header toggles the whole card
    /// body (all branch/worktree groups + sessions) on left-press;
    /// real-space headers open the rename/remove context menu on
    /// right-click, while synthetic cards render no menu.
    fn render_group_header(
        &self,
        group: &GroupCard,
        lift: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let menu_space = group.space_id.clone();
        // The header's hover group: hovering anywhere on the row reveals its
        // trailing plus (the button is always laid out — nothing moves).
        let hover_key = format!("space-add-hover-{}", group.key);
        let toggle_key = Self::project_group_key(&group.key);
        let title = group.title.clone();
        let device: SharedString = group.device.clone().into();
        let offline = group.offline;
        let hover_t = motion::hover_t(&hover_key);
        // Offline hosts stay named so the dead machine is obvious; online
        // ones fade in with the row hover (width too, so the title keeps the
        // room while hidden).
        let device_t = if !group.show_device || device.is_empty() {
            0.0
        } else if offline {
            1.0
        } else {
            hover_t
        };
        let collapsed = self.sidebar_group_collapsed(&toggle_key);
        let chat_count = group.chat_count();
        let quiet = chat_count == 0;
        // A collapsed card hides its rows' status corners, so the header
        // carries their summary, at the far right on the rows' corner column
        // (header 10px inset + 4 = a row's 6px inset + 8px padding).
        let status = collapsed
            .then(|| {
                render_status_summary(
                    &format!("space-{}", group.key),
                    status_summary(
                        group
                            .groups
                            .iter()
                            .flat_map(|g| g.chats.iter().map(|(status, _)| status)),
                    ),
                    theme,
                    cx,
                )
            })
            .flatten()
            .map(|summary| div().flex_none().mr(px(4.0)).child(summary));
        let inline_branch: Option<SharedString> = (group.kind != SidebarGroupKind::Scratch
            && group.inline_groups())
        .then(|| group.groups.first().and_then(|g| g.branch.clone()))
        .flatten()
        .map(Into::into);
        let card_icon = if group.kind == SidebarGroupKind::Scratch {
            icons::CHAT_ROUND_LINE
        } else {
            crate::appearance::space_style::space_icon(group.icon.as_deref())
        };
        let icon_tint = crate::appearance::space_style::space_color(group.color.as_deref(), theme)
            .unwrap_or(theme.text);
        let pinned = group.pinned;
        // Online is the normal case and says nothing; only an unreachable
        // host earns a mark.
        let unreachable = offline.then(|| {
            let hint: SharedString = if device.is_empty() {
                "Device offline".into()
            } else {
                format!("{device} is offline").into()
            };
            div()
                .id(SharedString::from(format!(
                    "space-card-{}-offline",
                    group.key
                )))
                .flex_none()
                .child(
                    icon(icons::CLOUD_OFF)
                        .size(px(13.0))
                        .text_color(theme.text_muted.opacity(0.7)),
                )
                .tooltip(move |_, cx| cx.new(|_| super::session::FindTooltip(hint.clone())).into())
        });
        let mut header = div()
            .id(SharedString::from(format!(
                "space-card-{}-header",
                group.key
            )))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(7.0))
            .px(px(10.0))
            .h(px(super::GROUP_CARD_HEADER_HEIGHT))
            .flex_none()
            // The whole header toggles the card body on left-press.
            // Right-click stays the project menu; see `menu_space` below.
            .cursor_pointer()
            .on_hover(motion::hover_listener(hover_key.clone()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.toggle_sidebar_group(toggle_key.clone(), cx);
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(self.render_card_glyph(group, card_icon, icon_tint, cx))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.5))
                            .line_height(px(18.0))
                            .font_weight(if lift >= 0.5 {
                                gpui::FontWeight::SEMIBOLD
                            } else {
                                gpui::FontWeight::MEDIUM
                            })
                            .text_color(if quiet {
                                theme.text_muted.opacity(0.8)
                            } else {
                                theme.text
                            })
                            .child(SharedString::from(title)),
                    )
                    .when_some(inline_branch, |el, branch| {
                        el.child(
                            div()
                                .min_w_0()
                                .max_w(px(120.0))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(3.0))
                                .text_size(px(11.0))
                                .text_color(theme.text_muted.opacity(0.55))
                                .child(
                                    icon(icons::GIT_BRANCH)
                                        .size(px(10.0))
                                        .flex_none()
                                        .text_color(theme.text_muted.opacity(0.5)),
                                )
                                .child(div().min_w_0().truncate().child(branch)),
                        )
                    })
                    .when(pinned, |el| {
                        el.child(
                            icon(icons::PIN)
                                .size(px(11.0))
                                .flex_none()
                                .text_color(theme.text_muted.opacity(0.6)),
                        )
                    }),
            )
            .when(device_t > 0.0, |el| {
                el.child(
                    div()
                        .flex_none()
                        .min_w_0()
                        .max_w(px(96.0 * device_t))
                        .mr(px(-7.0 * (1.0 - device_t)))
                        .opacity(device_t)
                        .truncate()
                        .text_right()
                        .text_size(px(10.5))
                        .text_color(theme.text_muted.opacity(0.62))
                        .child(device),
                )
            })
            .children(unreachable)
            .children(status);
        if let Some(space_id) = menu_space {
            // Real-space headers also get the trailing add plus (after the
            // offline glyph): a canvas explicitly targeted at the project's
            // ordinary/current checkout — pinned `CurrentCheckout { branch:
            // None }` so no stale worktree draft survives. Synthetic cards
            // get neither the plus nor the menu.
            let plus_space = space_id.clone();
            header = header.child(hover_add_plus(
                format!("space-add-{}", group.key),
                &hover_key,
                7.0,
                theme,
                cx,
                move |this, _, _, cx| {
                    this.open_new_session_for(
                        plus_space.clone(),
                        crate::pickers::CheckoutPlan::CurrentCheckout { branch: None },
                        cx,
                    );
                },
            ));
            // Right-click anywhere on the header opens the project menu.
            let menu_id = space_id;
            header = header.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.menus.space.open((menu_id.clone(), event.position));
                    cx.notify();
                }),
            );
        }
        header.into_any_element()
    }

    /// The card's glyph. On a real project it is a button: a left press
    /// opens the glyph/colour picker instead of toggling the card (the
    /// press is stopped before the header sees it).
    fn render_card_glyph(
        &self,
        group: &GroupCard,
        asset: &'static str,
        tint: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let glyph = icon(asset).size(px(14.0)).flex_none().text_color(tint);
        let Some(space_id) = group.space_id.clone() else {
            return glyph.into_any_element();
        };
        let fade_key = format!("space-glyph-{}", group.key);
        div()
            .id(SharedString::from(format!("space-glyph-{}", group.key)))
            .flex_none()
            .size(px(20.0))
            .ml(px(-3.0))
            .rounded(px(5.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .bg(motion::hover_blend(
                &fade_key,
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.14),
            ))
            .on_hover(motion::hover_listener(fade_key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                    this.close_space_menu(cx);
                    this.menus
                        .space_style
                        .open((space_id.clone(), event.position));
                    cx.notify();
                }),
            )
            .child(glyph)
            .into_any_element()
    }
}

/// A collapsed header's [`StatusSummary`], in the session rows' own marks:
/// awaiting input `? n` (amber), working (the spinner, uncounted), finished
/// unseen `✓ n` (green) — most urgent first. `None` when nothing shows.
fn render_status_summary(
    key: &str,
    summary: StatusSummary,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Option<AnyElement> {
    if summary.is_empty() {
        return None;
    }
    let counted = |mark: Option<AnyElement>, count: usize, tint: gpui::Hsla| {
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(3.0))
            .children(mark)
            .child(
                div()
                    .text_size(px(10.5))
                    .line_height(px(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(tint)
                    .child(SharedString::from(count.to_string())),
            )
    };
    let awaiting = (summary.awaiting > 0).then(|| {
        let mark =
            super::render::status_mark(String::new(), ChatIndicator::AwaitingInput, theme, cx);
        counted(mark, summary.awaiting, theme.warning)
    });
    let working = summary
        .working
        .then(|| {
            super::render::status_mark(format!("{key}-working"), ChatIndicator::Working, theme, cx)
        })
        .flatten();
    let completed = (summary.completed > 0).then(|| {
        let mark = super::render::status_mark(String::new(), ChatIndicator::Completed, theme, cx);
        counted(mark, summary.completed, theme.success.opacity(0.9))
    });
    Some(
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .children(awaiting)
            .children(working)
            .children(completed)
            .into_any_element(),
    )
}
