//! Rendering: the pane's rows, file bodies and the `Render` impl.

use super::*;

impl Changes {
    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(parsed) = &self.parsed else {
            return gpui::Empty.into_any_element();
        };
        let files = parsed.files.clone();
        let parsed_key = parsed.key.clone();
        let Some(row) = self.rows.get(ix).copied() else {
            return gpui::Empty.into_any_element();
        };
        let theme = crate::appearance::surface_style::theme(
            crate::appearance::surface_style::Region::Git,
            cx,
        );
        let mono = theme.mono();
        let font_id = window.text_system().resolve_font(&mono);
        self.mono_advance = window
            .text_system()
            .em_advance(font_id, px(DIFF_TEXT_SIZE))
            .map(f32::from)
            .unwrap_or(DIFF_TEXT_SIZE * 0.6);
        match row {
            DiffRow::FileHeader { file } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let fold = self.folds.get(&file_diff.path).copied().unwrap_or_default();
                self.render_file_header(file as usize, file_diff, &fold, &theme, cx)
            }
            DiffRow::Notice { file, notice } => files
                .get(file as usize)
                .and_then(|f| file_notices(f).into_iter().nth(notice as usize))
                .map(|text| notice_row(text, &theme))
                .unwrap_or_else(|| gpui::Empty.into_any_element()),
            DiffRow::HunkHeader { file, hunk } => files
                .get(file as usize)
                .and_then(|f| f.hunks.get(hunk as usize))
                .map(|h| hunk_header_row(&h.header, &theme))
                .unwrap_or_else(|| gpui::Empty.into_any_element()),
            DiffRow::Line {
                file,
                hunk,
                line,
                flat: _,
            } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let highlight = self.request_highlight(file_diff, &parsed_key, cx);
                // The row's line INDEX (stable while the diff is current) is
                // the key; `line` below is shadowed by the &DiffLine body.
                let line_ix = line;
                let Some(line) = file_diff
                    .hunks
                    .get(hunk as usize)
                    .and_then(|h| h.lines.get(line_ix as usize))
                else {
                    return gpui::Empty.into_any_element();
                };
                let spans = highlight
                    .as_deref()
                    .map(|highlights| highlights.spans(line))
                    .unwrap_or(&[]);
                // Stable per-line key (owner-scoped so multiple diff panes
                // never collide) + the shared selection callbacks: ONLY the
                // code text is selectable (gutters/markers stay inert).
                let key = diff_line_key(&self.owner, file, hunk, line_ix);
                let selection = self.selection_ui_for(self.sel_scope, None, cx);
                diff_line_row(
                    line,
                    spans,
                    &theme,
                    gutter_width(file_diff),
                    Some((self.sel_scope, &key, &selection)),
                )
            }
            DiffRow::SplitLine {
                file,
                hunk,
                old,
                new,
            } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let highlight = self.request_highlight(file_diff, &parsed_key, cx);
                let Some(h) = file_diff.hunks.get(hunk as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let split = SplitRow {
                    row: ix,
                    highlights: highlight.as_deref(),
                    gutter: gutter_width(file_diff),
                };
                let left = self.split_cell(
                    &split,
                    Side::Old,
                    old.and_then(|i| h.lines.get(i as usize)),
                    old.map(|i| format!("{}:old", diff_line_key(&self.owner, file, hunk, i))),
                    &theme,
                    cx,
                );
                let right = self.split_cell(
                    &split,
                    Side::New,
                    new.and_then(|i| h.lines.get(i as usize)),
                    new.map(|i| format!("{}:new", diff_line_key(&self.owner, file, hunk, i))),
                    &theme,
                    cx,
                );
                div()
                    .w_full()
                    .h(px(DIFF_LINE_HEIGHT))
                    .flex()
                    .child(left)
                    .child(right)
                    .into_any_element()
            }
            DiffRow::BodyPad { .. } => div().w_full().h(px(BODY_BOTTOM_PAD)).into_any_element(),
            DiffRow::FoldingBody { file } => {
                let Some(file_diff) = files.get(file as usize) else {
                    return gpui::Empty.into_any_element();
                };
                let fold = self.folds.get(&file_diff.path).copied().unwrap_or_default();
                let highlight = self.request_highlight(file_diff, &parsed_key, cx);
                let (from, to) = (fold.from, fold.to);
                // Only the revealable slice is built — the tween never pays
                // for lines it cannot show.
                let cap = from.max(to).min(FOLD_TWEEN_MAX_PX);
                let body = render_file_body_upto(file_diff, highlight, &theme, cap);
                let clipped = div().w_full().overflow_hidden().child(body);
                if fold.animating() {
                    clipped
                        .with_animation(
                            SharedString::from(format!("fold-{}-{}", file_diff.path, fold.epoch)),
                            COLLAPSE.animation(),
                            move |el, t| el.h(px(motion::lerp(from, to, t))),
                        )
                        .into_any_element()
                } else {
                    // Post-tween, pre-settle: hold the full target height so
                    // the settle splice swaps rows without any reflow (the
                    // capped slice always covers what the viewport can see —
                    // tweens start from a clicked, on-screen header).
                    clipped.h(px(to)).into_any_element()
                }
            }
        }
    }

    fn render_file_header(
        &mut self,
        ix: usize,
        file: &FileDiff,
        fold: &FileFold,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let collapsed = fold.collapsed;
        let path = file.path.clone();
        let adds = file.additions;
        let dels = file.deletions;

        // Chevron (zeron checkout-diff-sidebar): chevron-right closed,
        // chevron-down open; gpui divs have no rotation transform at the
        // pinned rev, so the glyph swap crossfades over the same 200 ms.
        let chevron_icon = if collapsed {
            crate::kit::icons::ALT_ARROW_RIGHT
        } else {
            crate::kit::icons::ALT_ARROW_DOWN
        };
        let chevron = div().flex_none().size(px(14.0)).child(
            crate::kit::icons::icon(chevron_icon)
                .size(px(13.0))
                .text_color(theme.text_muted.opacity(0.7)),
        );
        let chevron: AnyElement = if fold.animating() {
            chevron
                .with_animation(
                    SharedString::from(format!("chev-{path}-{}", fold.epoch)),
                    CHEVRON.animation(),
                    |el, t| el.opacity(0.25 + 0.75 * t),
                )
                .into_any_element()
        } else {
            chevron.into_any_element()
        };

        // Header row: chevron + mono path (one quiet tone) + right-aligned
        // +N / −N counts on a slightly raised wash. The header carries the
        // section separator (the per-file wrapper it used to hang on is
        // gone — rows are flat now).
        div()
            .id(SharedString::from(format!("file-hdr-{ix}")))
            .w_full()
            .h(px(FILE_HEADER_HEIGHT))
            .when(ix > 0, |el| {
                el.border_t_1()
                    .border_color(crate::kit::theme::hairline(0.04))
            })
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(Theme::SPACE_MD))
            .bg(crate::kit::theme::ink(0.025))
            .cursor_pointer()
            .hover(|s| s.bg(crate::kit::theme::ink(0.05)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_fold(ix, cx);
                cx.notify();
            }))
            .child(chevron)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .mono(theme)
                    .text_size(px(12.0))
                    .text_color(theme.text_dim)
                    .child(SharedString::from(file.path.clone())),
            )
            .when(file.binary, |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(10.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from("BIN")),
                )
            })
            .when(adds > 0 || !file.binary, |el| {
                el.child(
                    div()
                        .flex_none()
                        .mono(theme)
                        .text_size(px(11.0))
                        .text_color(add_color(theme))
                        .child(SharedString::from(format!("+{adds}"))),
                )
            })
            .when(dels > 0 || !file.binary, |el| {
                el.child(
                    div()
                        .flex_none()
                        .mono(theme)
                        .text_size(px(11.0))
                        .text_color(del_color(theme))
                        .child(SharedString::from(format!("−{dels}"))),
                )
            })
            .into_any_element()
    }

    /// A small hover-washed icon button for the pane header. The header lives
    /// inside the titlebar drag strip, so the button occludes and swallows the
    /// mouse-down (same discipline as the shell's `header_icon_button`).
    fn header_button(
        id: &'static str,
        icon_path: &'static str,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .size(px(24.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .cursor_pointer()
            .bg(motion::hover_blend(
                id,
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.14),
            ))
            .on_hover(motion::hover_listener(id))
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                window.prevent_default()
            })
            .child(
                crate::kit::icons::icon(icon_path)
                    .size(px(14.0))
                    .text_color(theme.text_muted.opacity(0.7)),
            )
    }

    /// The pane-header controls: scope dropdown, `{branch} → {base ⌄}` ref
    /// selector (branch scope), fold-all. Rendered BY THE SHELL inside the
    /// session titlebar's trailing section (the band above the pane) — the
    /// titlebar overlay owns that strip's hit-testing, so controls mounted
    /// under it would never see a click. The expand and close buttons ride
    /// alongside, shell-owned (they mutate shell state).
    pub fn render_header_controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = crate::appearance::surface_style::theme(
            crate::appearance::surface_style::Region::Git,
            cx,
        );
        // Commit-pinned pane: the pin never changes, so a fixed identity
        // chip (mono short sha + subject) replaces the scope dropdown;
        // fold-all still trails.
        if let Some(commit) = self.commit.clone() {
            let short: String = commit.sha.chars().take(7).collect();
            return div()
                .size_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_none()
                        .h(px(22.0))
                        .px(px(6.0))
                        .rounded(px(5.0))
                        .flex()
                        .items_center()
                        .bg(crate::kit::theme::ink(0.05))
                        .mono(&theme)
                        .text_size(px(10.5))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(short)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(theme.text)
                        .child(SharedString::from(commit.subject.clone())),
                )
                .child(self.layout_picker(&theme, cx))
                .child(
                    Self::header_button(
                        "changes-fold-all",
                        crate::kit::icons::FOLD_VERTICAL,
                        &theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_collapse_all(cx);
                    })),
                )
                .into_any_element();
        }
        let scope = self.scope;
        let history_count = (scope == DiffScope::History).then(|| self.history_count(cx));
        let history_fetch_button =
            (scope == DiffScope::History).then(|| self.history_fetch_button(cx));
        let trigger = div()
            .id("changes-scope-trigger")
            .h(px(24.0))
            .px(px(8.0))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .bg(motion::hover_blend(
                "changes-scope-trigger",
                crate::kit::theme::wash(0.05),
                crate::kit::theme::wash(0.14),
            ))
            .on_hover(motion::hover_listener("changes-scope-trigger"))
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.scope_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                if this.scope_menu.take_press_was_open() {
                    this.close_scope_menu(cx);
                } else {
                    this.scope_menu.open(());
                }
                cx.notify();
            }))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text)
                    .child(SharedString::from(scope.label())),
            )
            .child(
                crate::kit::icons::icon(crate::kit::icons::ALT_ARROW_DOWN)
                    .size(px(12.0))
                    .text_color(theme.text_muted.opacity(0.7)),
            );
        let trigger = if self.scope_menu.get().is_some() {
            let closing = self.scope_menu.closing_since();
            let menu = self.render_scope_menu(&theme, cx);
            trigger.relative().child(popover::anchored_menu_below_gap(
                "changes-scope-menu",
                menu,
                closing,
                10.0,
            ))
        } else {
            trigger
        };

        let trailing: AnyElement = if scope == DiffScope::History {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(2.0))
                .children(history_fetch_button)
                .child(
                    Self::header_button("history-refresh", crate::kit::icons::REFRESH, &theme)
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.history_pane(cx)
                                .update(cx, |history, cx| history.refresh(cx));
                        })),
                )
                .into_any_element()
        } else {
            Self::header_button("changes-fold-all", crate::kit::icons::FOLD_VERTICAL, &theme)
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_collapse_all(cx);
                }))
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(trigger)
            .when_some(history_count, |element, count| {
                element.child(div().flex_1().min_w_0().h_full().child(count))
            })
            .children(self.render_ref_selector(&theme, cx))
            .when(scope != DiffScope::History, |element| {
                element
                    .child(div().flex_1())
                    .child(self.layout_picker(&theme, cx))
            })
            .child(trailing)
            .into_any_element()
    }

    fn render_scope_menu(&mut self, _theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let popup_theme = Theme::of(cx).clone();
        let theme = &popup_theme;
        let current = self.scope;
        popover::popover_card(theme)
            .w(px(180.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_scope_menu(cx)))
            .child(
                // The 2px row gap every other menu carries — rows straight on
                // the card abutted, adjacent washes read as one slab (user
                // report).
                div().flex().flex_col().gap(px(2.0)).children(
                    DiffScope::ALL.into_iter().enumerate().map(|(ix, scope)| {
                        popover::menu_row(
                            theme,
                            scope == current,
                            format!("changes-scope-row-{ix}"),
                        )
                        .id(("changes-scope-row", ix))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_scope(scope, cx);
                            this.close_scope_menu(cx);
                        }))
                        .child(div().flex_1().child(SharedString::from(scope.label())))
                    }),
                ),
            )
            .into_any_element()
    }

    /// `{branch} → {base ⌄}` — which ref the branch scope compares against
    /// (t3code's ref strip), inlined into the pane header. Branch scope only.
    fn render_ref_selector(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.scope != DiffScope::Branch {
            return None;
        }
        let branch = self
            .state
            .read(cx)
            .selected_chat_row()
            .and_then(|chat| chat.branch.clone())
            .unwrap_or_else(|| "HEAD".to_string());
        let base = self.base_ref.clone().unwrap_or_else(|| "…".to_string());
        // Even truncation: taffy shrinks flex items ∝ factor × basis, and the
        // default factor of 1 splits the deficit proportionally to content —
        // a long branch stayed near-whole while a short base ("main") read as
        // a bare ellipsis (user report). Weighting each side's factor by its
        // own length SQUARED (mono font, so chars ∝ px) lands the deficit
        // ~cubically on the longer name: the short side's loss stays
        // sub-pixel even under a big deficit (a linear weight still cost it
        // a char), while equal lengths still split evenly.
        let branch_weight = (branch.chars().count().max(1) as f32).powi(2);
        let base_weight = (base.chars().count().max(1) as f32).powi(2);
        let trigger = div()
            .id("changes-ref-trigger")
            .h(px(22.0))
            .px(px(6.0))
            // Shrinkable, like the branch label beside it — a flex_none
            // trigger with a long base name plowed over the header buttons
            // (user report); both sides truncate instead.
            .min_w_0()
            .flex_shrink(base_weight)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .bg(motion::hover_blend(
                "changes-ref-trigger",
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.12),
            ))
            .on_hover(motion::hover_listener("changes-ref-trigger"))
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.ref_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                if this.ref_menu.take_press_was_open() {
                    this.close_ref_menu(cx);
                    cx.notify();
                } else {
                    this.open_ref_menu(window, cx);
                }
            }))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .mono(theme)
                    .text_size(px(11.5))
                    .text_color(theme.text)
                    .child(SharedString::from(base)),
            )
            .child(
                crate::kit::icons::icon(crate::kit::icons::ALT_ARROW_DOWN)
                    .size(px(11.0))
                    .flex_none()
                    .text_color(theme.text_muted.opacity(0.7)),
            );
        let trigger = if self.ref_menu.get().is_some() {
            let closing = self.ref_menu.closing_since();
            let menu = self.render_ref_menu(theme, cx);
            trigger.relative().child(popover::anchored_menu_below_gap(
                "changes-ref-menu",
                menu,
                closing,
                10.0,
            ))
        } else {
            trigger
        };
        Some(
            div()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                // Extra room off the scope dropdown (row gap alone read
                // cramped — user report).
                .ml(px(6.0))
                .child(
                    div()
                        .min_w_0()
                        .flex_shrink(branch_weight)
                        .truncate()
                        .mono(theme)
                        .text_size(px(11.5))
                        .text_color(theme.text_dim)
                        .child(SharedString::from(branch)),
                )
                .child(
                    crate::kit::icons::icon(crate::kit::icons::ARROW_RIGHT)
                        .size(px(12.0))
                        .flex_none()
                        .text_color(theme.text_faint),
                )
                .child(trigger)
                .into_any_element(),
        )
    }

    fn render_ref_menu(&mut self, _theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let popup_theme = Theme::of(cx).clone();
        let theme = &popup_theme;
        let (search, active, focus, list_scroll) = {
            let Some(menu) = self.ref_menu.get() else {
                return div().into_any_element();
            };
            (
                menu.search.clone(),
                menu.active,
                menu.focus.clone(),
                menu.list_scroll.clone(),
            )
        };
        let rows = self.ref_menu_rows(cx);
        let current = self.base_ref.clone();
        let branches = self.branches.clone();

        let list: AnyElement = if rows.is_empty() {
            div()
                .px(px(8.0))
                .py(px(6.0))
                .text_size(px(12.0))
                .text_color(theme.text_faint)
                .child(SharedString::from(if branches.is_empty() {
                    "No branches"
                } else {
                    "No matching branches"
                }))
                .into_any_element()
        } else {
            div()
                .id("changes-ref-list")
                .flex()
                .flex_col()
                .gap(px(2.0))
                .max_h(px(240.0))
                .overflow_y_scroll()
                .track_scroll(&list_scroll)
                .children(rows.into_iter().enumerate().map(|(row_ix, branch_ix)| {
                    let name = branches[branch_ix].clone();
                    let selected = current.as_deref() == Some(name.as_str());
                    let label = name.clone();
                    popover::menu_row_nav(
                        theme,
                        selected,
                        row_ix == active,
                        format!("changes-ref-row-{row_ix}"),
                    )
                    .id(("changes-ref-row", row_ix))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_base_ref(name.clone(), cx);
                        this.close_ref_menu(cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .mono(theme)
                            .text_size(px(12.0))
                            .child(SharedString::from(label)),
                    )
                }))
                .into_any_element()
        };

        popover::popover_card(theme)
            .w(px(240.0))
            .track_focus(&focus)
            .on_key_down(
                cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| this.ref_menu_key(event, cx)),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_ref_menu(cx)))
            .flex()
            .flex_col()
            .child(popover::search_input_frame(
                theme,
                search.into_any_element(),
            ))
            .child(list)
            .into_any_element()
    }

    fn render_header_strip(&self, theme: &Theme) -> Option<AnyElement> {
        let parsed = self.parsed.as_ref()?;
        Some(
            div()
                .flex_none()
                .h(px(36.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .px(px(Theme::SPACE_LG))
                .border_b_1()
                .border_color(crate::kit::theme::hairline(0.06))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(scope_label(
                            self.scope,
                            parsed.file_count,
                            self.base_ref.as_deref(),
                        ))),
                )
                .child(
                    div()
                        .mono(theme)
                        .text_size(px(11.0))
                        .text_color(add_color(theme))
                        .child(SharedString::from(format!("+{}", parsed.additions))),
                )
                .child(
                    div()
                        .mono(theme)
                        .text_size(px(11.0))
                        .text_color(del_color(theme))
                        .child(SharedString::from(format!("−{}", parsed.deletions))),
                )
                .child(div().flex_1())
                .when(parsed.truncated, |el| {
                    el.child(
                        div()
                            .flex_none()
                            .text_size(px(10.0))
                            .px(px(6.0))
                            .py(px(2.0))
                            .rounded(px(4.0))
                            .bg(theme.warning.opacity(0.08))
                            .text_color(theme.warning.opacity(0.75))
                            .child(SharedString::from("Partial snapshot")),
                    )
                })
                .into_any_element(),
        )
    }
}

