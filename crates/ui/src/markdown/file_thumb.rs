//! Pictures the system draws for file previews ([`super::file_card`]): a
//! PDF's first pages (PDFKit) and a video's poster frame (AVFoundation), as
//! PNG bytes as many pixels wide as they show at times the window's scale
//! factor, so they are sharp there. macOS only; elsewhere every call returns
//! `None` and the preview shows a plain chip.
//!
//! Both run off the UI thread inside their own autorelease pool.

/// The most pixels one drawn page or frame has (a very tall page draws
/// narrower than asked rather than past this).
pub const MAX_PIXELS: f64 = 16.0 * 1024.0 * 1024.0;

/// A PDF's first pages as PNGs.
pub struct PdfPages {
    /// The first pages drawn, in order (fewer than asked when it has fewer,
    /// or a page failed to draw).
    pub pages: Vec<Vec<u8>>,
    /// Each drawn page's size in PDF points, as it shows (rotation applied).
    pub sizes: Vec<(f64, f64)>,
    /// Pages the document has.
    pub count: usize,
}

/// The first `max` pages of a PDF as PNGs: `pixels` gets the aspects
/// (width / height) of those pages and the document's page count, and
/// says how many pixels wide to draw each (one entry per page to draw).
pub fn pdf_pages(
    bytes: &[u8],
    max: usize,
    pixels: impl FnOnce(&[f64], usize) -> Vec<f64>,
) -> Option<PdfPages> {
    #[cfg(target_os = "macos")]
    {
        mac::pdf_pages(bytes, max, pixels)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (bytes, max, pixels);
        None
    }
}

/// A frame about a second into the video at `path` (or its first), as a
/// PNG `pixels` wide (or the video's own width, when smaller).
pub fn video_frame(path: &std::path::Path, pixels: f64) -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        mac::video_frame(path, pixels)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, pixels);
        None
    }
}

