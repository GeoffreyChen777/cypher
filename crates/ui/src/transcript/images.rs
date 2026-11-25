//! Images in replies ([`crate::markdown::image`]): loading them for the
//! selected chat, and the full-size viewer a click opens.
//!
//! A local image is read through the engine — the Files browser's chunked
//! read — so a session on another device shows its own files; it is looked
//! up in the chat's folder, then each root it may read
//! ([`crate::markdown::image_source`]). `https://` images load over the
//! app's HTTP client (no cookies or credentials); `data:` URIs decode in
//! place. Every source stops at [`image_source::MAX_BYTES`].

use std::sync::Arc;

use base64::Engine as _;
use gpui::http_client::HttpClient;
use gpui::{
    AnyElement, ClipboardItem, Context, Image, ImageFormat, ObjectFit, SharedString, Task, Window,
    div, img, prelude::*, px,
};

use super::ToolItem;
use crate::files::{FilesContext, LinkContexts};
use crate::markdown::image::{self, ImageUi, Key, Loaded, Origin, Shown};
use crate::markdown::image_source::{self, Format, MAX_BYTES, Rejection, Source};
use crate::markdown::parser::Block;
use cypher_proto::ToolCall;

use super::Transcript;

/// The side of a tool row's image thumbnail.
const THUMB_SIDE: f32 = 22.0;

/// The viewer's state: which image, at which zoom.
#[derive(Clone)]
pub(super) struct ImageViewer {
    key: Key,
    alt: SharedString,
    /// One image pixel per point (else fitted to the window).
    actual: bool,
    copied: bool,
}

impl Transcript {
    /// The images wiring for this transcript's rows.
    pub(super) fn image_ui(&self, cx: &mut Context<Self>) -> ImageUi {
        let scope = self.image_scope(cx);
        let entity = cx.weak_entity();
        ImageUi {
            scope,
            on_open: std::rc::Rc::new(
                move |key: Key, alt: SharedString, window: &mut Window, cx: &mut gpui::App| {
                    if let Some(page) = svg_page(&key, &alt) {
                        let handler = entity
                            .read_with(cx, |this, _| this.artifact_handler.clone())
                            .ok()
                            .flatten();
                        if let Some(handler) = handler {
                            handler(page, window, cx);
                            return;
                        }
                    }
                    entity
                        .update(cx, |this, cx| {
                            this.image_viewer = Some(ImageViewer {
                                key,
                                alt,
                                actual: false,
                                copied: false,
                            });
                            window.focus(&this.attachment_preview_focus, cx);
                            cx.notify();
                        })
                        .ok();
                },
            ),
        }
    }

    /// The reply column's width that previews fill, rounded up to a
    /// [`PREVIEW_WIDTH_STEP`]: what an SVG, a PDF page or a video frame is
    /// drawn for (a column grown past it draws them again).
    pub(super) fn preview_width(&self, cx: &gpui::App) -> f32 {
        let gutter = if self.embedded { 14.0 } else { 48.0 };
        let tile = self
            .viewport_size
            .map(|(width, _)| width)
            .unwrap_or(super::MAX_CONTENT_WIDTH + 2.0 * gutter);
        let mut column = (tile - 2.0 * gutter).max(1.0);
        if !crate::appearance::chat_style::settings(cx).wide {
            column = column.min(super::MAX_CONTENT_WIDTH);
        }
        preview_step(column)
    }

    pub(super) fn image_scope(&self, cx: &Context<Self>) -> SharedString {
        self.state
            .read(cx)
            .selected_chat_row()
            .map(|chat| SharedString::from(chat.id.clone()))
            .unwrap_or_default()
    }

