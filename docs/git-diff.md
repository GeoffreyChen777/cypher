# Git diff layouts

The Git toolbar's two layout icons switch between **Unified** (the original
single-column layout) and **Split** (old version on the left, new version on the
right). Hover for the Unified diff / Side-by-side diff tooltip; the active icon
is highlighted.

- Unified remains the default.
- Split uses equal-width columns and one virtualized vertical list, so both
  versions stay vertically aligned.
- Within each contiguous change block, deleted and added lines pair in order.
  Unequal runs get empty padding cells. Context lines appear on both sides.
  This is positional pairing, not similarity matching or character-level diff.
- Each side uses its own source-line numbers and syntax-highlight document.
  No-newline markers remain metadata, not selectable source code.
- Long code lines do not wrap. Use a horizontal trackpad gesture, Shift+wheel,
  or the arrow buttons beside **Old / New** to scroll that column horizontally.
  Widen or expand the right pane for more space.
- Text selection/copy is isolated to the version where the drag began; blank
  padding is not copied. Comment/Side Chat context identifies Old or New and
  includes a file path when the entire quote belongs to one file.
- Switching layout retains file fold states and restores the nearest matching
  source row at the top of the list. Existing selections are cleared to avoid
  stale comment anchors. Split-mode folding is immediate; Unified retains its
  existing fold animation.

The choice applies to this client's open and future Git diff tabs, including
commit diffs opened from History. The History list itself does not become a
two-column diff.

The preference is saved atomically in `<client-data-dir>/diff-view.json`,
separate from other UI preferences. A failed save reports an error without
changing the active layout. No remote engine configuration or repository files
are modified.
