//! Thumbnails the system draws for file previews ([`super::file_card`]): a
//! PDF's first page (PDFKit) and a video's poster frame (AVFoundation), as
//! PNG bytes. macOS only; elsewhere every call returns `None` and the
//! preview shows a plain chip.
//!
//! Both run off the UI thread inside their own autorelease pool.

/// Pixels on the long side of a thumbnail: twice the preview's size so it is
/// sharp on a Retina display.
pub const THUMB_PIXELS: f64 = 720.0;

/// The first page of a PDF, as a PNG at most [`THUMB_PIXELS`] on its long
/// side.
pub fn pdf_first_page(bytes: &[u8]) -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        mac::pdf_first_page(bytes)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bytes;
        None
    }
}

/// A frame about a second into the video at `path` (or its first), as a
/// PNG at most [`THUMB_PIXELS`] on its long side.
pub fn video_frame(path: &std::path::Path) -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        mac::video_frame(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
mod mac {
    // `msg_send!` expands an obsolete `cfg` (see `shell/notify.rs`).
    use std::ffi::{CString, c_void};

    use objc::runtime::{BOOL, Class, NO, Object, YES};
    use objc::{class, msg_send, sel, sel_impl};

    use super::THUMB_PIXELS;

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

    pub(super) fn pdf_first_page(bytes: &[u8]) -> Option<Vec<u8>> {
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
            let page: *mut Object = msg_send![doc, pageAtIndex: 0usize];
            if page.is_null() {
                return None;
            }
            let bounds: CGRect = msg_send![page, boundsForBox: CROP_BOX];
            let rotation: isize = msg_send![page, rotation];
            let (mut w, mut h) = (bounds.size.width, bounds.size.height);
            if rotation % 180 != 0 {
                std::mem::swap(&mut w, &mut h);
            }
            if w <= 0.0 || h <= 0.0 {
                return None;
            }
            let scale = THUMB_PIXELS / w.max(h);
            let size = CGSize {
                width: (w * scale).round(),
                height: (h * scale).round(),
            };
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
        })
    }

    pub(super) fn video_frame(path: &std::path::Path) -> Option<Vec<u8>> {
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
            let max = CGSize {
                width: THUMB_PIXELS,
                height: THUMB_PIXELS,
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

    /// A one-page PDF, 200×100 points.
    fn tiny_pdf() -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R >>",
            "<< /Length 31 >>\nstream\n0 0 1 rg 10 10 80 40 re f\nendstream",
        ];
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
    fn a_pdf_first_page_draws_at_its_aspect() {
        let png = pdf_first_page(&tiny_pdf()).expect("a thumbnail");
        let (w, h) = crate::files::preview::image_dims(&png).expect("a png");
        assert!(w >= h * 2 - 4 && w <= h * 2 + 4, "{w}x{h}");
        assert!(pdf_first_page(b"not a pdf").is_none());
    }

    #[test]
    fn a_missing_video_has_no_frame() {
        assert!(video_frame(std::path::Path::new("/nonexistent/clip.mp4")).is_none());
    }
}