    /// Start loading every image `block` shows; its row remeasures as each
    /// lands. The streaming tail of a live reply waits: its source may still
    /// be half written.
    pub(super) fn request_images(
        &mut self,
        block: &Block,
        row_id: &SharedString,
        streaming_tail: bool,
        cx: &mut Context<Self>,
    ) {
        if streaming_tail {
            return;
        }
        let sources = image::sources(block);
        if sources.is_empty() {
            return;
        }
        let scope = self.image_scope(cx);
        let width = self.preview_width(cx);
        for (src, _) in sources {
            let key = Key {
                scope: scope.clone(),
                src: SharedString::from(src.to_owned()),
            };
            let row = row_id.clone();
            image::request(
                key,
                width,
                cx,
                |cx| self.load_image(src, width, cx),
                move |this: &mut Self, cx| this.remeasure_row(&row, cx),
            );
        }
    }

    /// Load `src` to fill a column `width` points wide (an SVG draws sharp
    /// there; 0 when it only shows small).
    pub(super) fn load_image(
        &self,
        src: &str,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Task<Result<Loaded, SharedString>> {
        let state = self.state.read(cx);
        let contexts = crate::files::link_contexts(state);
        let engine = state.engine().cloned();
        let home = chat_home(state);
        let cwd = state.selected_chat_row().and_then(|chat| chat.cwd.clone());
        let roots: Vec<String> = match &contexts {
            Ok(contexts) => contexts.contexts.iter().map(|c| c.root.clone()).collect(),
            Err(_) => Vec::new(),
        };
        let root_refs: Vec<&str> = roots.iter().map(String::as_str).collect();
        let source = image_source::classify(
            src,
            &image_source::Context {
                cwd: cwd.as_deref(),
                roots: &root_refs,
                home: home.as_deref(),
            },
        );
        let http = cx.http_client();
        match source {
            Source::Local { candidates, .. } => {
                let (Ok(contexts), Some(engine)) = (contexts, engine) else {
                    return Task::ready(Err("not connected to the session's files".into()));
                };
                let tries: Vec<(FilesContext, String, String)> = candidates
                    .into_iter()
                    .filter_map(|candidate| locate(&contexts, &candidate.root, candidate.path))
                    .collect();
                cx.spawn(async move |_, cx| {
                    let mut reason: SharedString = "not found".into();
                    for (context, relative, open) in tries {
                        match crate::files::preview::read_bytes(
                            &engine,
                            context.params(&relative),
                            MAX_BYTES,
                            false,
                            |_, _| {},
                        )
                        .await
                        {
                            Ok((bytes, _)) => {
                                return decode(bytes, Origin::File(open), width, cx).await;
                            }
                            Err(message) if message.starts_with("Too large") => {
                                return Err(too_large());
                            }
                            // Not there: the next candidate.
                            Err(message) => {
                                if message.contains("needs an update") {
                                    reason = message;
                                }
                            }
                        }
                    }
                    Err(reason)
                })
            }
            Source::Outside { .. } => Task::ready(Err("outside the session's folders".into())),
            Source::Remote { url } => cx.spawn(async move |_, cx| {
                let bytes = fetch(http, &url).await?;
                decode(bytes, Origin::Url(url), width, cx).await
            }),
            Source::Data { payload, .. } => cx.spawn(async move |_, cx| {
                if payload.len() as u64 / 4 * 3 > MAX_BYTES {
                    return Err(too_large());
                }
                let bytes = cx
                    .background_executor()
                    .spawn(async move {
                        base64::engine::general_purpose::STANDARD
                            .decode(payload.trim().as_bytes())
                            .map_err(|_| SharedString::from("unreadable data"))
                    })
                    .await?;
                decode(bytes, Origin::Inline, width, cx).await
            }),
            Source::Rejected(Rejection::Insecure) => {
                Task::ready(Err("not loaded over insecure http".into()))
            }
            Source::Rejected(Rejection::Unsupported) | Source::NotImage => {
                Task::ready(Err("unsupported".into()))
            }
        }
    }

    /// The thumbnail on a tool row whose call read or wrote an image
    /// ([`tool_image`]): requested here, shown once loaded, nothing while it
    /// loads or when it can't be shown. A click opens the viewer (a local
    /// SVG opens as a page).
    pub(super) fn tool_thumbnail(
        &self,
        tool: &ToolItem,
        row_id: &SharedString,
        id: &SharedString,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let src = tool_image(tool)?;
        let key = Key {
            scope: self.image_scope(cx),
            src: SharedString::from(src.to_owned()),
        };
        let row = row_id.clone();
        image::request(
            key.clone(),
            0.0,
            cx,
            |cx| self.load_image(src, 0.0, cx),
            move |this: &mut Self, cx| this.remeasure_row(&row, cx),
        );
        let Shown::Ready(loaded) = image::lookup(&key) else {
            return None;
        };
        let on_open = self.image_ui(cx).on_open;
        let name: SharedString = src.rsplit('/').next().unwrap_or(src).to_owned().into();
        Some(
            div()
                .id(SharedString::from(format!("{id}-thumb")))
                .size(px(THUMB_SIDE))
                .flex_none()
                .rounded(px(4.0))
                .overflow_hidden()
                .border_1()
                .border_color(crate::kit::theme::hairline(0.1))
                .bg(crate::kit::theme::ink(0.035))
                .cursor_pointer()
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    on_open(key.clone(), name.clone(), window, cx);
                })
                .child(
                    img(loaded.image.clone())
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                )
                .into_any_element(),
        )
    }

    /// The viewer over the transcript, when an image is open.
    pub(super) fn image_viewer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let viewer = self.image_viewer.clone()?;
        let Shown::Ready(loaded) = image::lookup(&viewer.key) else {
            self.image_viewer = None;
            return None;
        };
        let viewport = window.viewport_size();
        let max_h = px(f32::from(viewport.height) * 0.8);
        let max_w = px(f32::from(viewport.width) * 0.9);
        let close = cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
            this.image_viewer = None;
            cx.notify();
        });
        let picture: AnyElement = if viewer.actual {
            div()
                .id("image-viewer-scroll")
                .max_w(max_w)
                .max_h(max_h)
                .overflow_scroll()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    img(loaded.image.clone())
                        .w(px(loaded.width))
                        .h(px(loaded.height))
                        .flex_none(),
                )
                .into_any_element()
        } else {
            let scale = (f32::from(max_w) / loaded.width)
                .min(f32::from(max_h) / loaded.height)
                .min(1.0);
            img(loaded.image.clone())
                .object_fit(ObjectFit::Contain)
                .w(px(loaded.width * scale))
                .h(px(loaded.height * scale))
                .rounded(px(6.0))
                .into_any_element()
        };
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .text_size(px(12.0))
                .text_color(gpui::white().opacity(0.85))
                .bg(gpui::white().opacity(0.1))
                .hover(|style| style.bg(gpui::white().opacity(0.18)))
                .cursor_pointer()
                .child(label)
        };
        let zoom = button(
            "image-viewer-zoom",
            if viewer.actual { "Fit" } else { "Actual size" },
        )
        .on_click(cx.listener(|this, _, _, cx| {
            cx.stop_propagation();
            if let Some(viewer) = &mut this.image_viewer {
                viewer.actual = !viewer.actual;
            }
            cx.notify();
        }));
        let copy_image = loaded.image.clone();
        let copy = button(
            "image-viewer-copy",
            if viewer.copied {
                "Copied"
            } else {
                "Copy image"
            },
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            cx.write_to_clipboard(ClipboardItem::new_image(&copy_image));
            if let Some(viewer) = &mut this.image_viewer {
                viewer.copied = true;
            }
            cx.notify();
        }));
        let open = match loaded.origin.clone() {
            Origin::File(path) => Some(button("image-viewer-open", "Show in Files").on_click(
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.image_viewer = None;
                    if let Some(handler) = this.link_handler.clone() {
                        let url = super::code_ref::file_url(&path, None);
                        window.defer(cx, move |window, cx| {
                            handler(&url, window, cx);
                        });
                    }
                    cx.notify();
                }),
            )),
            Origin::Url(url) => Some(button("image-viewer-open", "Open in browser").on_click(
                move |_, _, cx| {
                    cx.stop_propagation();
                    cx.open_url(&url);
                },
            )),
            Origin::Inline => None,
        };
        let caption = (!viewer.alt.is_empty()).then(|| {
            div()
                .min_w_0()
                .overflow_hidden()
                .text_size(px(12.0))
                .text_color(gpui::white().opacity(0.6))
                .child(viewer.alt.clone())
        });
        let close_on_key = cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                cx.stop_propagation();
                this.image_viewer = None;
                cx.notify();
            }
        });
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(gpui::point(px(0.0), px(0.0)))
                    .child(
                        div()
                            .id("image-viewer")
                            .occlude()
                            .track_focus(&self.attachment_preview_focus)
                            .w(viewport.width)
                            .h(viewport.height)
                            .bg(crate::kit::popover::scrim_alpha(0.75))
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(px(12.0))
                            .on_key_down(close_on_key)
                            .on_click(close)
                            .child(picture)
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(8.0))
                                    .max_w(max_w)
                                    .children(caption)
                                    .child(zoom)
                                    .child(copy)
                                    .children(open),
                            ),
                    ),
            )
            .priority(3)
            .into_any_element(),
        )
    }
}

