//! The relay's camera transport: shell out to the `gphoto2` CLI over USB-PTP.
//!
//! Design decision (issue 808 design comment, Prístup 1): the relay drives the camera
//! by spawning the system `gphoto2` binary — NOT a build-time `libgphoto2` FFI binding.
//! This keeps the crate free of any C build-time dependency, so it cross-compiles cleanly
//! for ARM (Pi Zero 2 W, the handheld SBC relay), and it reuses the exact gphoto2
//! semantics the dev2 MVP verified. The `Gphoto2Runner` trait is the seam: `Gphoto2Cli`
//! is the real impl, and tests inject a fake so every path is exercised without a camera.

use std::sync::Mutex;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use bkshading_proto::mapping::{parse_choices, DEFAULT_FPS100};
use bkshading_proto::read::{fps_supported, params_and_caps, plan_writes, RawConfigs};
use bkshading_proto::wire::{RelayState, SetRequest};

/// The seam over the `gphoto2` CLI. Blocking (std process); handlers call it via
/// `spawn_blocking`. `Send + Sync` so it can live behind an `Arc` shared across tasks.
pub trait Gphoto2Runner: Send + Sync {
    /// `gphoto2 --auto-detect` stdout.
    fn auto_detect(&self) -> Result<String>;
    /// `gphoto2 --get-config <key>` stdout.
    fn get_config(&self, key: &str) -> Result<String>;
    /// `gphoto2 --set-config <key>=<value>`.
    fn set_config(&self, key: &str, value: &str) -> Result<()>;
}

/// Real transport: spawns the `gphoto2` binary.
pub struct Gphoto2Cli {
    pub binary: String,
}

impl Default for Gphoto2Cli {
    fn default() -> Self {
        Gphoto2Cli {
            binary: "gphoto2".to_string(),
        }
    }
}

