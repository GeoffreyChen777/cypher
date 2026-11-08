//! Artifacts: pages the model writes to be looked at — an explainer, a
//! diagram, a small interactive demo. A fence tagged `artifact`
//! (```` ```html artifact title="…" ````) renders as a card instead of its
//! code; clicking it asks the surface to open the page
//! ([`super::render::RenderOptions::artifact`]), which shows it in the side panel. Plain
//! ```` ```html ```` and ```` ```svg ```` fences keep their code and gain a
//! Preview button beside Copy that opens them the same way. A fence tagged
//! `artifact-patch` edits a named page in place ([`patch`]); its card opens
//! the page it updated.

pub mod patch;

use std::rc::Rc;

use gpui::{AnyElement, SharedString, Window, div, prelude::*, px};

use super::parser::ArtifactKind;
use super::render::{BlockCtx, FENCE_RADIUS};
use crate::kit::icons::{self, icon};
use crate::kit::theme::code_block_background;

/// A page to open: what a card (or an HTML fence's Preview) hands the
/// surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactOpen {
    /// The fence's place in its transcript: row key and block index.
    pub key: SharedString,
    pub kind: ArtifactKind,
    /// The fence's `title=`, else the page's `<title>`, else the kind.
    pub title: SharedString,
    /// The model named it (`title=`): a later artifact with the same title
    /// is the same page revised.
    pub named: bool,
    pub code: SharedString,
}

/// Opens a clicked artifact.
pub type ArtifactHandler = Rc<dyn Fn(ArtifactOpen, &mut Window, &mut gpui::App)>;

/// Takes a named page's new revision as it lands in the chat: the page's
/// open tab, if any, reloads.
pub type ArtifactRevised = Rc<dyn Fn(ArtifactOpen, &mut gpui::App)>;

/// How a patch fence went: its hunks that took, of all of them, and
/// whether there was a page under its title to patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchOutcome {
    pub applied: usize,
    pub total: usize,
    pub found_page: bool,
}

/// A patch fence's outcome, by its row and its code.
pub type PatchLookup = Rc<dyn Fn(&str, &str) -> Option<PatchOutcome>>;

/// What to open for the fence at `ctx`.
pub fn open_request(
    kind: ArtifactKind,
    title: Option<&str>,
    code: &str,
    ctx: BlockCtx,
) -> ArtifactOpen {
    ArtifactOpen {
        key: format!("{}#{}", ctx.opts.row_key, ctx.ix).into(),
        kind,
        title: display_title(kind, title, code).into(),
        named: title.is_some(),
        code: code.to_string().into(),
    }
}

/// A card's title: the fence's, else the page's own `<title>`, else a
/// generic name.
pub fn display_title(kind: ArtifactKind, title: Option<&str>, code: &str) -> String {
    title
        .map(str::to_string)
        .or_else(|| page_title(code))
        .unwrap_or_else(|| match kind {
            ArtifactKind::Html => "HTML page".to_string(),
            ArtifactKind::Svg => "SVG image".to_string(),
        })
}

/// The text of a page's `<title>` element, entities decoded.
pub fn page_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let start = open + lower[open..].find('>')? + 1;
    let end = start + lower[start..].find("</title")?;
    let title = html[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    (!title.is_empty()).then_some(title)
}

