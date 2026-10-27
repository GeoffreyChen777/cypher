//! ```` ```mermaid ```` fences drawn as diagrams.
//!
//! Merman (a headless Rust Mermaid) parses and lays the source out to SVG
//! themed from the chat palette; gpui's resvg rasterizes it. Both run on the
//! background executor, so a fence shows as code until its diagram lands —
//! which is also what a fence still streaming shows: nothing is drawn before
//! the closing fence arrives ([`super::parser::Block::CodeBlock::closed`]).
//! Source Merman rejects stays code, with the reason in the fence header.
//!
//! Surfaces ask for the diagrams they show ([`request`]) and get called back
//! when one lands, so a virtual list can remeasure the row; the renderer only
//! reads what is ready ([`lookup`]). One store per thread (the UI thread)
//! serves every surface: the same source in two places renders once.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, EntityId, Hsla, ImageSource, RenderImage, Rgba, SharedString, Window,
    div, img, prelude::*, px,
};
use merman::svg::{
    CssOverridePolicy, HostTheme, HostThemeAppearance, Presentation, PresentationProfile,
    SvgOutputPolicy, SvgPipelinePreset, SvgRenderOptions, ThemeRole,
};
use merman::{
    Engine, MermaidConfig, OperationControl, RenderOutput, RenderRequest, Renderer, SvgRequest,
};

use crate::kit::theme::{Theme, code_block_background};

use super::parser::Block;
use super::render::{
    BlockCtx, code_block_with_header, copy_button, fence_frame, fence_label, horizontal_scroller,
    render_code_block,
};

/// Fence tags drawn as a diagram.
pub fn is_mermaid_fence(language: Option<&str>) -> bool {
    language
        .is_some_and(|tag| tag.eq_ignore_ascii_case("mermaid") || tag.eq_ignore_ascii_case("mmd"))
}

/// Longer sources are not drawn (and keep a runaway reply from tying up a
/// background thread).
const MAX_SOURCE_LEN: usize = 64 * 1024;
/// Merman checks this between phases; a pathological layout gives up.
const RENDER_DEADLINE: Duration = Duration::from_secs(5);
/// Longest raster side in device pixels. Bigger diagrams rasterize at a lower
/// density rather than allocating a huge texture.
const MAX_RASTER_PX: f32 = 4096.0;
/// Ready diagrams kept, least recently used evicted first.
const MAX_READY: usize = 64;
/// Longest error kept for the fence header.
const MAX_ERROR_LEN: usize = 160;
/// Air around a diagram inside its panel (logical px).
const DIAGRAM_PADDING: f32 = 16.0;
/// A diagram wider than the column shrinks to fit down to this share of its
/// natural size; past that it keeps the size and scrolls sideways, so labels
/// stay legible.
const MIN_FIT: f32 = 0.6;

/// A closed ```` ```mermaid ```` fence: its diagram once drawn (or its
/// source, when the reader switched to it), the code with Merman's complaint
/// when it cannot be drawn, the plain code until then.
pub fn render(code: &str, ctx: BlockCtx, window: &Window) -> AnyElement {
    match lookup(code, Palette::of(ctx.theme), window.scale_factor()) {
        Diagram::Ready { .. } if ctx.opts.diagram_toggled.is_some() && shows_source(code) => {
            code_block_with_header(Some(drawn_header(code, true, ctx)), code, ctx, None, None)
        }
        Diagram::Ready {
            image,
            width,
            height,
        } => render_diagram(image, width, height, code, ctx),
        Diagram::Failed(error) => {
            code_block_with_header(Some(failed_header(error, ctx)), code, ctx, None, None)
        }
        Diagram::Pending => render_code_block(Some("mermaid"), code, ctx, None),
    }
}

fn render_diagram(
    image: Arc<RenderImage>,
    width: f32,
    height: f32,
    code: &str,
    ctx: BlockCtx,
) -> AnyElement {
    let BlockCtx { ix, opts, .. } = ctx;
    let scroll_id: SharedString = format!("{}-diagram{ix}", opts.row_key).into();
    fence_frame(ctx, Some(drawn_header(code, false, ctx)))
        .child(
            horizontal_scroller(scroll_id)
                .p(px(DIAGRAM_PADDING))
                .flex()
                .flex_row()
                .child(
                    // The box owns the size (column width, between MIN_FIT
                    // and 1× natural, at the diagram's aspect); auto margins
                    // center a narrow one and collapse for a wide one, so its
                    // left edge stays reachable by scrolling.
                    div()
                        .flex_none()
                        .mx_auto()
                        .w_full()
                        .max_w(px(width))
                        .min_w(px(width * MIN_FIT))
                        .aspect_ratio(width / height)
                        .child(img(ImageSource::Render(image)).size_full()),
                ),
        )
        .children(copy_button(code, ctx))
        .into_any_element()
}

