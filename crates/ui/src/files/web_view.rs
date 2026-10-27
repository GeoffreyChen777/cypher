//! The HTML preview: a native `WKWebView` over the Files surface, wherever
//! the preview element paints. gpui draws the whole window into one Metal
//! layer, so a web page can only be a sibling AppKit view on top of it. Each
//! paint moves it to the element's bounds, clipped to the pane.
//!
//! Hiding it when the element stops painting goes through gpui's element
//! state: every paint stores a [`Mount`] guard. A cached view's replay
//! carries its states into the next frame without painting; a frame that no
//! longer draws the element drops them, and the guard's drop hides the view.
//!
//! A native view also covers gpui's own floating layers, so the shell's
//! dialogs suppress every web preview while they show ([`set_suppressed`]),
//! and the Files surface hides its own while its menu is open.
//!
//! macOS only: elsewhere HTML opens as source ([`AVAILABLE`]). Chat
//! artifacts ([`crate::artifacts`]) show in the same view.

use std::rc::Rc;

use gpui::{AnyElement, Bounds, ElementId, Pixels, SharedString, prelude::*};

/// Whether this platform can show an HTML preview.
pub const AVAILABLE: bool = cfg!(target_os = "macos");

/// What a preview shows.
pub enum Source<'a> {
    /// A file on this machine, as saved: loaded from disk so its relative
    /// stylesheets, scripts and images (anywhere under `root`) load too.
    File { path: &'a str, root: &'a str },
    /// Text (unsaved edits, or another device's file). Relative links
    /// resolve against `base` when given, but WebKit only lets them read
    /// files once a [`Source::File`] load has opened the folder.
    Text {
        html: &'a str,
        base: Option<&'a str>,
    },
}

/// One Files surface's web view (created hidden; shown by painting
/// [`WebView::element`]).
pub struct WebView {
    inner: Rc<native::Inner>,
}

impl WebView {
    pub fn new() -> Self {
        Self {
            inner: native::Inner::new(),
        }
    }

    /// Show `source`, unless `key` names what is already loaded.
    pub fn load(&self, key: u64, source: Source<'_>) {
        if self.inner.key.replace(Some(key)) != Some(key) {
            match source {
                Source::File { path, root } => self.inner.load_file(path, root),
                Source::Text { html, base } => self.inner.load(html, base),
            }
        }
    }

    /// Round the page's corners by `radius` points (it paints above gpui,
    /// so a rounded frame drawn around it can't clip it).
    pub fn set_corner_radius(&self, radius: f64) {
        self.inner.set_corner_radius(radius);
    }

    /// The element the web view tracks; `suppressed` keeps it hidden (a menu
    /// over it).
    pub fn element(&self, id: SharedString, suppressed: bool) -> AnyElement {
        let inner = self.inner.clone();
        gpui::canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                let clip = window.content_mask().bounds;
                window.with_global_id(ElementId::Name(id), |global_id, window| {
                    window.with_element_state::<Mount, _>(global_id, |mount, window| {
                        let mount = mount
                            .filter(|mount| Rc::ptr_eq(&mount.0, &inner))
                            .unwrap_or_else(|| Mount::new(inner.clone()));
                        inner.place(bounds, clip, suppressed, window);
                        ((), mount)
                    })
                });
            },
        )
        .size_full()
        .into_any_element()
    }
}

/// Alive while the element is in the rendered frame.
struct Mount(Rc<native::Inner>);

impl Mount {
    fn new(inner: Rc<native::Inner>) -> Self {
        inner.mounts.set(inner.mounts.get() + 1);
        Self(inner)
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        self.0.mounts.set(self.0.mounts.get() - 1);
        self.0.apply();
    }
}

/// An AppKit frame: `(x, y, width, height)` in points.
type Frame = (f64, f64, f64, f64);

