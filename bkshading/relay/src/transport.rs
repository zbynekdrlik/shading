//! The relay's camera transport: shell out to the `gphoto2` CLI over USB-PTP.
//!
//! Design decision (issue 808 design comment, Prístup 1): the relay drives the camera
//! by spawning the system `gphoto2` binary — NOT a build-time `libgphoto2` FFI binding.
//! This keeps the crate free of any C build-time dependency, so it cross-compiles cleanly
//! for ARM (a zero-class arm64 SBC handheld relay — Pi Zero 2 W / Radxa ZERO 3W / Orange
//! Pi Zero 2W), and it reuses the exact gphoto2
//! semantics the dev2 MVP verified. The `Gphoto2Runner` trait is the seam: `Gphoto2Cli`
//! is the real impl, and tests inject a fake so every path is exercised without a camera.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use bkshading_proto::aperture::not_applied_after_aperture_motion;
use bkshading_proto::mapping::{
    choices_to_norm, fnumber_to_av, norm_to_choice_index, parse_fnumber, parse_fnumber_labels,
    DEFAULT_FPS100,
};
use bkshading_proto::read::{
    fps_supported, not_applied_keys, params_and_caps, plan_writes, FpsNotSettable, RawConfigs,
};
use bkshading_proto::wire::{
    summarize_set_request, RelayState, SetQueue, SetRequest, ShadingParams, SubmitAction,
};

use crate::burst::{
    burst_step, BurstAction, BurstEvent, BurstState, Gphoto2Shell, WRITE_SESSION_IDLE_MS,
};

// issue 1452: the aperture convergence a write burst runs after an aperture write.
mod aperture_converge;
// issue 1337 (moved by issue 1452): one shading write inside a write burst.
mod burst_apply;
// issue 808: the gphoto2 CLI seam and the read constants/helpers, split out for the size budget.
mod gphoto2;
mod reads;

pub use gphoto2::{
    gphoto2_stderr_tail, parse_first_model, split_focus_and_summary, Gphoto2Cli, Gphoto2Runner,
    GPHOTO2_TIMEOUT,
};
pub use reads::{
    build_get_config_many_args, parse_capture_fps_env, parse_min_read_interval_env, read_is_fresh,
    split_config_blocks, InstantClock, MonoClock, APERTURE_SETTLE_MAX_READS,
    APERTURE_SETTLE_POLL_MS, CORE_CONFIG_KEYS, DEFAULT_MIN_READ_INTERVAL_MS, FNUMBER_RAW_KEY,
    FOCUS_DISTANCE_KEY, MAX_MIN_READ_INTERVAL_MS, PACED_READ_KEYS, READ_PACING_MS,
};

/// The outcome of a [`CameraSession::submit`] (issue 1309 single-flight; issue 1337 immediate
/// response). `Applied` = this call ran the write(s) now and carries the count PLUS the relay's
/// resulting shading state (projected from the writes it just applied), so the service can push an
/// immediate confirmation to the panel without waiting for the next 2 s pump tick. `Queued` = a
/// write was already in flight, so this SET was appended to the FIFO queue and will be applied
/// IN ORDER by that in-flight worker (never a second parallel gphoto2 — the issue-1309 guarantee).
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyOutcome {
    Applied {
        count: usize,
        state: Box<RelayState>,
    },
    Queued,
}

/// RAII guard around the single-flight write drain (issue 1309, review YELLOW): while `armed`, its
/// `Drop` resets the [`SetQueue`] gate (clearing `in_flight` + any pending follow-up) so a PANIC
/// inside `apply` can never leave shading permanently wedged (`in_flight` stuck true → every later
/// SET coalesces forever). The normal completion paths disarm it; the error path calls
/// `disarm_and_abort` (which also logs the dropped, already-acked coalesced follow-up).
struct FlightGuard<'a> {
    queue: &'a std::sync::Mutex<SetQueue>,
    armed: bool,
}

impl FlightGuard<'_> {
    /// Explicitly abort the flight (drop in-flight + pending) and disarm the guard, logging any
    /// dropped coalesced follow-up at warn so its loss is reconstructible from the journal.
    fn disarm_and_abort(&mut self) {
        self.armed = false;
        let dropped = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .abort();
        for req in &dropped {
            tracing::warn!(
                dropped = %summarize_set_request(req),
                "shading SET dropped: the in-flight write failed and a queued follow-up (already acknowledged) was not applied"
            );
        }
    }
}

impl Drop for FlightGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            // Reached only on an UNWIND (a panic in the burst apply) — reset the gate so the next
            // SET runs, and log every queued follow-up dropped with it.
            let dropped = self
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .abort();
            for req in &dropped {
                tracing::warn!(
                    dropped = %summarize_set_request(req),
                    "shading SET dropped: the in-flight write panicked; single-flight gate reset"
                );
            }
        }
    }
}

