//! Syntax-highlighting contracts shared by Cypher's desktop surfaces.
//!
//! This crate intentionally has no UI, RPC, or engine dependencies. Public
//! ranges are byte offsets relative to one UTF-8 source line.

use std::{collections::BTreeSet, ops::Range, sync::atomic::AtomicUsize};

use tree_sitter_highlight::{HighlightEvent, Highlighter};

mod languages;

use languages::{configuration, injected_languages};
pub use languages::{detect_language, language_for_alias, language_for_path};

pub(crate) const DEFAULT_MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub(crate) const DEFAULT_MAX_SPANS: usize = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HighlightLimits {
    pub max_source_bytes: usize,
    pub max_spans: usize,
}

impl Default for HighlightLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: DEFAULT_MAX_SOURCE_BYTES,
            max_spans: DEFAULT_MAX_SPANS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageId {
    Rust,
    JavaScript,
    Jsx,
    TypeScript,
    Tsx,
    Python,
    Go,
    Json,
    Jsonc,
    Bash,
    Toml,
    Markdown,
    Html,
    Css,
    Yaml,
    C,
    Cpp,
    CSharp,
    Java,
    Kotlin,
    Swift,
    Ruby,
    Php,
    Sql,
    Lua,
    Dockerfile,
    Nix,
    Make,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HighlightKind {
    Comment,
    Keyword,
    String,
    StringSpecial,
    Escape,
    Number,
    Boolean,
    Type,
    TypeBuiltin,
    Constructor,
    Function,
    FunctionBuiltin,
    Macro,
    Property,
    Constant,
    Variable,
    VariableSpecial,
    Parameter,
    Operator,
    Punctuation,
    Tag,
    Attribute,
    Label,
    Embedded,
    Invalid,
}

impl HighlightKind {
    /// Stable precedence used to resolve overlapping parser captures.
    pub(crate) const fn precedence(self) -> u8 {
        match self {
            Self::Invalid => 100,
            Self::Escape => 95,
            Self::Macro => 90,
            Self::Property | Self::Attribute => 85,
            Self::FunctionBuiltin | Self::TypeBuiltin | Self::VariableSpecial => 80,
            Self::StringSpecial | Self::Constructor | Self::Parameter => 75,
            Self::Function | Self::Type | Self::Constant | Self::Tag | Self::Label => 70,
            Self::Comment | Self::Keyword | Self::String | Self::Number | Self::Boolean => 60,
            Self::Variable | Self::Operator => 50,
            Self::Punctuation | Self::Embedded => 40,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    pub range: Range<usize>,
    pub kind: HighlightKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightedDocument {
    pub language: LanguageId,
    pub lines: Vec<Vec<HighlightSpan>>,
}

#[derive(Debug, Clone, Copy)]
pub struct HighlightRequest<'a> {
    pub source: &'a str,
    pub path: Option<&'a str>,
    pub fence_tag: Option<&'a str>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HighlightError {
    #[error("the source language is not registered")]
    UnknownLanguage,
    #[error("highlight range {start}..{end} is invalid for a {len}-byte source")]
    InvalidRange {
        start: usize,
        end: usize,
        len: usize,
    },
    #[error("highlight range {start}..{end} is not on UTF-8 boundaries")]
    InvalidUtf8Boundary { start: usize, end: usize },
    #[error("source exceeds the configured highlighting limit")]
    SourceTooLarge,
    #[error("highlight output exceeds the configured span limit")]
    TooManySpans,
    #[error("parser failed: {0}")]
    Parser(String),
    #[error("the {0:?} grammar is not bundled")]
    GrammarUnavailable(LanguageId),
}

impl HighlightedDocument {
    /// Validate, split, and normalize absolute source spans into line-relative spans.
    pub(crate) fn from_absolute_spans(
        language: LanguageId,
        source: &str,
        spans: impl IntoIterator<Item = HighlightSpan>,
    ) -> Result<Self, HighlightError> {
        let starts = line_starts(source);
        let mut lines = vec![Vec::new(); starts.len()];
        for span in spans {
            validate_span(source, &span.range)?;
            if span.range.is_empty() {
                continue;
            }
            let first_line = starts.partition_point(|&start| start <= span.range.start) - 1;
            for (line_ix, &start) in starts.iter().enumerate().skip(first_line) {
                let raw_end = starts.get(line_ix + 1).copied().unwrap_or(source.len());
                let mut end = raw_end;
                if source.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\n') {
                    end -= 1;
                    if source.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\r') {
                        end -= 1;
                    }
                }
                let segment_start = span.range.start.max(start);
                let segment_end = span.range.end.min(end);
                if segment_start < segment_end {
                    lines[line_ix].push(HighlightSpan {
                        range: segment_start - start..segment_end - start,
                        kind: span.kind,
                    });
                }
                if raw_end >= span.range.end {
                    break;
                }
            }
        }
        for line in &mut lines {
            *line = normalize_line(std::mem::take(line));
        }
        Ok(Self { language, lines })
    }
}

fn validate_span(source: &str, range: &Range<usize>) -> Result<(), HighlightError> {
    if range.start > range.end || range.end > source.len() {
        return Err(HighlightError::InvalidRange {
            start: range.start,
            end: range.end,
            len: source.len(),
        });
    }
    if !source.is_char_boundary(range.start) || !source.is_char_boundary(range.end) {
        return Err(HighlightError::InvalidUtf8Boundary {
            start: range.start,
            end: range.end,
        });
    }
    Ok(())
}

fn normalize_line(spans: Vec<HighlightSpan>) -> Vec<HighlightSpan> {
    #[derive(Clone, Copy)]
    enum Edge {
        Start(usize),
        End(usize),
    }

    let mut edges = spans
        .iter()
        .enumerate()
        .flat_map(|(index, span)| {
            [
                (span.range.start, Edge::Start(index)),
                (span.range.end, Edge::End(index)),
            ]
        })
        .collect::<Vec<_>>();
    edges.sort_unstable_by_key(|(offset, _)| *offset);

    // The span index is the tie-breaker so equal-precedence overlaps retain
    // the old `Iterator::max_by_key` behavior (the later span wins).
    let mut active = BTreeSet::new();
    let mut normalized: Vec<HighlightSpan> = Vec::new();
    let mut cursor = 0;
    while cursor < edges.len() {
        let offset = edges[cursor].0;
        let group_start = cursor;
        while cursor < edges.len() && edges[cursor].0 == offset {
            if let Edge::End(index) = edges[cursor].1 {
                active.remove(&(spans[index].kind.precedence(), index));
            }
            cursor += 1;
        }
        for (_, edge) in &edges[group_start..cursor] {
            if let Edge::Start(index) = *edge {
                active.insert((spans[index].kind.precedence(), index));
            }
        }

        let Some(next_offset) = edges.get(cursor).map(|(next, _)| *next) else {
            break;
        };
        if offset == next_offset {
            continue;
        }
        if let Some((_, index)) = active.last().copied() {
            let kind = spans[index].kind;
            if let Some(previous) = normalized.last_mut()
                && previous.kind == kind
                && previous.range.end == offset
            {
                previous.range.end = next_offset;
            } else {
                normalized.push(HighlightSpan {
                    range: offset..next_offset,
                    kind,
                });
            }
        }
    }
    normalized
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        source
            .match_indices('\n')
            .map(|(index, _)| index + 1)
            .filter(|start| *start < source.len()),
    );
    starts
}