/// Green for additions — sampled from the reference diff (soft emerald).
fn add_color(theme: &Theme) -> gpui::Hsla {
    theme.diff_add // emerald-400
}

/// Red for deletions — softer than the theme danger, per the reference diff.
fn del_color(theme: &Theme) -> gpui::Hsla {
    theme.diff_del // red-400
}

/// One notice row ("New file", "Binary file — contents not shown", …).
fn notice_row(notice: String, theme: &Theme) -> AnyElement {
    div()
        .h(px(NOTICE_HEIGHT))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .px(px(Theme::SPACE_LG))
        .text_size(px(11.0))
        .text_color(theme.text_faint)
        .child(SharedString::from(notice))
        .into_any_element()
}

/// One `@@ … @@` hunk-header row on the bluish-grey wash.
fn hunk_header_row(header: &str, theme: &Theme) -> AnyElement {
    div()
        .h(px(HUNK_HEADER_HEIGHT))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .px(px(Theme::SPACE_LG))
        .bg(theme.diff_hunk_bg)
        .mono(theme)
        .text_size(px(11.0))
        .text_color(theme.text_faint)
        .child(SharedString::from(header.to_string()))
        .into_any_element()
}

/// Stable selectable-element key for one diff line: owner-scoped (multiple
/// diff panes never collide) and content-independent — file/hunk/line
/// indices are stable while the diff is current.
pub fn diff_line_key(owner: &str, file: u32, hunk: u32, line: u32) -> String {
    format!("{owner}:f{file}:h{hunk}:l{line}")
}