/// The header of a fence that has a diagram: the tag, then the switch
/// between the diagram and its source (when the surface wires one).
fn drawn_header(code: &str, source: bool, ctx: BlockCtx) -> AnyElement {
    let header = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .child(fence_label("mermaid"));
    match ctx.opts.diagram_toggled.clone() {
        Some(toggled) => header
            .child(view_switch(code, source, toggled, ctx))
            .into_any_element(),
        None => header.into_any_element(),
    }
}

/// Two icon segments, Diagram and Source; the inactive one switches.
fn view_switch(
    code: &str,
    source: bool,
    toggled: super::render::WindowHandler,
    ctx: BlockCtx,
) -> AnyElement {
    let BlockCtx {
        ix, opts, theme, ..
    } = ctx;
    let chrome = theme.code_block_text.unwrap_or(theme.text_muted);
    let code: SharedString = code.to_string().into();
    let segment = |icon: &'static str, label: &'static str, active: bool| {
        let code = code.clone();
        let toggled = toggled.clone();
        div()
            .id(SharedString::from(format!(
                "{}-diagram{ix}-{label}",
                opts.row_key
            )))
            .h(px(16.0))
            .px(px(5.0))
            .rounded(px(4.0))
            .flex()
            .items_center()
            .when(active, |el| el.bg(theme.ink(0.1)))
            .when(!active, |el| {
                el.cursor_pointer()
                    .hover(|style| style.bg(theme.ink(0.06)))
                    .on_click(move |_, window, cx| {
                        set_shows_source(&code, label == "Source");
                        toggled(window, cx);
                    })
            })
            .child(
                crate::kit::icons::icon(icon)
                    .size(px(11.0))
                    .text_color(if active { chrome } else { chrome.opacity(0.55) }),
            )
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(1.0))
        .p(px(1.0))
        .rounded(px(5.0))
        .bg(theme.ink(0.035))
        .child(segment(crate::kit::icons::HIERARCHY, "Diagram", !source))
        .child(segment(crate::kit::icons::CODE, "Source", source))
        .into_any_element()
}

/// The fence tag, then why the diagram could not be drawn. Leaves the right
/// edge to the copy button.
fn failed_header(error: SharedString, ctx: BlockCtx) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .gap(px(8.0))
        .pr(px(64.0))
        .child(fence_label("mermaid"))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .text_color(ctx.theme.danger_muted)
                .child(error),
        )
        .into_any_element()
}

/// What a fence shows right now.
pub enum Diagram {
    /// Rasterized, with its natural size in logical px.
    Ready {
        image: Arc<RenderImage>,
        width: f32,
        height: f32,
    },
    /// Merman could not draw it; the fence stays code.
    Failed(SharedString),
    /// Not drawn yet (or never asked for): the fence shows as code.
    Pending,
}

/// The chat palette a diagram is drawn in, as opaque RGBA: theme changes
/// re-render, and two surfaces in the same theme share rasters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Palette {
    dark: bool,
    roles: [[u8; 4]; ROLES.len()],
    series: [[u8; 4]; 6],
}

const ROLES: [ThemeRole; 22] = [
    ThemeRole::Canvas,
    ThemeRole::Surface,
    ThemeRole::SurfaceAlt,
    ThemeRole::SurfaceMuted,
    ThemeRole::Text,
    ThemeRole::SubtleText,
    ThemeRole::Border,
    ThemeRole::Line,
    ThemeRole::EdgeLabelBackground,
    ThemeRole::ClusterBackground,
    ThemeRole::ClusterBorder,
    ThemeRole::NoteBackground,
    ThemeRole::NoteBorder,
    ThemeRole::NoteText,
    ThemeRole::ActorBackground,
    ThemeRole::ActorBorder,
    ThemeRole::ActorText,
    ThemeRole::ActivationBackground,
    ThemeRole::ActivationBorder,
    ThemeRole::Error,
    ThemeRole::Warning,
    ThemeRole::Success,
];