/// Whether this build contains a parser and compatible highlight queries.
pub const fn supports_language(language: LanguageId) -> bool {
    let _ = language;
    true
}

/// Highlight a complete document with the default resource limits.
pub fn highlight(request: HighlightRequest<'_>) -> Result<HighlightedDocument, HighlightError> {
    highlight_with_limits(request, HighlightLimits::default(), None)
}

/// Highlight a complete document with explicit limits and cooperative cancellation.
pub(crate) fn highlight_with_limits(
    request: HighlightRequest<'_>,
    limits: HighlightLimits,
    cancellation_flag: Option<&AtomicUsize>,
) -> Result<HighlightedDocument, HighlightError> {
    if request.source.len() > limits.max_source_bytes {
        return Err(HighlightError::SourceTooLarge);
    }
    let language = detect_language(
        request.path,
        request.fence_tag,
        request.source.lines().next(),
    )
    .ok_or(HighlightError::UnknownLanguage)?;
    if !supports_language(language) {
        return Err(HighlightError::GrammarUnavailable(language));
    }

    let mut primary_configuration = configuration(language)?;
    primary_configuration.configure(CAPTURE_NAMES);
    let injected = if matches!(language, LanguageId::Html | LanguageId::Markdown) {
        injected_languages(language)
            .into_iter()
            .filter_map(|language| {
                let mut config = configuration(language).ok()?;
                config.configure(CAPTURE_NAMES);
                Some((language, config))
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(
            &primary_configuration,
            request.source.as_bytes(),
            cancellation_flag,
            |name| {
                let language = language_for_alias(name)?;
                injected
                    .iter()
                    .find(|(candidate, _)| *candidate == language)
                    .map(|(_, config)| config)
            },
        )
        .map_err(|error| HighlightError::Parser(error.to_string()))?;

    let mut active = Vec::new();
    let mut spans = Vec::new();
    for event in events {
        match event.map_err(|error| HighlightError::Parser(error.to_string()))? {
            HighlightEvent::HighlightStart(highlight) => active.push(CAPTURE_KINDS[highlight.0]),
            HighlightEvent::HighlightEnd => {
                active.pop();
            }
            HighlightEvent::Source { start, end } => {
                if let Some(kind) = active.iter().copied().max_by_key(|kind| kind.precedence()) {
                    spans.push(HighlightSpan {
                        range: start..end,
                        kind,
                    });
                    if spans.len() > limits.max_spans {
                        return Err(HighlightError::TooManySpans);
                    }
                }
            }
        }
    }
    HighlightedDocument::from_absolute_spans(language, request.source, spans)
}

// Ordered from generic to specific. `HighlightConfiguration::configure`
// resolves dotted captures to the best recognized name in this table.
const CAPTURE_NAMES: &[&str] = &[
    "comment",
    "keyword",
    "string",
    "string.special",
    "string.escape",
    "number",
    "boolean",
    "type",
    "type.builtin",
    "constructor",
    "function",
    "function.builtin",
    "function.macro",
    "property",
    "constant",
    "variable",
    "variable.builtin",
    "variable.parameter",
    "operator",
    "punctuation",
    "tag",
    "attribute",
    "label",
    "embedded",
    "error",
];

const CAPTURE_KINDS: &[HighlightKind] = &[
    HighlightKind::Comment,
    HighlightKind::Keyword,
    HighlightKind::String,
    HighlightKind::StringSpecial,
    HighlightKind::Escape,
    HighlightKind::Number,
    HighlightKind::Boolean,
    HighlightKind::Type,
    HighlightKind::TypeBuiltin,
    HighlightKind::Constructor,
    HighlightKind::Function,
    HighlightKind::FunctionBuiltin,
    HighlightKind::Macro,
    HighlightKind::Property,
    HighlightKind::Constant,
    HighlightKind::Variable,
    HighlightKind::VariableSpecial,
    HighlightKind::Parameter,
    HighlightKind::Operator,
    HighlightKind::Punctuation,
    HighlightKind::Tag,
    HighlightKind::Attribute,
    HighlightKind::Label,
    HighlightKind::Embedded,
    HighlightKind::Invalid,
];

#[cfg(test)]
mod tests;
