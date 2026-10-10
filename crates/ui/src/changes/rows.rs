//! Row model — the diff flattened to line granularity (pure).

use super::{FileDiff, file_notices};

/// One virtualized list row. The diff is flattened so each visible LINE is
/// its own row (Zed's editor draws exactly the visible line range the same
/// way): scrolling a 10k-line file materializes ~50 line rows per frame, not
/// one 10k-line element, and a collapsed file contributes no body rows at
/// all. Heights are the analytic constants above — no measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRow {
    FileHeader {
        file: u32,
    },
    Notice {
        file: u32,
        notice: u32,
    },
    HunkHeader {
        file: u32,
        hunk: u32,
    },
    Line {
        file: u32,
        hunk: u32,
        line: u32,
        /// Flat index across the file's hunks — keys into the highlight slot.
        flat: u32,
    },
    SplitLine {
        file: u32,
        hunk: u32,
        old: Option<u32>,
        new: Option<u32>,
    },
    /// Trailing pad closing an expanded body ([`BODY_BOTTOM_PAD`]).
    BodyPad {
        file: u32,
    },
    /// A body mid-fold-tween: one height-animated, clipped row standing in
    /// for the whole body. Only the slice that can be revealed is built —
    /// the tween never pays for off-screen lines.
    FoldingBody {
        file: u32,
    },
}

/// Rows an expanded body contributes (notices + hunk headers + lines + pad).
pub fn body_row_count(file: &FileDiff) -> usize {
    let lines: usize = file.hunks.iter().map(|h| h.lines.len()).sum();
    file_notices(file).len() + file.hunks.len() + lines + 1
}

/// The steady-state body rows of one expanded file.
pub fn body_rows(file_ix: u32, file: &FileDiff) -> Vec<DiffRow> {
    let mut rows = Vec::with_capacity(body_row_count(file));
    for notice in 0..file_notices(file).len() {
        rows.push(DiffRow::Notice {
            file: file_ix,
            notice: notice as u32,
        });
    }
    let mut flat = 0u32;
    for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
        rows.push(DiffRow::HunkHeader {
            file: file_ix,
            hunk: hunk_ix as u32,
        });
        for line_ix in 0..hunk.lines.len() {
            rows.push(DiffRow::Line {
                file: file_ix,
                hunk: hunk_ix as u32,
                line: line_ix as u32,
                flat,
            });
            flat += 1;
        }
    }
    rows.push(DiffRow::BodyPad { file: file_ix });
    rows
}

/// Flatten all files into rows + each file's row span (header at
/// `range.start`, body rows after it). `collapsed(ix)` folds a file to just
/// its header.
#[cfg(test)]
pub fn flatten_rows(
    files: &[FileDiff],
    mut collapsed: impl FnMut(usize) -> bool,
) -> (Vec<DiffRow>, Vec<std::ops::Range<usize>>) {
    let mut rows = Vec::new();
    let mut ranges = Vec::with_capacity(files.len());
    for (ix, file) in files.iter().enumerate() {
        let start = rows.len();
        rows.push(DiffRow::FileHeader { file: ix as u32 });
        if !collapsed(ix) {
            rows.extend(body_rows(ix as u32, file));
        }
        ranges.push(start..rows.len());
    }
    (rows, ranges)
}
