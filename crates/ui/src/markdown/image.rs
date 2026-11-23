//! Image blocks in chat replies ([`Block::Image`]): the preview, fitted to
//! the column (at most [`MAX_HEIGHT`] tall, never past its natural size)
//! with its alt text as a caption, and the store of loaded images.
//!
//! The store follows the diagram store ([`super::mermaid`]): a surface asks
//! for every image a block shows before rendering it ([`request`]), with the
//! load to run when nobody has started it, and hears when it lands (the row
//! is then taller: it remeasures). Rendering only looks up ([`lookup`]).
//! Keys are scoped by chat: a relative path means another file elsewhere.
//! Bytes are decoded by gpui's image cache, once per loaded image.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, EntityId, Image, SharedString, Task, Window, div, img, prelude::*, px,
};

use super::parser::Block;
use super::render::BlockCtx;
use crate::kit::icons::{self, icon};
use crate::kit::theme::{hairline, ink};

/// The tallest an image block gets.
pub const MAX_HEIGHT: f32 = 360.0;
/// The neutral box an image holds while it loads.
pub const PLACEHOLDER_HEIGHT: f32 = 120.0;
const PLACEHOLDER_WIDTH: f32 = 320.0;
const RADIUS: f32 = 8.0;
/// Loaded images kept; older ones load again when shown.
const MAX_READY: usize = 64;
/// A failed load is tried again when asked for after this long (the file
/// may exist by now).
const RETRY_AFTER: Duration = Duration::from_secs(20);

/// One image of one chat.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub scope: SharedString,
    pub src: SharedString,
}

/// Where the viewer can take a loaded image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A file of the chat, as its link handler opens it.
    File(String),
    Url(String),
    /// Carried in the reply itself.
    Inline,
}

/// A loaded image: its bytes (gpui decodes them once) and natural size.
#[derive(Clone)]
pub struct Loaded {
    pub image: Arc<Image>,
    pub width: f32,
    pub height: f32,
    pub origin: Origin,
}

/// What [`lookup`] finds.
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
                    State::Ready(loaded) => Shown::Ready(loaded.clone()),
                    State::Failed(reason, _) => Shown::Failed(reason.clone()),
                }
            }
            None => Shown::Pending,
        }
    })
}

