//! The side panel's Artifact surface: a page from the chat
//! ([`crate::markdown::artifact`]) shown live in a web view, with its source
//! a toggle away. One tab per artifact; opening a named artifact again —
//! the model's revision under the same title — reloads its tab.
//!
//! Pages load as text with no base URL, so they can't read local files;
//! scripts and stylesheets from the web load, and clicked web links open in
//! the browser. Without a web view (Linux) the source shows, and the page
//! opens in the browser.

use std::sync::Arc;

use gpui::{AnyElement, ClipboardItem, Context, SharedString, Task, Window, div, prelude::*, px};

use crate::files::web_view::{self, WebView};
use crate::kit::icons::{self, icon};
use crate::kit::theme::MonoStyled as _;
use crate::kit::theme::Theme;
use crate::markdown::artifact::ArtifactOpen;
use crate::markdown::parser::ArtifactKind;
use crate::markdown::render;

/// What the panel body shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Preview,
    Source,
}

pub struct ArtifactPanel {
    artifact: ArtifactOpen,
    /// Bumped by every load: the web view reloads when it moves.
    revision: u64,
    mode: Mode,
    web: Option<WebView>,
    /// The source's syntax colors, once highlighted off the main thread.
    highlight: Option<Arc<cypher_syntax::HighlightedDocument>>,
    highlight_task: Option<Task<()>>,
    copied: bool,
    copied_clear: Option<Task<()>>,
    error: Option<SharedString>,
    /// How far the dock's rail reaches in from the right edge.
    header_right: f32,
}

impl ArtifactPanel {
    pub fn new(artifact: ArtifactOpen, cx: &mut Context<Self>) -> Self {
        let mut panel = Self {
            artifact: artifact.clone(),
            revision: 0,
            mode: if web_view::AVAILABLE {
                Mode::Preview
            } else {
                Mode::Source
            },
            web: None,
            highlight: None,
            highlight_task: None,
            copied: false,
            copied_clear: None,
            error: None,
            header_right: 8.0,
        };
        panel.show(artifact, cx);
        panel
    }

    /// Whether `artifact` belongs in this tab: the same named page (a
    /// revision included), or the same fence.
    pub fn shows(&self, artifact: &ArtifactOpen) -> bool {
        same_page(&self.artifact, artifact)
    }

