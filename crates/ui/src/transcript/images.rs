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

use crate::files::{FilesContext, LinkContexts};
use crate::markdown::image::{self, ImageUi, Key, Loaded, Origin, Shown};
use crate::markdown::image_source::{self, Format, MAX_BYTES, Rejection, Source};
use crate::markdown::parser::Block;

use super::Transcript;

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
        for (src, _) in sources {
            let key = Key {
                scope: scope.clone(),
                src: SharedString::from(src.to_owned()),
            };
            let row = row_id.clone();
            image::request(
                key,
                cx,
                |cx| self.load_image(src, cx),
                move |this: &mut Self, cx| this.remeasure_row(&row, cx),
            );
        }
    }

    pub(super) fn load_image(
        &self,
        src: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Loaded, SharedString>> {
        let state = self.state.read(cx);
        let contexts = crate::files::link_contexts(state);
        let engine = state.engine().cloned();
        let home = state
            .selected_chat_row()
            .filter(|chat| state.local_device_id.as_deref() == Some(chat.device_id.as_str()))
            .and_then(|_| std::env::var("HOME").ok());
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
                                return decode(bytes, Origin::File(open), cx).await;
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
                decode(bytes, Origin::Url(url), cx).await
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
                decode(bytes, Origin::Inline, cx).await
            }),
            Source::Rejected(Rejection::Insecure) => {
                Task::ready(Err("not loaded over insecure http".into()))
            }
            Source::Rejected(Rejection::Unsupported) | Source::NotImage => {
                Task::ready(Err("unsupported".into()))
            }
        }
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

/// GET an `https://` image, at most [`MAX_BYTES`].
async fn fetch(http: Arc<dyn HttpClient>, url: &str) -> Result<Vec<u8>, SharedString> {
    use futures::AsyncReadExt as _;
    let parsed = url::Url::parse(url).map_err(|_| SharedString::from("network error"))?;
    if parsed.scheme() != "https" {
        return Err("not loaded over insecure http".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("not loaded: the address carries credentials".into());
    }
    let mut response = http
        .get(parsed.as_str(), ().into(), true)
        .await
        .map_err(|_| SharedString::from("network error"))?;
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

/// Check the bytes are a supported image and measure it, off the UI thread.
async fn decode(
    bytes: Vec<u8>,
    origin: Origin,
    cx: &mut gpui::AsyncApp,
) -> Result<Loaded, SharedString> {
    cx.background_executor()
        .spawn(async move { measure(bytes, origin) })
        .await
}

/// A loaded image from its bytes: format by content (the name may lie),
/// size from the header; an SVG is enlarged so a small icon is legible.
pub(super) fn measure(bytes: Vec<u8>, origin: Origin) -> Result<Loaded, SharedString> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(too_large());
    }
    let format = Format::sniff(&bytes).ok_or_else(|| SharedString::from("unsupported format"))?;
    let (gpui_format, bytes, (width, height)) = match format {
        Format::Svg => {
            let source = String::from_utf8_lossy(&bytes).into_owned();
            let (bytes, declared) = crate::files::preview::svg_preview_bytes(&source);
            let min = crate::files::preview::SVG_PREVIEW_MIN;
            // Drawn with its long side at least `min` (see the preview).
            let (w, h) = declared.unwrap_or((min, min));
            let scale = (min / w.max(h)).max(1.0);
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

    #[test]
    fn bytes_are_measured_by_content() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(PNG_1X1)
            .unwrap();
        let loaded = measure(bytes, Origin::Inline).ok().unwrap();
        assert_eq!((loaded.width, loaded.height), (1.0, 1.0));
        assert_eq!(loaded.image.format(), ImageFormat::Png);
        let svg = measure(b"<svg width=\"16\" height=\"8\"/>".to_vec(), Origin::Inline)
            .ok()
            .unwrap();
        assert_eq!(svg.image.format(), ImageFormat::Svg);
        assert!(svg.width >= 16.0 && (svg.width / svg.height - 2.0).abs() < 0.01);
        assert_eq!(
            measure(b"%PDF-1.7".to_vec(), Origin::Inline).err().unwrap(),
            "unsupported format"
        );
    }

    #[test]
    fn oversize_bytes_are_refused() {
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(PNG_1X1)
            .unwrap();
        bytes.resize(MAX_BYTES as usize + 1, 0);
        assert!(
            measure(bytes, Origin::Inline)
                .err()
                .unwrap()
                .starts_with("too large")
        );
    }
}
