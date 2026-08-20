//! RGB → JPEG encoding for the preview wire format.
//!
//! Uses `jpeg-encoder` (pure Rust, no C dependency → the service cross-compiles cleanly to
//! the strih PC (Windows first) and future ARM SBCs). JPEG is what goes over the HTTP
//! `preview.jpg` surface to the web UI's preview blocks.

use jpeg_encoder::{ColorType, Encoder};

use crate::preview::frame::RawFrame;

/// Encode a packed-RGB frame to JPEG bytes at `quality` (0–100). Errors (never panics) on an
/// invalid frame or on a dimension exceeding JPEG's 16-bit limit.
pub fn encode_jpeg(frame: &RawFrame, quality: u8) -> anyhow::Result<Vec<u8>> {
    if !frame.is_valid() {
        anyhow::bail!(
            "invalid preview frame {}x{} (rgb {} bytes, expected {})",
            frame.width,
            frame.height,
            frame.rgb.len(),
            RawFrame::expected_len(frame.width, frame.height)
        );
    }
    let w = u16::try_from(frame.width)
        .map_err(|_| anyhow::anyhow!("preview width {} exceeds JPEG max 65535", frame.width))?;
    let h = u16::try_from(frame.height)
        .map_err(|_| anyhow::anyhow!("preview height {} exceeds JPEG max 65535", frame.height))?;

    let mut buf = Vec::new();
    let encoder = Encoder::new(&mut buf, quality);
    encoder
        .encode(&frame.rgb, w, h, ColorType::Rgb)
        .map_err(|e| anyhow::anyhow!("jpeg encode {w}x{h}: {e}"))?;
    Ok(buf)
}