/// The pixel size a `w` × `h` picture draws at `pixels` wide, within
/// [`MAX_PIXELS`].
pub fn drawn_size(w: f64, h: f64, pixels: f64) -> (f64, f64) {
    let mut scale = pixels.max(1.0) / w;
    if w * h * scale * scale > MAX_PIXELS {
        scale = (MAX_PIXELS / (w * h)).sqrt();
    }
    ((w * scale).round().max(1.0), (h * scale).round().max(1.0))
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
mod mac {
    // `msg_send!` expands an obsolete `cfg` (see `shell/notify.rs`).
    use std::ffi::{CString, c_void};

    use objc::runtime::{BOOL, Class, NO, Object, YES};
    use objc::{class, msg_send, sel, sel_impl};

    use super::drawn_size;

    #[link(name = "PDFKit", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "AVFoundation", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "CoreMedia", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGImageRelease(image: *mut c_void);
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CMTime {
        value: i64,
        timescale: i32,
        flags: u32,
        epoch: i64,
    }

    /// `kCMTimeFlags_Valid`.
    const TIME_VALID: u32 = 1;
    /// `NSBitmapImageFileTypePNG`.
    const PNG: usize = 4;
    /// `kPDFDisplayBoxCropBox`.
    const CROP_BOX: isize = 1;

    /// Runs `body` inside an autorelease pool.
    fn pooled<T>(body: impl FnOnce() -> Option<T>) -> Option<T> {
        unsafe {
            let pool: *mut Object = msg_send![class!(NSAutoreleasePool), new];
            let out = body();
            let _: () = msg_send![pool, drain];
            out
        }
    }

    /// An `NSData`'s bytes.
    unsafe fn data_bytes(data: *mut Object) -> Option<Vec<u8>> {
        if data.is_null() {
            return None;
        }
        let len: usize = unsafe { msg_send![data, length] };
        let ptr: *const u8 = unsafe { msg_send![data, bytes] };
        if ptr.is_null() || len == 0 {
            return None;
        }
        Some(unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec())
    }

    /// PNG bytes of an `NSBitmapImageRep`.
    unsafe fn rep_png(rep: *mut Object) -> Option<Vec<u8>> {
        if rep.is_null() {
            return None;
        }
        let props: *mut Object = unsafe { msg_send![class!(NSDictionary), dictionary] };
        let png: *mut Object =
            unsafe { msg_send![rep, representationUsingType: PNG properties: props] };
        unsafe { data_bytes(png) }
    }

    pub(super) fn pdf_pages(
        bytes: &[u8],
        max: usize,
        pixels: impl FnOnce(&[f64], usize) -> Vec<f64>,
    ) -> Option<super::PdfPages> {
        let doc_class = Class::get("PDFDocument")?;
        pooled(|| unsafe {
            let data: *mut Object = msg_send![
                class!(NSData),
                dataWithBytes: bytes.as_ptr() as *const c_void
                length: bytes.len()
            ];
            let doc: *mut Object = msg_send![doc_class, alloc];
            let doc: *mut Object = msg_send![doc, initWithData: data];
            if doc.is_null() {
                return None;
            }
            let _: *mut Object = msg_send![doc, autorelease];
            let locked: BOOL = msg_send![doc, isLocked];
            let count: usize = msg_send![doc, pageCount];
            if locked != NO || count == 0 {
                return None;
            }
            let mut sizes = Vec::new();
            for ix in 0..count.min(max) {
                match page_size(doc, ix) {
                    Some(size) => sizes.push(size),
                    None if ix == 0 => return None,
                    None => break,
                }
            }
            let aspects: Vec<f64> = sizes.iter().map(|(w, h)| w / h).collect();
            let widths = pixels(&aspects, count);
            sizes.truncate(widths.len());
            let mut pages = Vec::new();
            for (ix, pixels) in widths.into_iter().enumerate().take(sizes.len()) {
                // Each page in its own pool: a bitmap per page adds up.
                match pooled(|| page_png(doc, ix, sizes[ix], pixels)) {
                    Some(png) => pages.push(png),
                    None if ix == 0 => return None,
                    None => break,
                }
            }
            sizes.truncate(pages.len());
            Some(super::PdfPages {
                pages,
                sizes,
                count,
            })
        })
    }

    /// Page `ix` of `doc`'s size in points as it shows (rotation applied).
    unsafe fn page_size(doc: *mut Object, ix: usize) -> Option<(f64, f64)> {
        unsafe {
            let page: *mut Object = msg_send![doc, pageAtIndex: ix];
            if page.is_null() {
                return None;
            }
            let bounds: CGRect = msg_send![page, boundsForBox: CROP_BOX];
            let rotation: isize = msg_send![page, rotation];
            let (mut w, mut h) = (bounds.size.width, bounds.size.height);
            if rotation % 180 != 0 {
                std::mem::swap(&mut w, &mut h);
            }
            (w > 0.0 && h > 0.0).then_some((w, h))
        }
    }

    /// Page `ix` of `doc`, `w` × `h` points, as a PNG `pixels` wide.
    unsafe fn page_png(
        doc: *mut Object,
        ix: usize,
        (w, h): (f64, f64),
        pixels: f64,
    ) -> Option<Vec<u8>> {
        unsafe {
            let page: *mut Object = msg_send![doc, pageAtIndex: ix];
            if page.is_null() {
                return None;
            }
            let (width, height) = drawn_size(w, h, pixels);
            let size = CGSize { width, height };
            let image: *mut Object = msg_send![page, thumbnailOfSize: size forBox: CROP_BOX];
            if image.is_null() {
                return None;
            }
            let tiff: *mut Object = msg_send![image, TIFFRepresentation];
            if tiff.is_null() {
                return None;
            }
            let rep: *mut Object = msg_send![class!(NSBitmapImageRep), imageRepWithData: tiff];
            rep_png(rep)
        }
    }

    pub(super) fn video_frame(path: &std::path::Path, pixels: f64) -> Option<Vec<u8>> {
        let asset_class = Class::get("AVURLAsset")?;
        let generator_class = Class::get("AVAssetImageGenerator")?;
        let path = CString::new(path.to_str()?).ok()?;
        pooled(|| unsafe {
            let ns_path: *mut Object =
                msg_send![class!(NSString), stringWithUTF8String: path.as_ptr()];
            let url: *mut Object = msg_send![class!(NSURL), fileURLWithPath: ns_path];
            let nil: *mut Object = std::ptr::null_mut();
            let asset: *mut Object = msg_send![asset_class, URLAssetWithURL: url options: nil];
            if asset.is_null() {
                return None;
            }
            let generator: *mut Object =
                msg_send![generator_class, assetImageGeneratorWithAsset: asset];
            if generator.is_null() {
                return None;
            }
            let _: () = msg_send![generator, setAppliesPreferredTrackTransform: YES];
            // The width decides (a tall frame shows its top, column wide).
            let max = CGSize {
                width: pixels.max(1.0),
                height: pixels.max(1.0) * 8.0,
            };
            let _: () = msg_send![generator, setMaximumSize: max];
            // A second in skips a black first frame; a shorter clip falls
            // back to its start.
            for seconds in [1i64, 0] {
                let at = CMTime {
                    value: seconds,
                    timescale: 1,
                    flags: TIME_VALID,
                    epoch: 0,
                };
                let mut actual = CMTime::default();
                let mut error: *mut Object = std::ptr::null_mut();
                let image: *mut c_void = msg_send![
                    generator,
                    copyCGImageAtTime: at
                    actualTime: &mut actual as *mut CMTime
                    error: &mut error as *mut *mut Object
                ];
                if image.is_null() {
                    continue;
                }
                let rep: *mut Object = msg_send![class!(NSBitmapImageRep), alloc];
                let rep: *mut Object = msg_send![rep, initWithCGImage: image];
                CGImageRelease(image);
                if rep.is_null() {
                    continue;
                }
                let _: *mut Object = msg_send![rep, autorelease];
                if let Some(png) = rep_png(rep) {
                    return Some(png);
                }
            }
            None
        })
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// A PDF of `pages` pages, 200×100 points each.
    fn tiny_pdf(pages: usize) -> Vec<u8> {
        let kids: Vec<String> = (0..pages).map(|ix| format!("{} 0 R", ix + 4)).collect();
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            format!(
                "<< /Type /Pages /Kids [{}] /Count {pages} >>",
                kids.join(" ")
            ),
            "<< /Length 31 >>\nstream\n0 0 1 rg 10 10 80 40 re f\nendstream".to_owned(),
        ];
        for _ in 0..pages {
            objects.push(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 3 0 R >>"
                    .to_owned(),
            );
        }
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (ix, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", ix + 1));
        }
        let xref = out.len();
        out.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for offset in offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    #[test]
    fn pdf_pages_draw_at_their_aspect_and_asked_widths() {
        let mut asked = None;
        let drawn = pdf_pages(&tiny_pdf(5), 3, |aspects, count| {
            asked = Some((aspects.to_vec(), count));
            vec![600.0, 300.0, 300.0]
        })
        .expect("pages");
        assert_eq!(asked, Some((vec![2.0; 3], 5)));
        assert_eq!((drawn.pages.len(), drawn.count), (3, 5));
        assert_eq!(drawn.sizes, vec![(200.0, 100.0); 3]);
        for (png, asked) in drawn.pages.iter().zip([600, 300, 300]) {
            let (w, h) = crate::files::preview::image_dims(png).expect("a png");
            assert!(w >= h * 2 - 4 && w <= h * 2 + 4, "{w}x{h}");
            // At least as wide as asked (a Retina screen may draw twice it).
            assert!(w >= asked - 4, "{w}x{h}");
        }
        // Fewer widths than pages: only those draw.
        let two = pdf_pages(&tiny_pdf(5), 4, |_, _| vec![300.0; 2]).expect("pages");
        assert_eq!((two.pages.len(), two.sizes.len(), two.count), (2, 2, 5));
        let one = pdf_pages(&tiny_pdf(1), 3, |_, _| vec![300.0]).expect("a page");
        assert_eq!((one.pages.len(), one.count), (1, 1));
        assert!(pdf_pages(b"not a pdf", 3, |_, _| vec![600.0]).is_none());
    }

    #[test]
    fn drawn_sizes_follow_the_width_within_the_pixel_cap() {
        assert_eq!(drawn_size(200.0, 100.0, 1536.0), (1536.0, 768.0));
        let (w, h) = drawn_size(100.0, 10_000.0, 1536.0);
        assert!(w * h <= MAX_PIXELS * 1.01 && w < 1536.0, "{w}x{h}");
    }

    #[test]
    fn a_missing_video_has_no_frame() {
        assert!(video_frame(std::path::Path::new("/nonexistent/clip.mp4"), 600.0).is_none());
    }
}
