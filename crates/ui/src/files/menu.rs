//! The tree's context menu (rows and the empty area) and the delete
//! confirmation dialog.

use gpui::{AnyElement, Context, Pixels, Point, SharedString, Window, div, prelude::*, px};

use super::FilesPanel;
use super::tree::{name_of, parent_of};
use crate::kit::icons::{self, icon};
use crate::kit::popover;
use crate::kit::theme::Theme;

/// An open context menu: where, and over which rows (empty = the folder
/// background, i.e. the project root).
#[derive(Debug, Clone)]
pub(super) struct MenuState {
    pub position: Point<Pixels>,
    pub targets: Vec<(String, bool)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuAction {
    Open,
    OpenDefault,
    Reveal,
    NewFile,
    NewFolder,
    Cut,
    Copy,
    Paste,
    Duplicate,
    CopyPath,
    CopyRelativePath,
    Rename,
    Delete,
    Mention,
    Refresh,
    CollapseAll,
    ToggleHidden,
}

/// What the menu needs to know to offer the right rows.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct MenuFacts {
    pub local: bool,
    pub can_paste: bool,
    pub show_hidden: bool,
}

pub(super) struct MenuEntry {
    pub action: MenuAction,
    pub label: &'static str,
    pub icon: Option<&'static str>,
    pub hint: Option<&'static str>,
    pub danger: bool,
}

fn entry(action: MenuAction, label: &'static str) -> MenuEntry {
    MenuEntry {
        action,
        label,
        icon: None,
        hint: None,
        danger: false,
    }
}

impl MenuEntry {
    fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }
    fn hint(mut self, hint: &'static str) -> Self {
        self.hint = Some(hint);
        self
    }
}

/// The menu's rows in sections (`None` = a separator). Pure.
pub(super) fn menu_entries(targets: &[(String, bool)], facts: MenuFacts) -> Vec<Option<MenuEntry>> {
    use MenuAction::*;
    let reveal = if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else {
        "Show in Folder"
    };
    let mut out = Vec::new();
    let section = |entries: Vec<MenuEntry>, out: &mut Vec<Option<MenuEntry>>| {
        if entries.is_empty() {
            return;
        }
        if !out.is_empty() {
            out.push(None);
        }
        out.extend(entries.into_iter().map(Some));
    };
    let single = targets.len() == 1;
    let single_file = matches!(targets, [(_, false)]);
    if targets.is_empty() {
        section(
            vec![
                entry(NewFile, "New File…").icon(icons::DOCUMENT_ADD),
                entry(NewFolder, "New Folder…").icon(icons::FOLDER_ADD),
            ],
            &mut out,
        );
        let mut edit = Vec::new();
        if facts.can_paste {
            edit.push(entry(Paste, "Paste").hint("⌘V"));
        }
        section(edit, &mut out);
        let mut host = vec![entry(CopyPath, "Copy Path").icon(icons::COPY)];
        if facts.local {
            host.push(entry(Reveal, reveal));
        }
        section(host, &mut out);
        section(
            vec![
                entry(
                    ToggleHidden,
                    if facts.show_hidden {
                        "Hide Hidden Files"
                    } else {
                        "Show Hidden Files"
                    },
                ),
                entry(CollapseAll, "Collapse All").icon(icons::FOLD_VERTICAL),
                entry(Refresh, "Refresh").icon(icons::REFRESH),
            ],
            &mut out,
        );
        return out;
    }
    let mut open = Vec::new();
    if single_file {
        open.push(entry(Open, "Open"));
    }
    if single && facts.local {
        open.push(entry(OpenDefault, "Open with Default App"));
        open.push(entry(Reveal, reveal));
    }
    section(open, &mut out);
    if single {
        section(
            vec![
                entry(NewFile, "New File…").icon(icons::DOCUMENT_ADD),
                entry(NewFolder, "New Folder…").icon(icons::FOLDER_ADD),
            ],
            &mut out,
        );
    }
    let mut edit = vec![entry(Cut, "Cut").hint("⌘X"), entry(Copy, "Copy").hint("⌘C")];
    if facts.can_paste {
        edit.push(entry(Paste, "Paste").hint("⌘V"));
    }
    edit.push(entry(Duplicate, "Duplicate").hint("⌘D"));
    section(edit, &mut out);
    section(
        vec![
            entry(CopyPath, "Copy Path").icon(icons::COPY),
            entry(CopyRelativePath, "Copy Relative Path"),
            entry(Mention, "Mention in Chat").icon(icons::CHAT_ROUND_LINE),
        ],
        &mut out,
    );
    let mut change = Vec::new();
    if single {
        change.push(entry(Rename, "Rename…").icon(icons::PEN).hint("F2"));
    }
    let mut delete = entry(Delete, "Move to Trash")
        .icon(icons::TRASH_BIN_MINIMALISTIC)
        .hint("⌘⌫");
    delete.danger = true;
    change.push(delete);
    section(change, &mut out);
    out
}