    /// Load `artifact` (a revision of this tab's page, or the page again).
    pub fn show(&mut self, artifact: ArtifactOpen, cx: &mut Context<Self>) {
        if self.revision > 0 && artifact == self.artifact {
            return;
        }
        self.revision += 1;
        self.artifact = artifact;
        self.error = None;
        self.highlight = None;
        let source = self.artifact.code.clone();
        let task = cx.background_spawn(async move {
            cypher_syntax::highlight(cypher_syntax::HighlightRequest {
                source: &source,
                path: None,
                fence_tag: Some("html"),
            })
            .ok()
        });
        let revision = self.revision;
        self.highlight_task = Some(cx.spawn(async move |this, cx| {
            let document = task.await;
            this.update(cx, |this, cx| {
                if this.revision == revision {
                    this.highlight = document.map(Arc::new);
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn tab_title(&self) -> SharedString {
        self.artifact.title.clone()
    }

    pub fn set_header_right(&mut self, inset: f32, cx: &mut Context<Self>) {
        if self.header_right != inset {
            self.header_right = inset;
            cx.notify();
        }
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            cx.notify();
        }
    }

    fn copy_source(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.artifact.code.to_string()));
        self.copied = true;
        self.copied_clear = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1200))
                .await;
            this.update(cx, |this, cx| {
                this.copied = false;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Write the page to a private temp file and open it in the browser.
    fn open_in_browser(&mut self, cx: &mut Context<Self>) {
        match write_page(&self.artifact) {
            Ok(path) => {
                self.error = None;
                cx.open_with_system(&path);
            }
            Err(error) => {
                tracing::warn!(error = %error, "could not write the artifact for the browser");
                self.error = Some("Couldn't open it in the browser".into());
            }
        }
        cx.notify();
    }

    /// The margin around the page card; on the right the rail's column
    /// when that is wider.
    fn margin_right(&self) -> f32 {
        self.header_right.max(MARGIN)
    }

    /// The toolbar over the card, its edges aligned with the card's: the
    /// page's name on the left, the view toggle and actions on the right.
    fn render_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.mode;
        let kind = self.artifact.kind;
        let lines = self.artifact.code.lines().count().max(1);
        let meta = format!(
            "{} · {lines} {}",
            kind.label(),
            if lines == 1 { "line" } else { "lines" }
        );
        let accent = theme.accent;
        div()
            .flex_none()
            .h(px(TOOLBAR_HEIGHT))
            .pl(px(MARGIN))
            .pr(px(self.margin_right()))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_none()
                    .size(px(22.0))
                    .rounded(px(6.0))
                    .bg(accent.opacity(0.12))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(icons::CODE).size(px(12.0)).text_color(accent)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap(px(8.0))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(self.artifact.title.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(meta)),
                    ),
            )
            .when_some(self.error.clone(), |el, error| {
                el.child(
                    div()
                        .flex_none()
                        .max_w(px(200.0))
                        .truncate()
                        .text_size(px(11.0))
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            .when(web_view::AVAILABLE, |el| {
                el.child(segmented([
                    chip("artifact-preview", "Preview", mode == Mode::Preview, theme)
                        .on_click(cx.listener(|this, _, _, cx| this.set_mode(Mode::Preview, cx))),
                    chip("artifact-source", "Source", mode == Mode::Source, theme)
                        .on_click(cx.listener(|this, _, _, cx| this.set_mode(Mode::Source, cx))),
                ]))
                .child(div().flex_none().w(px(1.0)).h(px(14.0)).bg(theme.border))
            })
            .child(
                icon_button(
                    "artifact-copy",
                    if self.copied {
                        icons::CHECK
                    } else {
                        icons::COPY
                    },
                    if self.copied { "Copied" } else { "Copy source" },
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.copy_source(cx))),
            )
            .child(
                icon_button(
                    "artifact-browser",
                    icons::GLOBAL,
                    match kind {
                        ArtifactKind::Html => "Open in browser",
                        ArtifactKind::Svg => "Open in default app",
                    },
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.open_in_browser(cx))),
            )
            .into_any_element()
    }

    /// The source, numbered and colored, on the code surface. Rows are
    /// rebuilt per render: artifacts run to hundreds of lines, not tens of
    /// thousands.
    fn render_source(&self, theme: &Theme) -> AnyElement {
        let mono = theme.mono();
        let code = self.artifact.code.as_ref();
        let total = code.split('\n').count();
        let gutter = total.to_string().len() as f32 * 7.5 + 20.0;
        let spans = self.highlight.as_deref().map(|document| &document.lines);
        let rows = code.split('\n').enumerate().map(|(ix, line)| {
            // An empty row still needs a run to lay out at full height.
            let text = if line.is_empty() { " " } else { line };
            let line_spans = spans
                .and_then(|lines| lines.get(ix))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let runs = render::runs_for_syntax_line(text, line_spans, &mono, theme);
            div()
                .flex()
                .flex_row()
                .child(
                    div()
                        .flex_none()
                        .w(px(gutter))
                        .pr(px(12.0))
                        .flex()
                        .justify_end()
                        .text_color(theme.text_faint)
                        .child(SharedString::from((ix + 1).to_string())),
                )
                .child(gpui::StyledText::new(text.to_string()).with_runs(runs))
        });
        div()
            .id("artifact-source-scroll")
            .size_full()
            .overflow_scroll()
            .py(px(10.0))
            .pr(px(16.0))
            .mono(theme)
            .text_size(px(theme.markdown.code_size))
            .line_height(px(theme.markdown.code_line_height))
            .whitespace_nowrap()
            .cursor_text()
            .child(
                div()
                    .min_w_full()
                    .flex()
                    .flex_col()
                    .items_start()
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_preview(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let page = page_html(&self.artifact);
        let web = self.web.get_or_insert_with(WebView::new);
        web.set_corner_radius((CARD_RADIUS - 1.0) as f64);
        web.load(
            self.revision,
            web_view::Source::Text {
                html: &page,
                base: None,
            },
        );
        web.element(format!("artifact-web:{}", cx.entity_id()).into(), false)
    }
}

/// The page card's margin to the panel's edges.
const MARGIN: f32 = 10.0;
const CARD_RADIUS: f32 = crate::markdown::render::FENCE_RADIUS;
const TOOLBAR_HEIGHT: f32 = 40.0;

impl Render for ArtifactPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let toolbar = self.render_toolbar(&theme, cx);
        let preview = self.mode == Mode::Preview && web_view::AVAILABLE;
        let body = if preview {
            self.render_preview(cx)
        } else {
            self.render_source(&theme)
        };
        // The page sits on a card of its own: a hairline frame around the
        // native view (inset by the border, its own corners rounded to
        // match). The web view paints the page's own background (white
        // unless the page sets one), so the card keeps the theme's tone: a
        // white card showed through the 1px inset as a bright ring in dark
        // mode. The source shows on the same card: the code wash over the
        // opaque card tone.
        let card = div()
            .flex_1()
            .min_h_0()
            .ml(px(MARGIN))
            .mr(px(self.margin_right()))
            .mb(px(MARGIN))
            .p(px(1.0))
            .rounded(px(CARD_RADIUS))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_card)
            .overflow_hidden()
            .child(
                div()
                    .size_full()
                    .rounded(px(CARD_RADIUS - 1.0))
                    .when(!preview, |el| {
                        el.bg(crate::kit::theme::code_block_background(&theme))
                    })
                    .child(body),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(card)
    }
}

/// Whether `other` is the page `shown` is: a named page by its title,
/// another by its fence.
fn same_page(shown: &ArtifactOpen, other: &ArtifactOpen) -> bool {
    if other.named {
        shown.named && shown.title == other.title
    } else {
        shown.key == other.key
    }
}

/// The document the web view loads: HTML as written, an SVG centered on
/// a page of its own with room around it.
fn page_html(artifact: &ArtifactOpen) -> String {
    match artifact.kind {
        ArtifactKind::Html => artifact.code.to_string(),
        ArtifactKind::Svg => format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><style>\
             html,body{{margin:0;height:100%}}\
             body{{box-sizing:border-box;padding:24px;\
             display:flex;align-items:center;justify-content:center}}\
             svg{{max-width:100%;max-height:100%}}</style></head><body>{}</body></html>",
            artifact.code
        ),
    }
}

/// The artifact as a file in a private temp folder, named by its content.
fn write_page(artifact: &ArtifactOpen) -> std::io::Result<std::path::PathBuf> {
    use std::hash::{Hash as _, Hasher as _};
    let dir = std::env::temp_dir().join("cypher-artifacts");
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    artifact.code.hash(&mut hasher);
    let extension = match artifact.kind {
        ArtifactKind::Html => "html",
        ArtifactKind::Svg => "svg",
    };
    let path = dir.join(format!("{:016x}.{extension}", hasher.finish()));
    std::fs::write(&path, artifact.code.as_bytes())?;
    Ok(path)
}

fn chip(
    id: &'static str,
    label: &'static str,
    active: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(20.0))
        .px(px(7.0))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .cursor_pointer()
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(active, |el| {
            el.bg(crate::kit::theme::wash(0.12)).text_color(theme.text)
        })
        .when(!active, |el| {
            el.text_color(theme.text_muted)
                .hover(|s| s.bg(crate::kit::theme::wash(0.06)))
        })
        .child(SharedString::from(label))
}

fn segmented<const N: usize>(chips: [gpui::Stateful<gpui::Div>; N]) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(2.0))
        .p(px(1.0))
        .rounded(px(6.0))
        .bg(crate::kit::theme::ink(0.04))
        .children(chips)
}

