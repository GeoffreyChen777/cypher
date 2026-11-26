//! File preview blocks in chat replies ([`Block::FilePreview`]): a paragraph
//! that is only a path to a csv/tsv, pdf, video or html file
//! ([`super::file_preview`]). A table shows its first rows, a PDF its first
//! pages and a video a poster frame (macOS), each the column's full width
//! ([`super::preview_size`]), an html page a full-width card that opens it
//! in the artifact viewer. Anything that can't show reads as a neutral chip
//! saying why.
//!
//! Loaded previews live in a store keyed like images (chat, source); the
//! surface loads them ([`request`]) and rendering only looks up.
//!
//! [`Block::FilePreview`]: super::parser::Block::FilePreview

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, EntityId, Image, SharedString, Task, Window, div, prelude::*, px,
};

use super::file_preview::{Kind, Table};
use super::image::Key;
use super::render::BlockCtx;
use crate::kit::icons::{self, icon};
use crate::kit::theme::{MonoStyled as _, hairline, ink};

const RADIUS: f32 = 8.0;
/// Space between a PDF's stacked pages.
const PAGE_GAP: f32 = 8.0;
/// Pixel width a cell's character takes in the table's monospace face.
const CHAR_W: f32 = 7.0;
const MAX_READY: usize = 32;
const RETRY_AFTER: Duration = Duration::from_secs(20);

/// A drawn page or frame and its pixel size (only its aspect matters: it
/// shows column wide).
#[derive(Clone)]
pub struct Picture {
    pub image: Arc<Image>,
    pub width: f32,
    pub height: f32,
}

/// What a loaded preview shows.
#[derive(Clone)]
pub enum Body {
    Table(Arc<Table>),
    /// A video's frame, sized like an image (a tall one cropped).
    Frame(Picture),
    /// A PDF's first pages, stacked uncropped, and how many it has.
    Pages {
        pages: Vec<Picture>,
        count: usize,
    },
    /// An html page: its `<title>` and source (for the artifact viewer).
    Page {
        title: Option<SharedString>,
        code: SharedString,
    },
    /// Only the name and size (no thumbnail on this platform).
    Plain,
}

/// A loaded preview.
#[derive(Clone)]
pub struct Loaded {
    pub kind: Kind,
    pub name: SharedString,
    pub size: u64,
    pub body: Body,
    /// The path the chat's link handler opens.
    pub open: String,
    /// The file itself, when the chat is on this device.
    pub local: Option<std::path::PathBuf>,
    /// The widest column (points) its pictures are drawn sharp for; a wider
    /// one draws them again ([`request`]). `f32::INFINITY` without any.
    pub sharp_to: f32,
}

#[derive(Clone)]
pub enum Shown {
    Pending,
    Ready(Loaded),
    Failed(SharedString),
}

type Waiter = Box<dyn FnOnce(&mut App)>;

enum State {
    Pending(Vec<(EntityId, Waiter)>),
    Ready(Loaded),
    /// Shown as it is while it draws again for a wider column.
    Sharpening(Loaded, Vec<(EntityId, Waiter)>),
    Failed(SharedString, Instant),
}

struct Entry {
    state: State,
    used: u64,
}

#[derive(Default)]
struct Store {
    entries: HashMap<Key, Entry>,
    clock: u64,
}

thread_local! {
    static STORE: RefCell<Store> = RefCell::new(Store::default());
}

/// What `key` shows now.
pub fn lookup(key: &Key) -> Shown {
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        store.clock += 1;
        let now = store.clock;
        match store.entries.get_mut(key) {
            Some(entry) => {
                entry.used = now;
                match &entry.state {
                    State::Pending(_) => Shown::Pending,
                    State::Ready(loaded) | State::Sharpening(loaded, _) => {
                        Shown::Ready(loaded.clone())
                    }
                    State::Failed(reason, _) => Shown::Failed(reason.clone()),
                }
            }
            None => Shown::Pending,
        }
    })
}

