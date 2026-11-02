//! The relay's read constants and pure read helpers (moved out of `transport.rs`, issue 808, no
//! behaviour change): the capture-fps and read-floor env parsers, the read-throttle clock seam
//! (issue 1229), the config-key lists of a read cycle, the paced-read and aperture-settle timings,
//! and the multi `--get-config` argv/split helpers. Re-exported from `transport`.

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

/// Default minimum interval between real gphoto2 read cycles (issue 1229). A `GET /api/state`
/// arriving within this window of the last real read is served from the cache with NO USB-PTP
/// session, so the service pump polling every ~2 s cannot hammer the shared xHCI bus and starve
/// the grabber's isochronous capture stream. Overridable via `BKSHADING_RELAY_MIN_READ_INTERVAL_MS`.
pub const DEFAULT_MIN_READ_INTERVAL_MS: u64 = 10_000;

/// Upper sanity bound (1 h) on an env-supplied read floor (issue 1229). A generous ceiling for any
/// real tuning (5–120 s) that rejects an absurd value from a units mistake (seconds instead of ms,
/// or an extra ×1000) which would otherwise freeze readback for the process lifetime — mirrors
/// `parse_capture_fps_env`'s own sanity ceiling.
pub const MAX_MIN_READ_INTERVAL_MS: u64 = 3_600_000;

/// gphoto2 config key for the BMPCC manual focus DISTANCE (issue 1238). Documented as PTP
/// property `0xd003` (RANGE, ~0=closest..65536=infinite) in the TalOrg BMPCC-over-PTP
/// control-point list (the MVP mapping covers the shading d-codes, not d003). It is read
/// BEST-EFFORT (see `read_raw`): a camera /
/// firmware / lens that does not answer it yields an empty block (-> a `None` `focus_distance`),
/// never a failed read that would degrade the whole shading state to offline.
///
/// NB — the honest constraint (issue 1238): the BMPCC's documented PTP property space exposes NO
/// AF/MF focus-MODE selector and NO auto/manual exposure-MODE (program) selector; `d003` is focus
/// DISTANCE, not a mode flag. The undiscovered `d001`/`d008`/`d009`/`d00a` MIGHT hold a mode, but
/// identifying any needs a live-cabled `--get-config` + camera-menu-toggle discovery step (the
/// supervisor's rig step) before it could ever be wired as a field. Until then this key is the
/// only honest focus signal, and no mode field is fabricated.
pub const FOCUS_DISTANCE_KEY: &str = "d003";

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
/// (issue 1229) — i.e. `/api/state` may be served from cache without a real gphoto2 read. `None`
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

/// The seven CORE shading config keys read together in ONE gphoto2 invocation (issue 1229). Their
/// ORDER is the wire contract for [`split_config_blocks`]'s positional mapping in `read_raw`, and
/// matches the field order they are assigned to in [`RawConfigs`] (`d006` = the PROJECT fps,
/// `d007` = the OFF-SPEED fps, issue 1402). The best-effort `d003` focus distance (issue 1238) is
/// deliberately NOT here — it is read as a separate call so a camera that does not answer it can
/// never abort the core batch (which would wrongly degrade the read to offline). `d001`-style
/// undiscovered keys are not read.
pub const CORE_CONFIG_KEYS: [&str; 7] = ["iso", "f-number", "d002", "d004", "d005", "d006", "d007"];

/// The PTP F-Number property as its own config key (issue 1371): a MENU whose `Current:` is the raw
/// aperture x100, present even when the `f-number` RADIO reads `(null)` (an off-grid lens). The
/// paced read takes it instead of the `--summary` dump.
pub const FNUMBER_RAW_KEY: &str = "5007";

/// The keys one PACED read cycle sends through the persistent shell, in this order (issue 1371):
/// the seven [`CORE_CONFIG_KEYS`], then the best-effort [`FNUMBER_RAW_KEY`] and
/// [`FOCUS_DISTANCE_KEY`] (an error answer for those two is an empty block, never a failed read).
pub const PACED_READ_KEYS: [&str; 9] = [
    "iso", "f-number", "d002", "d004", "d005", "d006", "d007", "5007", "d003",
];

/// The gap between two keys of a paced read (issue 1371). Measured on cam5 (BMPCC + Cam Link on
/// one USB 3 root port): today's two gphoto2 processes per cycle put ~250 transfers on the port in
/// 4-6 ms bursts and the Cam Link's frame timestamps stepped +3..+16 ms late, re-seeding the
/// camera-box capture phase ~7x a minute; one key every 120 ms through one session brought it to
/// the no-relay baseline. A cycle of 9 keys takes ~1 s, under the service's 1.5 s poll timeout.
pub const READ_PACING_MS: u64 = 100;

/// The pause before each 0x5007 read-back after an aperture write in a write burst (issue 1452),
/// while the iris moves. Reads go through the burst's open shell, never a new process per read.
pub const APERTURE_SETTLE_POLL_MS: u64 = 300;

/// The most 0x5007 reads one settle takes (issue 1452), so a body that never moves its iris (cam7)
/// costs at most `APERTURE_SETTLE_MAX_READS * APERTURE_SETTLE_POLL_MS` per aperture write.
pub const APERTURE_SETTLE_MAX_READS: usize = 5;

/// Builds the gphoto2 argv for reading many config keys in ONE process (issue 1229):
/// `["--get-config", k1, "--get-config", k2, …]`. A single `gphoto2` invocation with this argv
/// opens the camera ONCE (one USB open/enumerate/close) and reads every key in that one PTP
/// session — the whole point of the coalesce. Pure; unit-tested (proves it is one command, not N).
pub fn build_get_config_many_args(keys: &[&str]) -> Vec<String> {
    let mut args = Vec::with_capacity(keys.len() * 2);
    for &k in keys {
        args.push("--get-config".to_string());
        args.push(k.to_string());
    }
    args
}

/// Splits the combined stdout of one multi-`--get-config` gphoto2 invocation into per-key blocks
/// (issue 1229). gphoto2 prints one block per `--get-config`, in flag order, each terminated by a
/// line that is exactly `END`; a block's own `Label:`/`Readonly:`/`Type:` header lines are kept in
/// the block (the [`crate`]'s per-block parsers read only `Current:`/`Choice:`/`Bottom:`/`Top:` and
/// ignore the rest). Returns `Some(blocks)` ONLY when the number of `END`-terminated blocks equals
/// `n` (the key count); any shortfall/excess (a key that errored mid-batch, a missing terminating
/// `END`, an unexpected extra block) yields `None`, so the caller degrades to a failed read →
/// offline rather than mis-assigning a block to the wrong key. Pure; unit-tested + rustc-replicated.
pub fn split_config_blocks(combined: &str, n: usize) -> Option<Vec<String>> {
    let mut blocks: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for raw in combined.lines() {
        if raw.trim() == "END" {
            blocks.push(cur.join("\n"));
            cur.clear();
        } else {
            cur.push(raw);
        }
    }
    // Any lines after the last `END` are an incomplete (un-terminated) block: they are NOT counted,
    // so a truncated final block shows up as a block-count shortfall below and fails safe.
    if blocks.len() == n {
        Some(blocks)
    } else {
        None
    }
}