/// The chat's card for an artifact fence. While its fence streams it reads
/// "Writing…" and doesn't open: a half-written page shows nothing useful.
/// A patch's card reads how its edits went and opens the page it updated.
pub fn render_card(
    kind: ArtifactKind,
    title: Option<&str>,
    code: &str,
    closed: bool,
    patch: bool,
    ctx: BlockCtx,
) -> AnyElement {
    let BlockCtx {
        ix, opts, theme, ..
    } = ctx;
    let writing = !closed && opts.live;
    let (subtitle, warn, opens) = if patch {
        let outcome = opts
            .artifact_patches
            .as_ref()
            .filter(|_| !writing)
            .and_then(|lookup| lookup(&opts.row_key, code));
        patch_subtitle(writing, patch::hunks(code).len(), outcome)
    } else {
        let lines = code.lines().count().max(1);
        let lines = if lines == 1 {
            "1 line".to_string()
        } else {
            format!("{lines} lines")
        };
        let subtitle = if writing {
            format!("Writing… · {lines}")
        } else {
            format!("{} · {lines}", kind.label())
        };
        (subtitle, false, true)
    };
    let open = opts
        .artifact
        .clone()
        .filter(|_| !writing && opens)
        .map(|handler| (handler, open_request(kind, title, code, ctx)));
    let fade_key = format!("{}-artifact{ix}", opts.row_key);
    let plate = code_block_background(theme);
    let chrome_color = theme.code_block_text.unwrap_or(theme.text_muted);
    let body = theme.markdown.body_size;
    // The same plate as a code block, the whole card the click target: a
    // neutral glyph, the title at body size, and a quiet "Open →" in the
    // corner rather than a raised button.
    div()
        .id(SharedString::from(fade_key.clone()))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .px(px(12.0))
        .py(px(10.0))
        .rounded(px(FENCE_RADIUS))
        .border_1()
        .border_color(theme.border)
        .bg(plate)
        .child(
            div()
                .flex_none()
                .size(px(34.0))
                .rounded(px(8.0))
                .bg(theme.ink(0.06))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    icon(if patch { icons::PEN } else { icons::CODE })
                        .size(px(16.0))
                        .text_color(chrome_color),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(
                    div()
                        .truncate()
                        .text_size(px(body))
                        .line_height(px(theme.markdown.body_line_height - 2.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(SharedString::from(display_title(kind, title, code))),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(12.0))
                        .line_height(px(16.0))
                        .text_color(if warn {
                            theme.warning
                        } else {
                            theme.text_muted
                        })
                        .child(SharedString::from(subtitle)),
                ),
        )
        .when_some(open, |el, (handler, request)| {
            let group: SharedString = format!("{fade_key}-group").into();
            el.group(group.clone())
                .cursor_pointer()
                .bg(crate::kit::motion::hover_blend(
                    &fade_key,
                    plate,
                    plate.blend(theme.ink(0.04)),
                ))
                .on_hover(crate::kit::motion::hover_listener(fade_key.clone()))
                .on_click(move |_, window, cx| handler(request.clone(), window, cx))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.0))
                        .text_size(px(12.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text_muted)
                        .group_hover(group.clone(), |s| s.text_color(theme.text))
                        .child(SharedString::from("Open"))
                        .child(
                            icon(icons::ALT_ARROW_RIGHT)
                                .size(px(12.0))
                                .text_color(theme.text_muted)
                                .group_hover(group, |s| s.text_color(theme.text)),
                        ),
                )
        })
        .into_any_element()
}

/// A patch card's subtitle, whether it warns, and whether it opens: only a
/// patch that found its page has one to open.
fn patch_subtitle(
    writing: bool,
    hunks: usize,
    outcome: Option<PatchOutcome>,
) -> (String, bool, bool) {
    let edits = |n: usize| {
        if n == 1 {
            "1 edit".to_string()
        } else {
            format!("{n} edits")
        }
    };
    if writing {
        return (format!("Updating… · {}", edits(hunks)), false, false);
    }
    match outcome {
        None => (format!("Update · {}", edits(hunks)), false, false),
        Some(PatchOutcome {
            found_page: false, ..
        }) => ("No earlier version to update".to_string(), true, false),
        Some(PatchOutcome { applied, total, .. }) if applied == total && total > 0 => {
            (format!("Updated · {}", edits(total)), false, true)
        }
        Some(PatchOutcome {
            applied: 0, total, ..
        }) => (
            format!("Update didn't apply · 0 of {}", edits(total)),
            true,
            true,
        ),
        Some(PatchOutcome { applied, total, .. }) => (
            format!("Updated · {applied} of {} applied", edits(total)),
            true,
            true,
        ),
    }
}