/// A pending delete awaiting confirmation.
#[derive(Debug, Clone)]
pub(super) struct DeleteConfirm {
    pub items: Vec<(String, bool)>,
    pub permanent: bool,
    /// Why the Trash was not an option (offered permanent delete instead).
    pub note: Option<SharedString>,
}

impl FilesPanel {
    pub(super) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.begin_close() {
            popover::reap_popup(cx, |panel: &mut Self| &mut panel.menu);
            cx.notify();
        }
    }

    pub(super) fn run_menu_action(
        &mut self,
        action: MenuAction,
        targets: Vec<(String, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_menu(cx);
        // New entries go INTO a folder target, beside a file target.
        let folder = match targets.as_slice() {
            [(path, true)] => path.clone(),
            [(path, false)] => parent_of(path).to_string(),
            _ => String::new(),
        };
        let first = targets.first().cloned();
        match action {
            MenuAction::Open => {
                if let Some((path, false)) = first {
                    self.open_file(path, true, Some(window), cx);
                }
            }
            MenuAction::OpenDefault => {
                let path = first.map(|(p, _)| p).unwrap_or_default();
                self.open_with_default_app(&path, cx);
            }
            MenuAction::Reveal => {
                let path = first.map(|(p, _)| p).unwrap_or_default();
                self.reveal_in_finder(&path, cx);
            }
            MenuAction::NewFile => self.begin_create(folder, false, window, cx),
            MenuAction::NewFolder => self.begin_create(folder, true, window, cx),
            MenuAction::Cut => self.copy_entries(targets, true, cx),
            MenuAction::Copy => self.copy_entries(targets, false, cx),
            MenuAction::Paste => self.paste(folder, cx),
            MenuAction::Duplicate => self.duplicate(targets, cx),
            MenuAction::CopyPath => {
                let targets = if targets.is_empty() {
                    vec![(String::new(), true)]
                } else {
                    targets
                };
                self.copy_path(&targets, false, cx);
            }
            MenuAction::CopyRelativePath => self.copy_path(&targets, true, cx),
            MenuAction::Rename => {
                if let Some((path, is_dir)) = first {
                    self.begin_rename(path, is_dir, window, cx);
                }
            }
            MenuAction::Delete => self.request_delete(targets, false, window, cx),
            MenuAction::Mention => self.mention(&targets, cx),
            MenuAction::Refresh => self.refresh(cx),
            MenuAction::CollapseAll => self.collapse_all(cx),
            MenuAction::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                self.rebuild_rows();
                cx.notify();
            }
        }
    }

    pub(super) fn render_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.menu.get()?.clone();
        let closing = self.menu.closing_since();
        let facts = MenuFacts {
            local: self.is_local(),
            can_paste: self.can_paste(cx),
            show_hidden: self.show_hidden,
        };
        let mut card = popover::popover_card(theme)
            .w(px(220.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)))
            .flex()
            .flex_col();
        for (ix, item) in menu_entries(&state.targets, facts).into_iter().enumerate() {
            let Some(item) = item else {
                card = card.child(popover::menu_separator());
                continue;
            };
            let action = item.action;
            let targets = state.targets.clone();
            let color = if item.danger {
                theme.danger
            } else {
                theme.text_muted
            };
            card = card.child(
                popover::menu_row(theme, false, format!("files-menu-{ix}"))
                    .id(("files-menu-row", ix))
                    .when(item.danger, |el| el.text_color(theme.danger))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_menu_action(action, targets.clone(), window, cx)
                    }))
                    .child(
                        div()
                            .flex_none()
                            .size(px(16.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when_some(item.icon, |el, glyph| {
                                el.child(icon(glyph).size(px(15.0)).text_color(color))
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(item.label)),
                    )
                    .when_some(item.hint, |el, hint| {
                        el.child(
                            div()
                                .flex_none()
                                .text_size(px(11.0))
                                .text_color(theme.text_faint)
                                .child(SharedString::from(hint)),
                        )
                    }),
            );
        }
        Some(popover::menu_at(
            "files-context-menu",
            state.position,
            card.into_any_element(),
            closing,
        ))
    }

    pub(super) fn render_confirm(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let confirm = self.confirm.clone()?;
        if std::mem::take(&mut self.confirm_focus_pending) {
            window.focus(&self.confirm_focus, cx);
        }
        let count = confirm.items.len();
        let subject = match confirm.items.as_slice() {
            [(path, _)] => format!("“{}”", name_of(path)),
            _ => format!("{count} items"),
        };
        let title = if confirm.permanent {
            format!("Delete {subject} permanently?")
        } else {
            format!("Move {subject} to the Trash?")
        };
        let dirty = confirm.items.iter().any(|(path, _)| {
            self.open_files
                .iter()
                .any(|(open, file)| file.dirty && super::tree::within(path, open))
        });
        let mut body = match (&confirm.note, confirm.permanent) {
            (Some(note), _) => format!("{note}. Delete permanently instead? This can't be undone."),
            (None, true) => "This can't be undone.".to_string(),
            (None, false) => {
                "You can restore it from the Trash on the session's device.".to_string()
            }
        };
        if dirty {
            body.push_str(" Unsaved changes in open files will be lost.");
        }
        let action = if confirm.permanent {
            "Delete"
        } else {
            "Move to Trash"
        };
        let card = popover::dialog_card(theme)
            .w(px(380.0))
            .track_focus(&self.confirm_focus)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                match ev.keystroke.key.as_str() {
                    "escape" => {
                        this.confirm = None;
                        window.focus(&this.tree_focus, cx);
                        cx.notify();
                    }
                    "enter" => this.confirm_delete(window, cx),
                    _ => {}
                }
            }))
            .child(popover::dialog_title(theme, &title))
            .child(div().mt(px(8.0)).child(popover::dialog_body(theme, body)))
            .child(
                div()
                    .mt(px(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(
                        popover::btn_ghost(theme, "Cancel", "files-delete-cancel")
                            .id("files-delete-cancel")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm = None;
                                window.focus(&this.tree_focus, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        if confirm.permanent {
                            popover::btn_danger(theme, action)
                        } else {
                            popover::btn_primary(theme, action)
                        }
                        .id("files-delete-confirm")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.confirm_delete(window, cx)),
                        ),
                    ),
            )
            .into_any_element();
        Some(popover::modal(
            "files-delete-dialog",
            window.viewport_size(),
            card,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actions(entries: &[Option<MenuEntry>]) -> Vec<Option<MenuAction>> {
        entries
            .iter()
            .map(|e| e.as_ref().map(|e| e.action))
            .collect()
    }

    #[test]
    fn row_menus_offer_what_applies() {
        let facts = MenuFacts {
            local: true,
            can_paste: false,
            show_hidden: false,
        };
        let file = menu_entries(&[("a.txt".into(), false)], facts);
        let file = actions(&file);
        assert!(file.contains(&Some(MenuAction::Open)));
        assert!(file.contains(&Some(MenuAction::Reveal)));
        assert!(file.contains(&Some(MenuAction::Rename)));
        assert!(!file.contains(&Some(MenuAction::Paste)));
        assert_eq!(file.last(), Some(&Some(MenuAction::Delete)));

        let remote_many = actions(&menu_entries(
            &[("a".into(), true), ("b.txt".into(), false)],
            MenuFacts {
                local: false,
                can_paste: true,
                ..facts
            },
        ));
        assert!(!remote_many.contains(&Some(MenuAction::Open)));
        assert!(!remote_many.contains(&Some(MenuAction::Reveal)));
        assert!(!remote_many.contains(&Some(MenuAction::Rename)));
        assert!(remote_many.contains(&Some(MenuAction::Paste)));
        assert!(remote_many.contains(&Some(MenuAction::Delete)));
        // Separators only ever sit between sections.
        assert!(remote_many.first().unwrap().is_some());
        assert!(
            remote_many
                .windows(2)
                .all(|w| w[0].is_some() || w[1].is_some())
        );

        let background = actions(&menu_entries(&[], facts));
        assert_eq!(background[0], Some(MenuAction::NewFile));
        assert!(background.contains(&Some(MenuAction::ToggleHidden)));
        assert!(!background.contains(&Some(MenuAction::Delete)));
    }
}
