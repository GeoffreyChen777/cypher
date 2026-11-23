//! Which files a chat reply previews beyond images, and how a csv/tsv head
//! becomes a small table.
//!
//! The rules are shared with the iPhone app through one table,
//! `protocol/vectors/file-previews-v1.json`, which both test suites run (its
//! `description` is the spec):
//!
//! - A path (cleaned and resolved like a local image path, see
//!   [`super::image_source::local_path`]) previews by its extension, any case:
//!   csv, tsv/tab, pdf, mp4/mov/m4v/webm, html/htm and svg. URLs never do.
//! - Only the head of a csv/tsv file is read ([`TABLE_READ_BYTES`]); the table
//!   shows [`TABLE_ROWS`] data rows with cells clipped to [`COLUMN_CHARS`],
//!   and the row count reads "N+" when the head was all that was read.
//! - PDFs, videos and html/svg pages above their caps show a placeholder.

use super::image_source;

/// Only this much of the head of a csv/tsv file is read.
pub const TABLE_READ_BYTES: u64 = 256 * 1024;
/// Data rows a table preview shows.
pub const TABLE_ROWS: usize = 20;
/// The most characters (Unicode scalar values) a shown cell has.
pub const COLUMN_CHARS: usize = 40;
/// Larger PDFs show a "too large" placeholder.
pub const PDF_MAX_BYTES: u64 = 50 * 1024 * 1024;
/// Larger videos show a "too large" placeholder.
pub const VIDEO_MAX_BYTES: u64 = 200 * 1024 * 1024;
/// Larger html and svg files show a "too large" placeholder.
pub const HTML_MAX_BYTES: u64 = 5 * 1024 * 1024;

/// What kind of preview a file gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Csv,
    Tsv,
    Pdf,
    Video,
    Html,
    Svg,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Pdf => "pdf",
            Self::Video => "video",
            Self::Html => "html",
            Self::Svg => "svg",
        }
    }

    /// By file extension, any case.
    pub fn for_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "csv" => Some(Self::Csv),
            "tsv" | "tab" => Some(Self::Tsv),
            "pdf" => Some(Self::Pdf),
            "mp4" | "mov" | "m4v" | "webm" => Some(Self::Video),
            "html" | "htm" => Some(Self::Html),
            "svg" => Some(Self::Svg),
            _ => None,
        }
    }

    /// The table delimiter, for csv and tsv.
    pub fn delimiter(self) -> Option<char> {
        match self {
            Self::Csv => Some(','),
            Self::Tsv => Some('\t'),
            _ => None,
        }
    }

    /// The most bytes a file of this kind may be (for a table: how much of
    /// its head is read).
    pub fn max_bytes(self) -> u64 {
        match self {
            Self::Csv | Self::Tsv => TABLE_READ_BYTES,
            Self::Pdf => PDF_MAX_BYTES,
            Self::Video => VIDEO_MAX_BYTES,
            Self::Html | Self::Svg => HTML_MAX_BYTES,
        }
    }
}

/// The preview kind of a source (a path as written in a reply), if any.
pub fn kind(src: &str) -> Option<Kind> {
    let path = image_source::local_path(src)?;
    image_source::extension(&path).and_then(Kind::for_extension)
}

/// The file a whole reply paragraph names, given as its Markdown source:
/// one path, bare, in backticks or in `<…>` (the image-line token rules),
/// that previews here. An svg is left to the image rules (it shows as a
/// picture), so `None` for it as for anything else.
pub fn file_line(line: &str) -> Option<String> {
    let token = image_source::line_token(line)?;
    match kind(token)? {
        Kind::Svg => None,
        _ => Some(token.to_owned()),
    }
}

/// A csv/tsv head as a preview table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Table {
    /// The header's cells, when the first record is a header.
    pub columns: Option<Vec<String>>,
    /// The first [`TABLE_ROWS`] data rows, padded to one width.
    pub rows: Vec<Vec<String>>,
    /// Data rows in the text read (the header excluded).
    pub total_rows: usize,
    /// Only the head of the file was read: the count is a lower bound.
    pub truncated: bool,
}

