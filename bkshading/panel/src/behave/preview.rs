//! The live NDI preview of a block (issue 808, ported from the JS panel's loader): the pure
//! decisions. A frame is fetched only while the service reports the feed live AND the panel has
//! contact; one fetch in flight per block, aborted after [`FETCH_TIMEOUT_MS`]; the endpoint answers
//! a missing or stale frame with 204 (never a 4xx/5xx, which Chromium logs as a console error).

/// Frames per second the panel asks for (the service decimates to ~3 fps).
pub const PREVIEW_FPS: u32 = 3;
/// One fetch may take at most this long; a slower one is aborted.
pub const FETCH_TIMEOUT_MS: i32 = 2000;
/// A fetch that only timed out keeps a frame painted less than this long ago, so one slow request
/// never flashes "obraz sa zastavil" (the service's freshness floor).
pub const KEEP_ON_TIMEOUT_MS: f64 = 3000.0;

pub const WAIT_TEXT: &str = "NDI preview — čakám…";
pub const STOPPED_TEXT: &str = "NDI preview — obraz sa zastavil, čakám…";
/// A camera configured without an NDI preview (mockup arrange-v1).
pub const NO_PREVIEW_TEXT: &str = "bez NDI náhľadu";

/// The placeholder's text: whether a frame was ever shown in this block.
pub fn placeholder_text(seen: bool) -> &'static str {
    if seen {
        STOPPED_TEXT
    } else {
        WAIT_TEXT
    }
}

/// Whether the block fetches a frame on this tick.
pub fn should_fetch(live: bool, connected: bool, busy: bool) -> bool {
    live && connected && !busy
}

/// How often a VISIBLE panel tells the service it is watching (`POST /api/preview/viewer`,
/// shading#19). The service keeps the NDI receivers connected 10 s after the last call, so a few
/// lost calls never drop the picture.
pub const VIEWER_MS: u32 = 2000;

/// What a block's preview does on one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    /// Fetch one frame.
    Fetch,
    /// A fetch is in flight: leave it.
    Wait,
    /// Show the placeholder (abort a fetch in flight, drop the frame).
    Placeholder,
}

/// The block's decision on one tick (shading#19): a feed not live, no contact, or a page nobody
/// sees (`visible` false: a hidden tab, or a kiosk app that hides its view behind OBS) drops to the
/// placeholder and fetches nothing, so a page that comes back never shows a frozen frame.
pub fn tick_action(live: bool, connected: bool, visible: bool, busy: bool) -> Tick {
    if !(live && connected && visible) {
        Tick::Placeholder
    } else if should_fetch(live, connected, busy) {
        Tick::Fetch
    } else {
        Tick::Wait
    }
}

/// After a failed fetch: whether the frame on screen stays. Only a TIMEOUT, on a feed still live
/// with contact, keeps a frame shown less than [`KEEP_ON_TIMEOUT_MS`] ago (`shown_at`, monotonic).
pub fn keeps_frame(
    timed_out: bool,
    shown_at: Option<f64>,
    now: f64,
    live: bool,
    connected: bool,
) -> bool {
    timed_out && live && connected && shown_at.is_some_and(|t| now - t < KEEP_ON_TIMEOUT_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_only_live_connected_and_idle() {
        assert!(should_fetch(true, true, false));
        assert!(!should_fetch(false, true, false));
        assert!(!should_fetch(true, false, false));
        assert!(!should_fetch(true, true, true));
    }

    #[test]
    fn a_timeout_keeps_only_a_young_frame() {
        assert!(keeps_frame(true, Some(1000.0), 3000.0, true, true));
        assert!(
            !keeps_frame(true, Some(1000.0), 4000.0, true, true),
            "older than 3 s"
        );
        assert!(
            !keeps_frame(false, Some(1000.0), 1100.0, true, true),
            "not a timeout"
        );
        assert!(
            !keeps_frame(true, None, 1100.0, true, true),
            "no frame shown"
        );
        assert!(
            !keeps_frame(true, Some(1000.0), 1100.0, false, true),
            "feed stopped"
        );
        assert!(
            !keeps_frame(true, Some(1000.0), 1100.0, true, false),
            "no contact"
        );
    }

    /// shading#19: a page nobody sees (a hidden tab, a kiosk window behind OBS whose app hides its
    /// view) fetches no frame and drops the shown one, so it comes back to no frozen picture.
    #[test]
    fn a_hidden_page_fetches_nothing_and_shows_the_placeholder_19() {
        assert_eq!(tick_action(true, true, true, false), Tick::Fetch);
        assert_eq!(tick_action(true, true, false, false), Tick::Placeholder);
        assert_eq!(tick_action(true, true, false, true), Tick::Placeholder);
        assert_eq!(tick_action(false, true, true, false), Tick::Placeholder);
        assert_eq!(tick_action(true, false, true, false), Tick::Placeholder);
        assert_eq!(tick_action(true, true, true, true), Tick::Wait);
    }

    #[test]
    fn the_placeholder_says_whether_a_frame_was_seen() {
        assert_eq!(placeholder_text(false), "NDI preview — čakám…");
        assert_eq!(
            placeholder_text(true),
            "NDI preview — obraz sa zastavil, čakám…"
        );
    }
}
