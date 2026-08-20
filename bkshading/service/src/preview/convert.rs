//! Colour conversions from NDI wire pixel formats to packed RGB8.
//!
//! Split out of the (feature-gated, `unsafe`) NDI receiver so the pixel math stays PURE and
//! unit-testable on CI without libndi or a camera. The real receiver dispatches on the
//! captured frame's actual FourCC and calls one of these; RGB is what the JPEG encoder wants.
//!
//! The receiver requests the SDK's `UYVY_BGRA` colour format (mirroring the appliance's
//! `src/ndi.rs`), so a preview stream with no alpha arrives as `UYVY` and one with alpha as
//! `BGRA`; `RGBA`/`BGRX` are handled defensively for other sources.
//!
//! Pixel groups are walked with explicit index math (not `chunks_exact(N)` with a constant
//! size, which the 1.98 clippy lint rejects).

/// Clamp a float channel to a `u8`.
#[inline]
fn clamp8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// `BGRA`/`BGRX` (4 bytes/px, little-endian `B, G, R, A`) → RGB (3 bytes/px). Alpha dropped.
pub fn bgra_to_rgb(bgra: &[u8], width: usize, height: usize) -> Vec<u8> {
    let px = (width * height).min(bgra.len() / 4);
    let mut out = Vec::with_capacity(px * 3);
    for i in 0..px {
        let b = i * 4;
        out.push(bgra[b + 2]); // R
        out.push(bgra[b + 1]); // G
        out.push(bgra[b]); // B
    }
    out
}

/// `RGBA`/`RGBX` (4 bytes/px `R, G, B, A`) → RGB. Alpha dropped.
pub fn rgba_to_rgb(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let px = (width * height).min(rgba.len() / 4);
    let mut out = Vec::with_capacity(px * 3);
    for i in 0..px {
        let b = i * 4;
        out.push(rgba[b]);
        out.push(rgba[b + 1]);
        out.push(rgba[b + 2]);
    }
    out
}

/// `UYVY` (packed 4:2:2, 4 bytes = 2 px: `U Y0 V Y1`) → RGB, BT.601 full-range approximation.
/// Exact coefficients are unimportant for a shading PREVIEW (a rough colour reference, not a
/// graded master); the conversion is deterministic and unit-tested for grey/luma sanity.
/// EVEN width is assumed (each macropixel is 2 px); an odd-width source would yield
/// `rgb.len() != w*h*3`, which `RawFrame::is_valid` then rejects downstream (frame dropped,
/// never a panic) — NDI video widths are even in practice.
pub fn uyvy_to_rgb(uyvy: &[u8], width: usize, height: usize) -> Vec<u8> {
    let px = width * height;
    let pairs = (px / 2).min(uyvy.len() / 4);
    let mut out = Vec::with_capacity(pairs * 6);
    for i in 0..pairs {
        let b = i * 4;
        let u = uyvy[b] as f32 - 128.0;
        let y0 = uyvy[b + 1] as f32;
        let v = uyvy[b + 2] as f32 - 128.0;
        let y1 = uyvy[b + 3] as f32;
        for y in [y0, y1] {
            out.push(clamp8(y + 1.402 * v));
            out.push(clamp8(y - 0.344_136 * u - 0.714_136 * v));
            out.push(clamp8(y + 1.772 * u));
        }
    }
    out
}