/// Where the native views go for a page at `bounds` clipped to `clip` (both
/// window points, top-left origin): the clipping container's frame in its
/// host view (`host_height` tall, `flipped` or not), and the page's frame
/// inside the unflipped container — offset so the clip cuts it, never
/// squeezes it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn frames(
    bounds: Bounds<Pixels>,
    clip: Bounds<Pixels>,
    host_height: f64,
    flipped: bool,
) -> (Frame, Frame) {
    let rect = |b: Bounds<Pixels>| {
        (
            f32::from(b.origin.x) as f64,
            f32::from(b.origin.y) as f64,
            f32::from(b.size.width) as f64,
            f32::from(b.size.height) as f64,
        )
    };
    let (vx, vy, vw, vh) = rect(bounds.intersect(&clip));
    let (bx, by, bw, bh) = rect(bounds);
    let container_y = if flipped { vy } else { host_height - vy - vh };
    // The page's bottom edge sits its height (plus whatever the clip cuts
    // off above) below the container's top.
    (
        (vx, container_y, vw, vh),
        (bx - vx, vh - (by - vy) - bh, bw, bh),
    )
}

/// The folder an HTML file's relative links resolve against, from its
/// absolute path.
pub(super) fn base_dir(file: &str) -> String {
    match file.rsplit_once('/') {
        Some((dir, _)) if !dir.is_empty() => dir.to_string(),
        _ => "/".to_string(),
    }
}

/// Hide (or restore) every web preview — the shell's dialogs paint where a
/// native view would cover them.
pub fn set_suppressed(suppressed: bool) {
    native::set_suppressed(suppressed);
}

/// Dev capture: what the web views look like, for composing captures (gpui
/// renders its own scene only; the window server is off limits).
#[cfg(all(feature = "dev-capture", target_os = "macos"))]
pub mod debug {
    /// One shown web view: its visible frame and page frame in window points
    /// (top-left origin), and its page as PNG.
    pub struct Shot {
        pub visible: (f64, f64, f64, f64),
        pub page: (f64, f64, f64, f64),
        pub png: Option<Vec<u8>>,
    }

    /// One line per live web view: frames, visibility, mounts.
    pub fn describe() -> Vec<String> {
        super::native::describe()
    }

    /// Snapshots of every shown web view.
    pub async fn shots() -> Vec<Shot> {
        super::native::shots().await
    }
}

#[cfg(target_os = "macos")]
// `msg_send!` expands an obsolete `feature = "cargo-clippy"` gate that rustc
// now flags; the warning is the objc crate's, not this call site's.
#[allow(unexpected_cfgs)]
mod native {
    use std::cell::{Cell, RefCell};
    use std::rc::{Rc, Weak};

    use gpui::{Bounds, Pixels, Window};
    use objc::runtime::{BOOL, Class, NO, Object, Sel, YES};
    use objc::{class, msg_send, sel, sel_impl};

    #[link(name = "WebKit", kind = "framework")]
    unsafe extern "C" {}

