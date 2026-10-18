//! Pieces the Schedule, Notebook and Network cards and panels share: the
//! one-line card a section draws while it is empty, panel sections and
//! key/value lines, form fields, switch rows and segmented choices, the
//! confirm dialog, engine calls with an error path, and the time labels.

use super::panel::PanelItem;
use super::*;
use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};

/// Rows a card lists before its header's panel takes over.
pub(super) const CARD_ROWS: usize = 4;

/// What the confirm dialog asks before it does it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Confirm {
    DeleteRoutine { id: String, name: String },
    DeleteWork { id: String, name: String },
    RevokeConnection { id: String, name: String },
}

/// A card for a section with nothing in it yet: its header alone, with a
/// quiet hint where the count would be. The header opens the section's
/// panel like any card's, and the panel offers the way to add the first.
#[allow(clippy::too_many_arguments)]
pub(super) fn empty_card(
    key: &'static str,
    glyph: &'static str,
    title: &'static str,
    hint: &'static str,
    open: PanelItem,
    focused: bool,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let header = super::board::header_frame(key, glyph, title, focused, None, theme)
        .cursor_pointer()
        .on_hover(motion::hover_listener(key))
        .on_click(cx.listener(move |this, _, _, cx| this.open_panel(open.clone(), cx)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(11.5))
                .text_color(theme.text_muted.opacity(0.6))
                .child(SharedString::from(hint)),
        );
    super::board::card(theme).child(header).into_any_element()
}

/// A card row's right-hand note.
pub(super) fn row_note(text: impl Into<SharedString>, color: gpui::Hsla) -> gpui::Div {
    div()
        .flex_none()
        .text_size(px(11.0))
        .text_color(color)
        .child(text.into())
}

/// A panel's small upper-case section label ("ROUTINES").
pub(super) fn section_label(theme: &Theme, text: &str) -> gpui::Div {
    div()
        .px(px(8.0))
        .pt(px(12.0))
        .pb(px(6.0))
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text_muted.opacity(0.75))
        .child(SharedString::from(text.to_uppercase()))
}

/// A quiet line of panel copy (an empty list, a hint).
pub(super) fn quiet_line(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .px(px(8.0))
        .py(px(6.0))
        .text_size(px(12.5))
        .line_height(px(18.0))
        .text_color(theme.text_muted)
        .child(text.into())
}

/// One fact in a panel: a muted label column, then the value.
pub(super) fn fact(theme: &Theme, label: &str, value: impl IntoElement) -> gpui::Div {
    div()
        .px(px(8.0))
        .py(px(5.0))
        .flex()
        .flex_row()
        .items_start()
        .gap(px(12.0))
        .child(
            div()
                .w(px(78.0))
                .flex_none()
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(12.5))
                .line_height(px(18.0))
                .text_color(theme.text)
                .child(value),
        )
}

/// The bordered group switch rows sit in (the palette's Permissions box).
pub(super) fn group_box() -> gpui::Div {
    div()
        .mx(px(8.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(crate::kit::theme::hairline(0.06))
        .bg(crate::kit::theme::ink(0.02))
        .flex()
        .flex_col()
}

/// A switch row: title over detail, the switch on the right. The caller
/// adds the click. `first` drops the hairline above it.
pub(super) fn switch_row(
    id: impl Into<SharedString>,
    title: &str,
    detail: &str,
    on: bool,
    first: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id.into())
        .when(!first, |el| {
            el.border_t_1()
                .border_color(crate::kit::theme::hairline(0.06))
        })
        .px(px(12.0))
        .py(px(9.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .cursor_pointer()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .child(SharedString::from(title.to_string())),
                )
                .when(!detail.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(detail.to_string())),
                    )
                }),
        )
        .child(crate::settings::widgets::toggle_switch(theme, on))
}