/// Load `key` with `load` unless it is loaded, loading, or failed only
/// moments ago. `on_ready` runs on the asking entity when it lands — once
/// per entity, however often it asks meanwhile.
pub fn request<T: 'static>(
    key: Key,
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
                    State::Pending(waiters) => {
                        if !waiters.iter().any(|(id, _)| *id == entity) {
                            waiters.push((entity, waiter));
                        }
                        false
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
        let state = match loaded {
            Ok(loaded) => State::Ready(loaded),
            Err(reason) => State::Failed(reason, Instant::now()),
        };
        let waiters = match store.entries.get_mut(&key) {
            Some(entry) => match std::mem::replace(&mut entry.state, state) {
                State::Pending(waiters) => waiters,
                _ => Vec::new(),
            },
            None => Vec::new(),
        };
        let ready = store
            .entries
            .values()
            .filter(|entry| matches!(entry.state, State::Ready(_)))
            .count();
        if ready > MAX_READY {
            let mut ages: Vec<u64> = store
                .entries
                .values()
                .filter(|entry| matches!(entry.state, State::Ready(_)))
                .map(|entry| entry.used)
                .collect();
            ages.sort_unstable();
            let cutoff = ages[ready - MAX_READY - 1];
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

/// Every image `block` shows, nested ones included: `(src, alt)`.
pub fn sources(block: &Block) -> Vec<(&str, &str)> {
    fn walk<'a>(block: &'a Block, out: &mut Vec<(&'a str, &'a str)>) {
        match block {
            Block::Image { src, alt } => out.push((src, alt)),
            Block::BlockQuote { children } => children.iter().for_each(|b| walk(b, out)),
            Block::List { items, .. } => items.iter().flatten().for_each(|b| walk(b, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(block, &mut out);
    out
}

/// The size an image shows at in a column: as wide as the column allows,
/// at most [`MAX_HEIGHT`] tall, never larger than its natural size.
pub fn fitted(width: f32, height: f32, column: f32) -> (f32, f32) {
    if width <= 0.0 || height <= 0.0 {
        return (0.0, 0.0);
    }
    let scale = (column / width).min(MAX_HEIGHT / height).min(1.0);
    (width * scale, height * scale)
}

/// The widest an image may be before its height caps it (or its natural
/// width, when smaller): the block's `max_w`, its height following from
/// the aspect ratio.
fn max_width(width: f32, height: f32) -> f32 {
    fitted(width, height, f32::INFINITY).0
}

/// Opens the viewer for a clicked image.
pub type OpenHandler = Rc<dyn Fn(Key, SharedString, &mut Window, &mut App)>;

/// What a surface does with images: the chat they load in, and the viewer.
#[derive(Clone)]
pub struct ImageUi {
    pub scope: SharedString,
    pub on_open: OpenHandler,
}

/// How an image without a caption is named in its error line.
pub fn label(src: &str, alt: &str) -> String {
    if !alt.trim().is_empty() {
        return alt.trim().to_owned();
    }
    if src.trim_start().to_ascii_lowercase().starts_with("data:") {
        return "Image".into();
    }
    let path = src.split(['?', '#']).next().unwrap_or(src);
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("Image")
        .to_owned()
}

/// An image block: the preview with its caption, the neutral box while it
/// loads, or a line saying why it can't show.
pub fn render(src: &str, alt: &str, ctx: BlockCtx) -> AnyElement {
    let BlockCtx {
        ix, opts, theme, ..
    } = ctx;
    let caption = (!alt.trim().is_empty()).then(|| {
        div()
            .text_size(px(12.0))
            .line_height(px(16.0))
            .text_color(theme.text_muted)
            .child(SharedString::from(alt.trim().to_owned()))
    });
    let Some(ui) = opts.images.clone() else {
        // A surface without images: the caption says what was there.
        return div().children(caption).into_any_element();
    };
    let key = Key {
        scope: ui.scope.clone(),
        src: SharedString::from(src.to_owned()),
    };
    let id = SharedString::from(format!("{}-image{ix}", opts.row_key));
    let body: AnyElement = match lookup(&key) {
        Shown::Pending => div()
            .w_full()
            .max_w(px(PLACEHOLDER_WIDTH))
            .h(px(PLACEHOLDER_HEIGHT))
            .rounded(px(RADIUS))
            .bg(ink(0.035))
            .border_1()
            .border_color(hairline(0.08))
            .into_any_element(),
        Shown::Failed(reason) => {
            return div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(7.0))
                .rounded(px(RADIUS))
                .border_1()
                .border_color(hairline(0.11))
                .text_size(px(12.0))
                .line_height(px(16.0))
                .text_color(theme.text_muted)
                .child(
                    icon(icons::GALLERY)
                        .size(px(14.0))
                        .flex_none()
                        .text_color(theme.text_muted),
                )
                .child(div().min_w_0().child(SharedString::from(format!(
                    "{} — {reason}",
                    label(src, alt)
                ))))
                .into_any_element();
        }
        Shown::Ready(loaded) => {
            let on_open = ui.on_open.clone();
            let alt: SharedString = alt.trim().to_owned().into();
            div()
                .id(id)
                .w_full()
                .max_w(px(max_width(loaded.width, loaded.height)))
                .aspect_ratio(loaded.width / loaded.height)
                .rounded(px(RADIUS))
                .overflow_hidden()
                .cursor_pointer()
                .on_click(move |_, window, cx| on_open(key.clone(), alt.clone(), window, cx))
                .child(img(loaded.image.clone()).size_full().rounded(px(RADIUS)))
                .into_any_element()
        }
    };
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(body)
        .children(caption)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_fit_the_column_and_the_height_cap_without_upscaling() {
        // Wide: the column decides.
        assert_eq!(fitted(2000.0, 1000.0, 600.0), (600.0, 300.0));
        // Tall: the height cap decides.
        assert_eq!(fitted(400.0, 1600.0, 600.0), (90.0, 360.0));
        // Small: natural size.
        assert_eq!(fitted(64.0, 32.0, 600.0), (64.0, 32.0));
        assert_eq!(max_width(2000.0, 1000.0), 720.0);
        assert_eq!(max_width(64.0, 32.0), 64.0);
        assert_eq!(fitted(0.0, 10.0, 600.0), (0.0, 0.0));
    }

    #[test]
    fn errors_name_the_image() {
        assert_eq!(label("shots/a.png", " The chart "), "The chart");
        assert_eq!(label("shots/a.png", ""), "a.png");
        assert_eq!(label("https://x.dev/i/b.webp?s=2", ""), "b.webp");
        assert_eq!(label("data:image/png;base64,AAAA", ""), "Image");
    }

    #[test]
    fn blocks_list_their_images_nested_too() {
        let tree = super::super::parser::parse_full("- ![a](a.png)\n\n> ![b](b.gif)\n");
        let found: Vec<_> = tree.blocks.iter().flat_map(|b| sources(&b.block)).collect();
        assert_eq!(found, vec![("a.png", "a"), ("b.gif", "b")]);
    }
}