    #[repr(C)]
    #[derive(Clone, Copy, PartialEq)]
    struct NSPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, PartialEq)]
    struct NSSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, PartialEq)]
    struct NSRect {
        origin: NSPoint,
        size: NSSize,
    }

    impl NSRect {
        const ZERO: Self = Self::new(0.0, 0.0, 0.0, 0.0);

        const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
            Self {
                origin: NSPoint { x, y },
                size: NSSize { width, height },
            }
        }
    }

    const NS_WINDOW_ABOVE: isize = 1;
    const NS_UTF8_STRING_ENCODING: usize = 4;

    thread_local! {
        static SUPPRESSED: Cell<bool> = const { Cell::new(false) };
        static LIVE: RefCell<Vec<Weak<Inner>>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn set_suppressed(suppressed: bool) {
        if SUPPRESSED.replace(suppressed) == suppressed {
            return;
        }
        let live: Vec<Rc<Inner>> = LIVE.with_borrow_mut(|live| {
            live.retain(|inner| inner.strong_count() > 0);
            live.iter().filter_map(Weak::upgrade).collect()
        });
        for inner in live {
            inner.apply();
        }
    }

    pub(super) struct Inner {
        /// A clipping NSView (the pane's visible part) holding the web view
        /// at the element's full bounds. Both retained.
        container: *mut Object,
        web: *mut Object,
        /// gpui's view, the container's sibling below.
        host: Cell<*mut Object>,
        pub(super) key: Cell<Option<u64>>,
        pub(super) mounts: Cell<usize>,
        suppressed: Cell<bool>,
    }

    impl Inner {
        pub(super) fn new() -> Rc<Self> {
            let (container, web) = unsafe {
                let config: *mut Object = msg_send![class!(WKWebViewConfiguration), new];
                let web: *mut Object = msg_send![class!(WKWebView), alloc];
                let web: *mut Object =
                    msg_send![web, initWithFrame: NSRect::ZERO configuration: config];
                let _: () = msg_send![config, release];
                let _: () = msg_send![web, setNavigationDelegate: delegate()];
                // Safari's Web Inspector can attach (macOS 13.3+).
                let inspectable: BOOL = msg_send![web, respondsToSelector: sel!(setInspectable:)];
                if inspectable == YES {
                    let _: () = msg_send![web, setInspectable: YES];
                }
                let container: *mut Object = msg_send![class!(NSView), alloc];
                let container: *mut Object = msg_send![container, initWithFrame: NSRect::ZERO];
                let _: () = msg_send![container, setWantsLayer: YES];
                let layer: *mut Object = msg_send![container, layer];
                if !layer.is_null() {
                    let _: () = msg_send![layer, setMasksToBounds: YES];
                }
                let _: () = msg_send![container, addSubview: web];
                let _: () = msg_send![container, setHidden: YES];
                (container, web)
            };
            let inner = Rc::new(Self {
                container,
                web,
                host: Cell::new(std::ptr::null_mut()),
                key: Cell::new(None),
                mounts: Cell::new(0),
                suppressed: Cell::new(false),
            });
            LIVE.with_borrow_mut(|live| live.push(Rc::downgrade(&inner)));
            inner
        }

        pub(super) fn set_corner_radius(&self, radius: f64) {
            unsafe {
                let layer: *mut Object = msg_send![self.container, layer];
                if !layer.is_null() {
                    let _: () = msg_send![layer, setCornerRadius: radius];
                }
            }
        }

        pub(super) fn load(&self, html: &str, base: Option<&str>) {
            unsafe {
                let source = ns_string(html);
                let base_url: *mut Object = match base {
                    Some(dir) => {
                        let path = ns_string(dir);
                        let url: *mut Object =
                            msg_send![class!(NSURL), fileURLWithPath: path isDirectory: YES];
                        let _: () = msg_send![path, release];
                        url
                    }
                    None => std::ptr::null_mut(),
                };
                let _: *mut Object = msg_send![self.web, loadHTMLString: source baseURL: base_url];
                let _: () = msg_send![source, release];
            }
        }

        pub(super) fn load_file(&self, path: &str, root: &str) {
            unsafe {
                let file = ns_string(path);
                let url: *mut Object = msg_send![class!(NSURL), fileURLWithPath: file];
                let folder = ns_string(root);
                let access: *mut Object =
                    msg_send![class!(NSURL), fileURLWithPath: folder isDirectory: YES];
                let _: *mut Object =
                    msg_send![self.web, loadFileURL: url allowingReadAccessToURL: access];
                let _: () = msg_send![file, release];
                let _: () = msg_send![folder, release];
            }
        }

        /// Follow the element: `bounds` in window points, clipped to `clip`.
        pub(super) fn place(
            &self,
            bounds: Bounds<Pixels>,
            clip: Bounds<Pixels>,
            suppressed: bool,
            window: &Window,
        ) {
            self.suppressed.set(suppressed);
            let Some(host) = gpui_view(window) else {
                return self.apply();
            };
            unsafe {
                let superview: *mut Object = msg_send![host, superview];
                if superview.is_null() {
                    return self.apply();
                }
                let current: *mut Object = msg_send![self.container, superview];
                if current != superview {
                    let _: () = msg_send![self.container, removeFromSuperview];
                    let _: () = msg_send![
                        superview,
                        addSubview: self.container
                        positioned: NS_WINDOW_ABOVE
                        relativeTo: host
                    ];
                    accept_clicks_back(host);
                }
                self.host.set(host);
                let host_bounds: NSRect = msg_send![host, bounds];
                let flipped: BOOL = msg_send![host, isFlipped];
                let (container, page) =
                    super::frames(bounds, clip, host_bounds.size.height, flipped == YES);
                let (x, y, w, h) = container;
                let frame: NSRect = msg_send![
                    host,
                    convertRect: NSRect::new(x, y, w, h)
                    toView: superview
                ];
                set_frame(self.container, frame);
                let (x, y, w, h) = page;
                set_frame(self.web, NSRect::new(x, y, w, h));
            }
            self.apply();
        }

        pub(super) fn apply(&self) {
            let shown = self.mounts.get() > 0 && !self.suppressed.get() && !SUPPRESSED.get();
            unsafe {
                let hidden: BOOL = msg_send![self.container, isHidden];
                if (hidden == NO) == shown {
                    return;
                }
                if !shown {
                    self.release_focus();
                }
                let _: () = msg_send![self.container, setHidden: if shown { NO } else { YES }];
            }
        }

        /// Typing returns to gpui when the page that had it hides.
        unsafe fn release_focus(&self) {
            unsafe {
                let window: *mut Object = msg_send![self.container, window];
                if window.is_null() {
                    return;
                }
                let responder: *mut Object = msg_send![window, firstResponder];
                if responder.is_null() {
                    return;
                }
                let is_view: BOOL = msg_send![responder, isKindOfClass: class!(NSView)];
                if is_view == NO {
                    return;
                }
                let inside: BOOL = msg_send![responder, isDescendantOf: self.container];
                let host = self.host.get();
                if inside == YES && !host.is_null() {
                    let _: BOOL = msg_send![window, makeFirstResponder: host];
                }
            }
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            unsafe {
                self.release_focus();
                let _: () = msg_send![self.web, stopLoading];
                let _: () = msg_send![self.container, removeFromSuperview];
                let _: () = msg_send![self.web, release];
                let _: () = msg_send![self.container, release];
            }
        }
    }

    /// `view`'s bounds in window points, top-left origin.
    #[cfg(feature = "dev-capture")]
    unsafe fn window_rect(view: *mut Object) -> Option<(f64, f64, f64, f64)> {
        unsafe {
            let window: *mut Object = msg_send![view, window];
            if window.is_null() {
                return None;
            }
            let content: *mut Object = msg_send![window, contentView];
            let content: NSRect = msg_send![content, bounds];
            let bounds: NSRect = msg_send![view, bounds];
            let r: NSRect =
                msg_send![view, convertRect: bounds toView: std::ptr::null_mut::<Object>()];
            Some((
                r.origin.x,
                content.size.height - r.origin.y - r.size.height,
                r.size.width,
                r.size.height,
            ))
        }
    }

    #[cfg(feature = "dev-capture")]
    fn live() -> Vec<Rc<Inner>> {
        LIVE.with_borrow(|live| live.iter().filter_map(Weak::upgrade).collect())
    }

    #[cfg(feature = "dev-capture")]
    pub(super) fn describe() -> Vec<String> {
        live()
            .iter()
            .map(|inner| unsafe {
                let hidden: BOOL = msg_send![inner.container, isHidden];
                format!(
                    "webview hidden={} mounts={} suppressed={}/{} visible={:?} page={:?}",
                    hidden == YES,
                    inner.mounts.get(),
                    inner.suppressed.get(),
                    SUPPRESSED.get(),
                    window_rect(inner.container),
                    window_rect(inner.web),
                )
            })
            .collect()
    }

    #[cfg(feature = "dev-capture")]
    pub(super) async fn shots() -> Vec<super::debug::Shot> {
        let mut shots = Vec::new();
        for inner in live() {
            let (visible, page) = unsafe {
                let hidden: BOOL = msg_send![inner.container, isHidden];
                if hidden == YES {
                    continue;
                }
                match (window_rect(inner.container), window_rect(inner.web)) {
                    (Some(visible), Some(page)) => (visible, page),
                    _ => continue,
                }
            };
            let (tx, rx) = futures::channel::oneshot::channel::<Option<Vec<u8>>>();
            let tx = std::cell::RefCell::new(Some(tx));
            let handler =
                block::ConcreteBlock::new(move |image: *mut Object, _error: *mut Object| {
                    let png = unsafe { png_of(image) };
                    if let Some(tx) = tx.borrow_mut().take() {
                        let _ = tx.send(png);
                    }
                })
                .copy();
            unsafe {
                let _: () = msg_send![
                    inner.web,
                    takeSnapshotWithConfiguration: std::ptr::null_mut::<Object>()
                    completionHandler: &*handler
                ];
            }
            let png = rx.await.ok().flatten();
            shots.push(super::debug::Shot { visible, page, png });
        }
        shots
    }

    /// An NSImage as PNG bytes.
    #[cfg(feature = "dev-capture")]
    unsafe fn png_of(image: *mut Object) -> Option<Vec<u8>> {
        const PNG: usize = 4;
        unsafe {
            if image.is_null() {
                return None;
            }
            let tiff: *mut Object = msg_send![image, TIFFRepresentation];
            let rep: *mut Object = msg_send![class!(NSBitmapImageRep), imageRepWithData: tiff];
            if rep.is_null() {
                return None;
            }
            let props: *mut Object = msg_send![class!(NSDictionary), dictionary];
            let data: *mut Object = msg_send![rep, representationUsingType: PNG properties: props];
            if data.is_null() {
                return None;
            }
            let len: usize = msg_send![data, length];
            let bytes: *const u8 = msg_send![data, bytes];
            Some(std::slice::from_raw_parts(bytes, len).to_vec())
        }
    }

    unsafe fn set_frame(view: *mut Object, frame: NSRect) {
        unsafe {
            let current: NSRect = msg_send![view, frame];
            if current != frame {
                let _: () = msg_send![view, setFrame: frame];
            }
        }
    }

    /// A retained NSString (no NUL-termination needed).
    unsafe fn ns_string(text: &str) -> *mut Object {
        unsafe {
            let string: *mut Object = msg_send![class!(NSString), alloc];
            msg_send![
                string,
                initWithBytes: text.as_ptr()
                length: text.len()
                encoding: NS_UTF8_STRING_ENCODING
            ]
        }
    }

    /// The NSView gpui draws the window into.
    fn gpui_view(window: &Window) -> Option<*mut Object> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::AppKit(handle) => Some(handle.ns_view.as_ptr().cast()),
            _ => None,
        }
    }

    /// gpui's view never asks to be first responder (it is made one once,
    /// at window creation), so a click back into gpui after the page took
    /// the keyboard would leave typing in the page. Answering YES lets
    /// AppKit hand it back on that click; installed once, for gpui's class.
    unsafe fn accept_clicks_back(host: *mut Object) {
        use std::sync::atomic::{AtomicBool, Ordering};
        static INSTALLED: AtomicBool = AtomicBool::new(false);
        extern "C" fn accepts(_this: &Object, _sel: Sel) -> BOOL {
            YES
        }
        if INSTALLED.swap(true, Ordering::Relaxed) {
            return;
        }
        unsafe {
            let class = objc::runtime::object_getClass(host) as *mut Class;
            let imp: objc::runtime::Imp =
                std::mem::transmute(accepts as extern "C" fn(&Object, Sel) -> BOOL);
            objc::runtime::class_addMethod(
                class,
                sel!(acceptsFirstResponder),
                imp,
                c"c@:".as_ptr(),
            );
        }
    }

    /// Link clicks to the web open in the browser; everything else (anchors,
    /// relative pages, scripts) stays in the preview.
    fn delegate() -> *mut Object {
        use std::sync::OnceLock;

        use block::Block;
        use objc::declare::ClassDecl;

        const LINK_ACTIVATED: isize = 0;
        const CANCEL: isize = 0;
        const ALLOW: isize = 1;

        extern "C" fn decide(
            _this: &Object,
            _sel: Sel,
            _web: *mut Object,
            action: *mut Object,
            // A `void (^)(WKNavigationActionPolicy)` block.
            handler: *mut Object,
        ) {
            unsafe {
                let kind: isize = msg_send![action, navigationType];
                let frame: *mut Object = msg_send![action, targetFrame];
                let request: *mut Object = msg_send![action, request];
                let url: *mut Object = msg_send![request, URL];
                let scheme: *mut Object = msg_send![url, scheme];
                let scheme = if scheme.is_null() {
                    String::new()
                } else {
                    let utf8: *const std::ffi::c_char = msg_send![scheme, UTF8String];
                    std::ffi::CStr::from_ptr(utf8)
                        .to_string_lossy()
                        .to_ascii_lowercase()
                };
                let external = matches!(scheme.as_str(), "http" | "https" | "mailto")
                    && (kind == LINK_ACTIVATED || frame.is_null());
                if external {
                    let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
                    let _: BOOL = msg_send![workspace, openURL: url];
                }
                let handler = &*(handler as *const Block<(isize,), ()>);
                handler.call((if external { CANCEL } else { ALLOW },));
            }
        }

        // Leaked singleton (as usize — raw pointers aren't `Sync`): the web
        // view's `navigationDelegate` is weak.
        static DELEGATE: OnceLock<usize> = OnceLock::new();
        *DELEGATE.get_or_init(|| unsafe {
            let mut decl = ClassDecl::new("CypherWebPreviewDelegate", class!(NSObject))
                .expect("CypherWebPreviewDelegate registered twice");
            decl.add_method(
                sel!(webView:decidePolicyForNavigationAction:decisionHandler:),
                decide as extern "C" fn(&Object, Sel, *mut Object, *mut Object, *mut Object),
            );
            let class = decl.register();
            let instance: *mut Object = msg_send![class, new];
            instance as usize
        }) as *mut Object
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{Bounds, Pixels, Window};

    pub(super) fn set_suppressed(_suppressed: bool) {}

    pub(super) struct Inner {
        pub(super) key: Cell<Option<u64>>,
        pub(super) mounts: Cell<usize>,
    }

    impl Inner {
        pub(super) fn new() -> Rc<Self> {
            Rc::new(Self {
                key: Cell::new(None),
                mounts: Cell::new(0),
            })
        }

        pub(super) fn set_corner_radius(&self, _radius: f64) {}

        pub(super) fn load(&self, _html: &str, _base: Option<&str>) {}

        pub(super) fn load_file(&self, _path: &str, _root: &str) {}

        pub(super) fn place(
            &self,
            _bounds: Bounds<Pixels>,
            _clip: Bounds<Pixels>,
            _suppressed: bool,
            _window: &Window,
        ) {
        }

        pub(super) fn apply(&self) {}
    }
}

