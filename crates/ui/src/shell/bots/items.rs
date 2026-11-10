//! Pieces the coordination cards and panels share (Needs you, Work,
//! Activity and the chat header's bot actions): item rows led by a mark,
//! section labels, text blocks, step and outcome marks, the progress bar,
//! the arm-then-confirm Stop, and the session a panel looks into.

use super::*;
pub(super) use crate::transcript::bot_plan::progress_bar;
use crate::transcript::bot_plan::{PlanPhase, plan_phase};
use cypher_proto::{BotWork, StepState};

/// How long an armed Stop waits for its confirming click.
const STOP_ARM_MS: u64 = 4000;

/// A session the detail panel looks into: a session context following its
/// transcript (an asking worker's question, an orchestration thread), and
/// for a thread the read-only transcript drawn from it.
pub(super) struct Peek {
    pub chat_id: String,
    pub state: Entity<crate::state::AppState>,
    pub transcript: Option<Entity<Transcript>>,
    _observe: Subscription,
}

/// An item row on a card or in a panel: the caller adds its mark, text and
/// trailing note. Selected while its item is open in the panel.
pub(super) fn item_row(key: &str, selected: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(key.to_string()))
        .px(px(8.0))
        .py(px(6.0))
        .rounded(px(8.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .cursor_pointer()
        .bg(if selected {
            crate::kit::theme::wash(0.08)
        } else {
            motion::hover_blend(
                key,
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.05),
            )
        })
        .on_hover(motion::hover_listener(key.to_string()))
}

/// The row's leading mark, centred in the glyph column the project rows
/// use.
pub(super) fn mark_cell(mark: impl IntoElement) -> gpui::Div {
    div()
        .flex_none()
        .w(px(14.0))
        .flex()
        .justify_center()
        .child(mark)
}

/// A glyph in its tint, at the rows' size.
pub(super) fn glyph(path: &'static str, tint: gpui::Hsla) -> gpui::Svg {
    icon(path).size(px(14.0)).flex_none().text_color(tint)
}

/// A row's right-hand note (a time, a count).
pub(super) fn trailing(text: String, color: gpui::Hsla) -> gpui::Div {
    div()
        .flex_none()
        .text_size(px(11.0))
        .text_color(color)
        .child(SharedString::from(text))
}

/// A panel section's small caps label ("STEPS", "RESULT").
pub(super) fn section_label(text: &str, theme: &Theme) -> gpui::Div {
    super::parts::section_label(theme, text)
}

/// Agent-written text in a panel (a request, a note, a result) as markdown
/// at the panel's reading size: code, emphasis, lists and session links
/// drawn as the chat draws them. `size` is the body size (the line height
/// follows); `tone` colours the body text.
pub(super) fn panel_markdown(
    text: &str,
    key: impl Into<SharedString>,
    size: f32,
    tone: gpui::Hsla,
    theme: &Theme,
    window: &Window,
) -> AnyElement {
    let mut compact = theme.clone();
    compact.markdown = crate::kit::theme::MarkdownMetrics {
        body_size: size,
        body_line_height: (size * 1.44).round(),
        code_size: size - 1.0,
        code_line_height: (size * 1.3).round(),
        block_gap: 6.0,
    };
    compact.text = tone;
    let mut opts = crate::markdown::render::RenderOptions::settled(key.into());
    opts.justify = true;
    crate::markdown::render::render_tree(
        &crate::markdown::parser::parse_full(text),
        &opts,
        &compact,
        window,
        &|_| None,
    )
}

/// Markdown an agent wrote, read as plain text for a panel or a row: a
/// link keeps its text (a cited `cypher://session/…` reads as the session's
/// title), emphasis, code ticks and heading marks drop, a table row reads
/// as its cells joined by " · " (its rule line goes); lines and paragraphs
/// stay.
pub(super) fn plain_markdown(text: &str) -> String {
    let lines: Vec<String> = text
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with('|') {
                if trimmed.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                    return None;
                }
                let cells: Vec<&str> = trimmed
                    .trim_matches('|')
                    .split('|')
                    .map(str::trim)
                    .filter(|cell| !cell.is_empty())
                    .collect();
                return Some(cells.join(" · "));
            }
            Some(trimmed.trim_start_matches('#').trim_start().to_string())
        })
        .collect();
    let text = lines.join("\n");
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find("](").and_then(|close| {
            let url = &after[close + 2..];
            url.find(')').map(|end| (close, close + 2 + end + 1))
        }) {
            Some((close, skip)) if !after[..close].contains('\n') => {
                out.push_str(&after[..close]);
                rest = &after[skip..];
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "").replace('`', "")
}

