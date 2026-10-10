//! What a text input displays for its raw text. A projection maps raw
//! offsets to display offsets around atomic chips — spans the caret and
//! selection treat as one unit, painted on a wash with a hover tooltip — or
//! masks every character of a secret field. The composer projects its
//! mention links to chips; every other input shows its raw text as is.

use std::ops::Range;
use std::time::Duration;

use gpui::{
    Bounds, Context, IntoElement, ParentElement, Pixels, Point, Render, SharedString, Styled,
    Window, div, px,
};

use crate::kit::motion;
use crate::kit::theme::{MonoStyled, Theme};

/// How long the pointer rests on a chip before its tooltip appears.
pub const CHIP_TOOLTIP_DELAY: Duration = Duration::from_millis(420);

pub const CHIP_TOOLTIP_HEIGHT: f32 = 24.0;

/// One atomic chip: the raw range it stands for and the text its hover
/// tooltip shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub range: Range<usize>,
    pub tooltip: SharedString,
}

#[derive(Debug, Clone, Default)]
pub struct TextProjection {
    pub display: String,
    /// Each chip with its byte range in [`Self::display`], in raw order.
    pub chips: Vec<(Chip, Range<usize>)>,
    secret_boundaries: Vec<(usize, usize)>,
}

impl TextProjection {
    /// Raw text shown as is.
    pub fn plain(raw: &str) -> Self {
        Self {
            display: raw.to_owned(),
            chips: Vec::new(),
            secret_boundaries: Vec::new(),
        }
    }

    /// A projected display string and its chips, in raw order.
    pub fn with_chips(display: String, chips: Vec<(Chip, Range<usize>)>) -> Self {
        Self {
            display,
            chips,
            secret_boundaries: Vec::new(),
        }
    }

    pub fn secret(raw: &str) -> Self {
        let mut result = Self::default();
        for (offset, _) in raw.char_indices() {
            result
                .secret_boundaries
                .push((offset, result.display.len()));
            result.display.push('•');
        }
        result
            .secret_boundaries
            .push((raw.len(), result.display.len()));
        result
    }

    pub fn raw_to_display(&self, raw: usize) -> usize {
        if !self.secret_boundaries.is_empty() {
            return self
                .secret_boundaries
                .iter()
                .rev()
                .find(|(r, _)| *r <= raw)
                .map(|(_, d)| *d)
                .unwrap_or(0);
        }
        let mut raw_at = 0;
        let mut display_at = 0;
        for (chip, display) in &self.chips {
            if raw <= chip.range.start {
                return display_at + raw.saturating_sub(raw_at);
            }
            if raw < chip.range.end {
                return display.start;
            }
            raw_at = chip.range.end;
            display_at = display.end;
        }
        display_at + raw.saturating_sub(raw_at)
    }

    pub fn display_to_raw(&self, display_offset: usize) -> usize {
        if !self.secret_boundaries.is_empty() {
            return self
                .secret_boundaries
                .iter()
                .rev()
                .find(|(_, d)| *d <= display_offset)
                .map(|(r, _)| *r)
                .unwrap_or(0);
        }
        let mut raw_at = 0;
        let mut display_at = 0;
        for (chip, display) in &self.chips {
            if display_offset <= display.start {
                return raw_at + display_offset.saturating_sub(display_at);
            }
            if display_offset < display.end {
                return if display_offset - display.start < display.len() / 2 {
                    chip.range.start
                } else {
                    chip.range.end
                };
            }
            raw_at = chip.range.end;
            display_at = display.end;
        }
        raw_at + display_offset.saturating_sub(display_at)
    }

    pub fn normalize_range(&self, range: Range<usize>) -> Range<usize> {
        if range.is_empty() {
            for (chip, _) in &self.chips {
                if chip.range.start < range.start && range.start < chip.range.end {
                    let midpoint = chip.range.start + chip.range.len() / 2;
                    let at = if range.start < midpoint {
                        chip.range.start
                    } else {
                        chip.range.end
                    };
                    return at..at;
                }
            }
            return range;
        }
        let mut normalized = range;
        for (chip, _) in &self.chips {
            if normalized.start < chip.range.end && normalized.end > chip.range.start {
                normalized.start = normalized.start.min(chip.range.start);
                normalized.end = normalized.end.max(chip.range.end);
            }
        }
        normalized
    }