/// One cached read cycle (issue 1229): the last `RelayState` and the monotonic ms it was read at.
struct CachedRead {
    state: RelayState,
    read_at_ms: u64,
}

/// Projects the writes of a [`SetRequest`] onto a base [`ShadingParams`] (issue 1337): the params
/// the camera WILL report after the burst applies them, computed WITHOUT a fresh USB read. This is
/// returned in the `PUT /api/params` response so the service can push an immediate confirmation to
/// the panel (the authoritative read at burst close corrects any drift). Aperture is resolved
/// through the SAME `fnumber_labels` + [`norm_to_choice_index`] the write itself uses, so the
/// projected `aperture_av`/`aperture_norm` land exactly where the camera will. A `None` field in
/// `req` leaves the base value untouched. Pure — Tier-0 tested + rustc-replicated.
pub fn project_shading(
    base: &ShadingParams,
    req: &SetRequest,
    fnumber_labels: &[String],
) -> ShadingParams {
    let mut p = base.clone();
    if let Some(norm) = req.aperture_norm {
        let n = fnumber_labels.len() as i64;
        let idx = norm_to_choice_index(norm, n);
        if let Some(f) = fnumber_labels
            .get(idx as usize)
            .and_then(|l| parse_fnumber(l))
        {
            p.aperture_av = fnumber_to_av(f);
            p.aperture_norm = Some(choices_to_norm(idx, n));
        }
    }
    if let Some(iso) = req.iso {
        p.iso = Some(iso);
    }
    if let Some(kelvin) = req.kelvin {
        p.kelvin = Some(kelvin);
    }
    if let Some(tint) = req.tint {
        p.tint = Some(tint);
    }
    if let Some(shutter) = req.shutter {
        p.shutter = Some(shutter);
    }
    if let Some(fps) = req.fps {
        p.fps100 = Some(fps * 100);
    }
    p
}

/// The write-plan basis a burst reads ONCE at open (issue 1337): the parseable f-number labels,
/// the project fps x100 the shutter angle converts at, and the camera's d006 project-fps choices a
/// `fps` write must hit exactly (issue 1402).
#[derive(Debug, Clone)]
struct PlanBasis {
    fnumber_labels: Vec<String>,
    fps100: i64,
    fps_choices: Vec<i64>,
}

/// The relay's write-burst session state (issue 1337) — behind ONE mutex on the [`CameraSession`].
/// Holds the burst lifecycle [`BurstState`], the persistent `gphoto2 --shell` child (since issue
/// 1371 the session's camera channel for paced reads AND write bursts, kept open between bursts),
/// and the [`PlanBasis`] captured by the ONE read at burst open so every subsequent write
/// in the burst is planned WITHOUT a pre-write read.
#[derive(Default)]
struct BurstSession {
    state: BurstState,
    shell: Option<Gphoto2Shell>,
    /// The plan basis read once at burst open; `None` between bursts (re-read next open).
    plan: Option<PlanBasis>,
    /// The full [`RelayState`] read at burst open — the base the PUT-response state projects the
    /// burst's writes onto (so the panel gets an immediate confirmation with the camera's real
    /// caps/model). `None` between bursts.
    open_state: Option<RelayState>,
    /// The MERGED [`SetRequest`] of everything this burst has written so far (issue 1343): each
    /// SET's present fields overwrite. Compared field-by-field against the authoritative readback at
    /// the burst idle-close (via [`not_applied_keys`]) to fill `RelayState.not_applied` — the signal
    /// a silently-refused write (the BMPCC ACKs + ignores an aperture/focus PTP write) actually
    /// happened. Reset at each burst open; cleared with `plan`/`open_state` on a write error.
    written: SetRequest,
    /// The projected [`RelayState`] the burst should be REPRESENTED by while it is open (issue 1343
    /// item 0): the burst-open `open_state` with every write applied so far projected onto it
    /// (`project_shading(open.params, written, labels)`). `read_state`'s `BurstState::Open` arm
    /// serves THIS instead of the pre-burst `read_cache` snapshot — otherwise the service pump's
    /// next `/api/state` tick (~2 s) reconciled the panel's optimistic value BACK to the OLD value
    /// (the owner's "the number changes then reverts after half a second"), since the projected PUT
    /// response was immediately overridden by the pre-burst cache. The authoritative read at the
    /// burst idle-close still replaces it (and item 2's `not_applied` flags what the camera
    /// refused). `None` between bursts; reset at burst open, cleared on a write error + at close.
    /// (`read_state` holds read_cache THEN burst, so `submit` must NOT touch read_cache while
    /// holding burst — hence the projection lives on the burst session, not in read_cache.)
    projected: Option<RelayState>,
    /// A shell open/write failed this burst ⇒ use the CLI write path for the REST of the burst
    /// (never re-open a broken shell per write). Cleared when the burst idle-closes.
    shell_disabled: bool,
    /// The camera's aperture as the raw 0x5007 value x100, as last known in this burst (issue
    /// 1452): seeded from the burst-open read, then the settled read-back of each aperture write.
    /// The "before" of the next aperture write's convergence. `None` = not known (a write that went
    /// over the CLI, an unreadable read-back); cleared at idle-close and on a write error.
    aperture_raw: Option<i64>,
    /// The iris position before a run of QUEUED aperture clicks (issue 1452): a click with another
    /// aperture click queued behind it skips its read-back and leaves its "before" here, so the
    /// run's last click converges from where the run started. Cleared with `aperture_raw`.
    aperture_run_start: Option<i64>,
    /// The listed index the run's latest DEFERRED click asked for (issue 1452): where the iris is
    /// heading when the run's last click runs, so that click judges its overshoot direction from
    /// it (never from the run's start, which the iris already left). Cleared with
    /// `aperture_run_start`.
    aperture_run_last_request: Option<usize>,
    /// Whether the burst's LAST aperture write moved the iris (issue 1452): `Some(true)` = it moved
    /// (wherever it landed, so the idle-close does not report it as ignored), `Some(false)` = it
    /// did not move (cam7: flagged as before), `None` = not measured. Taken at idle-close.
    aperture_moved: Option<bool>,
}

