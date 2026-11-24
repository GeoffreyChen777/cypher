//! The Workspaces view's recently shown conversations, kept alive while
//! another one shows: switching back to a workspace finds its chat, dock
//! (Files, Git and their views), terminal, scroll and draft as left. The
//! oldest goes once more than [`KEPT_BOARD_SLOTS`] are kept; a workspace
//! whose chat is gone (deleted) is let go at once. Pure: the shell disposes
//! what this hands back (`Shell::sync_board`).

use super::BoardSide;
use crate::shell::session::SlotId;

/// How many hidden workspace conversations stay alive beside the one on
/// screen (each holds a session context with its watches).
pub(in crate::shell) const KEPT_BOARD_SLOTS: usize = 5;

/// One kept conversation: its chat, its slot, and what its right side
/// showed last (the side panel button's memory).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Kept {
    chat_id: String,
    sid: SlotId,
    side: BoardSide,
}

/// The kept slots, least recently shown first.
#[derive(Debug, Default)]
pub(in crate::shell) struct KeptSlots {
    kept: Vec<Kept>,
}

impl KeptSlots {
    /// Whether `sid` is a kept (hidden) slot.
    pub(in crate::shell) fn holds(&self, sid: SlotId) -> bool {
        self.kept.iter().any(|kept| kept.sid == sid)
    }

    /// Take `chat_id`'s slot back out to show it, with its side.
    pub(in crate::shell) fn take(&mut self, chat_id: &str) -> Option<(SlotId, BoardSide)> {
        let ix = self.kept.iter().position(|kept| kept.chat_id == chat_id)?;
        let kept = self.kept.remove(ix);
        Some((kept.sid, kept.side))
    }

    /// Keep `sid` (showing `chat_id`) as the most recent; returns the slots
    /// that no longer fit, oldest first, for disposal. A chat kept already
    /// is replaced (its old slot handed back).
    pub(in crate::shell) fn keep(
        &mut self,
        chat_id: &str,
        sid: SlotId,
        side: BoardSide,
    ) -> Vec<SlotId> {
        let mut out: Vec<SlotId> = self.take(chat_id).map(|(sid, _)| sid).into_iter().collect();
        self.kept.push(Kept {
            chat_id: chat_id.to_string(),
            sid,
            side,
        });
        let excess = self.kept.len().saturating_sub(KEPT_BOARD_SLOTS);
        out.extend(self.kept.drain(..excess).map(|kept| kept.sid));
        out
    }

    /// Let go of the slots whose chat `live` rejects; returns them.
    pub(in crate::shell) fn retain(&mut self, live: impl Fn(&str) -> bool) -> Vec<SlotId> {
        let (keep, gone): (Vec<Kept>, Vec<Kept>) = std::mem::take(&mut self.kept)
            .into_iter()
            .partition(|kept| live(&kept.chat_id));
        self.kept = keep;
        gone.into_iter().map(|kept| kept.sid).collect()
    }

    /// Let go of `sid` (its chat went while hidden); whether it was kept.
    pub(in crate::shell) fn forget(&mut self, sid: SlotId) -> bool {
        let before = self.kept.len();
        self.kept.retain(|kept| kept.sid != sid);
        self.kept.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coming_back_reuses_the_kept_slot() {
        let mut kept = KeptSlots::default();
        assert!(kept.keep("a", 1, BoardSide::Dock).is_empty());
        assert!(kept.keep("b", 2, BoardSide::Panel).is_empty());
        assert!(kept.holds(1) && kept.holds(2));
        assert_eq!(kept.take("a"), Some((1, BoardSide::Dock)));
        assert!(!kept.holds(1), "shown again, no longer kept");
        assert_eq!(kept.take("a"), None);
        assert_eq!(kept.take("c"), None);
        // Keeping a chat again hands its stale slot back.
        assert_eq!(kept.keep("b", 7, BoardSide::Dock), vec![2]);
        assert_eq!(kept.take("b"), Some((7, BoardSide::Dock)));
    }

    #[test]
    fn the_oldest_goes_past_the_cap() {
        let mut kept = KeptSlots::default();
        for ix in 0..KEPT_BOARD_SLOTS as u64 {
            assert!(
                kept.keep(&format!("c{ix}"), ix, BoardSide::Panel)
                    .is_empty()
            );
        }
        // Showing c0 again and hiding it makes it the most recent.
        let (sid, _) = kept.take("c0").unwrap();
        assert!(kept.keep("c0", sid, BoardSide::Panel).is_empty());
        assert_eq!(
            kept.keep("new", 99, BoardSide::Panel),
            vec![1],
            "c1 is the oldest now"
        );
        assert!(kept.holds(0) && kept.holds(99) && !kept.holds(1));
        assert_eq!(kept.kept.len(), KEPT_BOARD_SLOTS);
    }

    #[test]
    fn a_deleted_chat_lets_its_slot_go() {
        let mut kept = KeptSlots::default();
        kept.keep("a", 1, BoardSide::Panel);
        kept.keep("b", 2, BoardSide::Panel);
        kept.keep("c", 3, BoardSide::Panel);
        assert_eq!(kept.retain(|chat| chat != "b"), vec![2]);
        assert!(!kept.holds(2));
        assert!(kept.forget(3));
        assert!(!kept.forget(3));
        assert_eq!(kept.take("a"), Some((1, BoardSide::Panel)));
    }
}
