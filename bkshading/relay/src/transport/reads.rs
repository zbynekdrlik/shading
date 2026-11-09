//! The relay's read constants and pure read helpers: the capture-fps and read-floor env parsers
//! and the read-throttle clock seam (issue 1229). Re-exported from `transport`.

use std::time::Instant;

/// Parses the box's capture-mode fps from the raw `CAMERA_BOX_CAPTURE_FPS` environment value
/// (issue 809). The appliance uses this SAME env to request its `/dev/videoN` capture rate
/// (`src/capture.rs`), and the relay runs on the same cambox, so reading it here reports the
/// box's ACTUAL grab rate to the service — with ZERO change to the appliance. Accepts an integer
/// or decimal (rounded to the nearest integer — the rig is integer-genlock 60, and the fps-sync
/// model is integer; fractional NTSC is deferred). A non-positive, non-finite (`inf`/`nan`),
/// absurd (`> 1000` fps), empty, or unparseable value (or an unset env) yields `None` — never a
/// bogus value. Pure — unit-tested without any env.
pub fn parse_capture_fps_env(raw: Option<String>) -> Option<i64> {
    let v: f64 = raw?.trim().parse().ok()?;
    // Guard `is_finite` FIRST: `"inf"` parses to `f64::INFINITY`, passes `> 0.0`, and saturates
    // to `i64::MAX` through the cast -- a bogus giant `capture_fps` that would drive a spurious
    // desync/Mismatch. The `<= 1000` ceiling rejects an absurd finite value too (a real capture
    // rate is 24-60).
    if v.is_finite() && v > 0.0 && v <= 1000.0 {
        Some(v.round() as i64)
    } else {
        None
    }
}

/// Default minimum interval between real camera read cycles (issue 1229). A `GET /api/state`
/// arriving within this window of the last real read is served from the cache with NO camera
/// traffic, so the service pump polling every ~2 s never turns into a steady stream of requests
/// on the camera's USB link, which shares the box's xHCI bus with the grabber. Overridable via `BKSHADING_RELAY_MIN_READ_INTERVAL_MS`.
pub const DEFAULT_MIN_READ_INTERVAL_MS: u64 = 10_000;

/// Upper sanity bound (1 h) on an env-supplied read floor (issue 1229). A generous ceiling for any
/// real tuning (5–120 s) that rejects an absurd value from a units mistake (seconds instead of ms,
/// or an extra ×1000) which would otherwise freeze readback for the process lifetime — mirrors
/// `parse_capture_fps_env`'s own sanity ceiling.
pub const MAX_MIN_READ_INTERVAL_MS: u64 = 3_600_000;

/// Monotonic clock seam for the read-throttle floor (issue 1229). Only DIFFERENCES between
/// successive `now_ms` values are meaningful. Injectable so the floor is Tier-0 testable without
/// real sleeps.
pub trait MonoClock: Send + Sync {
    /// Monotonic milliseconds from an arbitrary fixed base.
    fn now_ms(&self) -> u64;
}

/// Production [`MonoClock`]: monotonic ms since construction, via `std::time::Instant` (immune to
/// wall-clock / NTP steps).
pub struct InstantClock {
    base: Instant,
}

impl InstantClock {
    pub fn new() -> Self {
        InstantClock {
            base: Instant::now(),
        }
    }
}

impl Default for InstantClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonoClock for InstantClock {
    fn now_ms(&self) -> u64 {
        self.base.elapsed().as_millis() as u64
    }
}

/// Whether a cached read taken at `read_at_ms` is still within the min-interval floor at `now_ms`
/// (issue 1229) — i.e. `/api/state` may be served from cache without a real camera read. `None`
/// (no prior read) is never fresh. Pure; unit-tested and mirrored by a Tier-0 python/bash replica.
pub fn read_is_fresh(read_at_ms: Option<u64>, now_ms: u64, floor_ms: u64) -> bool {
    read_at_ms.is_some_and(|t| now_ms.saturating_sub(t) < floor_ms)
}

/// Parses the relay read-throttle floor in ms from the raw `BKSHADING_RELAY_MIN_READ_INTERVAL_MS`
/// value (issue 1229). A positive integer up to [`MAX_MIN_READ_INTERVAL_MS`] is accepted; an unset
/// / empty / non-integer / zero / negative / absurdly-large value yields `None`, and the caller
/// falls back to [`DEFAULT_MIN_READ_INTERVAL_MS`] — so the floor can be TUNED but never disabled
/// and never frozen by a units mistake. Pure — unit-tested without any env.
pub fn parse_min_read_interval_env(raw: Option<String>) -> Option<u64> {
    let v: u64 = raw?.trim().parse().ok()?;
    // `1..=MAX` rejects 0 (never disable the floor) AND an absurd value; a range-`contains` avoids
    // the clippy `manual_range_contains` lint the two-comparison form would trip under -D warnings.
    if (1..=MAX_MIN_READ_INTERVAL_MS).contains(&v) {
        Some(v)
    } else {
        None
    }
}