/// A fixed grant in a [`group_box`], laid out like a [`switch_row`] with a
/// check where the switch would be.
pub(super) fn grant_row(theme: &Theme, title: &str, detail: &str, first: bool) -> gpui::Div {
    div()
        .when(!first, |el| {
            el.border_t_1()
                .border_color(crate::kit::theme::hairline(0.06))
        })
        .px(px(12.0))
        .py(px(9.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .child(SharedString::from(title.to_string())),
                )
                .when(!detail.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(11.5))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(detail.to_string())),
                    )
                }),
        )
        .child(
            div().w(px(32.0)).flex_none().flex().justify_center().child(
                icon(icons::CHECK)
                    .size(px(13.0))
                    .text_color(theme.text_muted),
            ),
        )
}

/// A segmented choice (the palette's Selected / All / None): `options` as
/// `(value, label)`, the current one lifted. `pick` applies a click.
pub(super) fn segmented<T: Copy + PartialEq + 'static>(
    key: &str,
    options: &[(T, &str)],
    current: T,
    pick: fn(&mut Shell, T, &mut Context<Shell>),
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> gpui::Div {
    div()
        .p(px(2.0))
        .rounded(px(8.0))
        .bg(crate::kit::theme::ink(0.05))
        .flex()
        .flex_row()
        .gap(px(2.0))
        .children(options.iter().enumerate().map(|(ix, &(value, label))| {
            let on = value == current;
            div()
                .id(SharedString::from(format!("{key}-{ix}")))
                .flex_1()
                .px(px(10.0))
                .py(px(3.0))
                .rounded(px(6.0))
                .flex()
                .justify_center()
                .text_size(px(12.0))
                .cursor_pointer()
                .text_color(if on { theme.text } else { theme.text_muted })
                .when(on, |el| {
                    el.bg(crate::kit::theme::card_selected_bg())
                        .shadow(crate::kit::theme::card_selected_shadows())
                        .font_weight(gpui::FontWeight::MEDIUM)
                })
                .when(!on, |el| el.hover(|s| s.text_color(theme.text)))
                .on_click(cx.listener(move |this, _, _, cx| pick(this, value, cx)))
                .child(SharedString::from(label.to_string()))
        }))
}