/// Merges every present (`Some`) field of `req` into `acc` (issue 1343) — a later SET's value wins,
/// an absent field leaves the accumulated one. So a burst's `written` reflects the LAST value asked
/// for each key, which is exactly what the readback at burst close should equal.
fn merge_written(acc: &mut SetRequest, req: &SetRequest) {
    if req.aperture_norm.is_some() {
        acc.aperture_norm = req.aperture_norm;
    }
    if req.iso.is_some() {
        acc.iso = req.iso;
    }
    if req.kelvin.is_some() {
        acc.kelvin = req.kelvin;
    }
    if req.tint.is_some() {
        acc.tint = req.tint;
    }
    if req.shutter.is_some() {
        acc.shutter = req.shutter;
    }
    if req.fps.is_some() {
        acc.fps = req.fps;
    }
    if req.auto_wb.is_some() {
        acc.auto_wb = req.auto_wb;
    }
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
    /// Last observed camera online/offline state (issue 1309): `read_state_uncached` logs ONE info
    /// line per transition (online -> offline and back), so the journal records a camera
    /// coming/going without per-read-cycle noise.
    last_online: Mutex<Option<bool>>,
    /// The camera model the last successful detect found (issue 1371). `gphoto2 --auto-detect`
    /// enumerates EVERY USB device on the box, the capture grabber included, and on cam5 that
    /// sweep every read cycle knocked the grabber's capture phase out of lock. A read cycle reuses
    /// this and detects again only at the first read and after a failed read.
    detected: Mutex<Option<String>>,
    /// Single-flight FIFO gate for shading writes (issue 1309 single-flight; issue 1337 FIFO).
    /// `submit` funnels every `PUT /api/params` through it so a burst of SETs never forks a second
    /// gphoto2; the in-flight worker drains the FIFO queue IN ORDER (20 clicks = 20 moves).
    set_queue: Mutex<SetQueue>,
    /// The write-burst session (issue 1337): the persistent `gphoto2 --shell` + its lifecycle +
    /// the cached plan basis. ONE mutex, taken by `submit` for a whole burst drain and by
    /// `read_state` for its idle-close check — so ALL camera access is serialized through it plus
    /// the read-cache lock (a burst holds this across its writes; `read_state` serves cache while
    /// the shell owns the camera, and only reads when the burst is idle).
    burst: Mutex<BurstSession>,
    /// The `gphoto2` binary path used to OPEN the burst shell (issue 1337). Empty string = the burst
    /// shell is disabled (fake-runner tests, or an explicit opt-out) — the burst then always uses
    /// the CLI runner write path, which is still pre-read-free and fast. The real binary comes from
    /// the CLI `--gphoto2` arg via [`with_gphoto2_binary`](Self::with_gphoto2_binary).
    gphoto2_binary: String,
    /// The pause before each 0x5007 read-back after an aperture write (issue 1452);
    /// [`APERTURE_SETTLE_POLL_MS`] in production, shortened by tests.
    aperture_settle_poll_ms: u64,
    /// The state the open burst is represented by, published by the burst drain whenever it
    /// changes (issue 1452): `read_state` serves it when the burst lock is busy (a write, a 0x5007
    /// settle) instead of waiting behind it — the service polls with a 1.5 s timeout and a settle
    /// can take longer. A leaf lock: nothing else is ever taken while it is held. `None` outside a
    /// burst.
    live_projection: Mutex<Option<RelayState>>,
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
            last_online: Mutex::new(None),
            detected: Mutex::new(None),
            set_queue: Mutex::new(SetQueue::default()),
            burst: Mutex::new(BurstSession::default()),
            gphoto2_binary: String::new(),
            aperture_settle_poll_ms: APERTURE_SETTLE_POLL_MS,
            live_projection: Mutex::new(None),
        }
    }

    /// Sets the pause before each 0x5007 read-back after an aperture write (issue 1452). Production
    /// keeps [`APERTURE_SETTLE_POLL_MS`]; tests shorten it so a fake shell settles fast.
    pub fn with_aperture_settle_poll_ms(mut self, ms: u64) -> Self {
        self.aperture_settle_poll_ms = ms;
        self
    }

    /// Sets the `gphoto2` binary path used to open the write-burst shell (issue 1337). The binary
    /// passes the CLI `--gphoto2` value; when unset (empty), the burst uses the CLI runner write
    /// path only (no persistent shell) — still pre-read-free, just one process per write.
    pub fn with_gphoto2_binary(mut self, binary: impl Into<String>) -> Self {
        self.gphoto2_binary = binary.into();
        self
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

    /// The camera for a read cycle (issue 1371): the cached model from an earlier detect, else ONE
    /// `gphoto2 --auto-detect` (cached when it finds a camera). [`Self::forget_detected`] clears it
    /// after a failed read, so a camera that went away is detected again on the next cycle.
    fn camera_for_read(&self) -> Option<String> {
        let mut cached = self
            .detected
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cached.is_none() {
            *cached = self.detect();
        }
        cached.clone()
    }

    /// Drops the cached camera model after a failed read (issue 1371).
    fn forget_detected(&self) {
        *self
            .detected
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// One real read of the camera's config (issue 1371): through the persistent, PACED shell when
    /// a gphoto2 binary is configured (one key every [`READ_PACING_MS`], the shell kept open in
    /// `shell` for the next cycle and for write bursts), else the CLI path. A shell that fails or
    /// wedges is killed and that cycle reads over the CLI instead, so a broken shell never loses a
    /// read; the next cycle opens a fresh one. The caller holds the burst lock, so the shell and a
    /// CLI gphoto2 never run against the camera at once.
    fn read_raw(&self, shell: &mut Option<Gphoto2Shell>) -> Result<RawConfigs> {
        if !self.gphoto2_binary.is_empty() {
            match self.read_raw_paced(shell) {
                Ok(raw) => return Ok(raw),
                Err(e) => {
                    tracing::warn!(error = %e, "paced shell read failed; shell closed, CLI read this cycle");
                    if let Some(sh) = shell.take() {
                        sh.close();
                    }
                }
            }
        }
        self.read_raw_cli()
    }

    /// The paced read (issue 1371): opens the shell when none is open, then sends
    /// [`PACED_READ_KEYS`] one at a time, [`READ_PACING_MS`] apart. A core key without an answer
    /// is an error (the caller falls back to the CLI); the best-effort `5007` / `d003` answer empty.
    fn read_raw_paced(&self, shell: &mut Option<Gphoto2Shell>) -> Result<RawConfigs> {
        if shell.is_none() {
            *shell = Some(Gphoto2Shell::open(&self.gphoto2_binary)?);
            tracing::info!("shading camera shell opened (paced reads + write bursts)");
        }
        let sh = shell.as_mut().expect("shell opened immediately above");
        let started = Instant::now();
        let mut blocks: Vec<String> = Vec::with_capacity(PACED_READ_KEYS.len());
        for (i, key) in PACED_READ_KEYS.iter().enumerate() {
            if i > 0 {
                std::thread::sleep(Duration::from_millis(READ_PACING_MS));
            }
            match sh.get_config(key)? {
                Some(block) => blocks.push(block),
                None if *key == FNUMBER_RAW_KEY || *key == FOCUS_DISTANCE_KEY => {
                    blocks.push(String::new())
                }
                None => anyhow::bail!("gphoto2 --shell answered get-config {key} with an error"),
            }
        }
        tracing::info!(
            keys = PACED_READ_KEYS.len(),
            pacing_ms = READ_PACING_MS,
            duration_ms = started.elapsed().as_millis() as u64,
            "gphoto2 paced shell read"
        );
        let [iso, fnumber, shutter_angle, kelvin, tint, project_fps, sensor_fps, fnumber_raw, focus_distance]: [String; 9] =
            blocks.try_into().map_err(|v: Vec<String>| {
                anyhow::anyhow!("paced read returned {} blocks, expected {}", v.len(), PACED_READ_KEYS.len())
            })?;
        Ok(RawConfigs {
            iso,
            fnumber,
            shutter_angle,
            kelvin,
            tint,
            project_fps,
            sensor_fps,
            focus_distance,
            summary: String::new(),
            fnumber_raw,
        })
    }

    /// The CLI read (issue 1229): the seven core keys in ONE gphoto2 process + the best-effort
    /// `d003 --summary` session. Used when no gphoto2 binary is configured for the shell (the fake
    /// runners of the tests) and for one cycle after the paced shell failed.
    fn read_raw_cli(&self) -> Result<RawConfigs> {
        // issue 1229: read the seven CORE shading keys in ONE gphoto2 invocation (ONE USB-PTP
        // session), not one session per key. `get_config_many` returns exactly one block per key,
        // in `CORE_CONFIG_KEYS` order, or an `Err` (spawn/exit/count mismatch) that propagates via
        // `?` -> offline, the same failure semantics the previous per-key `?` had.
        let core = self.runner.get_config_many(&CORE_CONFIG_KEYS)?;
        // Defensive typed destructure: `get_config_many`'s contract is EXACTLY `keys.len()` blocks
        // on `Ok`, but a buggy runner returning a different count must NOT index-panic (read_state
        // must never panic). `Vec<String>: TryInto<[String; 7]>` makes the count check, the
        // panic-freedom, and the field arity ONE construct that also moves each block into its field
        // with no per-block clone; a wrong length hands the Vec back and degrades to a failed read
        // (-> offline), the same fail-safe as before.
        // issue 1402: d006 is the PROJECT fps, d007 the OFF-SPEED fps (CORE_CONFIG_KEYS order).
        let [iso, fnumber, shutter_angle, kelvin, tint, project_fps, sensor_fps]: [String; 7] =
            core.try_into().map_err(|v: Vec<String>| {
                anyhow::anyhow!(
                    "get_config_many returned {} blocks, expected {}",
                    v.len(),
                    CORE_CONFIG_KEYS.len()
                )
            })?;
        // issue 1238 + 1306: the best-effort d003 focus distance AND the gphoto2 `--summary` text
        // are read TOGETHER in ONE gphoto2 session (`get_focus_and_summary`), NOT folded into the
        // core batch — so a camera/firmware/lens that does not answer them can never abort the
        // batched core read (which would wrongly degrade the whole shading state to offline), and a
        // full read stays detect + core batch + this = 3 USB sessions (issue 1229, was 9). A failure
        // degrades BOTH to empty: a `None` focus_distance, and the aperture falls back to the RADIO
        // `Current:` (the pre-1306 behaviour). `apply` also calls `read_raw`, so a write pays this
        // same shape — negligible (writes are rare + user-initiated).
        let (focus_distance, summary) = self.runner.get_focus_and_summary().unwrap_or_default();
        Ok(RawConfigs {
            iso,
            fnumber,
            shutter_angle,
            kelvin,
            tint,
            project_fps,
            sensor_fps,
            focus_distance,
            summary,
            fnumber_raw: String::new(),
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
        // issue 1337: coordinate with the write-burst. The burst lock is held ACROSS the read below
        // so a burst's shell and this CLI read can never run two gphoto2 processes against the one
        // USB camera at once (lock order: read_cache -> burst here; `submit` takes burst only, so no
        // cycle). While a burst's shell owns the camera, serve the cache (never read). When the
        // burst has gone idle, close the shell and take ONE authoritative read at burst close,
        // bypassing the issue-1229 floor once (a rare, user-interaction-driven event, not polling).
        // issue 1452: never WAIT for the burst lock. While the burst drain holds it (a write, a
        // 0x5007 settle that can outlast the service's 1.5 s poll timeout) the camera is owned by
        // the shell anyway, so answer at once with the burst's published state.
        let mut burst = match self.burst.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                return self.busy_burst_state(cache.as_ref().map(|c| &c.state));
            }
        };
        let (next_state, action) = burst_step(
            burst.state,
            BurstEvent::IdleCheck { now_ms },
            WRITE_SESSION_IDLE_MS,
        );
        // issue 1343: the writes this burst made, captured at idle-close to compare against the
        // authoritative readback below (`not_applied_keys`) — `Some` only on the burst-close read.
        let mut written_at_close: Option<SetRequest> = None;
        // issue 1452: whether the burst's last aperture write moved the iris (taken at idle-close).
        let mut aperture_moved_at_close: Option<bool> = None;
        let force_read = match action {
            BurstAction::CloseShellFinalRead => {
                burst.state = next_state;
                // issue 1371: the shell stays open (it is the paced read channel too); only the
                // burst's plan and projection end here.
                burst.plan = None;
                burst.open_state = None;
                burst.projected = None; // issue 1343 item 0: the authoritative read below supersedes it
                                        // issue 1343: take the merged writes BEFORE resetting, so the authoritative read
                                        // below can flag every key the camera did not apply.
                written_at_close = Some(std::mem::take(&mut burst.written));
                aperture_moved_at_close = burst.aperture_moved.take();
                burst.aperture_raw = None;
                burst.aperture_run_start = None;
                burst.aperture_run_last_request = None;
                self.publish_projection(&burst); // issue 1452: no burst state outlives the burst
                burst.shell_disabled = false;
                tracing::info!("shading write-burst idle-closed; taking one authoritative read");
                true
            }
            _ if matches!(burst.state, BurstState::Open { .. }) => {
                // The shell owns the camera this instant — never a second concurrent gphoto2 read.
                // issue 1343 item 0: serve the burst's PROJECTED state (open-read + every write
                // applied so far) so a pump `/api/state` tick during the burst confirms the
                // operator's clicks instead of reconciling them back to the pre-burst `read_cache`
                // snapshot ("the number reverts after half a second"). Falls back to the last
                // read_cache snapshot, then a benign offline snapshot, if no write has projected yet.
                return burst
                    .projected
                    .clone()
                    .or_else(|| cache.as_ref().map(|c| c.state.clone()))
                    .unwrap_or_else(|| RelayState {
                        capture_fps: self.capture_fps,
                        ..RelayState::offline(self.version.clone())
                    });
            }
            _ => false, // burst Idle -> normal floored read
        };
        if !force_read {
            if let Some(cached) = cache.as_ref() {
                if read_is_fresh(Some(cached.read_at_ms), now_ms, self.min_read_interval_ms) {
                    return cached.state.clone();
                }
            }
        }
        let mut state = self.read_state_uncached(&mut burst.shell);
        // issue 1343: on the burst idle-close read, flag every written key the camera did NOT
        // apply (compared against this authoritative readback). Only meaningful when the camera is
        // online and reported its caps (the aperture comparison needs the choice grid); an offline
        // close leaves `not_applied` empty. The flag rides the cached state to the panel via the
        // pump and clears on the next real read.
        if let Some(written) = written_at_close {
            if state.online {
                let choices: Vec<f64> = state
                    .caps
                    .as_ref()
                    .map(|c| c.fnumber_choices.clone())
                    .unwrap_or_default();
                // issue 1452: an aperture write that MOVED the iris is applied wherever it landed
                // (cam6 one listed step past the request, cam5 its lens's wide-open stop): the
                // state carries the camera's real value, so it is not listed as ignored. A write
                // that did not move (cam7) or was not measured keeps the issue-1343 flag.
                state.not_applied = not_applied_after_aperture_motion(
                    not_applied_keys(&written, &state.params, &choices),
                    aperture_moved_at_close,
                );
                if written.aperture_norm.is_some() && aperture_moved_at_close == Some(true) {
                    tracing::info!(
                        aperture_av = ?state.params.aperture_av,
                        "shading aperture write moved the iris; reported at the camera's own value"
                    );
                }
                if !state.not_applied.is_empty() {
                    tracing::info!(
                        not_applied = ?state.not_applied,
                        "shading write(s) NOT applied by the camera (ACKed + ignored)"
                    );
                }
            }
        }
        *cache = Some(CachedRead {
            state: state.clone(),
            read_at_ms: now_ms,
        });
        state
    }

    /// The real (un-throttled) read cycle: the camera (issue 1371: the cached model, else one
    /// `gphoto2 --auto-detect`) + `read_raw` (issue 1371: the nine keys through the persistent
    /// shell, one every [`READ_PACING_MS`]; the CLI's batched core read + `d003 --summary` when no
    /// shell is configured or the shell failed this cycle).
    /// A detect miss or a gphoto2 read error degrades to an offline [`RelayState`], the
    /// server-is-truth model. Reached only through [`read_state`](Self::read_state)'s floor.
    fn read_state_uncached(&self, shell: &mut Option<Gphoto2Shell>) -> RelayState {
        let state = self.compute_state(shell);
        self.note_online_transition(&state);
        state
    }

    /// Builds the current [`RelayState`] from one real read cycle (the old `read_state_uncached`
    /// body), WITHOUT the transition-log side effect (issue 1309 kept the two concerns separate).
    fn compute_state(&self, shell: &mut Option<Gphoto2Shell>) -> RelayState {
        let camera = self.camera_for_read();
        if camera.is_none() {
            // The box capture rate is known even with no camera — report it (issue 809).
            return RelayState {
                capture_fps: self.capture_fps,
                ..RelayState::offline(self.version.clone())
            };
        }
        match self.read_raw(shell) {
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
                    // issue 1343: a plain read carries no write-comparison; the burst idle-close
                    // path (read_state) fills this from `not_applied_keys` when it applies.
                    not_applied: Vec::new(),
                }
            }
            Err(_) => {
                self.forget_detected();
                RelayState {
                    camera,
                    capture_fps: self.capture_fps,
                    ..RelayState::offline(self.version.clone())
                }
            }
        }
    }

    /// Logs ONE info line whenever the camera online/offline state FLIPS since the last read cycle
    /// (issue 1309) — a camera coming online (with its model) or going offline. No line on a
    /// steady state, so this never adds per-cycle noise. Poison-immune (recover the inner value).
    fn note_online_transition(&self, state: &RelayState) {
        let mut last = self
            .last_online
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *last != Some(state.online) {
            if state.online {
                tracing::info!(camera = ?state.camera, "camera online");
            } else {
                tracing::info!("camera offline");
            }
            *last = Some(state.online);
        }
    }

    /// Applies a shading write request. Returns the number of gphoto2 `set-config` writes
    /// performed. Aperture is planned against the camera's live f-number choices and the
    /// live fps (for the shutter angle), so a write always matches the current camera.
    ///
    /// NOTE (issue 1337): this is the ORIGINAL per-invocation write path (read_raw + one
    /// `set_config` per param), UNCHANGED so its direct unit test still holds. The production
    /// `PUT /api/params` handler calls [`submit`](Self::submit) (the write-burst path), NOT this.
    /// `apply` locks only `read_cache` and is NOT burst-aware, so it MUST NOT be called
    /// concurrently with `submit` (which locks `burst`) — the two would not mutually exclude and
    /// could run two gphoto2 processes against the one USB camera. Keep it test-only.
    pub fn apply(&self, req: &SetRequest) -> Result<usize> {
        // issue 1229: HOLD the `read_cache` lock across the ENTIRE apply (the `read_raw` + the
        // `set_config` writes), not just at the final invalidate. `http.rs` dispatches `read_state`
        // and `apply` on independent `spawn_blocking` threads, so a panel write landing while the
        // service pump's read is in flight would otherwise run TWO gphoto2 processes against the one
        // USB camera at once — the second fails to claim the interface (a 502 write, or a cached
        // "offline" read for a whole floor). Serializing camera access is the SAME invariant
        // `read_state` relies on (it also holds this lock across its read). No re-entrancy: `apply`
        // never calls `read_state`, and `read_raw` never locks the cache — so this cannot deadlock.
        let mut cache = self
            .read_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Read the live config ONCE, and PROPAGATE a read failure: falling back to a default
        // fps here would plan a wrong d002 shutter angle and write a wrong exposure to a live
        // camera on a partial read (the handler maps this error to 502). This top `read_raw()?`
        // early-return drops the lock guard WITHOUT invalidating — correct: nothing was written.
        let raw = self.read_raw_cli()?;
        // issue 1304: the write index basis is the PARSEABLE-only f-number list — the SAME basis
        // `params_and_caps` uses for the readback `aperture_norm` and the caps `fnumber_choices`
        // the panel steps over. Using the unfiltered `parse_choices` here would desync the count.
        let fnumber_choices = parse_fnumber_labels(&raw.fnumber);
        let (params, caps) = params_and_caps(&raw);
        let fps100 = params.fps100.unwrap_or(DEFAULT_FPS100);
        // issue 1402: a `fps` the camera's d006 does not list refuses the whole request here,
        // before any write — the lock guard drops WITHOUT invalidating (nothing was written).
        let writes = plan_writes(req, &fnumber_choices, fps100, &caps.fps_choices)?;
        let n = writes.len();
        // Run the writes, then INVALIDATE the cache whether they ALL succeed OR one fails partway.
        // A mid-apply gphoto2 error (camera busy / unplugged — the handler maps it to 502) still
        // leaves the camera DIRTY: the earlier writes already landed, so the cached pre-write
        // snapshot is stale either way and must not be served for up to a floor. Writes are
        // user-initiated + rare, so holding the lock this long cannot reintroduce bus contention.
        let mut write_err: Option<anyhow::Error> = None;
        for (key, value) in writes {
            if let Err(e) = self.runner.set_config(&key, &value) {
                write_err = Some(e);
                break;
            }
        }
        *cache = None;
        match write_err {
            Some(e) => Err(e),
            None => Ok(n),
        }
    }

    /// Submits a shading write through the single-flight FIFO gate + the write-burst session
    /// (issue 1309 single-flight; issue 1337 immediate response) — the entry point the HTTP
    /// `PUT /api/params` handler uses. If NO write is in flight, this call drives the burst now and,
    /// as it runs, drains every SET that queued behind it IN ORDER (FIFO — 20 rapid clicks = 20
    /// camera moves). If a write IS already in flight, this SET is appended to the FIFO and returns
    /// [`ApplyOutcome::Queued`] (never a second concurrent gphoto2 — the issue-1309 guarantee).
    ///
    /// The burst plans each write from the choices read ONCE at burst open (no pre-write read) and
    /// applies it through the persistent `gphoto2 --shell` (falling back to a per-invocation CLI
    /// write on any shell error), so a click moves the camera in ~150 ms instead of ~1-3 s. The
    /// returned [`ApplyOutcome::Applied`] carries the projected resulting state so the service can
    /// push an immediate confirmation; the authoritative read happens when the burst idle-closes.
    pub fn submit(&self, req: &SetRequest) -> Result<ApplyOutcome> {
        let mut current = {
            let mut q = self
                .set_queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match q.submit(req.clone()) {
                SubmitAction::Queued { total } => {
                    tracing::info!(
                        queued_total = total,
                        "shading SET queued behind an in-flight burst (FIFO, applied in order)"
                    );
                    return Ok(ApplyOutcome::Queued);
                }
                SubmitAction::RunNow(r) => r,
            }
        };
        // Hold the burst lock for the WHOLE drain: the shell (or CLI fallback) exclusively owns the
        // camera until the drain finishes, so `read_state` serves cache meanwhile (no second
        // concurrent gphoto2). New clicks never block here — they touch only the set_queue above and
        // queue. Lock order: burst here (+ set_queue for finish/abort); never read_cache while held.
        let mut burst = self
            .burst
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Unwind guard (issue 1309, review YELLOW): a panic in the burst apply must reset the
        // single-flight gate (and log any dropped queued follow-up), or every later SET would
        // coalesce forever — the half-dead availability class this fights.
        let mut guard = FlightGuard {
            queue: &self.set_queue,
            armed: true,
        };
        let mut total_applied = 0usize;
        // issue 1402: THIS caller's own SET refused (a `fps` the camera's d006 does not list).
        // Returned after the drain, as the caller's answer (the handler maps it to a 422).
        let mut own_refusal: Option<anyhow::Error> = None;
        let mut own_set = true;
        loop {
            match self.burst_apply(&mut burst, &current) {
                Err(e) if e.is::<FpsNotSettable>() => {
                    // A refused fps is the CLIENT's error, decided before any write: the camera
                    // is untouched and the burst plan still holds, so the queued follow-ups (other
                    // clients' SETs, already acknowledged) still run in order. `burst_apply`
                    // logged the refusal; only the caller's own SET answers with it.
                    if own_set {
                        own_refusal = Some(e);
                    }
                }
                Err(e) => {
                    // A CLI-write failure (real camera error): drop the in-flight state AND the
                    // whole queued FIFO — writes planned against a now-uncertain camera must not run
                    // blind. The handler maps this to a 502.
                    guard.disarm_and_abort();
                    return Err(e);
                }
                Ok(applied) => total_applied += applied,
            }
            own_set = false;
            let next = self
                .set_queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .finish();
            match next {
                Some(r) => {
                    tracing::info!("draining queued shading SET (FIFO, in order)");
                    current = r;
                }
                None => {
                    guard.armed = false; // clean completion — finish() idled the gate
                    break;
                }
            }
        }
        if let Some(refused) = own_refusal {
            return Err(refused);
        }
        // Build the PUT-response state from the burst's running PROJECTED state (issue 1343 item 0:
        // the burst-open read with EVERY write applied so far projected onto it, maintained by
        // `burst_apply`) — no fresh USB read (the authoritative read happens at burst idle-close).
        // This is the SAME state the pump's `Open` arm serves, so the immediate confirmation and the
        // next pump tick agree. Falls back to the burst-open state, then a benign offline snapshot.
        let state = burst
            .projected
            .clone()
            .or_else(|| burst.open_state.clone())
            .unwrap_or_else(|| RelayState {
                capture_fps: self.capture_fps,
                ..RelayState::offline(self.version.clone())
            });
        drop(burst);
        Ok(ApplyOutcome::Applied {
            count: total_applied,
            state: Box::new(state),
        })
    }
}