/// One +/−/context/meta diff line: coloured accent bar, dual line-number
/// gutters (`gutter_px` wide — see [`gutter_width`]), marker column, and
/// paint-only syntax runs. `select` (the Changes pane only) makes the CODE
/// TEXT selectable with a stable per-line key + the shared selection
/// callbacks — gutters, markers and headers stay inert; the transcript's
/// tool-diff renders pass `None`.
fn diff_line_row(
    line: &DiffLine,
    spans: &[cypher_syntax::HighlightSpan],
    theme: &Theme,
    gutter_px: f32,
    select: Option<(
        crate::markdown::selection::SelectionScope,
        &str,
        &markdown::render::SelectionUi,
    )>,
) -> AnyElement {
    if line.kind == LineKind::Meta {
        return div()
            .h(px(DIFF_LINE_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .pl(px(ACCENT_BAR_WIDTH + 2.0 * gutter_px + MARKER_WIDTH + 12.0))
            .text_size(px(10.5))
            .text_color(theme.text_faint)
            .italic()
            .child(SharedString::from(line.text.clone()))
            .into_any_element();
    }

    // Row tints sampled from the reference: ~5–6% washes over the pane tone.
    let mut add_bg = add_color(theme);
    add_bg.a = 0.055;
    let mut del_bg = del_color(theme);
    del_bg.a = 0.055;
    let add_bg = theme.regions.git_added.unwrap_or(add_bg);
    let del_bg = theme.regions.git_deleted.unwrap_or(del_bg);

    let (marker, marker_color, row_bg, accent, number_color) = match line.kind {
        LineKind::Add => (
            "+",
            add_color(theme),
            Some(add_bg),
            Some(add_color(theme).opacity(0.55)),
            add_color(theme).opacity(0.9),
        ),
        LineKind::Del => (
            "−",
            del_color(theme),
            Some(del_bg),
            Some(del_color(theme).opacity(0.55)),
            del_color(theme).opacity(0.9),
        ),
        _ => (
            "·",
            theme.text_faint.opacity(0.5),
            None,
            None,
            theme.text_faint.opacity(0.8),
        ),
    };
    let gutter = |no: Option<u32>, color: gpui::Hsla| {
        div()
            .w(px(gutter_px))
            .flex_none()
            .mono(theme)
            .text_size(px(11.0))
            .text_color(theme.regions.git_line_number.unwrap_or(color))
            .flex()
            .justify_end()
            .pr(px(8.0))
            .child(SharedString::from(
                no.map(|n| n.to_string()).unwrap_or_default(),
            ))
    };
    let mono = theme.mono();
    let runs = markdown::render::runs_for_syntax_line_with_plain(
        &line.text,
        spans,
        &mono,
        theme.text.opacity(0.92),
        theme,
    );
    div()
        .h(px(DIFF_LINE_HEIGHT))
        .w_full()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .when_some(row_bg, |el, bg| el.bg(bg))
        // Accent bar: solid colour on +/− rows, invisible spacer on
        // context rows so columns always align.
        .child(
            div()
                .w(px(ACCENT_BAR_WIDTH))
                .h_full()
                .flex_none()
                .when_some(accent, |el, color| el.bg(color)),
        )
        .child(gutter(
            line.old_no,
            if line.kind == LineKind::Del {
                number_color
            } else {
                theme.text_faint.opacity(0.8)
            },
        ))
        .child(gutter(
            line.new_no,
            if line.kind == LineKind::Add {
                number_color
            } else {
                theme.text_faint.opacity(0.8)
            },
        ))
        .child(
            div()
                .w(px(MARKER_WIDTH))
                .flex_none()
                .flex()
                .justify_center()
                .text_size(px(DIFF_TEXT_SIZE))
                .text_color(marker_color)
                .mono(theme)
                .child(SharedString::from(marker)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .pl(px(12.0))
                .mono(theme)
                .text_size(px(DIFF_TEXT_SIZE))
                .whitespace_nowrap()
                .child(diff_text_element(line, runs, theme, select)),
        )
        .into_any_element()
}

/// Settings preview uses the same rows, gutters, tinting and syntax renderer
/// as the real diff, but never loads files or issues engine RPCs.
pub fn color_preview(theme: &Theme) -> AnyElement {
    use cypher_syntax::{HighlightKind, HighlightSpan};
    let rows = [
        (
            LineKind::Context,
            "let name = \"Cypher\";",
            Some(1),
            Some(1),
        ),
        (LineKind::Del, "let answer = 41;", Some(2), None),
        (LineKind::Add, "let answer = 42;", None, Some(2)),
    ];
    div()
        .p(px(10.0))
        .rounded(px(8.0))
        .bg(theme.regions.git_background.unwrap_or(theme.surface))
        .flex()
        .flex_col()
        .children(rows.map(|(kind, text, old_no, new_no)| {
            diff_line_row(
                &DiffLine {
                    kind,
                    text: text.into(),
                    old_no,
                    new_no,
                },
                &[HighlightSpan {
                    range: 0..3,
                    kind: HighlightKind::Keyword,
                }],
                theme,
                32.0,
                None,
            )
        }))
        .into_any_element()
}

/// The diff line's code text. With `select` the element registers into the
/// Changes-scoped selection registry via a paint-phase underlay canvas
/// (same wash/listeners as the transcript — `markdown::render::
/// paint_text_selection`), so drags select across diff lines and Cmd+C joins
/// the quote. Without it (transcript tool-diff, fold tweens) the text is
/// plain.
pub(super) fn diff_text_element(
    line: &DiffLine,
    runs: Vec<gpui::TextRun>,
    theme: &Theme,
    select: Option<(
        crate::markdown::selection::SelectionScope,
        &str,
        &markdown::render::SelectionUi,
    )>,
) -> AnyElement {
    let Some((scope, key, selection)) = select else {
        return gpui::StyledText::new(line.text.clone())
            .with_runs(runs)
            .into_any_element();
    };
    let styled = gpui::StyledText::new(line.text.clone()).with_runs(runs);
    let layout = styled.layout().clone();
    let sel_key: std::sync::Arc<str> = key.into();
    let sel_text: SharedString = line.text.clone().into();
    let theme = theme.clone();
    let selection = selection.clone();
    let underlay = gpui::canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            markdown::render::paint_text_selection(
                window,
                scope,
                &sel_key,
                &sel_text,
                &layout,
                &theme,
                Some(selection),
            );
        },
    )
    .absolute()
    .size_full();
    div()
        .relative()
        .min_h(px(DIFF_LINE_HEIGHT))
        .child(underlay)
        .child(styled)
        .into_any_element()
}

/// The expanded body of one file section: notices, hunk headers, +/-/context
/// lines with a coloured accent bar, dual line-number gutters, a marker
/// column, and paint-only syntax runs (zeron checkout-diff-sidebar).
/// Shared with the transcript's tool-diff detail blocks — the same component
/// renders a checkout diff section and an inline tool diff. (The changes
/// pane itself virtualizes these rows individually; this stacked form serves
/// the transcript and the fold tween's clipped stand-in.)
/// Full-document old/new highlighting for tool and checkout diffs.
pub fn render_file_body_with_syntax(
    file: &FileDiff,
    highlights: Option<Arc<DiffHighlights>>,
    theme: &Theme,
) -> AnyElement {
    let mut children: Vec<AnyElement> = Vec::new();
    let gutter_px = gutter_width(file);
    for notice in file_notices(file) {
        children.push(notice_row(notice, theme));
    }
    for hunk in &file.hunks {
        children.push(hunk_header_row(&hunk.header, theme));
        for line in &hunk.lines {
            let spans = highlights
                .as_deref()
                .map(|highlights| highlights.spans(line))
                .unwrap_or(&[]);
            // Inert here: this stacked form serves the transcript's tool-diff
            // and the fold tween — only the Changes pane's virtualized rows
            // are selectable.
            children.push(diff_line_row(line, spans, theme, gutter_px, None));
        }
    }
    div()
        .flex()
        .flex_col()
        .pb(px(BODY_BOTTOM_PAD))
        .children(children)
        .into_any_element()
}

/// Build only rows that start above `max_px` so the fold tween's stand-in
/// never materializes lines its clip cannot reveal.
fn render_file_body_upto(
    file: &FileDiff,
    highlight: Option<Arc<DiffHighlights>>,
    theme: &Theme,
    max_px: f32,
) -> AnyElement {
    let mut children: Vec<AnyElement> = Vec::new();
    let mut y = 0.0f32;
    let gutter_px = gutter_width(file);

    'build: {
        for notice in file_notices(file) {
            if y >= max_px {
                break 'build;
            }
            children.push(notice_row(notice, theme));
            y += NOTICE_HEIGHT;
        }
        for hunk in &file.hunks {
            if y >= max_px {
                break 'build;
            }
            children.push(hunk_header_row(&hunk.header, theme));
            y += HUNK_HEADER_HEIGHT;
            for line in &hunk.lines {
                if y >= max_px {
                    break 'build;
                }
                let spans = highlight
                    .as_deref()
                    .map(|highlights| highlights.spans(line))
                    .unwrap_or(&[]);
                children.push(diff_line_row(line, spans, theme, gutter_px, None));
                y += DIFF_LINE_HEIGHT;
            }
        }
    }

    div()
        .flex()
        .flex_col()
        .pb(px(BODY_BOTTOM_PAD))
        .children(children)
        .into_any_element()
}

