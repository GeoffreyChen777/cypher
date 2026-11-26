//! Image blocks in chat replies ([`Block::Image`]): the preview, the
//! column's full width ([`super::preview_size`]: a tall one cropped to 4:5,
//! its top kept, rounded with a hairline edge: [`picture_frame`]) with its
//! alt text as a caption, and the store of loaded images.
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

/// The neutral box an image holds while it loads.
pub const PLACEHOLDER_HEIGHT: f32 = 120.0;
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
    /// The widest (points) it stays sharp at: an SVG is rasterized for the
    /// column it was loaded for and loads again for a wider one; a bitmap
    /// is as sharp as it gets (`f32::INFINITY`).
    pub sharp_to: f32,
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
    /// Shown as it is while it loads again for a wider column.
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

/// Load `key` with `load` unless it is loaded, loading, or failed only
/// moments ago; one loaded for a narrower column than `width` (points,
/// see [`Loaded::sharp_to`]) loads again, showing meanwhile. `on_ready`
/// runs on the asking entity when it lands — once per entity, however often
/// it asks meanwhile.
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
                    // A failed sharpening keeps what showed.
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
            Block::Image { src, alt, .. } => out.push((src, alt)),
            Block::BlockQuote { children } => children.iter().for_each(|b| walk(b, out)),
            Block::List { items, .. } => items.iter().flatten().for_each(|b| walk(b, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(block, &mut out);
    out
}

/// A picture of natural size `width` × `height` filling the column by
/// [`super::preview_size`]: the frame is the column wide at the preview's
/// aspect, the picture inside it column wide at its own, its top at the
/// frame's top (a tall one is cut off at the frame's bottom). Style the
/// frame further (click, background, overlays).
pub fn picture_frame(image: Arc<Image>, width: f32, height: f32) -> gpui::Div {
    let size = super::preview_size::preview_size(1.0, width, height);
    div()
        .relative()
        .w_full()
        .aspect_ratio(size.aspect())
        .child(rounded_picture(
            image,
            size.cropped.then(|| size.content_aspect()),
        ))
}

/// `image` filling its box with [`RADIUS`] corners under a hairline edge:
/// a light picture (or a cropped one's cut) still shows where it ends.
/// `crop`: the picture's own aspect when it is taller than the box, drawn
/// box wide from the top and cut off at the bottom.
///
/// The picture rounds its own corners and the edge is drawn over it: gpui
/// clips overflow to a rectangle, so a frame's rounding never cuts its
/// child's square corners (they showed past the border).
pub fn rounded_picture(image: Arc<Image>, crop: Option<f32>) -> gpui::Div {
    let picture = match crop {
        // gpui's `img` only crops evenly (cover); painting the visible
        // part of the fitted box keeps the top and rounds the real corners.
        Some(aspect) => gpui::canvas(
            |_, _, _| {},
            move |bounds, (), window, cx| {
                let Some(data) = image.clone().use_render_image(window, cx) else {
                    return;
                };
                let fitted = gpui::Bounds {
                    origin: bounds.origin,
                    size: gpui::size(bounds.size.width, bounds.size.width / aspect.max(0.01)),
                };
                window
                    .paint_image_fitted(
                        bounds,
                        fitted,
                        gpui::Corners::all(px(RADIUS)),
                        data,
                        0,
                        false,
                    )
                    .ok();
            },
        )
        .size_full()
        .into_any_element(),
        None => img(image)
            .size_full()
            .rounded(px(RADIUS))
            .into_any_element(),
    };
    div().absolute().inset_0().child(picture).child(edge())
}

/// The hairline edge over a picture, at its [`RADIUS`].
pub fn edge() -> gpui::Div {
    div()
        .absolute()
        .inset_0()
        .rounded(px(RADIUS))
        .border_1()
        .border_color(hairline(0.11))
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

/// An image block: the preview with its caption (when `caption`: the text
/// it stood in did not say its alt already), the neutral box while it loads,
/// or a line saying why it can't show.
pub fn render(src: &str, alt: &str, caption: bool, ctx: BlockCtx) -> AnyElement {
    let BlockCtx {
        ix, opts, theme, ..
    } = ctx;
    let caption = (caption && !alt.trim().is_empty()).then(|| {
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
            picture_frame(loaded.image.clone(), loaded.width, loaded.height)
                .id(id)
                .cursor_pointer()
                .on_click(move |_, window, cx| on_open(key.clone(), alt.clone(), window, cx))
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

    #[gpui::test]
    fn a_wider_column_loads_again_showing_the_old_meanwhile(cx: &mut gpui::TestAppContext) {
        use std::cell::Cell;
        struct Host;
        let host = cx.new(|_| Host);
        let key = Key {
            scope: "sharpen-test".into(),
            src: "a.svg".into(),
        };
        let loads = Rc::new(Cell::new(0));
        let ask = |width: f32, result: Result<f32, &'static str>, cx: &mut gpui::TestAppContext| {
            let (key, loads) = (key.clone(), loads.clone());
            host.update(cx, |_, cx| {
                request(
                    key,
                    width,
                    cx,
                    move |_| {
                        loads.set(loads.get() + 1);
                        Task::ready(
                            result
                                .map(|sharp_to| Loaded {
                                    image: Arc::new(Image::from_bytes(
                                        gpui::ImageFormat::Svg,
                                        b"<svg/>".to_vec(),
                                    )),
                                    width: 10.0,
                                    height: 10.0,
                                    origin: Origin::Inline,
                                    sharp_to,
                                })
                                .map_err(SharedString::from),
                        )
                    },
                    |_, _| {},
                )
            });
        };
        let sharp_to = || match lookup(&key) {
            Shown::Ready(loaded) => Some(loaded.sharp_to),
            _ => None,
        };
        ask(512.0, Ok(512.0), cx);
        cx.run_until_parked();
        assert_eq!((loads.get(), sharp_to()), (1, Some(512.0)));
        // As wide or narrower: kept.
        ask(256.0, Ok(256.0), cx);
        ask(512.0, Ok(512.0), cx);
        cx.run_until_parked();
        assert_eq!(loads.get(), 1);
        // Wider: loads again, the old one showing until it lands.
        ask(768.0, Ok(768.0), cx);
        assert_eq!((loads.get(), sharp_to()), (2, Some(512.0)));
        cx.run_until_parked();
        assert_eq!(sharp_to(), Some(768.0));
        // A failed reload keeps what showed.
        ask(1024.0, Err("gone"), cx);
        cx.run_until_parked();
        assert_eq!((loads.get(), sharp_to()), (3, Some(768.0)));
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
