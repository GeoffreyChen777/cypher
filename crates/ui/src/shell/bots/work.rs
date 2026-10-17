//! The **Work** card (§13.1): running orchestrations with their current step
//! and progress, and the last few finished ones.
//!
//! Owned by the UI-A track; see `board.rs` for the card order and
//! `panel.rs` for the panel items it renders.

use super::panel::{PanelHead, PanelItem};
use super::*;

impl Shell {
    /// The sidebar card; `None` when there is nothing to show.
    pub(super) fn render_work_card(
        &self,
        _bot: &Bot,
        _theme: &Theme,
        _cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        None
    }

    /// The detail panel for this card's items; `None` when the item no
    /// longer resolves.
    pub(super) fn render_work_panel(
        &mut self,
        _bot: &Bot,
        _item: &PanelItem,
        _theme: &Theme,
        _cx: &mut Context<Self>,
    ) -> Option<(PanelHead, gpui::Div)> {
        None
    }
}