/// Load `key` with `load` unless it is loaded, loading, or failed moments
/// ago; one drawn for a narrower column than `width` (points, see
/// [`Loaded::sharp_to`]) loads again, showing meanwhile. `on_ready` runs
/// once per asking entity when it lands.
pub fn request<T: 'static>(
    key: Key,
    width: f32,
    cx: &mut Context<T>,
    load: impl FnOnce(&mut Context<T>) -> Task<Result<Loaded, SharedString>>,
    on_ready: impl FnOnce(&mut T, &mut Context<T>) + 'static,
) {
    let entity = cx.entity_id();
    let weak = cx.weak_entity();
    let waiter: Waiter = Box::new(move |cx: &mut App| {
        weak.update(cx, |this, cx| on_ready(this, cx)).ok();
    });
    let start = STORE.with(|store| {
        let mut store = store.borrow_mut();
        store.clock += 1;
        let now = store.clock;
        match store.entries.get_mut(&key) {
            Some(entry) => {
                entry.used = now;
                match &mut entry.state {
                    State::Pending(waiters) | State::Sharpening(_, waiters) => {
                        if !waiters.iter().any(|(id, _)| *id == entity) {
                            waiters.push((entity, waiter));
                        }
                        false
                    }
                    State::Ready(loaded) if loaded.sharp_to < width => {
                        entry.state = State::Sharpening(loaded.clone(), vec![(entity, waiter)]);
                        true
                    }
                    State::Failed(_, at) if at.elapsed() > RETRY_AFTER => {
                        entry.state = State::Pending(vec![(entity, waiter)]);
                        true
                    }
                    _ => false,
                }
            }
            None => {
                store.entries.insert(
                    key.clone(),
                    Entry {
                        state: State::Pending(vec![(entity, waiter)]),
                        used: now,
                    },
                );
                true
            }
        }
    });
    if !start {
        return;
    }
    let task = load(cx);
    cx.spawn(async move |_, cx| {
        let loaded = task.await;
        cx.update(|cx| land(key, loaded, cx));
    })
    .detach();
}

fn land(key: Key, loaded: Result<Loaded, SharedString>, cx: &mut App) {
    let waiters = STORE.with(|store| {
        let mut store = store.borrow_mut();
        let waiters = match store.entries.get_mut(&key) {
            Some(entry) => {
                let state = match (loaded, &entry.state) {
                    (Ok(loaded), _) => State::Ready(loaded),
                    // A failed redraw keeps what showed.
                    (Err(_), State::Sharpening(shown, _)) => State::Ready(shown.clone()),
                    (Err(reason), _) => State::Failed(reason, Instant::now()),
                };
                match std::mem::replace(&mut entry.state, state) {
                    State::Pending(waiters) | State::Sharpening(_, waiters) => waiters,
                    _ => Vec::new(),
                }
            }
            None => Vec::new(),
        };
        let ready: Vec<u64> = store
            .entries
            .values()
            .filter(|entry| matches!(entry.state, State::Ready(_)))
            .map(|entry| entry.used)
            .collect();
        if ready.len() > MAX_READY {
            let mut ages = ready;
            ages.sort_unstable();
            let cutoff = ages[ages.len() - MAX_READY - 1];
            store
                .entries
                .retain(|_, entry| !matches!(entry.state, State::Ready(_)) || entry.used > cutoff);
        }
        waiters
    });
    for (_, waiter) in waiters {
        waiter(cx);
    }
}

/// Opens a clicked preview (the surface decides where: the Files browser,
/// the system viewer, the artifact viewer).
pub type OpenHandler = Rc<dyn Fn(Key, &mut Window, &mut App)>;

/// What a surface does with file previews.
#[derive(Clone)]
pub struct FilePreviewUi {
    pub scope: SharedString,
    pub on_open: OpenHandler,
}

/// The file name a source shows.
pub fn file_name(src: &str) -> String {
    let path = super::image_source::local_path(src).unwrap_or_else(|| src.to_owned());
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(src)
        .to_owned()
}

/// `12 KB`, `3.4 MB`.
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / KB / KB)
    } else {
        format!("{:.1} GB", b / KB / KB / KB)
    }
}

fn kind_icon(kind: Kind) -> &'static str {
    match kind {
        Kind::Csv | Kind::Tsv => icons::DATABASE,
        Kind::Pdf => icons::DOCUMENT,
        Kind::Video => icons::PLAY,
        Kind::Html | Kind::Svg => icons::CODE,
    }
}