fn icon_button(
    id: &'static str,
    icon_path: &'static str,
    tip: &'static str,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(24.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        .hover(|s| s.bg(crate::kit::theme::wash(0.12)))
        .tooltip(move |_, cx| cx.new(|_| Tooltip(tip.into())).into())
        .child(icon(icon_path).size(px(13.0)).text_color(theme.text_muted))
}

struct Tooltip(SharedString);

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        crate::kit::motion::fade_quick(
            "artifact-tooltip",
            div()
                .px(px(8.0))
                .py(px(5.0))
                .rounded(px(5.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.surface_raised)
                .text_size(px(11.0))
                .text_color(theme.text_muted)
                .child(self.0.clone()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(kind: ArtifactKind, title: &str, named: bool, key: &str) -> ArtifactOpen {
        ArtifactOpen {
            key: key.to_string().into(),
            kind,
            title: title.to_string().into(),
            named,
            code: "<p>x</p>".into(),
        }
    }

    #[test]
    fn named_pages_match_by_title_and_others_by_fence() {
        let first = artifact(ArtifactKind::Html, "Sorting", true, "a#0");
        // The model's revision: another message, the same title.
        assert!(same_page(
            &first,
            &artifact(ArtifactKind::Html, "Sorting", true, "b#2")
        ));
        assert!(!same_page(
            &first,
            &artifact(ArtifactKind::Html, "Searching", true, "a#0")
        ));
        // An unnamed fence is its own page, even under the same `<title>`.
        let unnamed = artifact(ArtifactKind::Html, "Sorting", false, "c#1");
        assert!(!same_page(&first, &unnamed));
        assert!(same_page(&unnamed, &unnamed.clone()));
        assert!(!same_page(
            &unnamed,
            &artifact(ArtifactKind::Html, "Sorting", false, "c#3")
        ));
    }

    #[test]
    fn svg_pages_wrap_the_image_and_html_stays_as_written() {
        let svg = artifact(ArtifactKind::Svg, "Logo", true, "r#0");
        let page = page_html(&svg);
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.contains("<body><p>x</p></body>"));
        let html = artifact(ArtifactKind::Html, "Page", true, "r#0");
        assert_eq!(page_html(&html), "<p>x</p>");
    }

    #[test]
    fn pages_for_the_browser_are_named_by_content() {
        let page = artifact(ArtifactKind::Html, "Page", true, "r#0");
        let path = write_page(&page).unwrap();
        assert_eq!(path.extension().unwrap(), "html");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "<p>x</p>");
        assert_eq!(write_page(&page).unwrap(), path);
    }
}