/// A form field's label.
pub(super) fn field_label(theme: &Theme, text: &str) -> gpui::Div {
    div()
        .px(px(8.0))
        .pt(px(12.0))
        .pb(px(5.0))
        .text_size(px(11.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text_muted)
        .child(SharedString::from(text.to_string()))
}

/// A text input's frame (the dialogs' field, at the panel's size).
pub(super) fn field(input: &Entity<TextInput>) -> gpui::Div {
    div()
        .mx(px(8.0))
        .px(px(10.0))
        .py(px(6.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(crate::kit::theme::hairline(0.08))
        .bg(crate::kit::theme::ink(0.04))
        .text_size(px(13.0))
        .child(input.clone().into_any_element())
}

/// A quiet action: glyph and label, a wash on hover. The caller adds the
/// click.
pub(super) fn action(
    key: impl Into<SharedString>,
    glyph: &'static str,
    label: &str,
    color: gpui::Hsla,
) -> gpui::Stateful<gpui::Div> {
    let key = key.into();
    div()
        .id(key.clone())
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(9.0))
        .py(px(5.0))
        .rounded(px(7.0))
        .text_size(px(12.5))
        .text_color(color)
        .cursor_pointer()
        .bg(motion::hover_blend(
            &key,
            crate::kit::theme::wash(0.0),
            crate::kit::theme::wash(0.07),
        ))
        .on_hover(motion::hover_listener(key.clone()))
        .child(icon(glyph).size(px(13.0)).text_color(color))
        .child(SharedString::from(label.to_string()))
}

/// Inline link text that underlines on hover. The caller adds the click.
pub(super) fn link(
    key: impl Into<SharedString>,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(key.into())
        .flex_none()
        .text_color(theme.text_muted)
        .cursor_pointer()
        .hover(|s| s.text_color(theme.text).underline())
        .child(label.into())
}

/// A form's footer: the error on the left, then Cancel and the primary
/// button. The caller wires the two buttons by their ids.
pub(super) fn form_footer(
    theme: &Theme,
    key: &str,
    error: Option<SharedString>,
    primary: &str,
    busy: bool,
) -> (
    gpui::Div,
    gpui::Stateful<gpui::Div>,
    gpui::Stateful<gpui::Div>,
) {
    let cancel = popover::btn_ghost(theme, "Cancel", format!("{key}-cancel"))
        .id(SharedString::from(format!("{key}-cancel")));
    let save = popover::btn_primary(theme, primary)
        .id(SharedString::from(format!("{key}-save")))
        .py(px(5.0))
        .when(busy, |el| el.opacity(0.6));
    let row = div()
        .px(px(8.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(11.5))
                .line_height(px(16.0))
                .text_color(theme.danger)
                .children(error),
        );
    (row, cancel, save)
}

impl Shell {
    /// Call `method` on the engine; `done` gets the reply or the error's
    /// text. With no engine, `done` hears so at once.
    pub(super) fn board_call(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Shell, Result<serde_json::Value, CallError>, &mut Context<Shell>)
        + 'static,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return done(
                self,
                Err(CallError::Failed("Engine not connected".into())),
                cx,
            );
        };
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(method, params)
                .await
                .map_err(CallError::from);
            this.update(cx, |this, cx| {
                done(this, result, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A Mutate op whose failure shows in the sidebar notice, like
    /// [`Shell::mutate`], without taking its task slot (a quick second
    /// action must not cancel the first).
    pub(super) fn board_mutate(&mut self, params: serde_json::Value, cx: &mut Context<Self>) {
        self.board_call(methods::MUTATE, params, cx, |this, result, cx| {
            if let Err(err) = result {
                this.sidebar.notice = Some(err.to_string().into());
                cx.notify();
            }
        });
    }

    /// Open `item`, leaving it open when it already is (unlike a row's
    /// second click).
    pub(super) fn show_panel(&mut self, item: PanelItem, cx: &mut Context<Self>) {
        if self.board.panel.as_ref() != Some(&item) {
            self.open_panel(item, cx);
        }
    }

    pub(super) fn ask_confirm(&mut self, confirm: Confirm, cx: &mut Context<Self>) {
        self.board.confirm = Some(confirm);
        cx.notify();
    }

    fn run_confirm(&mut self, confirm: Confirm, cx: &mut Context<Self>) {
        self.board.confirm = None;
        match confirm {
            Confirm::DeleteRoutine { id, .. } => {
                self.board_mutate(
                    serde_json::json!({ "op": "deleteBotRoutine", "routineId": id }),
                    cx,
                );
                self.open_panel(PanelItem::Schedule, cx);
            }
            Confirm::DeleteWork { id, .. } => {
                self.board_mutate(
                    serde_json::json!({ "op": "deleteBotWork", "workId": id }),
                    cx,
                );
                self.open_panel(PanelItem::Work, cx);
            }
            Confirm::RevokeConnection { id, .. } => {
                self.board_mutate(
                    serde_json::json!({ "op": "revokeBotConnection", "connectionId": id }),
                    cx,
                );
            }
        }
        cx.notify();
    }

    /// The confirm dialog (the delete-bot dialog's pattern).
    pub(super) fn render_board_confirm(
        &self,
        viewport: gpui::Size<Pixels>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let confirm = self.board.confirm.clone()?;
        let (title, body, verb) = match &confirm {
            Confirm::DeleteRoutine { name, .. } => (
                "Delete routine?",
                format!(
                    "\u{201C}{name}\u{201D} stops running and is deleted with its \
                     sessions. Results it already posted stay in the chat."
                ),
                "Delete",
            ),
            Confirm::DeleteWork { name, .. } => (
                "Delete work?",
                format!(
                    "\u{201C}{name}\u{201D} and its background session are deleted. \
                     Sessions it started in your projects stay."
                ),
                "Delete",
            ),
            Confirm::RevokeConnection { name, .. } => (
                "Revoke connection?",
                format!(
                    "{name} can no longer ask this workspace anything, and this \
                     workspace can no longer ask it. Connecting again takes a new invite."
                ),
                "Revoke",
            ),
        };
        let card = popover::dialog_card(theme)
            .child(popover::dialog_title(theme, title))
            .child(div().mt(px(6.0)).child(popover::dialog_body(theme, body)))
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(theme, "Cancel", "board-confirm-cancel")
                            .id("board-confirm-cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.board.confirm = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_danger(theme, verb)
                            .id("board-confirm-ok")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.run_confirm(confirm.clone(), cx)
                            })),
                    ),
            )
            .into_any_element();
        Some(popover::modal("board-confirm-dialog", viewport, card))
    }
}