/// A file preview block.
pub fn render(src: &str, kind: Kind, ctx: BlockCtx) -> AnyElement {
    let BlockCtx {
        ix, opts, theme, ..
    } = ctx;
    let name = file_name(src);
    let Some(ui) = opts.file_previews.clone() else {
        return chip(kind, &name, None, theme).into_any_element();
    };
    let key = Key {
        scope: ui.scope.clone(),
        src: SharedString::from(src.to_owned()),
    };
    let id = SharedString::from(format!("{}-file{ix}", opts.row_key));
    let on_open = ui.on_open.clone();
    let open_key = key.clone();
    let clickable = move |el: gpui::Stateful<gpui::Div>| {
        el.cursor_pointer()
            .on_click(move |_, window, cx| on_open(open_key.clone(), window, cx))
    };
    match lookup(&key) {
        Shown::Pending => chip(kind, &name, Some("Loading…".into()), theme).into_any_element(),
        Shown::Failed(reason) => chip(kind, &name, Some(reason), theme).into_any_element(),
        Shown::Ready(loaded) => match &loaded.body {
            Body::Table(table) => {
                clickable(table_block(table, &loaded, theme).id(id)).into_any_element()
            }
            Body::Frame(frame) => {
                let play = (loaded.kind == Kind::Video).then(|| {
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .size(px(40.0))
                                .rounded_full()
                                .bg(gpui::black().opacity(0.45))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon(icons::PLAY).size(px(18.0)).text_color(gpui::white())),
                        )
                });
                clickable(
                    div()
                        .id(id)
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(
                            super::image::picture_frame(
                                frame.image.clone(),
                                frame.width,
                                frame.height,
                            )
                            .rounded(px(RADIUS))
                            .bg(gpui::black())
                            .children(play),
                        )
                        .child(caption(&loaded, theme)),
                )
                .into_any_element()
            }
            Body::Pages { pages, count } => {
                let (_, more) = super::preview_size::pdf_pages(*count);
                let more = super::preview_size::more_pages_label(more).map(|label| {
                    div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_center()
                        .gap(px(6.0))
                        .px(px(10.0))
                        .py(px(7.0))
                        .rounded(px(RADIUS))
                        .border_1()
                        .border_color(hairline(0.11))
                        .bg(ink(0.02))
                        .text_size(px(12.0))
                        .line_height(px(16.0))
                        .text_color(theme.text_muted)
                        .child(
                            icon(icons::DOCUMENT)
                                .size(px(14.0))
                                .flex_none()
                                .text_color(theme.text_muted),
                        )
                        .child(SharedString::from(label))
                });
                clickable(
                    div()
                        .id(id)
                        .flex()
                        .flex_col()
                        .gap(px(PAGE_GAP))
                        .children(pages.iter().map(|page| {
                            // A page shows whole: column wide at its aspect.
                            let size =
                                super::preview_size::preview_size(1.0, page.width, page.height);
                            div()
                                .relative()
                                .w_full()
                                .aspect_ratio(size.content_aspect())
                                .rounded(px(RADIUS))
                                .bg(gpui::white())
                                .child(super::image::rounded_picture(page.image.clone(), None))
                        }))
                        .children(more)
                        .child(caption(&loaded, theme)),
                )
                .into_any_element()
            }
            Body::Page { title, .. } => {
                let title = title.clone().unwrap_or_else(|| loaded.name.clone());
                clickable(
                    card_frame()
                        .id(id)
                        .child(
                            icon(kind_icon(loaded.kind))
                                .size(px(16.0))
                                .flex_none()
                                .text_color(theme.text_muted),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(13.0))
                                        .line_height(px(18.0))
                                        .truncate()
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .line_height(px(16.0))
                                        .text_color(theme.text_muted)
                                        .truncate()
                                        .child(loaded.name.clone()),
                                ),
                        ),
                )
                .into_any_element()
            }
            Body::Plain => {
                clickable(chip(kind, &name, Some(human_size(loaded.size).into()), theme).id(id))
                    .into_any_element()
            }
        },
    }
}

fn card_frame() -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(8.0))
        .w_full()
        .rounded(px(RADIUS))
        .border_1()
        .border_color(hairline(0.11))
        .bg(ink(0.02))
}

/// The neutral line a file shows as when there is nothing more: its icon,
/// name and a note (size, "Loading…", why it can't show).
fn chip(
    kind: Kind,
    name: &str,
    note: Option<SharedString>,
    theme: &crate::kit::theme::Theme,
) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(7.0))
        .max_w(px(420.0))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(hairline(0.11))
        .text_size(px(12.0))
        .line_height(px(16.0))
        .child(
            icon(kind_icon(kind))
                .size(px(14.0))
                .flex_none()
                .text_color(theme.text_muted),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(SharedString::from(name.to_owned())),
        )
        .children(note.map(|note| div().flex_none().text_color(theme.text_muted).child(note)))
}

