//! A decoded preview frame in packed 8-bit RGB.
//!
//! This is the common currency between a [`crate::preview::source::PreviewSource`] (stub
//! test pattern or the feature-gated real NDI receiver) and the JPEG encoder. Colour
//! conversion from the NDI wire format (UYVY/BGRA/RGBA) happens in
//! [`crate::preview::convert`], so everything downstream (decimate, encode) is
//! format-agnostic and unit-testable without libndi.

/// One decoded video frame as packed 8-bit RGB. `rgb.len()` is expected to be
/// `width * height * 3`.
#[derive(Clone, PartialEq, Eq)]
pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

impl RawFrame {
    pub fn new(width: u32, height: u32, rgb: Vec<u8>) -> Self {
        Self { width, height, rgb }
    }

    /// The RGB byte length a `width`x`height` frame must have.
    pub fn expected_len(width: u32, height: u32) -> usize {
        width as usize * height as usize * 3
    }

    /// A frame is valid when it has non-zero dimensions and exactly `w*h*3` RGB bytes.
    /// The encoder refuses an invalid frame rather than producing a corrupt JPEG.
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.rgb.len() == Self::expected_len(self.width, self.height)
    }
}

// Manual Debug so a logged frame never dumps megabytes of pixels.
impl std::fmt::Debug for RawFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rgb_len", &self.rgb.len())
            .finish()
    }
}