/// An engine call's failure, told apart where the UI says something
/// different for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CallError {
    /// The engine does not know the method (an older or unfinished host).
    Unknown,
    Failed(String),
}

impl From<cypher_rpc::RpcError> for CallError {
    fn from(err: cypher_rpc::RpcError) -> Self {
        match err {
            cypher_rpc::RpcError::UnknownMethod(_) => Self::Unknown,
            other => {
                let text = other.to_string();
                if text.to_ascii_lowercase().contains("unknown method") {
                    Self::Unknown
                } else {
                    Self::Failed(clean_error(&text))
                }
            }
        }
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("Not available on this device yet."),
            Self::Failed(text) => f.write_str(text),
        }
    }
}

/// An error as a sentence: without the RPC layer's "bad params: " lead.
fn clean_error(text: &str) -> String {
    let text = text
        .strip_prefix("bad params: ")
        .or_else(|| text.strip_prefix("transport: "))
        .unwrap_or(text)
        .trim();
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Something went wrong.".into(),
    }
}

// ── time labels ─────────────────────────────────────────────────────────────

pub(super) fn local(ms: i64) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(ms).single()
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

fn hm<Tz: TimeZone>(at: &DateTime<Tz>) -> String {
    format!("{:02}:{:02}", at.hour(), at.minute())
}

fn day_month<Tz: TimeZone>(at: &DateTime<Tz>) -> String {
    format!("{} {}", MONTHS[at.month0() as usize], at.day())
}

/// When something next happens, as a card's right-hand note: "in 25m",
/// "in 3h", "21:00" (later today), "Mon" (within a week), else "Oct 30".
/// A bare time is always today's, so it never reads as tomorrow's.
pub(super) fn next_label<Tz: TimeZone>(at: &DateTime<Tz>, now: &DateTime<Tz>) -> String {
    let minutes = (at.clone() - now.clone()).num_minutes();
    if minutes < 1 {
        return "now".into();
    }
    if minutes < 60 {
        return format!("in {minutes}m");
    }
    if minutes < 12 * 60 {
        return format!("in {}h", minutes / 60);
    }
    let days = (at.date_naive() - now.date_naive()).num_days();
    if days <= 0 {
        return hm(at);
    }
    if days < 7 {
        return WEEKDAYS[at.weekday().num_days_from_monday() as usize].into();
    }
    day_month(at)
}

/// When something next happens, in a panel: "Today 09:00", "Tomorrow
/// 02:00", "Mon 18:30", "Oct 30 09:00".
pub(super) fn next_full<Tz: TimeZone>(at: &DateTime<Tz>, now: &DateTime<Tz>) -> String {
    let days = (at.date_naive() - now.date_naive()).num_days();
    let time = hm(at);
    match days {
        ..=0 => format!("Today {time}"),
        1 => format!("Tomorrow {time}"),
        2..=6 => format!(
            "{} {time}",
            WEEKDAYS[at.weekday().num_days_from_monday() as usize]
        ),
        _ => format!("{} {time}", day_month(at)),
    }
}

/// When something happened, short: "14:02" (today), "Yesterday", "Mon"
/// (this week), "Oct 12", "Oct 12, 2025" (another year).
pub(super) fn past_label<Tz: TimeZone>(at: &DateTime<Tz>, now: &DateTime<Tz>) -> String {
    let days = (now.date_naive() - at.date_naive()).num_days();
    match days {
        ..=0 => hm(at),
        1 => "Yesterday".into(),
        2..=6 => WEEKDAYS[at.weekday().num_days_from_monday() as usize].into(),
        _ if at.year() == now.year() => day_month(at),
        _ => format!("{}, {}", day_month(at), at.year()),
    }
}

