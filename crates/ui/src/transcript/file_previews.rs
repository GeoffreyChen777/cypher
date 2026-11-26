//! File previews in replies ([`crate::markdown::file_card`]): loading them
//! for the selected chat, and where a click takes them.
//!
//! Files are read like images ([`super::images`]): through the engine's
//! chunked file read, looked up in the chat's folder, then each root it may
//! read, so a session on another device previews its own files. What is
//! read stops at the kind's cap ([`Kind::max_bytes`]); a table reads only
//! its head. A video's frame needs the file on disk, so only a chat on this
//! device gets one; elsewhere (and on Linux, see [`file_thumb`]) a PDF or
//! video shows its name and size.
//!
//! A click opens an html page in the artifact viewer, a PDF or video on
//! this device in the system's app for it, and anything else in Files.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Context, Image, ImageFormat, SharedString, Task, Window};

use crate::files::FilesContext;
use crate::markdown::artifact::{self, ArtifactOpen};
use crate::markdown::file_card::{self, Body, FilePreviewUi, Loaded, Picture, Shown};
use crate::markdown::file_preview::{self, Kind};
use crate::markdown::file_thumb;
use crate::markdown::image::Key;
use crate::markdown::image_source;
use crate::markdown::parser::{ArtifactKind, Block};
use crate::markdown::preview_size;

use super::Transcript;

/// One place a file may be: its Files context, the path there, the path
/// the link handler opens and, for a chat on this device, the file itself.
struct Place {
    context: FilesContext,
    relative: String,
    open: String,
    local: Option<PathBuf>,
}

/// Where a click on a loaded preview goes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Destination {
    /// The artifact viewer, with the page's source.
    Page,
    /// The system's app for the file on this device.
    System(PathBuf),
    /// The Files browser, at the file.
    Files(String),
}

/// A click's destination: pages open as artifacts, a PDF or video on this
/// device in its own app, the rest in Files.
fn destination(loaded: &Loaded) -> Destination {
    if matches!(loaded.body, Body::Page { .. }) {
        return Destination::Page;
    }
    match (&loaded.local, loaded.kind) {
        (Some(path), Kind::Pdf | Kind::Video) => Destination::System(path.clone()),
        _ => Destination::Files(loaded.open.clone()),
    }
}

impl Transcript {
    /// The file preview wiring for this transcript's rows.
    pub(super) fn file_preview_ui(&self, cx: &mut Context<Self>) -> FilePreviewUi {
        let scope = self.image_scope(cx);
        let entity = cx.weak_entity();
        FilePreviewUi {
            column: self.preview_column(cx),
            scope,
            on_open: Rc::new(move |key: Key, window: &mut Window, cx: &mut App| {
                let Shown::Ready(loaded) = file_card::lookup(&key) else {
                    return;
                };
                match destination(&loaded) {
                    Destination::Page => {
                        let Body::Page { title, code } = &loaded.body else {
                            return;
                        };
                        let open = ArtifactOpen {
                            key: format!("file:{}", loaded.open).into(),
                            kind: match loaded.kind {
                                Kind::Svg => ArtifactKind::Svg,
                                _ => ArtifactKind::Html,
                            },
                            title: title.clone().unwrap_or_else(|| loaded.name.clone()),
                            named: false,
                            code: code.clone(),
                        };
                        let handler = entity
                            .read_with(cx, |this, _| this.artifact_handler.clone())
                            .ok()
                            .flatten();
                        if let Some(handler) = handler {
                            handler(open, window, cx);
                        }
                    }
                    Destination::System(path) => cx.open_with_system(&path),
                    Destination::Files(path) => {
                        let handler = entity
                            .read_with(cx, |this, _| this.link_handler.clone())
                            .ok()
                            .flatten();
                        if let Some(handler) = handler {
                            let url = super::code_ref::file_url(&path, None);
                            window.defer(cx, move |window, cx| {
                                handler(&url, window, cx);
                            });
                        }
                    }
                }
            }),
        }
    }

    /// Start loading the file preview `block` is, if it is one; its row
    /// remeasures when it lands. The streaming tail of a live reply waits.
    /// Pages and frames draw for the column at the window's `scale` factor.
    pub(super) fn request_file_previews(
        &mut self,
        block: &Block,
        row_id: &SharedString,
        streaming_tail: bool,
        scale: f32,
        cx: &mut Context<Self>,
    ) {
        if streaming_tail {
            return;
        }
        let mut previews = Vec::new();
        sources(block, &mut previews);
        if previews.is_empty() {
            return;
        }
        let scope = self.image_scope(cx);
        let width = self.preview_width(cx);
        for (src, kind) in previews {
            let key = Key {
                scope: scope.clone(),
                src: SharedString::from(src.to_owned()),
            };
            let row = row_id.clone();
            file_card::request(
                key,
                width,
                cx,
                |cx| self.load_file_preview(src, kind, width, scale, cx),
                move |this: &mut Self, cx| this.remeasure_row(&row, cx),
            );
        }
    }