impl Render for Changes {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.scope == DiffScope::History {
            let history = self.history_pane(cx);
            history.update(cx, |history, cx| history.ensure_loaded(cx));
            return div().size_full().child(history).into_any_element();
        }
        let theme = crate::appearance::surface_style::theme(
            crate::appearance::surface_style::Region::Git,
            cx,
        );
        let active = self.active_diff(cx);
        let scope = self.scope;
        let base = self.base_ref.clone();
        // With no session selected (new-chat canvas) there is nothing to
        // prepare — show the quiet empty state, not an endless spinner.
        let no_chat = self.state.read(cx).selected_chat_row().is_none();
        let phase = if no_chat {
            DiffPhase::Clean
        } else {
            diff_phase(active.as_ref())
        };
        let error = self.error.clone();
        // Scoped fetch failures replace the content area. "no turn recorded"
        // is the expected pre-first-turn state, not an error; "unknown
        // method" is version skew — the chat's host engine predates
        // GetCheckoutDiff (a still-running daemon after an app update, or a
        // remote device behind on releases) — say that instead of leaking
        // the raw RPC error (user report).
        let scoped_notice: Option<(SharedString, bool)> = (!no_chat
            && scope != DiffScope::WorkingTree)
            .then(|| self.scoped_error.clone())
            .flatten()
            .map(|message| {
                if message.contains("no turn recorded") {
                    (
                        SharedString::from("No turn recorded yet — send a message first"),
                        false,
                    )
                } else if message.contains("unknown method") {
                    (
                        SharedString::from(
                            "This chat's device is running an older Cypher — update it to view branch and turn diffs",
                        ),
                        false,
                    )
                } else {
                    (message, true)
                }
            });