/// A local SVG file opens as a page in the artifact viewer, where it draws
/// at any size; other images (and SVGs carried in the reply or fetched)
/// open in the image viewer.
fn svg_page(key: &Key, alt: &str) -> Option<crate::markdown::artifact::ArtifactOpen> {
    let Shown::Ready(loaded) = image::lookup(key) else {
        return None;
    };
    let Origin::File(path) = &loaded.origin else {
        return None;
    };
    if loaded.image.format() != ImageFormat::Svg {
        return None;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let title = if alt.is_empty() || alt == key.src.as_ref() {
        name
    } else {
        alt
    };
    Some(crate::markdown::artifact::ArtifactOpen {
        key: format!("file:{path}").into(),
        kind: crate::markdown::parser::ArtifactKind::Svg,
        title: title.to_owned().into(),
        named: false,
        code: String::from_utf8_lossy(loaded.image.bytes())
            .into_owned()
            .into(),
    })
}

/// The home folder of the selected chat's host, for `~` in the paths its
/// replies name: known only when that host is this device. A `~` path still
/// reads only inside the chat's roots ([`image_source::local_candidates`]).
pub(super) fn chat_home(state: &crate::state::AppState) -> Option<String> {
    state
        .selected_chat_row()
        .filter(|chat| state.local_device_id.as_deref() == Some(chat.device_id.as_str()))
        .and_then(|_| std::env::var("HOME").ok())
        .filter(|home| home.starts_with('/'))
}

/// The image a finished call read or wrote (a `readFile` of a png, a
/// screenshot saved with `writeFile`), which its row shows as a thumbnail.
/// The doc keeps no tool result's image bytes (the harness passes text
/// only), so the file is read again through the chat's roots, like an image
/// a reply names ([`Transcript::load_image`]); one outside them shows none.
pub(super) fn tool_image(tool: &ToolItem) -> Option<&str> {
    if !tool.resolved || tool.is_error {
        return None;
    }
    let path = match &tool.call {
        ToolCall::ReadFile { path } | ToolCall::WriteFile { path, .. } => path,
        _ => return None,
    };
    // Whether the path names an image file at all: any root will do here.
    let any_root = image_source::Context {
        roots: &["/"],
        ..Default::default()
    };
    matches!(
        image_source::classify(path, &any_root),
        Source::Local { .. }
    )
    .then_some(path.as_str())
}

/// The context of `root` among the chat's, the path in it, and the path its
/// link handler opens (a workspace chat's are absolute).
pub(super) fn locate(
    contexts: &LinkContexts,
    root: &str,
    relative: String,
) -> Option<(FilesContext, String, String)> {
    let trim = |path: &str| path.trim_end_matches('/').to_owned();
    let ix = contexts
        .contexts
        .iter()
        .position(|context| trim(&context.root) == trim(root))?;
    let context = contexts.contexts[ix].clone();
    let open = match contexts.roots.get(ix) {
        Some(root) if contexts.multi_root() => crate::files::roots::join(root, &relative),
        _ => relative.clone(),
    };
    Some((context, relative, open))
}

fn too_large() -> SharedString {
    format!("too large (over {} MB)", MAX_BYTES / (1024 * 1024)).into()
}

/// Redirects one image load follows before giving up.
const MAX_REDIRECTS: usize = 5;

/// GET an `https://` image, at most [`MAX_BYTES`]. Redirects are followed
/// here, hop by hop, so each target passes the same check as the first
/// address ([`checked_url`]): a redirect to plain `http://` (or one carrying
/// credentials) fails like any other load error.
async fn fetch(http: Arc<dyn HttpClient>, url: &str) -> Result<Vec<u8>, SharedString> {
    use futures::AsyncReadExt as _;
    let mut current = checked_url(url::Url::parse(url).ok())?;
    let mut hops = 0;
    let mut response = loop {
        let response = http
            .get(current.as_str(), ().into(), false)
            .await
            .map_err(|_| SharedString::from("network error"))?;
        if !response.status().is_redirection() {
            break response;
        }
        hops += 1;
        if hops > MAX_REDIRECTS {
            return Err("network error (too many redirects)".into());
        }
        let location = response
            .headers()
            .get(gpui::http_client::http::header::LOCATION)
            .and_then(|value| value.to_str().ok());
        current = redirect_target(&current, location)?;
    };
    if !response.status().is_success() {
        return Err(format!("network error ({})", response.status().as_u16()).into());
    }
    let mut body = Vec::new();
    response
        .body_mut()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut body)
        .await
        .map_err(|_| SharedString::from("network error"))?;
    if body.len() as u64 > MAX_BYTES {
        return Err(too_large());
    }
    Ok(body)
}

