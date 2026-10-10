//! In-chat find (⌘F); the bar itself is shell chrome.

use super::*;

impl Transcript {
    /// Whether the find bar should be on screen. The shell renders from this,
    /// so closing here (a chat switch) closes the bar with it.
    pub fn find_open(&self) -> bool {
        self.find.is_some()
    }

    /// `(1-based position of the active match, total matches)` — `(0, 0)`
    /// with no query or no hits, which is what the counter renders as "No
    /// results".
    pub fn find_status(&self) -> (usize, usize) {
        let Some(find) = &self.find else {
            return (0, 0);
        };
        let total = find.total();
        if total == 0 {
            return (0, 0);
        }
        (find.active.min(total - 1) + 1, total)
    }

    /// Open the find bar (idempotent — ⌘F on an open bar just refocuses it,
    /// keeping the previous query the way every other find bar does).
    pub fn open_find(&mut self, cx: &mut Context<Self>) {
        if self.find.is_none() {
            self.find = Some(FindState::default());
            self.reindex_find();
            cx.notify();
        }
    }

    pub fn close_find(&mut self, cx: &mut Context<Self>) {
        if self.find.take().is_some() {
            crate::markdown::find::clear(self.scope);
            cx.notify();
        }
    }

    /// Re-run the search for a new query, landing on the first match at or
    /// after the current viewport (so opening find mid-scroll doesn't fling
    /// the transcript back to the top).
    pub fn set_find_query(&mut self, query: &str, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else {
            return;
        };
        if find.query == query {
            return;
        }
        find.query.clear();
        find.query.push_str(query);
        find.memo.clear();
        find.active = 0;
        self.reindex_find();
        self.select_nearest_find_match();
        self.reveal_find_match(cx);
        cx.notify();
    }

    /// Step to the next (`1`) or previous (`-1`) match, wrapping at both ends.
    pub fn step_find(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else {
            return;
        };
        let total = find.total();
        if total == 0 {
            return;
        }
        let active = find.active.min(total - 1) as isize;
        find.active = (active + delta).rem_euclid(total as isize) as usize;
        self.reveal_find_match(cx);
        cx.notify();
    }

    /// Rebuild the per-row counts. O(rows) plus a rescan of the rows whose
    /// version moved since the last pass.
    pub(super) fn reindex_find(&mut self) {
        let Some(find) = &mut self.find else {
            return;
        };
        find.counts.clear();
        find.counts.reserve(self.rows.len());
        find.prefix.clear();
        find.prefix.reserve(self.rows.len() + 1);
        let mut running = 0u32;
        find.prefix.push(0);
        if find.query.is_empty() {
            find.counts.resize(self.rows.len(), 0);
            find.prefix.resize(self.rows.len() + 1, 0);
            return;
        }
        for row in &self.rows {
            let count = match find.memo.get(&row.id) {
                Some(&(version, count)) if version == row.version => count,
                _ => {
                    let count = row_match_count(row, &find.query);
                    find.memo.insert(row.id.clone(), (row.version, count));
                    count
                }
            };
            find.counts.push(count);
            running = running.saturating_add(count);
            find.prefix.push(running);
        }
        // Rows the diff removed must not keep the memo growing for the life
        // of the session.
        if find.memo.len() > self.rows.len() * 2 + 64 {
            let live: std::collections::HashSet<&SharedString> =
                self.rows.iter().map(|row| &row.id).collect();
            find.memo.retain(|id, _| live.contains(id));
        }
        let total = running as usize;
        if total == 0 {
            find.active = 0;
        } else if find.active >= total {
            find.active = total - 1;
        }
    }

    /// Park the active match on the first hit at or after the row currently
    /// at the top of the viewport (browser find-bar behaviour), falling back
    /// to the first match overall.
    fn select_nearest_find_match(&mut self) {
        let top_row = self.list.logical_scroll_top().item_ix;
        let Some(find) = &mut self.find else {
            return;
        };
        if find.total() == 0 {
            return;
        }
        let at_or_after = find
            .prefix
            .get(top_row.min(self.rows.len()))
            .copied()
            .unwrap_or(0) as usize;
        find.active = if at_or_after < find.total() {
            at_or_after
        } else {
            0
        };
    }

    /// Bring the active match's row into view, below the chrome the find bar
    /// and titlebar occupy. A row already comfortably on screen is left alone
    /// so stepping through several matches inside one long reply doesn't
    /// re-snap the viewport for each of them.
    fn reveal_find_match(&mut self, cx: &mut Context<Self>) {
        let Some((row_ix, _)) = self.find.as_ref().and_then(FindState::target) else {
            return;
        };
        if row_ix >= self.rows.len() {
            return;
        }
        let inset = self.find_reveal_inset();
        let viewport = self.list.viewport_bounds();
        if let Some(bounds) = self.list.bounds_for_item(row_ix) {
            let top_limit = f32::from(viewport.top()) + inset;
            let bottom_limit = f32::from(viewport.bottom()) - self.bottom_clearance;
            if f32::from(bounds.top()) >= top_limit && f32::from(bounds.bottom()) <= bottom_limit {
                return;
            }
        }
        // Navigating is an explicit viewport move: release the bottom pin and
        // any own-turn hold, both of which re-assert a scroll position every
        // frame and would drag the view straight back off the match.
        self.pinned = false;
        self.own_turn = None;
        self.own_turn_kick = false;
        self.own_turn_last_tick = None;
        self.spring.reset();
        self.spring_last_tick = None;
        self.spring_settled_at = None;
        self.spring_kick = false;
        self.list.scroll_to(ListOffset {
            item_ix: row_ix,
            offset_in_item: px(0.0),
        });
        self.list.scroll_by(px(-inset));
        cx.notify();
    }

    /// How far below the viewport top a revealed match rests: the chrome the
    /// transcript scrolls under, plus room for the find bar floating in it.
    fn find_reveal_inset(&self) -> f32 {
        if self.embedded {
            EMBEDDED_TOP_INSET_PX
        } else {
            OWN_SEND_TOP_INSET_PX + FIND_BAR_CLEARANCE
        }
    }

    /// Hand the painter this frame's query + active match (see
    /// [`crate::markdown::find`]). Called once per render, before the list builds rows.
    pub(super) fn publish_find(&self) {
        let Some(find) = &self.find else {
            crate::markdown::find::clear(self.scope);
            return;
        };
        let active = find.target().and_then(|(row_ix, ordinal)| {
            self.rows
                .get(row_ix)
                .map(|row| (row.id.to_string(), ordinal))
        });
        crate::markdown::find::publish(self.scope, &find.query, active);
    }
}