    fn load_file_preview(
        &self,
        src: &str,
        kind: Kind,
        width: f32,
        scale: f32,
        cx: &mut Context<Self>,
    ) -> Task<Result<Loaded, SharedString>> {
        let state = self.state.read(cx);
        let contexts = crate::files::link_contexts(state);
        let engine = state.engine().cloned();
        let on_this_device = state
            .selected_chat_row()
            .is_some_and(|chat| state.local_device_id.as_deref() == Some(chat.device_id.as_str()));
        let home = super::images::chat_home(state);
        let cwd = state.selected_chat_row().and_then(|chat| chat.cwd.clone());
        let (Ok(contexts), Some(engine)) = (contexts, engine) else {
            return Task::ready(Err("not connected to the session's files".into()));
        };
        let Some(path) = image_source::local_path(src) else {
            return Task::ready(Err("unsupported".into()));
        };
        let roots: Vec<&str> = contexts.contexts.iter().map(|c| c.root.as_str()).collect();
        let candidates = match image_source::local_candidates(
            &path,
            &image_source::Context {
                cwd: cwd.as_deref(),
                roots: &roots,
                home: home.as_deref(),
            },
        ) {
            Ok(candidates) => candidates,
            Err(_) => return Task::ready(Err("outside the session's folders".into())),
        };
        let places: Vec<Place> = candidates
            .into_iter()
            .filter_map(|candidate| {
                let (context, relative, open) =
                    super::images::locate(&contexts, &candidate.root, candidate.path)?;
                let local = on_this_device.then(|| {
                    PathBuf::from(format!(
                        "{}/{}",
                        context.root.trim_end_matches('/'),
                        relative
                    ))
                });
                Some(Place {
                    context,
                    relative,
                    open,
                    local,
                })
            })
            .collect();
        let name: SharedString = file_card::file_name(src).into();
        cx.spawn(async move |_, cx| {
            let mut reason: SharedString = "not found".into();
            // A table reads its head; a video only its size here (its frame
            // comes from the file itself); the rest read whole, up to the cap.
            let (limit, head) = match kind {
                Kind::Csv | Kind::Tsv => (kind.max_bytes(), true),
                Kind::Video => (1, true),
                _ => (kind.max_bytes(), false),
            };
            for place in places {
                let read = crate::files::preview::read_bytes(
                    &engine,
                    place.context.params(&place.relative),
                    limit,
                    head,
                    |_, _| {},
                )
                .await;
                let (bytes, size) = match read {
                    Ok(read) => read,
                    Err(message) if message.starts_with("Too large") => {
                        return Err(too_large(kind));
                    }
                    Err(message) => {
                        if message.contains("needs an update") {
                            reason = message;
                        }
                        continue;
                    }
                };
                if kind == Kind::Video && size > kind.max_bytes() {
                    return Err(too_large(kind));
                }
                let local = place.local.clone();
                let body = cx
                    .background_executor()
                    .spawn(async move { body(kind, bytes, size, local.as_deref(), width, scale) })
                    .await;
                let sharp_to = match body {
                    Body::Frame(_) | Body::Pages { .. } => width,
                    _ => f32::INFINITY,
                };
                return Ok(Loaded {
                    kind,
                    name,
                    size,
                    body,
                    open: place.open,
                    local: place.local,
                    sharp_to,
                });
            }
            Err(reason)
        })
    }
}

/// Every file preview `block` holds, nested ones included.
fn sources<'a>(block: &'a Block, out: &mut Vec<(&'a str, Kind)>) {
    match block {
        Block::FilePreview { src, kind } => out.push((src, *kind)),
        Block::BlockQuote { children } => children.iter().for_each(|b| sources(b, out)),
        Block::List { items, .. } => items.iter().flatten().for_each(|b| sources(b, out)),
        _ => {}
    }
}

