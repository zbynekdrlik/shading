//! Colour conversions from NDI wire pixel formats to packed RGB8.
//!
//! Split out of the (feature-gated, `unsafe`) NDI receiver so the pixel math stays PURE and
//! unit-testable on CI without libndi or a camera. The real receiver dispatches on the
//! captured frame's actual FourCC and calls one of these; RGB is what the JPEG encoder wants.
//!
//! The receiver requests the SDK's `UYVY_BGRA` colour format (shading#34: the SDK then skips its
//! own conversion, ~1.5-2 % of a core less per receiver), so a preview stream with no alpha
//! arrives as `UYVY` and one with alpha as `BGRA`; `RGBA`/`BGRX` are handled defensively.
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

/// BT.709 luma weights (ITU-R BT.709): `Kr`, `Kb`; `Kg = 1 - Kr - Kb`.
const KR: f32 = 0.2126;
const KB: f32 = 0.0722;
const KG: f32 = 1.0 - KR - KB;
/// Video-range scale: luma 16..235 (219 steps), chroma 16..240 (224 steps) onto 0..255.
const Y_SCALE: f32 = 255.0 / 219.0;
const C_SCALE: f32 = 255.0 / 224.0;

/// `UYVY` (packed 4:2:2, 4 bytes = 2 px: `U Y0 V Y1`) → RGB, **video-range BT.709** (shading#34).
/// That is what the NDI SDK itself does for the 640x360 preview proxy: measured live on strih-pp
/// against the SDK's own BGRX of the same frame (BT.709 MAE 1.6 levels, BT.601 2.4, the old
/// full-range BT.601 15.5 with black lifted to 16). Both pixels of a macropixel use its chroma
/// (repeated, not interpolated: interpolation measured further from the SDK). Luma below 16 /
/// above 235 clamps to 0 / 255.
/// EVEN width is assumed (each macropixel is 2 px); an odd-width source would yield
/// `rgb.len() != w*h*3`, which `RawFrame::is_valid` then rejects downstream (frame dropped,
/// never a panic) — NDI video widths are even in practice.
pub fn uyvy_to_rgb(uyvy: &[u8], width: usize, height: usize) -> Vec<u8> {
    let px = width * height;
    let pairs = (px / 2).min(uyvy.len() / 4);
    let mut out = Vec::with_capacity(pairs * 6);
    for i in 0..pairs {
        let b = i * 4;
        let cb = (uyvy[b] as f32 - 128.0) * C_SCALE;
        let cr = (uyvy[b + 2] as f32 - 128.0) * C_SCALE;
        let r_off = 2.0 * (1.0 - KR) * cr;
        let b_off = 2.0 * (1.0 - KB) * cb;
        let g_off = -(KR * r_off + KB * b_off) / KG;
        for y in [uyvy[b + 1], uyvy[b + 3]] {
            let y = (y as f32 - 16.0) * Y_SCALE;
            out.push(clamp8(y + r_off));
            out.push(clamp8(y + g_off));
            out.push(clamp8(y + b_off));
        }
    }
    out
}