impl Gphoto2Runner for Gphoto2Cli {
    fn auto_detect(&self) -> Result<String> {
        let out = std::process::Command::new(&self.binary)
            .arg("--auto-detect")
            .output()
            .with_context(|| format!("spawn {} --auto-detect", self.binary))?;
        if !out.status.success() {
            bail!(
                "gphoto2 --auto-detect failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn get_config(&self, key: &str) -> Result<String> {
        let out = std::process::Command::new(&self.binary)
            .arg("--get-config")
            .arg(key)
            .output()
            .with_context(|| format!("spawn {} --get-config {}", self.binary, key))?;
        if !out.status.success() {
            bail!(
                "gphoto2 --get-config {} failed: {}",
                key,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn set_config(&self, key: &str, value: &str) -> Result<()> {
        let out = std::process::Command::new(&self.binary)
            .arg("--set-config")
            .arg(format!("{key}={value}"))
            .output()
            .with_context(|| format!("spawn {} --set-config {}={}", self.binary, key, value))?;
        if !out.status.success() {
            bail!(
                "gphoto2 --set-config {}={} failed: {}",
                key,
                value,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
}

/// Parses the first camera model out of `gphoto2 --auto-detect` table output. The table is
/// `Model<ws>Port` with a `---` separator line; each data row ends in a `usb:BBB,DDD` port
/// token, so the model is the row with that token stripped. Returns `None` if no camera row
/// is present (camera absent / unplugged). Pure — unit-tested without a camera.
pub fn parse_first_model(output: &str) -> Option<String> {
    for raw in output.lines() {
        let line = raw.trim_end();
        // A data row ends in a port token; the header/separator rows do not.
        if let Some(idx) = line.rfind("usb:") {
            let model = line[..idx].trim();
            if !model.is_empty() {
                return Some(model.to_string());
            }
        }
    }
    None
}

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
/// control-point list + the MVP mapping. It is read BEST-EFFORT (see `read_raw`): a camera /
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

/// One cached read cycle (issue 1229): the last `RelayState` and the monotonic ms it was read at.
struct CachedRead {
    state: RelayState,
    read_at_ms: u64,
}

/// One camera the relay owns, driven through a [`Gphoto2Runner`].
pub struct CameraSession {
    runner: Box<dyn Gphoto2Runner>,
    version: String,
    /// The box's capture-mode fps (issue 809), read from `CAMERA_BOX_CAPTURE_FPS` at startup;
    /// `None` when the env is unset. Reported in every `RelayState` (even a camera-offline one —
    /// it is a box property, not a camera one).
    capture_fps: Option<i64>,
    /// Minimum interval between real gphoto2 read cycles (issue 1229). `/api/state` within this
    /// window of the last read is served from `read_cache` (no USB-PTP session).
    min_read_interval_ms: u64,
    /// Monotonic clock seam (issue 1229), injectable for tests; prod uses [`InstantClock`].
    clock: Box<dyn MonoClock>,
    /// The last read cycle's state + the monotonic ms it was read at (issue 1229). Behind a
    /// `Mutex` for interior mutability through the `&self` handler API; the lock is held across
    /// the (blocking) read so a burst of concurrent `/api/state` requests coalesces to ONE real
    /// gphoto2 read (serializing gphoto2 access to the single USB camera is correct — concurrent
    /// gphoto2 processes would contend on the very bus this fix protects).
    read_cache: Mutex<Option<CachedRead>>,
}

impl CameraSession {
    pub fn new(runner: Box<dyn Gphoto2Runner>, version: impl Into<String>) -> Self {
        CameraSession {
            runner,
            version: version.into(),
            capture_fps: None,
            min_read_interval_ms: DEFAULT_MIN_READ_INTERVAL_MS,
            clock: Box::new(InstantClock::new()),
            read_cache: Mutex::new(None),
        }
    }

    /// Sets the box's capture-mode fps this relay reports (issue 809). The binary passes
    /// `parse_capture_fps_env(std::env::var("CAMERA_BOX_CAPTURE_FPS").ok())`.
    pub fn with_capture_fps(mut self, capture_fps: Option<i64>) -> Self {
        self.capture_fps = capture_fps;
        self
    }

    /// Sets the read-throttle floor (issue 1229). The binary passes
    /// `parse_min_read_interval_env(std::env::var("BKSHADING_RELAY_MIN_READ_INTERVAL_MS").ok())`
    /// falling back to [`DEFAULT_MIN_READ_INTERVAL_MS`].
    pub fn with_min_read_interval_ms(mut self, ms: u64) -> Self {
        self.min_read_interval_ms = ms;
        self
    }

    /// Injects a [`MonoClock`] (issue 1229) — used by tests to drive the floor without real sleeps.
    pub fn with_clock(mut self, clock: Box<dyn MonoClock>) -> Self {
        self.clock = clock;
        self
    }

    pub fn version(&self) -> String {
        self.version.clone()
    }

    /// Model string of the attached camera, or `None` if not detected. Deliberately NOT throttled
    /// by the issue-1229 read floor: `/api/detect` is a rare, manual probe (the service pump polls
    /// only `/api/state`, never this), so it is not a sustained bus-contention source. If a future
    /// client ever polls `/api/detect` in a loop, route it through the cache too.
    pub fn detect(&self) -> Option<String> {
        self.runner
            .auto_detect()
            .ok()
            .and_then(|o| parse_first_model(&o))
    }

    fn read_raw(&self) -> Result<RawConfigs> {
        Ok(RawConfigs {
            iso: self.runner.get_config("iso")?,
            fnumber: self.runner.get_config("f-number")?,
            shutter_angle: self.runner.get_config("d002")?,
            kelvin: self.runner.get_config("d004")?,
            tint: self.runner.get_config("d005")?,
            sensor_fps: self.runner.get_config("d006")?,
            project_fps: self.runner.get_config("d007")?,
            // issue 1238: the manual focus distance (d003) rides the SAME coalesced read cycle as
            // the seven keys above (one more `--get-config` per throttled read, NEVER a per-request
            // read and NEVER a second cadence — the issue-1229 bus-friendly floor is preserved). It
            // is read BEST-EFFORT: unlike the core exposure trio (iso/f-number/d002) whose failure
            // means the read is fundamentally broken and correctly degrades to offline via `?`,
            // focus_distance is a supplementary pre-run-check signal — a camera/firmware/lens that
            // does not answer d003 must NOT suppress the essential shading state, so its error maps
            // to an empty block (-> a `None` focus_distance), not a whole-read failure.
            focus_distance: self
                .runner
                .get_config(FOCUS_DISTANCE_KEY)
                .unwrap_or_default(),
        })
    }

    /// Reads the camera's live shading state, THROTTLED by the min-interval floor (issue 1229):
    /// a `GET /api/state` within `min_read_interval_ms` of the last real read is served from the
    /// cache with NO gphoto2 / USB-PTP session. The cache lock is held across the (blocking) real
    /// read so a burst of concurrent requests coalesces to a SINGLE gphoto2 read per floor. This
    /// is what keeps the relay bus-friendly on a production cambox — the service pump polls every
    /// ~2 s, but the shared USB bus sees at most one PTP session per floor. Never panics.
    pub fn read_state(&self) -> RelayState {
        // Poison-immune (recover the inner value): `read_state_uncached` is panic-free today, but
        // a future panic under the held lock must NOT wedge every later `/api/state` into a
        // permanent panic — there is no `Restart=` on the unit (issue 1228 is deliberately not
        // landed). This keeps the "never panics" contract literally true.
        let mut cache = self
            .read_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now_ms = self.clock.now_ms();
        if let Some(cached) = cache.as_ref() {
            if read_is_fresh(Some(cached.read_at_ms), now_ms, self.min_read_interval_ms) {
                return cached.state.clone();
            }
        }
        let state = self.read_state_uncached();
        *cache = Some(CachedRead {
            state: state.clone(),
            read_at_ms: now_ms,
        });
        state
    }

    /// The real (un-throttled) read cycle: one `gphoto2 --auto-detect` + seven `--get-config`.
    /// A detect miss or a gphoto2 read error degrades to an offline [`RelayState`], the
    /// server-is-truth model. Reached only through [`read_state`](Self::read_state)'s floor.
    fn read_state_uncached(&self) -> RelayState {
        let camera = self.detect();
        if camera.is_none() {
            // The box capture rate is known even with no camera — report it (issue 809).
            return RelayState {
                capture_fps: self.capture_fps,
                ..RelayState::offline(self.version.clone())
            };
        }
        match self.read_raw() {
            Ok(raw) => {
                let (params, caps) = params_and_caps(&raw);
                RelayState {
                    online: true,
                    camera,
                    params,
                    caps: Some(caps),
                    fps_supported: fps_supported(&raw),
                    capture_fps: self.capture_fps,
                    version: self.version.clone(),
                }
            }
            Err(_) => RelayState {
                camera,
                capture_fps: self.capture_fps,
                ..RelayState::offline(self.version.clone())
            },
        }
    }

    /// Applies a shading write request. Returns the number of gphoto2 `set-config` writes
    /// performed. Aperture is planned against the camera's live f-number choices and the
    /// live fps (for the shutter angle), so a write always matches the current camera.
    pub fn apply(&self, req: &SetRequest) -> Result<usize> {
        // Read the live config ONCE, and PROPAGATE a read failure: falling back to a default
        // fps here would plan a wrong d002 shutter angle and write a wrong exposure to a live
        // camera on a partial read (the handler maps this error to 502).
        let raw = self.read_raw()?;
        let fnumber_choices = parse_choices(&raw.fnumber);
        let (params, _) = params_and_caps(&raw);
        let fps100 = params.fps100.unwrap_or(DEFAULT_FPS100);
        let writes = plan_writes(req, &fnumber_choices, fps100);
        let n = writes.len();
        // issue 1229: run the writes, but INVALIDATE the cache whether they ALL succeed OR one
        // fails partway. A mid-apply gphoto2 error (camera busy / unplugged — the handler maps it
        // to 502) still leaves the camera DIRTY: the earlier writes already landed, so the cached
        // pre-write snapshot is stale either way and must not be served for up to a floor. (The
        // top `read_raw()?` stays non-invalidating: nothing was written there.) Writes are
        // user-initiated + rare, so this cannot reintroduce sustained bus contention.
        let mut write_err: Option<anyhow::Error> = None;
        for (key, value) in writes {
            if let Err(e) = self.runner.set_config(&key, &value) {
                write_err = Some(e);
                break;
            }
        }
        *self
            .read_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        match write_err {
            Some(e) => Err(e),
            None => Ok(n),
        }
    }
}