    pub fn previous_boundary(&self, raw: usize) -> Option<usize> {
        self.chips
            .iter()
            .find_map(|(chip, _)| (raw == chip.range.end).then_some(chip.range.start))
    }

    pub fn next_boundary(&self, raw: usize) -> Option<usize> {
        self.chips
            .iter()
            .find_map(|(chip, _)| (raw == chip.range.start).then_some(chip.range.end))
    }
}

/// The hover identity of one chip: the raw range plus its tooltip text. The
/// text alone is not enough — two identical relative paths can appear in a
/// draft, so the raw range remains part of the identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipTooltipTarget {
    pub range: Range<usize>,
    pub label: SharedString,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChipTooltipPhase {
    Hidden,
    Waiting {
        target: ChipTooltipTarget,
        generation: u64,
    },
    Visible {
        target: ChipTooltipTarget,
        generation: u64,
    },
}

impl ChipTooltipPhase {
    pub fn target(&self) -> Option<&ChipTooltipTarget> {
        match self {
            Self::Hidden => None,
            Self::Waiting { target, .. } | Self::Visible { target, .. } => Some(target),
        }
    }
}

/// Pure tooltip lifecycle reducer. Motion within the same chip preserves both
/// waiting and visible phases, so normal pointer jitter cannot starve the
/// delay or flicker an already-visible tooltip.
pub fn chip_tooltip_reduce(
    phase: ChipTooltipPhase,
    pointer_target: Option<ChipTooltipTarget>,
    pointer_in_popup: bool,
    generation: u64,
) -> ChipTooltipPhase {
    match pointer_target {
        Some(target) if phase.target() == Some(&target) => phase,
        Some(target) => ChipTooltipPhase::Waiting { target, generation },
        None if pointer_in_popup && matches!(phase, ChipTooltipPhase::Visible { .. }) => phase,
        None => ChipTooltipPhase::Hidden,
    }
}

pub fn chip_tooltip_promote(
    phase: ChipTooltipPhase,
    generation: u64,
    target_is_live: bool,
) -> ChipTooltipPhase {
    match phase {
        ChipTooltipPhase::Waiting {
            target,
            generation: current,
        } if current == generation && target_is_live => ChipTooltipPhase::Visible {
            target,
            generation: current,
        },
        ChipTooltipPhase::Waiting {
            generation: current,
            ..
        } if current == generation => ChipTooltipPhase::Hidden,
        phase => phase,
    }
}

pub fn chip_tooltip_contains(in_chip: bool, in_popup: bool) -> bool {
    in_chip || in_popup
}

pub fn display_row_segments(
    range: Range<usize>,
    row_ends: impl IntoIterator<Item = usize>,
) -> Vec<(usize, usize, Range<usize>)> {
    let mut segments = Vec::new();
    let mut row_start = 0usize;
    for (row_ix, row_end) in row_ends.into_iter().enumerate() {
        let start = range.start.max(row_start);
        let end = range.end.min(row_end);
        if start < end {
            segments.push((row_ix, row_start, start..end));
        }
        row_start = row_end;
        if row_start >= range.end {
            break;
        }
    }
    segments
}

#[derive(Debug, Clone)]
pub struct ChipHit {
    pub target: ChipTooltipTarget,
    pub bounds: Bounds<Pixels>,
    pub anchor: Point<Pixels>,
}

/// A chip's hover tooltip.
pub struct ChipTooltip {
    pub label: SharedString,
    /// Stable for one `Waiting → Visible` promotion; a later activation gets
    /// a new key and therefore exactly one fresh fade-in.
    pub activation: u64,
}

impl Render for ChipTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        motion::fade_quick(
            ("file-mention-path-tooltip", self.activation),
            div()
                .h(px(CHIP_TOOLTIP_HEIGHT))
                .max_w(px(480.0))
                .flex()
                .items_center()
                .px(px(8.0))
                .rounded(px(5.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.surface_raised)
                .mono(theme)
                .text_size(px(11.0))
                .text_color(theme.text_muted)
                .child(self.label.clone()),
        )
    }
}
