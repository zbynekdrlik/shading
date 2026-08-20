//! Animated test-pattern generator — the STUB preview source's frame producer.
//!
//! Pure and deterministic: `test_pattern_rgb(w, h, tick)` returns the same bytes for the
//! same inputs and visibly moves as `tick` advances. This is what lets the whole preview
//! pipeline (decimate → encode → HTTP → web UI) be built, run and verified on CI with NO
//! libndi and NO camera. The real NDI receiver (feature `ndi`) replaces the source, not the
//! rest of the pipeline.

/// SMPTE-ish 8-bar colour set (R, G, B).
const BARS: [(u8, u8, u8); 8] = [
    (0xff, 0x00, 0x00), // red
    (0x00, 0xff, 0x00), // green
    (0x00, 0x00, 0xff), // blue
    (0xff, 0xff, 0x00), // yellow
    (0x00, 0xff, 0xff), // cyan
    (0xff, 0x00, 0xff), // magenta
    (0xff, 0xff, 0xff), // white
    (0x20, 0x20, 0x20), // near-black
];

/// Render a `width`x`height` packed-RGB test pattern for frame index `tick`. Vertical colour
/// bars shift one bar per tick (horizontal motion) and a bright scan line sweeps down
/// (vertical motion) so consecutive frames differ — a real "is the preview updating?" signal.
pub fn test_pattern_rgb(width: u32, height: u32, tick: u64) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mut buf = vec![0u8; w * h * 3];
    if w == 0 || h == 0 {
        return buf;
    }
    let shift = (tick % BARS.len() as u64) as usize;
    // The scan line's row for this tick (wraps down the frame).
    let scan_row = (tick as usize).wrapping_mul(3) % h;
    for y in 0..h {
        let on_scan = y == scan_row;
        for x in 0..w {
            let bar = ((x * BARS.len() / w) + shift) % BARS.len();
            let (r, g, b) = if on_scan {
                (0xff, 0xff, 0xff)
            } else {
                BARS[bar]
            };
            let idx = (y * w + x) * 3;
            buf[idx] = r;
            buf[idx + 1] = g;
            buf[idx + 2] = b;
        }
    }
    buf
}