/// The Preview button of a plain HTML/SVG fence, beside its Copy; `None`
/// when the fence doesn't render or the surface opens no artifacts.
pub fn preview_chip(
    language: Option<&str>,
    code: &str,
    ctx: BlockCtx,
) -> Option<gpui::Stateful<gpui::Div>> {
    let kind = ArtifactKind::for_language(language?)?;
    let BlockCtx { opts, theme, .. } = ctx;
    let handler = opts.artifact.clone()?;
    let chrome_color = theme.code_block_text.unwrap_or(theme.text_muted);
    let request = open_request(kind, None, code, ctx);
    let fade_key = format!("{}-preview{}", opts.row_key, ctx.ix);
    Some(
        div()
            .id(SharedString::from(fade_key.clone()))
            .h(px(20.0))
            .px(px(6.0))
            .rounded(px(5.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .cursor_pointer()
            .bg(crate::kit::motion::hover_blend(
                &fade_key,
                gpui::transparent_black(),
                theme.ink(0.08),
            ))
            .on_hover(crate::kit::motion::hover_listener(fade_key))
            .text_size(px(10.5))
            .text_color(chrome_color)
            .on_click(move |_, window, cx| handler(request.clone(), window, cx))
            .child(
                icon(icons::WINDOW_FRAME)
                    .size(px(12.0))
                    .text_color(chrome_color),
            )
            .child(SharedString::from("Preview")),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{point, px};

    use super::*;
    use crate::kit::theme::Theme;
    use crate::markdown::render::RenderOptions;

    struct CardView {
        closed: bool,
        live: bool,
        opened: Rc<RefCell<Vec<ArtifactOpen>>>,
    }

    impl gpui::Render for CardView {
        fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let theme = Theme::of(cx).clone();
            let mut opts = RenderOptions::settled("row".into());
            opts.live = self.live;
            let opened = self.opened.clone();
            opts.artifact = Some(Rc::new(move |artifact, _, _| {
                opened.borrow_mut().push(artifact);
            }));
            div().w(px(400.0)).child(render_card(
                ArtifactKind::Html,
                Some("Demo"),
                "<p>x</p>",
                self.closed,
                false,
                BlockCtx::top(3, &opts, &theme),
            ))
        }
    }

    fn click_card(closed: bool, live: bool, cx: &mut gpui::TestAppContext) -> Vec<ArtifactOpen> {
        cx.update(|cx| cx.set_global(Theme::dark()));
        let opened = Rc::new(RefCell::new(Vec::new()));
        let view_opened = opened.clone();
        let window = cx.open_window(gpui::size(px(500.0), px(200.0)), |_, _| CardView {
            closed,
            live,
            opened: view_opened,
        });
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        visual.simulate_click(point(px(200.0), px(25.0)), Default::default());
        opened.take()
    }

    #[gpui::test]
    fn clicking_a_card_opens_its_page(cx: &mut gpui::TestAppContext) {
        assert_eq!(
            click_card(true, false, cx),
            vec![ArtifactOpen {
                key: "row#3".into(),
                kind: ArtifactKind::Html,
                title: "Demo".into(),
                named: true,
                code: "<p>x</p>".into(),
            }]
        );
    }

    #[gpui::test]
    fn a_card_still_being_written_does_not_open(cx: &mut gpui::TestAppContext) {
        assert!(click_card(false, true, cx).is_empty());
    }

    #[gpui::test]
    fn a_settled_reply_opens_even_an_unclosed_fence(cx: &mut gpui::TestAppContext) {
        assert_eq!(click_card(false, false, cx).len(), 1);
    }

    #[test]
    fn patch_cards_read_how_their_edits_went() {
        let outcome = |applied, total, found_page| {
            Some(PatchOutcome {
                applied,
                total,
                found_page,
            })
        };
        assert_eq!(
            patch_subtitle(true, 1, None),
            ("Updating… · 1 edit".into(), false, false)
        );
        assert_eq!(
            patch_subtitle(false, 2, outcome(2, 2, true)),
            ("Updated · 2 edits".into(), false, true)
        );
        assert_eq!(
            patch_subtitle(false, 3, outcome(2, 3, true)),
            ("Updated · 2 of 3 edits applied".into(), true, true)
        );
        assert_eq!(
            patch_subtitle(false, 1, outcome(0, 1, true)),
            ("Update didn't apply · 0 of 1 edit".into(), true, true)
        );
        assert_eq!(
            patch_subtitle(false, 1, outcome(0, 1, false)),
            ("No earlier version to update".into(), true, false)
        );
        // A surface that indexes no pages has nothing to open.
        assert_eq!(
            patch_subtitle(false, 1, None),
            ("Update · 1 edit".into(), false, false)
        );
    }

    #[test]
    fn page_titles_come_from_the_title_element() {
        assert_eq!(
            page_title("<html><head><TITLE>\n  Sorting &amp; searching </TITLE></head>"),
            Some("Sorting & searching".to_string())
        );
        assert_eq!(page_title("<title></title>"), None);
        assert_eq!(page_title("<div>no title</div>"), None);
        assert_eq!(page_title("<title>never closed"), None);
    }

    #[test]
    fn display_titles_fall_back_to_the_page_then_the_kind() {
        let page = "<title>From the page</title>";
        assert_eq!(
            display_title(ArtifactKind::Html, Some("Named"), page),
            "Named"
        );
        assert_eq!(
            display_title(ArtifactKind::Html, None, page),
            "From the page"
        );
        assert_eq!(
            display_title(ArtifactKind::Svg, None, "<svg/>"),
            "SVG image"
        );
    }
}