impl Table {
    /// "1 row", "12 rows", "20+ rows".
    pub fn count_label(&self) -> String {
        let plus = if self.truncated { "+" } else { "" };
        let noun = if self.total_rows == 1 && !self.truncated {
            "row"
        } else {
            "rows"
        };
        format!("{}{plus} {noun}", self.total_rows)
    }

    /// Columns in every shown row.
    pub fn width(&self) -> usize {
        self.rows
            .first()
            .map(Vec::len)
            .or_else(|| self.columns.as_ref().map(Vec::len))
            .unwrap_or(0)
    }
}

/// Parse `text` (what was read of a file) as a table; `complete` is false
/// when it is only the head of the file.
pub fn parse_table(text: &str, delimiter: char, complete: bool) -> Table {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut records = records(text, delimiter, complete);
    records.retain(|record| !(record.len() == 1 && record[0].is_empty()));
    let records: Vec<Vec<String>> = records
        .into_iter()
        .map(|record| record.iter().map(|cell| display(cell)).collect())
        .collect();
    let header = records.len() >= 2 && is_header(&records[0]);
    let (columns, data) = if header {
        (Some(records[0].clone()), &records[1..])
    } else {
        (None, &records[..])
    };
    let mut rows: Vec<Vec<String>> = data.iter().take(TABLE_ROWS).cloned().collect();
    let width = rows
        .iter()
        .chain(columns.iter())
        .map(Vec::len)
        .max()
        .unwrap_or(0);
    let pad = |row: &mut Vec<String>| {
        row.resize(width, String::new());
        for cell in row.iter_mut() {
            *cell = clip(cell);
        }
    };
    rows.iter_mut().for_each(pad);
    let columns = columns.map(|mut columns| {
        pad(&mut columns);
        columns
    });
    Table {
        columns,
        rows,
        total_rows: data.len(),
        truncated: !complete,
    }
}

/// RFC 4180 records, raw cells (quotes resolved, line breaks kept).
fn records(text: &str, delimiter: char, complete: bool) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut at_field_start = true;
    let mut in_quotes = false;
    // Whether the text ended right after a record break outside quotes.
    let mut ended_on_break = true;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        ended_on_break = false;
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        if c == '"' && at_field_start {
            in_quotes = true;
            at_field_start = false;
        } else if c == delimiter {
            record.push(std::mem::take(&mut field));
            at_field_start = true;
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            record.push(std::mem::take(&mut field));
            records.push(std::mem::take(&mut record));
            at_field_start = true;
            ended_on_break = true;
        } else {
            field.push(c);
            at_field_start = false;
        }
    }
    if !ended_on_break && complete {
        record.push(field);
        records.push(record);
    }
    records
}

/// A cell as shown: line breaks as spaces, trimmed.
fn display(cell: &str) -> String {
    cell.replace("\r\n", " ")
        .replace(['\r', '\n'], " ")
        .trim()
        .to_owned()
}

fn clip(cell: &str) -> String {
    if cell.chars().count() <= COLUMN_CHARS {
        return cell.to_owned();
    }
    let mut out: String = cell.chars().take(COLUMN_CHARS - 1).collect();
    out.push('…');
    out
}

fn is_header(first: &[String]) -> bool {
    first
        .iter()
        .enumerate()
        .all(|(at, cell)| !cell.is_empty() && !is_numeric(cell) && !first[..at].contains(cell))
}