impl Palette {
    /// The fence panel's colors: the diagram's canvas is the code block fill
    /// over the content plane, its nodes the same ink washes chat chrome uses.
    pub fn of(theme: &Theme) -> Self {
        let canvas = Rgba::from(theme.bg).blend(code_block_background(theme).into());
        let over = |color: Hsla| canvas.blend(color.into());
        let text = theme.code_block_text.unwrap_or(theme.text);
        let surface = over(theme.ink(0.07));
        let surface_alt = over(theme.ink(0.12));
        let surface_muted = over(theme.ink(0.035));
        let border = over(theme.border_strong);
        let roles = [
            canvas,
            surface,
            surface_alt,
            surface_muted,
            over(text),
            over(theme.text_muted),
            border,
            over(theme.text_muted),
            canvas,
            surface_muted,
            border,
            over(theme.warning.opacity(0.14)),
            over(theme.warning.opacity(0.55)),
            over(text),
            surface,
            border,
            over(text),
            surface_alt,
            border,
            over(theme.danger),
            over(theme.warning),
            over(theme.success),
        ];
        let series = [
            theme.accent,
            theme.success,
            theme.warning,
            theme.busy,
            theme.caret,
            theme.danger,
        ];
        Self {
            dark: theme.appearance.is_dark(),
            roles: roles.map(rgba8),
            series: series.map(|color| rgba8(over(color))),
        }
    }

    fn host_theme(&self) -> Result<HostTheme, String> {
        let mut theme = HostTheme::new().with_appearance(if self.dark {
            HostThemeAppearance::Dark
        } else {
            HostThemeAppearance::Light
        });
        for (role, color) in ROLES.into_iter().zip(self.roles) {
            theme = theme
                .try_with_role(role, hex(color))
                .map_err(|error| error.to_string())?;
        }
        theme
            .try_with_series_palette(self.series.map(hex))
            .map_err(|error| error.to_string())
    }
}

fn rgba8(color: Rgba) -> [u8; 4] {
    let Rgba { r, g, b, a } = color;
    [r, g, b, a].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn hex([r, g, b, _]: [u8; 4]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    source: SharedString,
    palette: Palette,
    /// Device pixels per logical px, as f32 bits.
    scale: u32,
}

/// Called with the app once a requested diagram lands.
type Waiter = Box<dyn FnOnce(&mut App)>;

enum State {
    Pending(Vec<(EntityId, Waiter)>),
    Ready {
        image: Arc<RenderImage>,
        width: f32,
        height: f32,
    },
    Failed(SharedString),
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
    /// Diagrams the reader switched to their source, by source text: every
    /// copy of the same diagram follows, and find can tell without a row.
    static SHOWN_AS_SOURCE: RefCell<HashSet<SharedString>> = RefCell::default();
}

fn shows_source(source: &str) -> bool {
    SHOWN_AS_SOURCE.with(|shown| shown.borrow().contains(source))
}

fn set_shows_source(source: &SharedString, shown: bool) {
    SHOWN_AS_SOURCE.with(|set| {
        let mut set = set.borrow_mut();
        if shown {
            set.insert(source.clone());
        } else {
            set.remove(source.as_ref());
        }
    });
}

/// What `source` shows in `palette` at `scale`. A diagram drawn in another
/// palette or at another scale stands in while its redraw is underway, so an
/// appearance switch or a move to another display never flashes the source.
pub fn lookup(source: &str, palette: Palette, scale: f32) -> Diagram {
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        store.clock += 1;
        let now = store.clock;
        let key = Key {
            source: SharedString::from(source.to_string()),
            palette,
            scale: scale.to_bits(),
        };
        if let Some(entry) = store.entries.get_mut(&key) {
            entry.used = now;
            match &entry.state {
                State::Ready {
                    image,
                    width,
                    height,
                } => {
                    return Diagram::Ready {
                        image: image.clone(),
                        width: *width,
                        height: *height,
                    };
                }
                State::Failed(error) => return Diagram::Failed(error.clone()),
                State::Pending(_) => {}
            }
        }
        store
            .entries
            .iter()
            .filter(|(other, _)| other.source == key.source)
            .find_map(|(_, entry)| match &entry.state {
                State::Ready {
                    image,
                    width,
                    height,
                } => Some(Diagram::Ready {
                    image: image.clone(),
                    width: *width,
                    height: *height,
                }),
                _ => None,
            })
            .unwrap_or(Diagram::Pending)
    })
}