/// Epoch milliseconds as a time.
pub(super) fn at(ms: i64) -> chrono::DateTime<Utc> {
    chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default()
}

/// "5m", "2h": how long ago `ms` was.
pub(super) fn ago(ms: i64, now: chrono::DateTime<Utc>) -> String {
    format_time_ago(at(ms), now)
}

/// A step's state mark at the rows' size.
pub(super) fn step_mark(state: StepState, ticker: gpui::EntityId, theme: &Theme) -> AnyElement {
    let glyph_mark = |path, color| {
        icon(path)
            .size(px(12.0))
            .text_color(color)
            .into_any_element()
    };
    match state {
        StepState::Pending => div()
            .size(px(9.0))
            .rounded_full()
            .border_1()
            .border_color(theme.text_faint.opacity(0.8))
            .into_any_element(),
        StepState::Running => {
            super::super::render::status_mark(ChatIndicator::Working, ticker, theme)
                .unwrap_or_else(|| gpui::Empty.into_any_element())
        }
        StepState::Asking => glyph_mark(icons::QUESTION_CIRCLE, theme.warning),
        StepState::Done => glyph_mark(icons::CHECK, theme.success),
        StepState::Failed => glyph_mark(icons::CLOSE_CIRCLE, theme.danger),
        StepState::Skipped => div()
            .w(px(8.0))
            .h(px(1.5))
            .rounded_full()
            .bg(theme.text_faint)
            .into_any_element(),
    }
}

/// A work item's mark: a spinner while it runs, then how it ended.
pub(super) fn work_mark(work: &BotWork, ticker: gpui::EntityId, theme: &Theme) -> AnyElement {
    let glyph_mark = |path, color| {
        icon(path)
            .size(px(12.0))
            .text_color(color)
            .into_any_element()
    };
    match plan_phase(work) {
        PlanPhase::Running | PlanPhase::Starting | PlanPhase::Stopping => {
            super::super::render::status_mark(ChatIndicator::Working, ticker, theme)
                .unwrap_or_else(|| gpui::Empty.into_any_element())
        }
        PlanPhase::Awaiting => glyph_mark(icons::CHECKLIST, theme.warning),
        PlanPhase::Done => glyph_mark(icons::CHECK, theme.success),
        PlanPhase::Failed => glyph_mark(icons::CLOSE_CIRCLE, theme.danger),
        PlanPhase::Stopped | PlanPhase::Cancelled | PlanPhase::Cancelling => div()
            .size(px(8.0))
            .rounded(px(2.0))
            .bg(theme.text_faint)
            .into_any_element(),
    }
}

/// A session's status mark, with a quiet dot where the sidebar shows none.
pub(super) fn session_mark(
    status: ChatIndicator,
    ticker: gpui::EntityId,
    theme: &Theme,
) -> AnyElement {
    super::super::render::status_mark(status, ticker, theme).unwrap_or_else(|| {
        div()
            .flex_none()
            .size(px(5.0))
            .rounded_full()
            .bg(if status == ChatIndicator::Errored {
                theme.danger
            } else {
                theme.text_faint.opacity(0.6)
            })
            .into_any_element()
    })
}