#[cfg(test)]
mod tests {
    use gpui::{point, px, size};

    use super::*;

    fn b(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    #[test]
    fn an_unclipped_page_fills_its_container() {
        let bounds = b(100.0, 50.0, 300.0, 400.0);
        let window = b(0.0, 0.0, 1000.0, 800.0);
        assert_eq!(
            frames(bounds, window, 800.0, false),
            ((100.0, 350.0, 300.0, 400.0), (0.0, 0.0, 300.0, 400.0))
        );
        // A flipped host keeps gpui's top-left origin.
        assert_eq!(
            frames(bounds, window, 800.0, true).0,
            (100.0, 50.0, 300.0, 400.0)
        );
    }

    #[test]
    fn a_clip_cuts_the_page_instead_of_squeezing_it() {
        let bounds = b(100.0, 50.0, 300.0, 400.0);
        // Cut above and on the right (a pane sliding in under the header).
        assert_eq!(
            frames(bounds, b(100.0, 100.0, 200.0, 600.0), 800.0, false),
            ((100.0, 350.0, 200.0, 350.0), (0.0, 0.0, 300.0, 400.0))
        );
        // Cut below: the page hangs out of the container's bottom.
        assert_eq!(
            frames(bounds, b(0.0, 0.0, 1000.0, 300.0), 800.0, false),
            ((100.0, 500.0, 300.0, 250.0), (0.0, -150.0, 300.0, 400.0))
        );
        // Cut on the left: the page shifts left inside the container.
        assert_eq!(
            frames(bounds, b(150.0, 0.0, 1000.0, 800.0), 800.0, false).1,
            (-50.0, 0.0, 300.0, 400.0)
        );
    }

    #[test]
    fn a_page_clipped_away_has_an_empty_container() {
        let (container, _) = frames(
            b(100.0, 50.0, 300.0, 400.0),
            b(600.0, 0.0, 100.0, 100.0),
            800.0,
            false,
        );
        assert_eq!(container.2 * container.3, 0.0);
    }

    #[test]
    fn relative_links_resolve_beside_the_file() {
        assert_eq!(base_dir("/work/site/index.html"), "/work/site");
        assert_eq!(base_dir("/index.html"), "/");
        assert_eq!(base_dir("index.html"), "/");
    }
}