/// Whether `source` currently draws as a diagram anywhere — its code text is
/// then not on screen, so in-chat find skips it.
pub fn is_drawn(source: &str) -> bool {
    !shows_source(source)
        && STORE.with(|store| {
            store.borrow().entries.iter().any(|(key, entry)| {
                key.source.as_ref() == source && matches!(entry.state, State::Ready { .. })
            })
        })
}

/// Draw `source` in `palette` at `scale` unless it is drawn (or drawing)
/// already. `on_ready` runs on the requesting entity when it lands — once per
/// entity, however often it asks while the render is underway.
pub fn request<T: 'static>(
    source: &str,
    palette: Palette,
    scale: f32,
    cx: &mut Context<T>,
    on_ready: impl FnOnce(&mut T, &mut Context<T>) + 'static,
) {
    let key = Key {
        source: SharedString::from(source.to_string()),
        palette,
        scale: scale.to_bits(),
    };
    let entity = cx.entity_id();
    let weak = cx.weak_entity();
    let waiter: Waiter = Box::new(move |cx: &mut App| {
        weak.update(cx, |this, cx| on_ready(this, cx)).ok();
    });
    let started = STORE.with(|store| {
        let mut store = store.borrow_mut();
        store.clock += 1;
        let now = store.clock;
        match store.entries.get_mut(&key) {
            Some(entry) => {
                entry.used = now;
                if let State::Pending(waiters) = &mut entry.state
                    && !waiters.iter().any(|(id, _)| *id == entity)
                {
                    waiters.push((entity, waiter));
                }
                false
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
    if !started {
        return;
    }
    let svg_renderer = cx.svg_renderer();
    let job = {
        let key = key.clone();
        cx.background_spawn(async move { draw(&key.source, &key.palette, scale, &svg_renderer) })
    };
    cx.spawn(async move |_, cx| {
        let drawn = job.await;
        cx.update(|cx| land(key, drawn, cx));
    })
    .detach();
}

/// [`request`] every diagram `block` draws, nested ones included, in the
/// theme's palette.
pub fn request_block<T: 'static>(
    block: &Block,
    theme: &Theme,
    scale: f32,
    cx: &mut Context<T>,
    on_ready: impl Fn(&mut T, &mut Context<T>) + Clone + 'static,
) {
    let mut sources = Vec::new();
    collect_sources(block, &mut sources);
    if sources.is_empty() {
        return;
    }
    let palette = Palette::of(theme);
    for source in sources {
        request(source, palette, scale, cx, on_ready.clone());
    }
}

fn collect_sources<'a>(block: &'a Block, out: &mut Vec<&'a str>) {
    match block {
        Block::CodeBlock {
            language,
            code,
            closed: true,
        } if is_mermaid_fence(language.as_deref()) => out.push(code),
        Block::BlockQuote { children } => {
            for child in children {
                collect_sources(child, out);
            }
        }
        Block::List { items, .. } => {
            for child in items.iter().flatten() {
                collect_sources(child, out);
            }
        }
        _ => {}
    }
}

/// Store a finished render, evict past the bound, and wake its waiters.
fn land(key: Key, drawn: Result<(Arc<RenderImage>, f32, f32), String>, cx: &mut App) {
    if let Err(error) = &drawn {
        tracing::debug!(%error, "mermaid diagram failed");
    }
    let (waiters, evicted) = STORE.with(|store| {
        let mut store = store.borrow_mut();
        let state = match drawn {
            Ok((image, width, height)) => State::Ready {
                image,
                width,
                height,
            },
            Err(error) => State::Failed(error.into()),
        };
        let waiters = match store.entries.get_mut(&key) {
            Some(entry) => match std::mem::replace(&mut entry.state, state) {
                State::Pending(waiters) => waiters,
                _ => Vec::new(),
            },
            None => Vec::new(),
        };
        let mut evicted = Vec::new();
        let ready = store
            .entries
            .values()
            .filter(|entry| matches!(entry.state, State::Ready { .. }))
            .count();
        if ready > MAX_READY {
            let mut ages: Vec<u64> = store
                .entries
                .values()
                .filter(|entry| matches!(entry.state, State::Ready { .. }))
                .map(|entry| entry.used)
                .collect();
            ages.sort_unstable();
            let cutoff = ages[ready - MAX_READY - 1];
            store.entries.retain(|_, entry| match &entry.state {
                State::Ready { image, .. } if entry.used <= cutoff => {
                    evicted.push(image.clone());
                    false
                }
                _ => true,
            });
        }
        (waiters, evicted)
    });
    for image in evicted {
        cx.drop_image(image, None);
    }
    for (_, waiter) in waiters {
        waiter(cx);
    }
}

/// Source → themed SVG → raster, off the UI thread. Merman is young and fed
/// model output, so a panic inside it fails this one diagram.
fn draw(
    source: &str,
    palette: &Palette,
    scale: f32,
    svg_renderer: &gpui::SvgRenderer,
) -> Result<(Arc<RenderImage>, f32, f32), String> {
    if source.trim().is_empty() {
        return Err("Empty diagram".into());
    }
    if source.len() > MAX_SOURCE_LEN {
        return Err("Diagram source is too long to draw".into());
    }
    let svg = std::panic::catch_unwind(|| to_svg(source, palette))
        .map_err(|_| "The diagram renderer crashed".to_string())??;
    let (width, height) = svg_size(&svg).ok_or("The diagram has no size")?;
    let raster_scale = scale
        .min(MAX_RASTER_PX / width)
        .min(MAX_RASTER_PX / height)
        .max(0.1);
    let image = svg_renderer
        .render_single_frame(svg.as_bytes(), raster_scale / gpui::SMOOTH_SVG_SCALE_FACTOR)
        .map_err(|error| format!("Could not rasterize the diagram: {error}"))?;
    Ok((image, width, height))
}

fn to_svg(source: &str, palette: &Palette) -> Result<String, String> {
    let presentation = Presentation::new()
        .with_profile(PresentationProfile::MermanModern)
        .with_theme(palette.host_theme()?)
        .resolve();
    // resvg cannot draw HTML labels; Merman's stand-in for them is sized from
    // estimated widths and clips. Native SVG text wraps and fits instead.
    // The font stays Mermaid's default, which Merman's text widths model.
    let config = MermaidConfig::from_value(serde_json::json!({
        "htmlLabels": false,
        "flowchart": { "htmlLabels": false },
    }));
    let renderer = Renderer::new()
        .with_engine(presentation.materialize_engine(Engine::new().with_site_config(config)));
    let output = SvgOutputPolicy {
        preset: SvgPipelinePreset::ResvgSafe,
        css_override_policy: CssOverridePolicy::StripExistingImportant,
        ..SvgOutputPolicy::default()
    };
    let request = SvgRequest {
        pipeline: Some(output.pipeline()),
        presentation: presentation.render_policy(),
        options: SvgRenderOptions {
            diagram_id: Some("cypher-mermaid".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let control = OperationControl::new().with_deadline(RENDER_DEADLINE);
    match renderer.render(RenderRequest::svg(source, control, request)) {
        Ok(RenderOutput::Svg(Some(svg))) => Ok(svg.svg().to_string()),
        Ok(_) => Err("Not a Mermaid diagram".into()),
        Err(error) => Err(short_error(&error.to_string())),
    }
}

/// The first line of an error, bounded for a one-line header.
fn short_error(error: &str) -> String {
    let line = error.lines().next().unwrap_or(error).trim();
    match line.char_indices().nth(MAX_ERROR_LEN) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

/// The root `<svg>`'s viewBox size: the diagram's natural size in px.
fn svg_size(svg: &str) -> Option<(f32, f32)> {
    let start = svg.find("<svg")?;
    let tag = &svg[start..start + svg[start..].find('>')?];
    let view_box = tag.split("viewBox=\"").nth(1)?.split('"').next()?;
    let mut numbers = view_box
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<f32>().ok());
    let (_, _, width, height) = (
        numbers.next()??,
        numbers.next()??,
        numbers.next()??,
        numbers.next()??,
    );
    (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
        .then_some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Palette {
        Palette::of(&Theme::dark())
    }

    #[test]
    fn fence_tags() {
        assert!(is_mermaid_fence(Some("mermaid")));
        assert!(is_mermaid_fence(Some("Mermaid")));
        assert!(is_mermaid_fence(Some("mmd")));
        assert!(!is_mermaid_fence(Some("markdown")));
        assert!(!is_mermaid_fence(None));
    }

    #[test]
    fn common_diagrams_render_to_sized_svg_without_html_labels() {
        for source in [
            "flowchart TD\n  A[Start] -->|go| B{Ready?}\n  B -->|yes| C[(Store)]",
            "sequenceDiagram\n  UI->>Engine: send\n  Engine-->>UI: reply",
            "stateDiagram-v2\n  [*] --> Idle\n  Idle --> Busy: run",
            "classDiagram\n  class Row {\n    +id: String\n  }",
            "erDiagram\n  CHAT ||--o{ MESSAGE : contains",
            "pie title Tokens\n  \"In\" : 40\n  \"Out\" : 60",
            "mindmap\n  root((Cypher))\n    UI\n    Engine",
            "flowchart LR\n  用户 --> 引擎[引擎处理请求]",
        ] {
            let svg =
                to_svg(source, &palette()).unwrap_or_else(|error| panic!("{source}: {error}"));
            let (width, height) = svg_size(&svg).expect("viewBox");
            assert!(width > 0.0 && height > 0.0, "{source}");
            assert!(!svg.contains("<foreignObject"), "{source}");
        }
    }

    #[test]
    fn bad_source_reports_a_one_line_error() {
        let error = to_svg("flowchart TD\n  A --> ", &palette()).unwrap_err();
        assert!(error.contains("parse error"), "{error}");
        assert!(!error.contains('\n'));
        assert_eq!(
            to_svg("hello world", &palette()).unwrap_err(),
            "No Mermaid diagram type detected"
        );
    }

    #[test]
    fn palettes_follow_the_appearance() {
        let dark = Palette::of(&Theme::dark());
        let light = Palette::of(&Theme::light());
        assert_ne!(dark, light);
        assert!(dark.dark && !light.dark);
        // Every color is opaque: the SVG gets plain #rrggbb.
        assert!(dark.roles.iter().chain(&dark.series).all(|c| c[3] == 255));
    }

    #[test]
    fn view_box_size_parses() {
        assert_eq!(
            svg_size(r#"<svg id="x" viewBox="5 5 335.5 399" style="">"#),
            Some((335.5, 399.0))
        );
        assert_eq!(svg_size(r#"<svg viewBox="0,0,10,20">"#), Some((10.0, 20.0)));
        assert_eq!(svg_size(r#"<svg width="10">"#), None);
        assert_eq!(svg_size(r#"<svg viewBox="0 0 0 20">"#), None);
    }

    #[test]
    fn a_diagram_switched_to_source_is_findable_again() {
        let code: SharedString = "flowchart TD\n  Toggle --> Source".into();
        assert!(!shows_source(&code));
        set_shows_source(&code, true);
        assert!(shows_source(&code));
        assert!(!is_drawn(&code), "its text is on screen");
        set_shows_source(&code, false);
        assert!(!shows_source(&code));
    }

    /// The mock theme gallery (`[mock:mermaid-themes]`): every diagram
    /// draws, and the ones that name a theme in their front matter differ
    /// from the app palette.
    #[test]
    fn the_theme_gallery_draws() {
        let gallery = cypher_harness::mock::mermaid_themes_text();
        let tree = crate::markdown::parser::parse_full(&gallery);
        let sources: Vec<&str> = tree
            .blocks
            .iter()
            .filter_map(|top| match &top.block {
                Block::CodeBlock { language, code, .. }
                    if is_mermaid_fence(language.as_deref()) =>
                {
                    Some(code.as_str())
                }
                _ => None,
            })
            .collect();
        assert!(sources.len() >= 30);
        let svgs: Vec<String> = sources
            .iter()
            .map(|source| to_svg(source, &palette()).unwrap_or_else(|e| panic!("{source}: {e}")))
            .collect();
        let themed = svgs
            .iter()
            .filter(|svg| *svg != &svgs[0] && *svg != &svgs[1]);
        assert!(themed.count() >= sources.len() - 4);
    }

    #[test]
    fn errors_stay_short() {
        let long = "x".repeat(400);
        assert_eq!(short_error(&long).chars().count(), MAX_ERROR_LEN + 1);
        assert_eq!(short_error("first\nsecond"), "first");
    }
}