/// A square icon button (the chat header's bot actions): a hover wash and a
/// tooltip.
pub(super) fn icon_button(
    key: &'static str,
    path: &'static str,
    tip: &'static str,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(key)
        .flex_none()
        .size(px(28.0))
        .rounded(px(8.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(motion::hover_blend(
            key,
            crate::kit::theme::wash(0.0),
            crate::kit::theme::wash(0.14),
        ))
        .on_hover(motion::hover_listener(key))
        .tooltip(move |_, cx| {
            cx.new(|_| super::super::session::FindTooltip(tip.into()))
                .into()
        })
        .child(icon(path).size(px(14.0)).text_color(motion::hover_blend(
            key,
            theme.text_muted.opacity(0.8),
            theme.text,
        )))
}

/// A project as a step names it: glyph, tint, name and device.
pub(super) struct ProjectLabel {
    pub glyph: &'static str,
    pub tint: gpui::Hsla,
    pub name: String,
    pub device: String,
}

pub(super) fn project_label(
    state: &crate::state::AppState,
    space_id: &str,
    theme: &Theme,
) -> Option<ProjectLabel> {
    let space = state.space_row(space_id)?;
    Some(ProjectLabel {
        glyph: crate::appearance::space_style::space_icon(space.icon.as_deref()),
        tint: crate::appearance::space_style::space_color(space.color.as_deref(), theme)
            .unwrap_or(theme.text_muted),
        name: space.display_name().to_string(),
        device: state
            .device_name(&space.device_id)
            .unwrap_or("Unknown device")
            .to_string(),
    })
}

/// A status as a fact's value: its mark, then its words in its colour.
pub(super) fn status_value(
    mark: AnyElement,
    text: impl Into<SharedString>,
    color: gpui::Hsla,
) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .child(div().flex_none().child(mark))
        .child(div().min_w_0().text_color(color).child(text.into()))
}

/// A mark for a status line: a glyph at the facts' size.
pub(super) fn status_glyph(path: &'static str, color: gpui::Hsla) -> AnyElement {
    icon(path)
        .size(px(12.0))
        .text_color(color)
        .into_any_element()
}

/// How a decision ended, as a status value: a check when it went through.
pub(super) fn outcome_value(text: &str, ok: bool, theme: &Theme) -> gpui::Div {
    let (path, color) = if ok {
        (icons::CHECK, theme.success)
    } else {
        (icons::CLOSE_CIRCLE, theme.text_muted)
    };
    status_value(status_glyph(path, color), text.to_string(), theme.text)
}

/// A long fact (a request, a prompt, a result): the text an agent or the
/// owner wrote, read as plain text, justified like the panel's prose.
pub(super) fn text_value(text: &str) -> crate::markdown::justified::JustifiedText {
    crate::markdown::justified::JustifiedText::new(plain_markdown(text), None)
}

/// The pinned footer of a panel that asks for a decision: a quiet note on
/// the left, then the caller's buttons, the primary one last.
pub(super) fn decision_bar(note: Option<&str>, theme: &Theme) -> gpui::Div {
    div().flex().flex_row().items_center().gap(px(6.0)).child(
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_size(px(11.5))
            .text_color(theme.text_muted)
            .children(note.map(|n| SharedString::from(n.to_string()))),
    )
}

impl Shell {
    /// The open panel item's own buttons, drawn in the panel header before
    /// its close button (icon buttons: edit, delete, open its session…).
    pub(super) fn set_panel_actions(&mut self, buttons: Vec<AnyElement>) {
        if buttons.is_empty() {
            return;
        }
        self.board.panel_actions = Some(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .children(buttons)
                .into_any_element(),
        );
    }

    /// The open panel item's decision bar, pinned under its body.
    pub(super) fn set_panel_footer(&mut self, footer: impl IntoElement) {
        self.board.panel_footer = Some(footer.into_any_element());
    }

    /// The session context following `chat_id` for the panel, made on first
    /// use (with the read-only transcript when `transcript`).
    pub(super) fn peek(
        &mut self,
        chat_id: &str,
        transcript: bool,
        cx: &mut Context<Self>,
    ) -> Entity<crate::state::AppState> {
        if self
            .board
            .peek
            .as_ref()
            .is_none_or(|p| p.chat_id != chat_id)
        {
            let state = crate::state::AppState::new_session_context(
                &self.state,
                Some(chat_id.to_string()),
                cx,
            );
            let observe = cx.observe(&state, |_, _, cx| cx.notify());
            self.board.peek = Some(Peek {
                chat_id: chat_id.to_string(),
                state,
                transcript: None,
                _observe: observe,
            });
        }
        let state = self
            .board
            .peek
            .as_ref()
            .map(|p| p.state.clone())
            .expect("peek was just made");
        if transcript
            && self
                .board
                .peek
                .as_ref()
                .is_some_and(|p| p.transcript.is_none())
        {
            let view = cx.new(|cx| Transcript::for_side_chat(state.clone(), cx));
            if let Some(peek) = self.board.peek.as_mut() {
                peek.transcript = Some(view);
            }
        }
        state
    }

    /// The Stop button of a running work item: the first click arms it, a
    /// second within [`STOP_ARM_MS`] stops the work and its sessions.
    pub(super) fn stop_button(
        &self,
        key: &str,
        work_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let armed = self.board.stop_armed.as_deref() == Some(work_id);
        let work_id = work_id.to_string();
        div()
            .id(SharedString::from(key.to_string()))
            .px(px(10.0))
            .py(px(5.0))
            .rounded(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(7.0))
            .cursor_pointer()
            .text_size(px(13.0))
            .when(armed, |el| {
                el.bg(theme.danger.opacity(0.12)).text_color(theme.danger)
            })
            .when(!armed, |el| {
                el.text_color(motion::hover_blend(key, theme.text_muted, theme.text))
                    .bg(motion::hover_blend(
                        key,
                        crate::kit::theme::wash(0.0),
                        crate::kit::theme::ink(0.06),
                    ))
            })
            .on_hover(motion::hover_listener(key.to_string()))
            .on_click(cx.listener(move |this, _, _, cx| this.arm_or_stop(work_id.clone(), cx)))
            .child(div().size(px(8.0)).rounded(px(2.0)).bg(if armed {
                theme.danger
            } else {
                theme.text_muted
            }))
            .child(SharedString::from(if armed {
                "Stop the work and its sessions?"
            } else {
                "Stop"
            }))
    }

    fn arm_or_stop(&mut self, work_id: String, cx: &mut Context<Self>) {
        if self.board.stop_armed.as_deref() == Some(work_id.as_str()) {
            self.board.stop_armed = None;
            self.board.stop_disarm = None;
            self.mutate(
                serde_json::json!({ "op": "stopBotWork", "workId": work_id }),
                cx,
            );
            cx.notify();
            return;
        }
        self.board.stop_armed = Some(work_id.clone());
        self.board.stop_disarm = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(STOP_ARM_MS))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.board.stop_armed.as_deref() == Some(work_id.as_str()) {
                    this.board.stop_armed = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }
}

/// A section heading on a panel page (Request, Plan, Sessions…): small
/// caps, its content below; a row, so a section can add a note on the
/// right. `first` takes less air above it.
pub(super) fn section_head(label: &str, first: bool, theme: &Theme) -> gpui::Div {
    div()
        .px(px(8.0))
        .pt(px(if first { 4.0 } else { 14.0 }))
        .pb(px(8.0))
        .min_h(px(20.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_none()
                .text_size(px(11.0))
                .line_height(px(14.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text_muted)
                .child(SharedString::from(label.to_uppercase())),
        )
}

/// A section heading's note on its right (a count, a place).
pub(super) fn head_note(text: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
    div()
        .min_w_0()
        .truncate()
        .text_size(px(11.0))
        .text_color(theme.text_muted.opacity(0.85))
        .child(text.into())
}

/// The hairline that closes one section of a panel page before the next.
pub(super) fn section_rule() -> gpui::Div {
    div()
        .mx(px(8.0))
        .mt(px(16.0))
        .h(px(1.0))
        .bg(crate::kit::theme::hairline(0.08))
}

/// A row of a panel list that washes on hover: rounded, its content in a
/// line. Rows sit 2px in from the section's edge, so text inside meets
/// the headings' 8px.
pub(super) fn hover_row(key: &str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(key.to_string()))
        .mx(px(2.0))
        .px(px(6.0))
        .py(px(7.0))
        .rounded(px(10.0))
        .flex()
        .flex_row()
        .items_start()
        .gap(px(12.0))
        .bg(motion::hover_blend(
            key,
            crate::kit::theme::wash(0.0),
            crate::kit::theme::wash(0.04),
        ))
        .on_hover(motion::hover_listener(key.to_string()))
}

/// The arrow at the end of a row that opens something: it fades in and
/// slides in from the left while `key` is hovered.
pub(super) fn arrow_in(key: &str, theme: &Theme) -> gpui::Div {
    let t = motion::hover_t(key);
    div().flex_none().ml(px(-4.0 * (1.0 - t))).child(
        icon(icons::ALT_ARROW_RIGHT)
            .size(px(10.0))
            .text_color(theme.text_faint.opacity(t)),
    )
}

/// A session's state in a word, beside its row; none while it sits idle.
pub(super) fn session_word(
    status: ChatIndicator,
    theme: &Theme,
) -> Option<(&'static str, gpui::Hsla)> {
    match status {
        ChatIndicator::Working => Some(("Working", theme.text_muted)),
        ChatIndicator::AwaitingInput => Some(("Needs input", theme.warning)),
        ChatIndicator::Errored => Some(("Failed", theme.danger)),
        ChatIndicator::Completed => Some(("Done", theme.text_faint)),
        ChatIndicator::Idle => None,
    }
}

impl Shell {
    /// A Copy button that copies `text`, then says Copied for a moment.
    /// `reveal` (0..1) is how shown it is at rest: a row's hover, so it
    /// appears with its row; it stays shown while it says Copied.
    pub(super) fn copy_button(
        &self,
        key: String,
        text: String,
        reveal: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let copied = self.board.copied.as_deref() == Some(key.as_str());
        let ink = if copied {
            theme.success
        } else {
            motion::hover_blend(&key, theme.text_muted, theme.text)
        };
        let id = key.clone();
        div()
            .id(SharedString::from(key.clone()))
            .flex_none()
            .h(px(20.0))
            .px(px(6.0))
            .rounded(px(6.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .cursor_pointer()
            .opacity(if copied { 1.0 } else { reveal })
            .bg(motion::hover_blend(
                &key,
                crate::kit::theme::wash(0.0),
                crate::kit::theme::wash(0.08),
            ))
            .on_hover(motion::hover_listener(key.clone()))
            .on_click(
                cx.listener(move |this, _, _, cx| this.copy_text(id.clone(), text.clone(), cx)),
            )
            .text_size(px(11.0))
            .text_color(ink)
            .child(
                icon(if copied { icons::CHECK } else { icons::COPY })
                    .size(px(11.0))
                    .text_color(ink),
            )
            .child(if copied { "Copied" } else { "Copy" })
    }

    /// A page reached from another panel (a connection from Network):
    /// a back arrow in its icon's place, returning to `to`.
    pub(super) fn set_panel_back(
        &mut self,
        key: &'static str,
        tip: &'static str,
        to: PanelItem,
        cx: &mut Context<Self>,
    ) {
        let theme = Theme::of(cx).clone();
        self.board.panel_mark = Some(
            div()
                .id(key)
                .flex_none()
                .size(px(30.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .bg(motion::hover_blend(
                    key,
                    crate::kit::theme::ink(0.06),
                    crate::kit::theme::ink(0.11),
                ))
                .on_hover(motion::hover_listener(key))
                .tooltip(move |_, cx| {
                    cx.new(|_| super::super::session::FindTooltip(tip.into()))
                        .into()
                })
                .on_click(cx.listener(move |this, _, _, cx| this.open_panel(to.clone(), cx)))
                .child(
                    icon(icons::ARROW_LEFT)
                        .size(px(15.0))
                        .text_color(motion::hover_blend(key, theme.text_muted, theme.text)),
                )
                .into_any_element(),
        );
    }

    /// Put `text` on the clipboard; the button `key` says Copied for a
    /// moment.
    pub(super) fn copy_text(&mut self, key: String, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.board.copied = Some(key.clone());
        self.board.copied_reset = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1600))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.board.copied.as_deref() == Some(key.as_str()) {
                    this.board.copied = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_reads_plainly() {
        assert_eq!(
            plain_markdown(
                "Fixed in [Fix rounding](cypher://session/c1); ran `make test` **twice**.\n\nSee [x]"
            ),
            "Fixed in Fix rounding; ran make test twice.\n\nSee [x]"
        );
        assert_eq!(
            plain_markdown("## Result\n| Tests | Result |\n|---|:--:|\n| 2 | OK |"),
            "Result\nTests · Result\n2 · OK"
        );
    }
}