/// When something happened, in a panel: "Today 14:02", "Yesterday 09:00",
/// "Oct 12, 14:02".
pub(super) fn past_full<Tz: TimeZone>(at: &DateTime<Tz>, now: &DateTime<Tz>) -> String {
    let days = (now.date_naive() - at.date_naive()).num_days();
    let time = hm(at);
    match days {
        ..=0 => format!("Today {time}"),
        1 => format!("Yesterday {time}"),
        _ if at.year() == now.year() => format!("{}, {time}", day_month(at)),
        _ => format!("{}, {}", day_month(at), at.year()),
    }
}

/// [`past_label`] for epoch ms, in this device's time zone.
pub(super) fn past_ms(ms: i64) -> String {
    local(ms)
        .map(|at| past_label(&at, &Local::now()))
        .unwrap_or_default()
}

/// How long ago, for sub-lines: "just now", "5m ago", "2h ago", "3d ago".
pub(super) fn ago_ms(ms: i64) -> String {
    let Some(then) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms) else {
        return String::new();
    };
    match format_time_ago(then, chrono::Utc::now()).as_str() {
        "now" => "just now".into(),
        short => format!("{short} ago"),
    }
}

/// [`past_full`] for epoch ms, in this device's time zone.
pub(super) fn past_full_ms(ms: i64) -> String {
    local(ms)
        .map(|at| past_full(&at, &Local::now()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn at(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    #[test]
    fn next_labels_go_from_minutes_to_dates() {
        // Sunday 2026-10-18, 10:00.
        let now = at("2026-10-18T10:00:00+01:00");
        let label = |s: &str| next_label(&at(s), &now);
        assert_eq!(label("2026-10-18T10:00:20+01:00"), "now");
        assert_eq!(label("2026-10-18T10:25:00+01:00"), "in 25m");
        assert_eq!(label("2026-10-18T14:30:00+01:00"), "in 4h");
        assert_eq!(label("2026-10-18T23:00:00+01:00"), "23:00");
        assert_eq!(
            label("2026-10-19T02:00:00+01:00"),
            "Mon",
            "tomorrow, past 12h"
        );
        assert_eq!(label("2026-10-18T21:00:00+01:00"), "in 11h");
        assert_eq!(label("2026-10-20T18:30:00+01:00"), "Tue");
        assert_eq!(label("2026-10-30T09:00:00+01:00"), "Oct 30");
        let full = |s: &str| next_full(&at(s), &now);
        assert_eq!(full("2026-10-18T18:30:00+01:00"), "Today 18:30");
        assert_eq!(full("2026-10-19T02:00:00+01:00"), "Tomorrow 02:00");
        assert_eq!(full("2026-10-21T09:00:00+01:00"), "Wed 09:00");
        assert_eq!(full("2026-11-02T09:00:00+01:00"), "Nov 2 09:00");
    }

    #[test]
    fn past_labels_shorten_with_age() {
        let now = at("2026-10-18T10:00:00+01:00");
        let label = |s: &str| past_label(&at(s), &now);
        assert_eq!(label("2026-10-18T08:05:00+01:00"), "08:05");
        assert_eq!(label("2026-10-17T22:00:00+01:00"), "Yesterday");
        assert_eq!(label("2026-10-14T09:00:00+01:00"), "Wed");
        assert_eq!(label("2026-10-02T09:00:00+01:00"), "Oct 2");
        assert_eq!(label("2025-12-30T09:00:00+01:00"), "Dec 30, 2025");
        assert_eq!(
            past_full(&at("2026-10-12T14:02:00+01:00"), &now),
            "Oct 12, 14:02"
        );
        assert_eq!(
            past_full(&at("2026-10-17T09:00:00+01:00"), &now),
            "Yesterday 09:00"
        );
    }

    #[test]
    fn errors_read_as_sentences() {
        assert_eq!(
            CallError::from(cypher_rpc::RpcError::BadParams("name the routine.".into())),
            CallError::Failed("Name the routine.".into())
        );
        assert_eq!(
            CallError::from(cypher_rpc::RpcError::UnknownMethod(
                "CreateBotInvite".into()
            )),
            CallError::Unknown
        );
        assert_eq!(
            CallError::from(cypher_rpc::RpcError::Failed(
                "unknown method: CreateBotInvite".into()
            )),
            CallError::Unknown
        );
    }
}