        let content: AnyElement = if let Some((message, warn)) = scoped_notice {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px(px(Theme::SPACE_LG))
                .text_size(px(12.0))
                .text_color(if warn {
                    theme.warning.opacity(0.85)
                } else {
                    theme.text_faint
                })
                .child(message)
                .into_any_element()
        } else {
            match phase {
                DiffPhase::Preparing => div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(Theme::SPACE_SM))
                    .child(crate::kit::loaders::gradient_spinner(
                        "changes-preparing",
                        &theme,
                        3.0,
                        cx.entity_id(),
                        cx,
                    ))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.text_faint)
                            .child(SharedString::from("Preparing diff…")),
                    )
                    .into_any_element(),
                DiffPhase::Clean => div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(clean_message(scope, base.as_deref())))
                    .into_any_element(),
                DiffPhase::List => {
                    if self.parsed.is_some() {
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .children(self.render_header_strip(&theme))
                            .when(self.view_layout == DiffLayout::Split, |el| {
                                el.child(self.split_headers(&theme, cx))
                            })
                            // The frame's CHANGES selection registry is
                            // cleared FIRST (before any diff line registers) —
                            // scoped so the transcript's own reset never
                            // touches it (paint order cannot conflict).
                            .child(
                                div()
                                    .relative()
                                    .flex_1()
                                    .min_h_0()
                                    .flex()
                                    .flex_col()
                                    .child(crate::markdown::render::selection_frame_reset(
                                        self.sel_scope,
                                    ))
                                    .child(crate::markdown::render::selection_frame_reset(
                                        self.split_scopes[0],
                                    ))
                                    .child(crate::markdown::render::selection_frame_reset(
                                        self.split_scopes[1],
                                    ))
                                    .child(
                                        list(self.list.clone(), cx.processor(Self::render_row))
                                            .flex_1()
                                            .with_sizing_behavior(gpui::ListSizingBehavior::Auto),
                                    ),
                            )
                            .into_any_element()
                    } else {
                        // Diff known, parse still running.
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(crate::kit::loaders::gradient_spinner(
                                "changes-parsing",
                                &theme,
                                3.0,
                                cx.entity_id(),
                                cx,
                            ))
                            .into_any_element()
                    }
                }
            }
        };

        let measure = self.pane_width.clone();
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(
                gpui::canvas(
                    move |bounds, _, _| measure.set(f32::from(bounds.size.width)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .when_some(self.layout_error.clone(), |el, error| {
                el.child(
                    div()
                        .px(px(12.0))
                        .py(px(4.0))
                        .text_size(px(11.0))
                        .text_color(theme.warning)
                        .child(error),
                )
            })
            .when_some(error, |el, message| {
                el.child(
                    div()
                        .flex_none()
                        .px(px(Theme::SPACE_MD))
                        .py(px(4.0))
                        .border_b_1()
                        .border_color(theme.border)
                        .text_size(px(11.0))
                        .text_color(theme.warning)
                        .child(message),
                )
            })
            .child(content)
            .into_any_element()
    }
}
