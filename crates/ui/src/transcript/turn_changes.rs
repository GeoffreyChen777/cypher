//! The round's changed files, closing the round's last answer: a quiet
//! header in the tool group's voice — chevron tile, "6 files changed", the
//! line counts and a "Review" link — over the files, hung off the same guide
//! rail as tool chips. The header folds the list; "Review" opens the round's
//! whole diff and each file opens it at that file.

use super::*;

use cypher_proto::{DiffFileSummary, TurnChanges};

/// Files listed under the header; the rest are counted on its last row.
pub const CARD_FILES: usize = 5;

const HEADER_HEIGHT: f32 = 26.0;
const FILE_ROW_HEIGHT: f32 = 22.0;

/// The quiet word a file's status earns beside its name — `None` for a
/// plain modification, the common case — and whether it reads as an
/// addition, a deletion, or neither.
pub fn status_tag(status: &str) -> Option<(&'static str, Option<bool>)> {
    match status {
        "added" => Some(("new", Some(true))),
        "deleted" => Some(("deleted", Some(false))),
        "renamed" => Some(("renamed", None)),
        "copied" => Some(("copied", None)),
        "unmerged" => Some(("conflict", None)),
        _ => None,
    }
}

/// A path split for the list: the file name, then its folder (empty at the
/// checkout root).
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (name, dir),
        None => (path, ""),
    }
}

/// The header's label: "3 files changed" (the count covers files the doc
/// entry left out).
pub fn changed_label(changes: &TurnChanges) -> String {
    let count = changes.file_count.max(changes.files.len() as u32);
    if count == 1 {
        "1 file changed".to_string()
    } else {
        format!("{count} files changed")
    }
}

/// How many changed files the list does not show.
pub fn hidden_files(changes: &TurnChanges) -> u32 {
    let count = changes.file_count.max(changes.files.len() as u32);
    count.saturating_sub(changes.files.len().min(CARD_FILES) as u32)
}

/// `+12 −3` in small mono, each side only when non-zero; muted so the
/// numbers sit behind the names.
fn counts(additions: u32, deletions: u32, binary: bool, theme: &Theme) -> gpui::Div {
    let label = |text: String, color: gpui::Hsla| {
        div()
            .flex_none()
            .mono(theme)
            .text_size(px(10.5))
            .text_color(color)
            .child(SharedString::from(text))
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(5.0))
        .when(binary, |el| el.child(label("bin".into(), theme.text_faint)))
        .when(additions > 0, |el| {
            el.child(label(format!("+{additions}"), theme.diff_add.opacity(0.75)))
        })
        .when(deletions > 0, |el| {
            el.child(label(format!("−{deletions}"), theme.diff_del.opacity(0.75)))
        })
}

/// One row under the header: the guide rail, then `content` inset like a
/// tool chip's card.
fn railed(content: impl IntoElement) -> gpui::Div {
    div()
        .h(px(FILE_ROW_HEIGHT))
        .w_full()
        .flex()
        .flex_row()
        .child(guide_rail().h_full())
        .child(div().ml(px(8.0)).flex_1().min_w_0().child(content))
}

/// A click that opens the round's diff at `path` (the whole round when
/// `None`).
fn open_diff(
    changes: &Arc<TurnChanges>,
    path: Option<String>,
    cx: &mut Context<Transcript>,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let changes = changes.clone();
    cx.listener(move |_, _: &gpui::ClickEvent, _, cx| {
        cx.stop_propagation();
        cx.emit(TranscriptEvent::OpenTurnDiff {
            changes: changes.clone(),
            path: path.clone(),
        });
    })
}