fn caption(loaded: &Loaded, theme: &crate::kit::theme::Theme) -> gpui::Div {
    div()
        .text_size(px(12.0))
        .line_height(px(16.0))
        .text_color(theme.text_muted)
        .child(SharedString::from(format!(
            "{} · {}",
            loaded.name,
            human_size(loaded.size)
        )))
}

/// Column widths in points: the widest shown cell of each, clipped.
fn column_widths(table: &Table) -> Vec<f32> {
    let width = table.width();
    (0..width)
        .map(|col| {
            let chars = table
                .columns
                .iter()
                .chain(table.rows.iter())
                .filter_map(|row| row.get(col))
                .map(|cell| cell.chars().count())
                .max()
                .unwrap_or(0)
                .clamp(1, super::file_preview::COLUMN_CHARS);
            chars as f32 * CHAR_W + 16.0
        })
        .collect()
}

fn table_block(table: &Table, loaded: &Loaded, theme: &crate::kit::theme::Theme) -> gpui::Div {
    let widths = column_widths(table);
    let row = |cells: &[String], header: bool, last: bool| {
        div()
            .flex()
            .flex_row()
            .flex_none()
            .when(header, |row| {
                row.bg(ink(0.04)).font_weight(gpui::FontWeight::SEMIBOLD)
            })
            .when(!last, |row| row.border_b_1().border_color(hairline(0.07)))
            .children(cells.iter().zip(&widths).map(|(cell, w)| {
                div()
                    .w(px(*w))
                    .flex_none()
                    .px(px(8.0))
                    .py(px(3.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(SharedString::from(cell.clone()))
            }))
    };
    let mut body = div().flex().flex_col().min_w_0();
    let count = table.rows.len();
    if let Some(columns) = &table.columns {
        body = body.child(row(columns, true, count == 0));
    }
    for (at, cells) in table.rows.iter().enumerate() {
        body = body.child(row(cells, false, at + 1 == count));
    }
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .max_w_full()
                .overflow_hidden()
                .rounded(px(RADIUS))
                .border_1()
                .border_color(hairline(0.11))
                .text_size(px(12.0))
                .line_height(px(17.0))
                .mono(theme)
                .child(body),
        )
        .child(
            div()
                .text_size(px(12.0))
                .line_height(px(16.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(format!(
                    "{} · {}",
                    loaded.name,
                    table.count_label()
                ))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_sizes() {
        assert_eq!(file_name("out/results.csv:3"), "results.csv");
        assert_eq!(file_name("file:///tmp/my%20clip.mov"), "my clip.mov");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(12 * 1024), "12 KB");
        assert_eq!(human_size(3 * 1024 * 1024 + 400 * 1024), "3.4 MB");
    }

    #[gpui::test]
    fn pages_draw_again_for_a_wider_column(cx: &mut gpui::TestAppContext) {
        use std::cell::Cell;
        struct Host;
        let host = cx.new(|_| Host);
        let key = Key {
            scope: "sharpen-test".into(),
            src: "a.pdf".into(),
        };
        let loads = Rc::new(Cell::new(0));
        let ask = |width: f32, cx: &mut gpui::TestAppContext| {
            let (key, loads) = (key.clone(), loads.clone());
            host.update(cx, |_, cx| {
                request(
                    key,
                    width,
                    cx,
                    move |_| {
                        loads.set(loads.get() + 1);
                        Task::ready(Ok(Loaded {
                            kind: Kind::Pdf,
                            name: "a.pdf".into(),
                            size: 1,
                            body: Body::Plain,
                            open: "a.pdf".into(),
                            local: None,
                            sharp_to: width,
                        }))
                    },
                    |_, _| {},
                )
            });
        };
        let sharp_to = || match lookup(&key) {
            Shown::Ready(loaded) => Some(loaded.sharp_to),
            _ => None,
        };
        ask(512.0, cx);
        cx.run_until_parked();
        ask(512.0, cx);
        cx.run_until_parked();
        assert_eq!((loads.get(), sharp_to()), (1, Some(512.0)));
        ask(768.0, cx);
        assert_eq!(sharp_to(), Some(512.0));
        cx.run_until_parked();
        assert_eq!((loads.get(), sharp_to()), (2, Some(768.0)));
    }

    #[test]
    fn columns_fit_their_widest_cell_up_to_the_clip() {
        let table = super::super::file_preview::parse_table(
            &format!("a,b\n1,{}\n", "x".repeat(80)),
            ',',
            true,
        );
        let widths = column_widths(&table);
        assert_eq!(widths[0], CHAR_W + 16.0);
        assert_eq!(widths[1], 40.0 * CHAR_W + 16.0);
    }
}