/// What a read file shows (off the UI thread): `bytes` is what was read of
/// it, `size` its whole size, `local` the file on this device; pages and
/// frames draw sharp for a column up to `width` points at `scale`.
fn body(
    kind: Kind,
    bytes: Vec<u8>,
    size: u64,
    local: Option<&std::path::Path>,
    width: f32,
    scale: f32,
) -> Body {
    let scale = f64::from(scale.max(1.0));
    let pixels = f64::from(width) * scale;
    match kind {
        Kind::Csv | Kind::Tsv => {
            let delimiter = kind.delimiter().unwrap_or(',');
            let text = String::from_utf8_lossy(&bytes);
            let complete = bytes.len() as u64 >= size;
            Body::Table(Arc::new(file_preview::parse_table(
                &text, delimiter, complete,
            )))
        }
        Kind::Pdf => {
            // Each page as wide as it gets in any row up to this column.
            let drawn =
                file_thumb::pdf_pages(&bytes, preview_size::PDF_ROW_MAX, |aspects, count| {
                    let aspects: Vec<f32> = aspects.iter().map(|a| *a as f32).collect();
                    preview_size::pdf_draw_widths(width, &aspects, count)
                        .into_iter()
                        .map(|points| f64::from(points) * scale)
                        .collect()
                });
            let Some(drawn) = drawn else {
                return Body::Plain;
            };
            // A page's shape is its own size in points, not its bitmap's
            // (rounded to whole pixels).
            let pages: Vec<Picture> = drawn
                .pages
                .into_iter()
                .zip(drawn.sizes)
                .map_while(|(png, (w, h))| {
                    picture(png).map(|picture| Picture {
                        width: w as f32,
                        height: h as f32,
                        ..picture
                    })
                })
                .collect();
            if pages.is_empty() {
                return Body::Plain;
            }
            Body::Pages {
                pages,
                count: drawn.count,
            }
        }
        Kind::Video => local
            .and_then(|path| file_thumb::video_frame(path, pixels))
            .and_then(picture)
            .map_or(Body::Plain, Body::Frame),
        Kind::Html | Kind::Svg => {
            let code = String::from_utf8_lossy(&bytes).into_owned();
            Body::Page {
                title: artifact::page_title(&code).map(SharedString::from),
                code: code.into(),
            }
        }
    }
}

/// A drawn page or frame from PNG bytes (`None`: not a PNG).
fn picture(png: Vec<u8>) -> Option<Picture> {
    let (width, height) = crate::files::preview::image_dims(&png)?;
    if width == 0 || height == 0 {
        return None;
    }
    Some(Picture {
        image: Arc::new(Image::from_bytes(ImageFormat::Png, png)),
        width: width as f32,
        height: height as f32,
    })
}

fn too_large(kind: Kind) -> SharedString {
    format!("too large (over {} MB)", kind.max_bytes() / (1024 * 1024)).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(kind: Kind, body: Body, local: Option<&str>) -> Loaded {
        Loaded {
            kind,
            name: "f".into(),
            size: 1,
            body,
            open: "out/f".into(),
            local: local.map(PathBuf::from),
            sharp_to: f32::INFINITY,
        }
    }

    #[test]
    fn clicks_go_where_the_file_can_show() {
        let page = Body::Page {
            title: None,
            code: "<p>".into(),
        };
        assert_eq!(
            destination(&loaded(Kind::Html, page, None)),
            Destination::Page
        );
        assert_eq!(
            destination(&loaded(Kind::Pdf, Body::Plain, Some("/w/f.pdf"))),
            Destination::System("/w/f.pdf".into())
        );
        assert_eq!(
            destination(&loaded(Kind::Video, Body::Plain, None)),
            Destination::Files("out/f".into())
        );
        let table = Body::Table(Arc::new(file_preview::Table::default()));
        assert_eq!(
            destination(&loaded(Kind::Csv, table, Some("/w/f.csv"))),
            Destination::Files("out/f".into())
        );
    }

    #[test]
    fn a_table_counts_its_rows_as_read() {
        let Body::Table(table) = body(Kind::Csv, b"a,b\n1,2\n3,4\n".to_vec(), 12, None, 600.0, 2.0)
        else {
            panic!("a table");
        };
        assert_eq!(table.count_label(), "2 rows");
        // Only the head was read: the last record may be cut, so it goes.
        let Body::Table(table) = body(
            Kind::Tsv,
            b"a\tb\n1\t2\n3\t".to_vec(),
            4096,
            None,
            600.0,
            2.0,
        ) else {
            panic!("a table");
        };
        assert_eq!(table.count_label(), "1+ rows");
    }

    #[test]
    fn pages_carry_their_title_and_source() {
        let html = b"<html><title>Q3 report</title><body>hi</body></html>".to_vec();
        let Body::Page { title, code } = body(Kind::Html, html, 52, None, 600.0, 2.0) else {
            panic!("a page");
        };
        assert_eq!(title.as_deref(), Some("Q3 report"));
        assert!(code.contains("<body>hi"));
    }

    #[test]
    fn no_picture_reads_as_plain() {
        assert!(picture(b"not a png".to_vec()).is_none());
        assert!(matches!(
            body(Kind::Video, vec![0], 10, None, 600.0, 2.0),
            Body::Plain
        ));
        assert!(matches!(
            body(Kind::Pdf, b"not a pdf".to_vec(), 9, None, 600.0, 2.0),
            Body::Plain
        ));
    }

    #[test]
    fn nested_previews_are_found() {
        let block = Block::BlockQuote {
            children: vec![Block::FilePreview {
                src: "a.csv".into(),
                kind: Kind::Csv,
            }],
        };
        let mut out = Vec::new();
        sources(&block, &mut out);
        assert_eq!(out, vec![("a.csv", Kind::Csv)]);
    }
}