/// Where a redirect from `from` goes: its `Location`, resolved against
/// `from`, and checked like the first address.
fn redirect_target(from: &url::Url, location: Option<&str>) -> Result<url::Url, SharedString> {
    let location = location.ok_or_else(|| SharedString::from("network error (bad redirect)"))?;
    checked_url(from.join(location).ok())
}

/// An address an image may load from: `https://`, without credentials.
fn checked_url(url: Option<url::Url>) -> Result<url::Url, SharedString> {
    let url = url.ok_or_else(|| SharedString::from("network error"))?;
    if url.scheme() != "https" {
        return Err("not loaded over insecure http".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("not loaded: the address carries credentials".into());
    }
    Ok(url)
}

/// Previews draw for the column rounded up to this many points, so a
/// slowly widening window draws them again only every step.
pub(super) const PREVIEW_WIDTH_STEP: f32 = 256.0;

/// `column` rounded up to a [`PREVIEW_WIDTH_STEP`].
pub(super) fn preview_step(column: f32) -> f32 {
    (column / PREVIEW_WIDTH_STEP).ceil().max(1.0) * PREVIEW_WIDTH_STEP
}

/// Check the bytes are a supported image and measure it, off the UI thread.
async fn decode(
    bytes: Vec<u8>,
    origin: Origin,
    width: f32,
    cx: &mut gpui::AsyncApp,
) -> Result<Loaded, SharedString> {
    cx.background_executor()
        .spawn(async move { measure(bytes, origin, width) })
        .await
}

/// The most points an enlarged SVG covers (width × height): gpui draws it
/// at twice that in pixels, so this bounds its bitmap (~64 MB).
const SVG_MAX_AREA: f32 = 2048.0 * 2048.0;

/// A loaded image from its bytes: format by content (the name may lie),
/// size from the header. An SVG is enlarged to draw sharp `width` points
/// wide (the column it fills), and so a small icon is legible elsewhere.
pub(super) fn measure(bytes: Vec<u8>, origin: Origin, width: f32) -> Result<Loaded, SharedString> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(too_large());
    }
    let format = Format::sniff(&bytes).ok_or_else(|| SharedString::from("unsupported format"))?;
    let column = width;
    let mut sharp_to = f32::INFINITY;
    let (gpui_format, bytes, (width, height)) = match format {
        Format::Svg => {
            let source = String::from_utf8_lossy(&bytes).into_owned();
            let min = crate::files::preview::SVG_PREVIEW_MIN;
            // Its long side at least `min` (see the preview) and as wide as
            // the column, within the area bound.
            let scale_for = |w: f32, h: f32| {
                (min / w.max(h))
                    .max(column / w)
                    .min((SVG_MAX_AREA / (w * h)).sqrt())
                    .max(1.0)
            };
            let (bytes, declared) = crate::files::preview::svg_scaled_bytes(&source, scale_for);
            let (w, h) = declared.unwrap_or((min, min));
            let scale = scale_for(w, h);
            sharp_to = column;
            (ImageFormat::Svg, bytes, (w * scale, h * scale))
        }
        raster => {
            let (w, h) = crate::files::preview::image_dims(&bytes)
                .ok_or_else(|| SharedString::from("couldn't decode"))?;
            let format = match raster {
                Format::Png => ImageFormat::Png,
                Format::Jpeg => ImageFormat::Jpeg,
                Format::Gif => ImageFormat::Gif,
                _ => ImageFormat::Webp,
            };
            (format, bytes, (w as f32, h as f32))
        }
    };
    if width <= 0.0 || height <= 0.0 {
        return Err("couldn't decode".into());
    }
    Ok(Loaded {
        image: Arc::new(Image::from_bytes(gpui_format, bytes)),
        width,
        height,
        origin,
        sharp_to,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

    #[test]
    fn previews_draw_for_the_column_in_steps() {
        assert_eq!(preview_step(1.0), 256.0);
        assert_eq!(preview_step(640.0), 768.0);
        assert_eq!(preview_step(768.0), 768.0);
        assert_eq!(preview_step(769.0), 1024.0);
    }

    #[test]
    fn bytes_are_measured_by_content() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(PNG_1X1)
            .unwrap();
        let loaded = measure(bytes, Origin::Inline, 0.0).ok().unwrap();
        assert_eq!((loaded.width, loaded.height), (1.0, 1.0));
        assert_eq!(loaded.image.format(), ImageFormat::Png);
        assert_eq!(loaded.sharp_to, f32::INFINITY);
        let svg = measure(
            b"<svg width=\"16\" height=\"8\"/>".to_vec(),
            Origin::Inline,
            0.0,
        )
        .ok()
        .unwrap();
        assert_eq!(svg.image.format(), ImageFormat::Svg);
        assert!(svg.width >= 16.0 && (svg.width / svg.height - 2.0).abs() < 0.01);
        // Enlarged to draw sharp across the column it was loaded for, its
        // area bounded.
        let svg = measure(
            b"<svg width=\"16\" height=\"8\"/>".to_vec(),
            Origin::Inline,
            768.0,
        )
        .ok()
        .unwrap();
        assert_eq!((svg.width, svg.sharp_to), (768.0, 768.0));
        let source = String::from_utf8_lossy(svg.image.bytes()).into_owned();
        assert!(source.contains("width=\"768\""), "{source}");
        let tall = measure(
            b"<svg width=\"10\" height=\"1000\"/>".to_vec(),
            Origin::Inline,
            768.0,
        )
        .ok()
        .unwrap();
        assert!(tall.width * tall.height <= SVG_MAX_AREA * 1.001);
        assert_eq!(
            measure(b"%PDF-1.7".to_vec(), Origin::Inline, 0.0)
                .err()
                .unwrap(),
            "unsupported format"
        );
    }

    #[test]
    fn finished_reads_and_writes_of_images_get_a_thumbnail() {
        let item = |call: ToolCall, resolved: bool, is_error: bool| ToolItem {
            call,
            is_error,
            resolved,
            detail: None,
            invocation: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            depth: 0,
            permission: None,
            blocked_first: false,
        };
        let read = |path: &str| ToolCall::ReadFile { path: path.into() };
        let write = |path: &str| ToolCall::WriteFile {
            path: path.into(),
            content: None,
        };
        let done = |call| tool_image_owned(&item(call, true, false));
        assert_eq!(
            done(read("/w/shots/a.png")).as_deref(),
            Some("/w/shots/a.png")
        );
        assert_eq!(done(read("shots/B.JPEG")).as_deref(), Some("shots/B.JPEG"));
        assert_eq!(
            done(write("~/out/chart.svg")).as_deref(),
            Some("~/out/chart.svg")
        );
        // Not an image file, not a file call, a URL: none.
        assert_eq!(done(read("src/main.rs")), None);
        assert_eq!(done(read("notes.pdf")), None);
        assert_eq!(done(read("https://x.dev/a.png")), None);
        assert_eq!(
            done(ToolCall::EditFile {
                path: "a.png".into(),
                old_string: None,
                new_string: None
            }),
            None
        );
        // Still running, or failed: none.
        assert_eq!(tool_image(&item(read("a.png"), false, false)), None);
        assert_eq!(tool_image(&item(read("a.png"), true, true)), None);
    }

    fn tool_image_owned(tool: &ToolItem) -> Option<String> {
        tool_image(tool).map(str::to_owned)
    }

    #[test]
    fn redirects_are_checked_like_the_first_address() {
        let from = url::Url::parse("https://img.example/a/chart.png").unwrap();
        let hop =
            |location: &str| redirect_target(&from, Some(location)).map(|url| url.to_string());
        assert_eq!(
            hop("https://cdn.example/x.png").unwrap(),
            "https://cdn.example/x.png"
        );
        // Relative and scheme-relative targets resolve against the page.
        assert_eq!(hop("../b.png").unwrap(), "https://img.example/b.png");
        assert_eq!(
            hop("//cdn.example/c.png").unwrap(),
            "https://cdn.example/c.png"
        );
        // A downgrade fails like any other load error.
        assert_eq!(
            hop("http://img.example/a/chart.png").err().unwrap(),
            "not loaded over insecure http"
        );
        assert_eq!(
            hop("HTTP://img.example/x.png").err().unwrap(),
            "not loaded over insecure http"
        );
        assert!(hop("file:///etc/passwd").is_err());
        assert!(hop("data:image/png;base64,AAAA").is_err());
        assert_eq!(
            hop("https://me:secret@img.example/x.png").err().unwrap(),
            "not loaded: the address carries credentials"
        );
        assert!(redirect_target(&from, None).is_err());
    }

    #[test]
    fn a_downgrading_redirect_fails_the_load() {
        use gpui::http_client::{AsyncBody, FakeHttpClient, Response};
        let http = FakeHttpClient::create(|request| async move {
            let path = request.uri().path().to_owned();
            let response = match path.as_str() {
                "/up.png" => Response::builder()
                    .status(302)
                    .header("location", "/final.png")
                    .body(AsyncBody::default()),
                "/down.png" => Response::builder()
                    .status(301)
                    .header("location", "http://img.example/final.png")
                    .body(AsyncBody::default()),
                "/loop.png" => Response::builder()
                    .status(307)
                    .header("location", "/loop.png")
                    .body(AsyncBody::default()),
                _ => Response::builder()
                    .status(200)
                    .body(AsyncBody::from(b"\x89PNG\r\n\x1a\nbytes".to_vec())),
            };
            Ok(response.unwrap())
        });
        let http: Arc<dyn HttpClient> = http;
        let run = |url: &str| futures::executor::block_on(fetch(http.clone(), url));
        assert!(
            run("https://img.example/up.png")
                .unwrap()
                .starts_with(b"\x89PNG")
        );
        assert_eq!(
            run("https://img.example/down.png").err().unwrap(),
            "not loaded over insecure http"
        );
        assert!(
            run("https://img.example/loop.png")
                .err()
                .unwrap()
                .contains("too many redirects")
        );
        assert_eq!(
            run("http://img.example/up.png").err().unwrap(),
            "not loaded over insecure http"
        );
    }

    #[test]
    fn oversize_bytes_are_refused() {
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(PNG_1X1)
            .unwrap();
        bytes.resize(MAX_BYTES as usize + 1, 0);
        assert!(
            measure(bytes, Origin::Inline, 0.0)
                .err()
                .unwrap()
                .starts_with("too large")
        );
    }
}