impl Transcript {
    pub(super) fn render_turn_changes(
        &mut self,
        row_id: &SharedString,
        changes: &Arc<TurnChanges>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self
            .folds
            .get(row_id)
            .and_then(|fold| fold.open)
            .unwrap_or(true);
        let toggle_id = row_id.clone();
        let review = div()
            .id((row_id.clone(), 0usize))
            .flex_none()
            .px(px(6.0))
            .h(px(20.0))
            .flex()
            .items_center()
            .rounded(px(5.0))
            .cursor_pointer()
            .text_size(px(11.5))
            .text_color(theme.text_faint)
            .hover(|s| {
                s.text_color(theme.text_muted)
                    .bg(crate::kit::theme::ink(0.05))
            })
            .on_click(open_diff(changes, None, cx))
            .child(SharedString::from("Review"));
        let header = div()
            .id(SharedString::from(format!("{row_id}-hdr")))
            .h(px(HEADER_HEIGHT))
            .w_full()
            .px(px(4.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .text_size(px(12.0))
            .text_color(theme.text_muted)
            .hover(|s| s.text_color(theme.text))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_fold(toggle_id.clone(), 0.0, true);
                cx.notify();
            }))
            .child(
                div()
                    .size(px(18.0))
                    .flex_none()
                    .rounded(px(5.0))
                    .bg(crate::kit::theme::ink(0.06))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.0))
                    .text_color(theme.text_muted.opacity(0.7))
                    .child(SharedString::from(if open { "▾" } else { "▸" })),
            )
            .child(
                div()
                    .flex_none()
                    .child(SharedString::from(changed_label(changes))),
            )
            .child(counts(changes.additions, changes.deletions, false, theme))
            .child(div().flex_1())
            .child(review);
        let mut card = div().w_full().flex().flex_col().child(header);
        if open {
            card = card.children(changes.files.iter().take(CARD_FILES).enumerate().map(
                |(ix, file)| {
                    self.turn_change_file_row(
                        row_id,
                        ix + 1,
                        file,
                        open_diff(changes, Some(file.path.clone()), cx),
                        theme,
                    )
                },
            ));
            let hidden = hidden_files(changes);
            if hidden > 0 {
                card = card.child(railed(
                    div()
                        .id((row_id.clone(), CARD_FILES + 1))
                        .h_full()
                        .px(px(6.0))
                        .flex()
                        .items_center()
                        .rounded(px(5.0))
                        .cursor_pointer()
                        .text_size(px(11.5))
                        .text_color(theme.text_faint)
                        .hover(|s| s.text_color(theme.text_muted))
                        .on_click(open_diff(changes, None, cx))
                        .child(SharedString::from(if hidden == 1 {
                            "1 more file".to_string()
                        } else {
                            format!("{hidden} more files")
                        })),
                ));
            }
        }
        card.into_any_element()
    }

    fn turn_change_file_row(
        &self,
        row_id: &SharedString,
        ix: usize,
        file: &DiffFileSummary,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
        theme: &Theme,
    ) -> AnyElement {
        let (name, dir) = split_path(&file.path);
        let deleted = file.status == "deleted";
        let tag = status_tag(&file.status).map(|(word, added)| {
            let color = match added {
                Some(true) => theme.diff_add.opacity(0.7),
                Some(false) => theme.diff_del.opacity(0.7),
                None => theme.text_faint,
            };
            div()
                .flex_none()
                .text_size(px(10.5))
                .text_color(color)
                .child(SharedString::from(word))
        });
        railed(
            div()
                .id((row_id.clone(), ix))
                .h_full()
                .px(px(6.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .rounded(px(5.0))
                .cursor_pointer()
                .hover(|s| s.bg(crate::kit::theme::ink(0.04)))
                .on_click(on_click)
                .child(
                    div()
                        .flex_none()
                        .max_w(gpui::relative(0.6))
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(if deleted {
                            theme.text_muted
                        } else {
                            theme.text.opacity(0.85)
                        })
                        .when(deleted, |el| el.line_through())
                        .child(SharedString::from(name.to_string())),
                )
                .children(tag)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(dir.to_string())),
                )
                .child(counts(file.additions, file.deletions, file.binary, theme)),
        )
        .into_any_element()
    }
}