/// `-1.5`, `.5`, `+2e10`, `12%`; not `1,000`.
pub fn is_numeric(cell: &str) -> bool {
    let s = cell.strip_suffix('%').unwrap_or(cell);
    let s = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (mantissa, exponent) = match s.find(['e', 'E']) {
        Some(at) => (&s[..at], Some(&s[at + 1..])),
        None => (s, None),
    };
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (mantissa, None),
    };
    let digits = |part: &str| part.chars().all(|c| c.is_ascii_digit());
    let fraction = fraction.unwrap_or_default();
    let mantissa_ok =
        digits(whole) && digits(fraction) && !(whole.is_empty() && fraction.is_empty());
    let exponent_ok = exponent.is_none_or(|exp| {
        let exp = exp.strip_prefix(['+', '-']).unwrap_or(exp);
        !exp.is_empty() && digits(exp)
    });
    mantissa_ok && exponent_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn vectors() -> Value {
        serde_json::from_str(include_str!(
            "../../../../protocol/vectors/file-previews-v1.json"
        ))
        .unwrap()
    }

    #[test]
    fn shared_caps() {
        let vectors = vectors();
        assert_eq!(vectors["version"], 1);
        let caps = &vectors["caps"];
        assert_eq!(caps["tableReadBytes"], TABLE_READ_BYTES);
        assert_eq!(caps["tableRows"], TABLE_ROWS);
        assert_eq!(caps["columnChars"], COLUMN_CHARS);
        assert_eq!(caps["pdfMaxBytes"], PDF_MAX_BYTES);
        assert_eq!(caps["videoMaxBytes"], VIDEO_MAX_BYTES);
        assert_eq!(caps["htmlMaxBytes"], HTML_MAX_BYTES);
        assert_eq!(caps.as_object().unwrap().len(), 6);
    }

    #[test]
    fn shared_kinds() {
        let vectors = vectors();
        let cases = vectors["kinds"].as_array().unwrap();
        assert!(cases.len() > 20);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let path = case["path"].as_str().unwrap();
            let got = kind(path).map_or("none", Kind::as_str);
            assert_eq!(got, case["expect"]["kind"], "{name}: {path}");
        }
    }

    #[test]
    fn shared_tables() {
        let vectors = vectors();
        let cases = vectors["table"].as_array().unwrap();
        assert!(cases.len() > 20);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let text = case["text"].as_str().unwrap();
            let delimiter = case["delimiter"].as_str().unwrap();
            assert_eq!(delimiter.chars().count(), 1, "{name}");
            let complete = case["complete"].as_bool().unwrap_or(true);
            let table = parse_table(text, delimiter.chars().next().unwrap(), complete);
            let mut got = json!({
                "header": table.columns.is_some(),
                "rows": table.rows,
                "totalRows": table.total_rows,
                "truncated": table.truncated,
            });
            if let Some(columns) = &table.columns {
                got["columns"] = json!(columns);
            }
            assert_eq!(got, case["expect"], "{name}");
        }
    }

    #[test]
    fn shared_lines() {
        let vectors = vectors();
        let cases = vectors["lines"].as_array().unwrap();
        assert!(cases.len() > 6);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let line = case["line"].as_str().unwrap();
            assert_eq!(
                file_line(line).as_deref(),
                case["src"].as_str(),
                "{name}: {line}"
            );
        }
    }

    #[test]
    fn count_labels() {
        let table = |total_rows, truncated| Table {
            total_rows,
            truncated,
            ..Table::default()
        };
        assert_eq!(table(1, false).count_label(), "1 row");
        assert_eq!(table(12, false).count_label(), "12 rows");
        assert_eq!(table(20, true).count_label(), "20+ rows");
        assert_eq!(table(1, true).count_label(), "1+ rows");
    }

    #[test]
    fn kinds_carry_their_caps_and_delimiters() {
        assert_eq!(Kind::Csv.delimiter(), Some(','));
        assert_eq!(Kind::Tsv.delimiter(), Some('\t'));
        assert_eq!(Kind::Pdf.delimiter(), None);
        assert_eq!(Kind::Csv.max_bytes(), TABLE_READ_BYTES);
        assert_eq!(Kind::Svg.max_bytes(), HTML_MAX_BYTES);
        assert_eq!(Kind::Video.max_bytes(), VIDEO_MAX_BYTES);
    }

    #[test]
    fn numbers() {
        for yes in ["0", "-1.5", ".5", "3.", "+2e10", "3E-2", "12%", "-0.5%"] {
            assert!(is_numeric(yes), "{yes}");
        }
        for no in [
            "",
            "-",
            ".",
            "e5",
            "1e",
            "1,000",
            "1.2.3",
            "abc",
            "%",
            "2024-01-01",
        ] {
            assert!(!is_numeric(no), "{no}");
        }
    }
}
