//! The pointer, for viewers that draw it themselves (`"cursor":"client"` in the live-view request):
//! where it is and what it looks like, read from the system instead of being drawn into the
//! picture. A moving pointer then costs a few bytes instead of re-encoding the frame under it, and
//! the viewer can draw it at its own refresh rate. Windows and macOS; elsewhere the pointer stays
//! in the picture.

/// Where the pointer is, in screen coordinates (the space of the stream's `screen` rect).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    /// Identifies the shape: the same id is the same picture.
    pub shape: u64,
}

/// A pointer picture: straight-alpha BGRA, `width * height * 4` bytes, top row first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    pub width: u32,
    pub height: u32,
    pub hotspot: (u32, u32),
    pub bgra: Vec<u8>,
}

/// Whether this platform can report the pointer separately.
pub fn supported() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

#[cfg(target_os = "macos")]
pub use mac::{position, shape};

#[cfg(windows)]
pub fn position() -> Option<Position> {
    use windows::Win32::UI::WindowsAndMessaging::{CURSOR_SHOWING, CURSORINFO, GetCursorInfo};
    let mut info = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
    // SAFETY: a plain query into a correctly sized struct.
    unsafe { GetCursorInfo(&mut info) }.ok()?;
    let visible = info.flags.0 & CURSOR_SHOWING.0 != 0 && !info.hCursor.is_invalid();
    Some(Position { x: info.ptScreenPos.x, y: info.ptScreenPos.y, visible, shape: info.hCursor.0 as usize as u64 })
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn position() -> Option<Position> {
    None
}

/// The picture of the pointer whose `Position::shape` is `id`, if it is still the current one.
#[cfg(windows)]
pub fn shape(id: u64) -> Option<Shape> {
    use windows::Win32::{
        Graphics::Gdi::{
            BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW,
            HBITMAP, HGDIOBJ, ReleaseDC,
        },
        UI::WindowsAndMessaging::{GetIconInfo, HICON, ICONINFO},
    };

    /// A bitmap's pixels as 32-bit BGRA rows, top first, with its size.
    fn pixels(bitmap: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
        let mut bm = BITMAP::default();
        // SAFETY: `bm` is a BITMAP-sized out buffer for this GDI handle.
        let n =
            unsafe { GetObjectW(HGDIOBJ(bitmap.0), std::mem::size_of::<BITMAP>() as i32, Some((&raw mut bm).cast())) };
        let (w, h) = (u32::try_from(bm.bmWidth).ok()?, u32::try_from(bm.bmHeight).ok()?);
        if n == 0 || w == 0 || h == 0 || w > 1024 || h > 2048 {
            return None;
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                // Negative: top-down rows.
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut out = vec![0u8; (w * h * 4) as usize];
        // SAFETY: the screen DC is released below; `out` holds h rows of w 32-bit pixels, as asked.
        let rows = unsafe {
            let dc = GetDC(None);
            let rows = GetDIBits(dc, bitmap, 0, h, Some(out.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
            ReleaseDC(None, dc);
            rows
        };
        (rows == h as i32).then_some((w, h, out))
    }

    // Ask before and after: the pointer may have changed in between.
    if position()?.shape != id {
        return None;
    }
    let mut icon = ICONINFO::default();
    // SAFETY: the handle came from GetCursorInfo; GetIconInfo copies its bitmaps, deleted below.
    unsafe { GetIconInfo(HICON(id as usize as *mut _), &mut icon) }.ok()?;
    let mask = pixels(icon.hbmMask);
    let color = (!icon.hbmColor.is_invalid()).then(|| pixels(icon.hbmColor)).flatten();
    // SAFETY: both bitmaps are owned copies made by GetIconInfo.
    unsafe {
        let _ = DeleteObject(HGDIOBJ(icon.hbmMask.0));
        if !icon.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(icon.hbmColor.0));
        }
    }
    let shape = match color {
        Some((w, h, bgra)) => color_shape(w, h, bgra, mask.filter(|m| (m.0, m.1) == (w, h)).map(|m| m.2)),
        None => {
            let (w, h, mask) = mask?;
            monochrome_shape(w, h / 2, &mask)
        }
    }?;
    (position()?.shape == id).then_some(Shape { hotspot: (icon.xHotspot, icon.yHotspot), ..shape })
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn shape(_id: u64) -> Option<Shape> {
    None
}

/// The pointer on macOS: its place from a CoreGraphics event, its picture from AppKit's current
/// system cursor, rendered at point size so that it matches the stream's `screen` rect (points,
/// not Retina pixels) and hashed to tell shapes apart. A still pointer costs one small render a
/// poll; a cached handle would be cheaper but AppKit hands out no stable identity for cursors.
#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;

    use objc2_app_kit::{NSCursor, NSImage};

    use super::{Position, Shape};

    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    struct CGRect {
        origin: CGPoint,
        size: CGPoint,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: *const c_void) -> *mut c_void;
        fn CGEventGetLocation(event: *const c_void) -> CGPoint;
        fn CGCursorIsVisible() -> bool;
        fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
        fn CGColorSpaceRelease(space: *mut c_void);
        fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: *const c_void,
            bitmap_info: u32,
        ) -> *mut c_void;
        fn CGContextDrawImage(context: *mut c_void, rect: CGRect, image: *const c_void);
        fn CGContextRelease(context: *mut c_void);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    /// `kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little`: BGRA in memory.
    const BGRA_PREMULTIPLIED: u32 = 2 | (2 << 12);
    /// Larger than any cursor; a runaway size is refused rather than rendered.
    const MAX_SIDE: f64 = 256.0;

    pub fn position() -> Option<Position> {
        // SAFETY: a plain query; the event object is released once.
        let at = unsafe {
            let event = CGEventCreate(std::ptr::null());
            if event.is_null() {
                return None;
            }
            let at = CGEventGetLocation(event);
            CFRelease(event);
            at
        };
        if !at.x.is_finite() || !at.y.is_finite() {
            return None;
        }
        // SAFETY: a plain query.
        let visible = unsafe { CGCursorIsVisible() };
        let shape = current().map(|(id, _)| id).unwrap_or(0);
        Some(Position { x: at.x.round() as i32, y: at.y.round() as i32, visible, shape })
    }

    pub fn shape(id: u64) -> Option<Shape> {
        current().filter(|(now, _)| *now == id).map(|(_, shape)| shape)
    }

    /// The current pointer picture with its id (a hash of the picture and hot spot).
    fn current() -> Option<(u64, Shape)> {
        // Deprecated in favour of ScreenCaptureKit drawing the pointer into the picture, which is
        // the very thing this reader avoids; `currentCursor` would be this windowless host's own.
        #[allow(deprecated)]
        let cursor = NSCursor::currentSystemCursor().unwrap_or_else(NSCursor::currentCursor);
        let image = cursor.image();
        let hot = cursor.hotSpot();
        let size = image.size();
        if !(size.width >= 1.0 && size.width <= MAX_SIDE && size.height >= 1.0 && size.height <= MAX_SIDE) {
            return None;
        }
        let (width, height) = (size.width.round() as u32, size.height.round() as u32);
        let bgra = render(&image, width, height)?;
        let hotspot = (
            hot.x.round().clamp(0.0, f64::from(width - 1)) as u32,
            hot.y.round().clamp(0.0, f64::from(height - 1)) as u32,
        );
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in bgra.iter().copied().chain([hotspot.0 as u8, hotspot.1 as u8]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Some((hash.max(1), Shape { width, height, hotspot, bgra }))
    }

    /// The image drawn into a `width` x `height` straight-alpha BGRA bitmap, top row first.
    fn render(image: &NSImage, width: u32, height: u32) -> Option<Vec<u8>> {
        // SAFETY: a null rect asks for the representation that fits best; no context or hints.
        let cg = unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }?;
        let (w, h) = (width as usize, height as usize);
        let mut premultiplied = vec![0u8; w * h * 4];
        // SAFETY: the bitmap context draws into `premultiplied`, which outlives it; every object
        // created here is released on every path.
        unsafe {
            let space = CGColorSpaceCreateDeviceRGB();
            if space.is_null() {
                return None;
            }
            let context =
                CGBitmapContextCreate(premultiplied.as_mut_ptr().cast(), w, h, 8, w * 4, space, BGRA_PREMULTIPLIED);
            CGColorSpaceRelease(space);
            if context.is_null() {
                return None;
            }
            let rect = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGPoint { x: f64::from(width), y: f64::from(height) },
            };
            CGContextDrawImage(context, rect, objc2::rc::Retained::as_ptr(&cg).cast());
            CGContextRelease(context);
        }
        // Core Graphics bitmaps are bottom-up; a pointer picture is top row first.
        let mut bgra = Vec::with_capacity(premultiplied.len());
        for row in premultiplied.chunks_exact(w * 4).rev() {
            bgra.extend_from_slice(row);
        }
        for p in bgra.chunks_exact_mut(4) {
            let a = u32::from(p[3]);
            if a != 0 && a != 255 {
                for c in &mut p[..3] {
                    *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        Some(bgra)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_current_pointer_can_be_read() {
            // A session without a window server (CI) has no pointer to read.
            // Read one snapshot: the person's cursor may legitimately change between position()
            // and shape(id), in which case the public shape lookup must return None.
            let Some((id, shape)) = current() else { return };
            assert_ne!(id, 0);
            assert_eq!(shape.bgra.len(), (shape.width * shape.height * 4) as usize);
            assert!(shape.hotspot.0 < shape.width && shape.hotspot.1 < shape.height);
            // The arrow is mostly transparent with some opaque pixels.
            assert!(shape.bgra.chunks_exact(4).any(|p| p[3] == 255));
            assert!(shape.bgra.chunks_exact(4).any(|p| p[3] == 0));
        }

        /// The shape id must not flicker between polls, or a still pointer would be sent at the
        /// poll rate; a person using the machine may change the cursor a time or two meanwhile.
        #[test]
        fn the_shape_id_is_stable_between_polls() {
            let Some(first) = position() else { return };
            if first.shape == 0 {
                return;
            }
            let mut ids = std::collections::BTreeSet::new();
            let start = std::time::Instant::now();
            for _ in 0..200 {
                ids.insert(position().unwrap().shape);
            }
            eprintln!("200 polls in {:?}: {} distinct shape ids", start.elapsed(), ids.len());
            assert!(ids.len() <= 3, "{ids:?}");
        }
    }
}

/// A colour pointer: its own alpha when it has one, otherwise opaque where the AND mask is clear.
// Only `shape()` (Windows) calls it; the tests exercise it on every platform.
#[cfg_attr(not(windows), allow(dead_code))]
fn color_shape(width: u32, height: u32, mut bgra: Vec<u8>, mask: Option<Vec<u8>>) -> Option<Shape> {
    let has_alpha = bgra.chunks_exact(4).any(|p| p[3] != 0);
    if !has_alpha {
        let mask = mask?;
        for (p, m) in bgra.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
            // A white AND-mask pixel keeps the screen: transparent.
            p[3] = if m[0] == 0 { 255 } else { 0 };
        }
    }
    Some(Shape { width, height, hotspot: (0, 0), bgra })
}

/// A monochrome pointer: an AND mask over an XOR mask, each `height` rows of the mask bitmap.
/// Pixels that invert the screen (AND and XOR both set, as in the text cursor) become black with a
/// white edge, which reads on any background.
// Only `shape()` (Windows) calls it; the tests exercise it on every platform.
#[cfg_attr(not(windows), allow(dead_code))]
fn monochrome_shape(width: u32, height: u32, mask: &[u8]) -> Option<Shape> {
    let (w, h) = (width as usize, height as usize);
    if mask.len() < w * h * 8 {
        return None;
    }
    let at = |plane: usize, x: usize, y: usize| mask[((plane * h + y) * w + x) * 4] != 0;
    let mut bgra = vec![0u8; w * h * 4];
    let mut inverted = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let (and, xor) = (at(0, x, y), at(1, x, y));
            let p = &mut bgra[(y * w + x) * 4..][..4];
            match (and, xor) {
                (false, false) => p.copy_from_slice(&[0, 0, 0, 255]),
                (false, true) => p.copy_from_slice(&[255, 255, 255, 255]),
                (true, false) => {}
                (true, true) => {
                    p.copy_from_slice(&[0, 0, 0, 255]);
                    inverted[y * w + x] = true;
                }
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            if bgra[(y * w + x) * 4 + 3] != 0 {
                continue;
            }
            let near = (y.saturating_sub(1)..(y + 2).min(h))
                .any(|ny| (x.saturating_sub(1)..(x + 2).min(w)).any(|nx| inverted[ny * w + nx]));
            if near {
                bgra[(y * w + x) * 4..][..4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    Some(Shape { width, height, hotspot: (0, 0), bgra })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monochrome_pointers_become_bgra_and_inverting_pixels_get_an_edge() {
        // 3x3: AND plane (white = keep the screen) over XOR plane; a single inverting centre pixel.
        let (w, h) = (3usize, 3usize);
        let mut mask = vec![0u8; w * h * 2 * 4];
        let mut set = |plane: usize, x: usize, y: usize| mask[((plane * h + y) * w + x) * 4..][..3].fill(255);
        for y in 0..3 {
            for x in 0..3 {
                set(0, x, y);
            }
        }
        set(1, 1, 1);
        // Top-left: AND clear, XOR clear -> black.
        mask[0..3].fill(0);
        let shape = monochrome_shape(3, 3, &mask).unwrap();
        let px = |x: usize, y: usize| &shape.bgra[(y * w + x) * 4..][..4];
        assert_eq!(px(1, 1), [0, 0, 0, 255], "inverting -> black");
        assert_eq!(px(0, 0), [0, 0, 0, 255], "black");
        assert_eq!(px(2, 2), [255, 255, 255, 255], "white edge around the inverting pixel");
    }

    #[test]
    fn colour_pointers_without_alpha_take_it_from_the_mask() {
        let bgra = vec![10, 20, 30, 0, 40, 50, 60, 0];
        let mask = vec![0, 0, 0, 0, 255, 255, 255, 0];
        let shape = color_shape(2, 1, bgra, Some(mask)).unwrap();
        assert_eq!(shape.bgra, [10, 20, 30, 255, 40, 50, 60, 0]);
    }

    #[cfg(windows)]
    #[test]
    fn the_current_pointer_can_be_read() {
        // A session without a desktop (a service, CI without one) has no pointer to read.
        let Some(at) = position() else { return };
        if !at.visible {
            return;
        }
        if let Some(shape) = shape(at.shape) {
            assert_eq!(shape.bgra.len(), (shape.width * shape.height * 4) as usize);
            assert!(shape.hotspot.0 < shape.width && shape.hotspot.1 < shape.height);
        }
    }
}
