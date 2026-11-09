//! The relay's camera session: one Blackmagic camera driven through its REST API (shading#6).
//!
//! The owner's decision (shading#6, 9.11.2026): the relay talks to the camera through the
//! official Blackmagic REST API over the camera's own USB-C CDC-NCM link, not the `gphoto2` CLI.
//! [`rest`] is the HTTP client, [`discover`] finds the camera's address on that link, and the pure
//! mapping lives in `bkshading_proto::read`. What this module keeps from the PTP relay:
//! - the read floor (issue 1229): `/api/state` is served from a cache between real reads, so the
//!   service's 2 s poll never becomes a steady stream of requests on the shared USB bus;
//! - the single-flight FIFO write gate (issue 1309/1337): concurrent SETs never write at once, and
//!   20 rapid clicks are 20 in-order camera writes;
//! - the `notApplied` report (issue 1343), now from the read-back after each write.
//!
//! What REST made obsolete is gone: the persistent `gphoto2 --shell`, the paced key reads, and the
//! 0x5007 aperture converge/settle (issue 1452). `GET lens/iris` returns the f-stop the lens
//! actually landed on.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use bkshading_proto::mapping::aperture_probe_target;
use bkshading_proto::read::{
    fps_supported, iris_moved, not_applied_keys, params_and_caps, plan_writes, FpsNotSettable,
    RestWrite, WriteBasis,
};
use bkshading_proto::wire::{
    summarize_set_request, RelayState, SetQueue, SetRequest, SubmitAction,
};

pub mod discover;
mod reads;
pub mod rest;

pub use discover::{
    camera_links, parse_inet, parse_lease, peer_of, CameraLink, DiscoverPaths,
    BLACKMAGIC_USB_VENDOR, CAMERA_LINK_DRIVER,
};
pub use reads::{
    parse_capture_fps_env, parse_min_read_interval_env, read_is_fresh, InstantClock, MonoClock,
    DEFAULT_MIN_READ_INTERVAL_MS, MAX_MIN_READ_INTERVAL_MS,
};
pub use rest::{
    parse_camera_addr, CameraAddr, RestClient, RestError, CAMERA_HTTP_PORT, REST_TIMEOUT,
};

/// How long the relay waits after a write that moved the iris before reading it back: the iris
/// motor takes about a second to land (live on cam5), and the read-back is what the panel shows.
pub const APERTURE_READBACK_DELAY_MS: u64 = 1000;

/// A write drain that has run this long answers its caller and hands any still-queued SETs to a
/// background drain (shading#6): the service's write client gives up at 10 s, and a run of aperture
/// steps costs ~1-5 s per press.
pub const DRAIN_ANSWER_BUDGET_MS: u64 = 6000;

/// How long an `apertureStep` probe waits for the iris to leave its current f-stop before it widens
/// the target (shading#6, 0.2.1). The iris lands in about a second (live on cam5).
pub const IRIS_PROBE_WAIT_MS: u64 = 1200;

/// The most probes one `apertureStep` writes that the iris does not follow before the press gives
/// up (1/16 stop each, so 3/8 stop of travel). The first probe that crosses the midpoint to the
/// next native step lands, so a lens with steps up to 3/4 stop apart still lands within this;
/// cam5's widest live case (its zoom-shifted 4.4 end against a description min of 4.1) needs 4.
/// An iris that follows nothing (auto exposure, a manual lens, a cam7-class body) costs at most
/// this many waits and is reported in `notApplied` as `apertureStep`.
pub const MAX_DEAD_APERTURE_PROBES: u32 = 6;

/// A write drain that has run this long reads back WITHOUT the iris wait from then on (shading#6):
/// SETs that keep arriving during read-backs add rounds, and the first caller's PUT must answer
/// well inside the service's 10 s write-client timeout. The next poll shows the landed iris.
pub const DRAIN_SETTLE_BUDGET_MS: u64 = 4000;

/// The longest a cached read is used as a write's basis (shading#6), shorter than the read floor:
/// a format, shutter-unit or iris change made on the camera body shortly before a click is seen.
pub const WRITE_BASIS_MAX_AGE_MS: u64 = 2000;

/// How the relay finds its camera.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraLocator {
    /// A fixed address (`--camera` / `BKSHADING_CAMERA`).
    Fixed(CameraAddr),
    /// The DHCP server on the camera's `cdc_ncm` link ([`discover`]), at `port`.
    Discover { paths: DiscoverPaths, port: u16 },
}

impl CameraLocator {
    /// The default: discovery on the real sysfs + networkd lease paths, HTTP port 80.
    pub fn discover() -> Self {
        CameraLocator::Discover {
            paths: DiscoverPaths::default(),
            port: CAMERA_HTTP_PORT,
        }
    }

    /// The addresses to try this time, and one line describing how they were found.
    fn candidates(&self) -> (Vec<CameraAddr>, String) {
        match self {
            CameraLocator::Fixed(addr) => (vec![addr.clone()], format!("fixed address {addr}")),
            CameraLocator::Discover { paths, port } => {
                let links = camera_links(paths);
                let addrs = links
                    .iter()
                    .filter_map(|l| l.camera_ip)
                    .map(|ip| CameraAddr {
                        host: ip.to_string(),
                        port: *port,
                    })
                    .collect();
                (addrs, discover::describe(&links))
            }
        }
    }
}

/// The outcome of a [`CameraSession::submit`] (issue 1309 single-flight; issue 1337 immediate
/// response). `Applied` = this call ran the write(s) now and carries the count PLUS the camera's
/// state read back after them, so the service pushes a confirmation at once. `Queued` = a write
/// was already in flight, so this SET was appended to the FIFO queue and will be applied IN ORDER
/// by that in-flight worker (never two concurrent writes — the issue-1309 guarantee).
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyOutcome {
    Applied {
        count: usize,
        state: Box<RelayState>,
    },
    Queued,
}

/// RAII guard around the single-flight write drain (issue 1309): while `armed`, its `Drop` resets
/// the [`SetQueue`] gate so a PANIC inside a write can never leave shading permanently wedged
/// (`in_flight` stuck true -> every later SET queues forever).
struct FlightGuard<'a> {
    queue: &'a Mutex<SetQueue>,
    armed: bool,
}

impl FlightGuard<'_> {
    /// Abort the flight (drop in-flight + pending) and disarm the guard, logging every dropped,
    /// already-acknowledged follow-up at warn.
    fn disarm_and_abort(&mut self, why: &str) {
        self.armed = false;
        let dropped = lock(self.queue).abort();
        for req in &dropped {
            tracing::warn!(
                dropped = %summarize_set_request(req), why,
                "shading SET dropped: a queued follow-up (already acknowledged) was not applied"
            );
        }
    }
}

impl Drop for FlightGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.disarm_and_abort("the in-flight write panicked; single-flight gate reset");
        }
    }
}

/// Poison-immune lock: a panic under a lock must not wedge every later request.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One cached read (issue 1229): the state, the write basis it gave (when the camera answered) and
/// when it was read. A write inside the read floor plans on this basis instead of reading again.
struct CachedRead {
    state: RelayState,
    basis: Option<WriteBasis>,
    read_at_ms: u64,
}

/// The camera the session talks to: its client and model, found once and kept until a read fails.
#[derive(Debug, Clone)]
struct Link {
    client: RestClient,
    model: String,
}

/// One real read: the state to report and, when the camera answered, the basis to plan writes on.
struct Reading {
    state: RelayState,
    basis: Option<WriteBasis>,
}

/// The writes of one drain, accumulated for the read-back comparison.
#[derive(Default)]
struct DrainLog {
    count: usize,
    written: SetRequest,
    aperture_written: bool,
    /// Keys to report as not applied that the read-back comparison cannot see (an `apertureStep`
    /// the iris did not follow).
    not_applied: Vec<String>,
}

/// One camera the relay owns, driven through its REST API.
pub struct CameraSession {
    locator: CameraLocator,
    version: String,
    /// The box's capture-mode fps (issue 809), reported in every state (a box property).
    capture_fps: Option<i64>,
    /// Minimum interval between real reads (issue 1229).
    min_read_interval_ms: u64,
    clock: Box<dyn MonoClock>,
    timeout: Duration,
    aperture_readback_delay_ms: u64,
    drain_settle_budget_ms: u64,
    iris_probe_wait_ms: u64,
    drain_answer_budget_ms: u64,
    /// The last real read. The lock is held ACROSS a read, so concurrent `/api/state` requests
    /// coalesce to one camera read per floor.
    read_cache: Mutex<Option<CachedRead>>,
    /// Last online/offline state: one info line per flip (issue 1309).
    last_online: Mutex<Option<bool>>,
    /// The camera found by the last successful probe; dropped when a read fails, so a camera that
    /// went away (or got a new lease) is found again.
    link: Mutex<Option<Link>>,
    /// The last discovery description, logged only when it changes.
    last_discovery: Mutex<Option<String>>,
    /// Single-flight FIFO gate for writes (issue 1309/1337).
    set_queue: Mutex<SetQueue>,
}

impl CameraSession {
    pub fn new(locator: CameraLocator, version: impl Into<String>) -> Self {
        CameraSession {
            locator,
            version: version.into(),
            capture_fps: None,
            min_read_interval_ms: DEFAULT_MIN_READ_INTERVAL_MS,
            clock: Box::new(InstantClock::new()),
            timeout: REST_TIMEOUT,
            aperture_readback_delay_ms: APERTURE_READBACK_DELAY_MS,
            drain_settle_budget_ms: DRAIN_SETTLE_BUDGET_MS,
            iris_probe_wait_ms: IRIS_PROBE_WAIT_MS,
            drain_answer_budget_ms: DRAIN_ANSWER_BUDGET_MS,
            read_cache: Mutex::new(None),
            last_online: Mutex::new(None),
            link: Mutex::new(None),
            last_discovery: Mutex::new(None),
            set_queue: Mutex::new(SetQueue::default()),
        }
    }

    /// Sets the box's capture-mode fps this relay reports (issue 809).
    pub fn with_capture_fps(mut self, capture_fps: Option<i64>) -> Self {
        self.capture_fps = capture_fps;
        self
    }

    /// Sets the read floor (issue 1229).
    pub fn with_min_read_interval_ms(mut self, ms: u64) -> Self {
        self.min_read_interval_ms = ms;
        self
    }

    /// Injects a [`MonoClock`] (tests drive the floor without real sleeps).
    pub fn with_clock(mut self, clock: Box<dyn MonoClock>) -> Self {
        self.clock = clock;
        self
    }

    /// Sets the pause before the read-back after a write that moved the iris. Production keeps
    /// [`APERTURE_READBACK_DELAY_MS`]; tests shorten it.
    pub fn with_aperture_readback_delay_ms(mut self, ms: u64) -> Self {
        self.aperture_readback_delay_ms = ms;
        self
    }

    /// Sets the drain time after which read-backs skip the iris wait. Production keeps
    /// [`DRAIN_SETTLE_BUDGET_MS`]; tests shorten it.
    pub fn with_drain_settle_budget_ms(mut self, ms: u64) -> Self {
        self.drain_settle_budget_ms = ms;
        self
    }

    /// Sets how long an `apertureStep` probe waits for the iris to move before widening. Production
    /// keeps [`IRIS_PROBE_WAIT_MS`]; tests shorten it.
    pub fn with_iris_probe_wait_ms(mut self, ms: u64) -> Self {
        self.iris_probe_wait_ms = ms;
        self
    }

    /// Sets the drain time after which the caller is answered and the rest of the queue drains in
    /// the background. Production keeps [`DRAIN_ANSWER_BUDGET_MS`]; tests shorten it.
    pub fn with_drain_answer_budget_ms(mut self, ms: u64) -> Self {
        self.drain_answer_budget_ms = ms;
        self
    }

    pub fn version(&self) -> String {
        self.version.clone()
    }

    fn offline(&self, camera: Option<String>) -> RelayState {
        RelayState {
            camera,
            capture_fps: self.capture_fps,
            ..RelayState::offline(self.version.clone())
        }
    }

    /// The camera to talk to: the one found before, else the first candidate address that answers
    /// `GET system/product` with a product name.
    fn link(&self) -> Option<Link> {
        let mut cached = lock(&self.link);
        if let Some(l) = cached.as_ref() {
            return Some(l.clone());
        }
        let (addrs, how) = self.locator.candidates();
        let mut tried: Vec<String> = Vec::new();
        for addr in addrs {
            let client = RestClient::new(addr.clone(), self.timeout);
            match client.product() {
                Ok(p) => match p.model() {
                    Some(model) => {
                        tracing::info!(
                            camera = %addr, model = %model,
                            device = p.device_name.as_deref().unwrap_or(""),
                            software = p.software_version.as_deref().unwrap_or(""),
                            found_by = %how,
                            "camera found (REST API answers)"
                        );
                        *lock(&self.last_discovery) = None;
                        let link = Link { client, model };
                        *cached = Some(link.clone());
                        return Some(link);
                    }
                    None => tried.push(format!("{addr}: system/product names no product")),
                },
                Err(e) => tried.push(format!("{addr}: {e}")),
            }
        }
        let summary = if tried.is_empty() {
            how
        } else {
            format!("{how}; {}", tried.join("; "))
        };
        let mut last = lock(&self.last_discovery);
        if last.as_deref() != Some(summary.as_str()) {
            tracing::warn!(discovery = %summary, "no camera reachable over REST");
            *last = Some(summary);
        }
        None
    }

    fn forget_link(&self, why: &str) {
        if let Some(l) = lock(&self.link).take() {
            tracing::warn!(camera = %l.client.addr, model = %l.model, why, "camera link dropped; it is looked up again on the next read");
        }
    }

    /// Model string of the attached camera (`system/product`), or `None`. Not floored: `/api/detect`
    /// is a rare manual probe, and once a camera is found this reuses it without a request.
    pub fn detect(&self) -> Option<String> {
        self.link().map(|l| l.model)
    }

    /// One real read cycle, no cache: find the camera, GET every endpoint, assemble the state.
    fn read_now(&self) -> Reading {
        let Some(link) = self.link() else {
            return Reading {
                state: self.offline(None),
                basis: None,
            };
        };
        match link.client.read_all() {
            Ok(read) => {
                let (params, caps) = params_and_caps(&read);
                Reading {
                    state: RelayState {
                        online: true,
                        camera: Some(link.model),
                        params,
                        caps: Some(caps),
                        fps_supported: fps_supported(&read),
                        capture_fps: self.capture_fps,
                        version: self.version.clone(),
                        not_applied: Vec::new(),
                    },
                    basis: Some(WriteBasis::from_read(&read)),
                }
            }
            Err(e) => {
                tracing::warn!(camera = %link.client.addr, error = %e, "camera read failed");
                self.forget_link("a read failed");
                Reading {
                    state: self.offline(Some(link.model)),
                    basis: None,
                }
            }
        }
    }

    fn note_online_transition(&self, state: &RelayState) {
        let mut last = lock(&self.last_online);
        if *last != Some(state.online) {
            if state.online {
                tracing::info!(camera = ?state.camera, "camera online");
            } else {
                tracing::info!(camera = ?state.camera, "camera offline");
            }
            *last = Some(state.online);
        }
    }

    /// The camera's live shading state, THROTTLED by the read floor (issue 1229): within
    /// `min_read_interval_ms` of the last real read it is served from the cache with no camera
    /// traffic. Never panics.
    pub fn read_state(&self) -> RelayState {
        let mut cache = lock(&self.read_cache);
        let now_ms = self.clock.now_ms();
        if let Some(c) = cache.as_ref() {
            if read_is_fresh(Some(c.read_at_ms), now_ms, self.min_read_interval_ms) {
                return c.state.clone();
            }
        }
        let reading = self.read_now();
        self.note_online_transition(&reading.state);
        *cache = Some(CachedRead {
            state: reading.state.clone(),
            basis: reading.basis,
            read_at_ms: now_ms,
        });
        reading.state
    }

    /// What a write plans on, and the camera it goes to: the cached read's basis while it is inside
    /// the read floor (no extra camera traffic per click), else one real read, which also refreshes
    /// the cache. `None` = the camera is offline.
    fn write_basis(&self) -> Option<(WriteBasis, Link)> {
        let mut cache = lock(&self.read_cache);
        let now_ms = self.clock.now_ms();
        let cached = cache
            .as_ref()
            .filter(|c| {
                read_is_fresh(
                    Some(c.read_at_ms),
                    now_ms,
                    self.min_read_interval_ms.min(WRITE_BASIS_MAX_AGE_MS),
                )
            })
            .and_then(|c| c.basis.clone());
        let basis = match cached {
            Some(b) => {
                tracing::debug!("write planned on the cached read (inside the read floor)");
                b
            }
            None => {
                let reading = self.read_now();
                self.note_online_transition(&reading.state);
                *cache = Some(CachedRead {
                    state: reading.state,
                    basis: reading.basis.clone(),
                    read_at_ms: now_ms,
                });
                reading.basis?
            }
        };
        drop(cache);
        let link = lock(&self.link).clone()?;
        Some((basis, link))
    }

    /// Submits a shading write through the single-flight FIFO gate (issue 1309/1337) — the entry
    /// point of `PUT /api/params`. With no write in flight this call plans on the cached read (one
    /// real read when it is older than the floor), writes this SET and every SET that queues behind
    /// it IN ORDER, then reads the camera back (after [`APERTURE_READBACK_DELAY_MS`] when the iris
    /// was written) and returns that state with `notApplied` filled. The gate stays shut through
    /// the read-back; a SET that arrived during it is written in a further round. With a write in flight the SET is queued and `Queued`
    /// returned at once.
    pub fn submit(self: &Arc<Self>, req: &SetRequest) -> Result<ApplyOutcome> {
        let current = match lock(&self.set_queue).submit(req.clone()) {
            SubmitAction::Queued { total } => {
                tracing::info!(
                    set = %summarize_set_request(req), queued_total = total,
                    "shading SET queued behind an in-flight write (FIFO, applied in order)"
                );
                return Ok(ApplyOutcome::Queued);
            }
            SubmitAction::RunNow(r) => r,
        };
        self.drain(current, true)
    }

    /// Runs the in-flight drain from `current` (the gate is already shut for it). `own` = this
    /// thread answers a caller whose SET is `current` (its refusal is that caller's answer). Once
    /// the drain has run [`DRAIN_ANSWER_BUDGET_MS`] with SETs still queued (a run of aperture steps
    /// is ~1-5 s each), the caller is answered and the rest is drained IN ORDER on a handoff
    /// thread that keeps the gate shut, so the service's 10 s write client never times out.
    fn drain(self: &Arc<Self>, mut current: SetRequest, own: bool) -> Result<ApplyOutcome> {
        let mut guard = FlightGuard {
            queue: &self.set_queue,
            armed: true,
        };
        let Some((mut basis, link)) = self.write_basis() else {
            guard.disarm_and_abort("the camera is offline");
            *lock(&self.read_cache) = None;
            anyhow::bail!("camera offline: no camera answers its REST API, nothing written");
        };
        let mut before_stop = basis.aperture_stop;
        let drain_started = std::time::Instant::now();
        let over_answer_budget =
            || drain_started.elapsed() >= Duration::from_millis(self.drain_answer_budget_ms);
        let mut log = DrainLog::default();
        let mut count = 0usize;
        let mut own_refusal: Option<anyhow::Error> = None;
        let mut own_set = own;
        let mut not_applied_extra: Vec<String> = Vec::new();
        let state = loop {
            // Write every queued SET, in order. The gate stays shut through the read-back below, so
            // a SET arriving meanwhile queues instead of writing inside this drain's read-back.
            loop {
                match self.apply_one(&link, &mut basis, &current, &mut log) {
                    Ok(()) => {}
                    Err(e) if e.is::<FpsNotSettable>() => {
                        // The CLIENT's error, decided before any write: the camera is untouched,
                        // so the queued follow-ups (other clients' SETs, already acknowledged) run.
                        if own_set {
                            own_refusal = Some(e);
                        }
                    }
                    Err(e) => {
                        guard.disarm_and_abort("a camera write failed");
                        *lock(&self.read_cache) = None;
                        return Err(e);
                    }
                }
                own_set = false;
                if over_answer_budget() {
                    break; // answer now; what is still queued goes to the handoff below
                }
                match lock(&self.set_queue).pop_pending() {
                    Some(next) => {
                        tracing::info!(set = %summarize_set_request(&next), "draining queued shading SET (FIFO, in order)");
                        current = next;
                    }
                    None => break,
                }
            }
            let settle =
                drain_started.elapsed() < Duration::from_millis(self.drain_settle_budget_ms);
            not_applied_extra.append(&mut log.not_applied);
            let (mut state, read_basis) = self.read_back(&log, before_stop, settle);
            for k in &not_applied_extra {
                if state.online && !state.not_applied.contains(k) {
                    state.not_applied.push(k.clone());
                }
            }
            count += log.count;
            let next = lock(&self.set_queue).finish();
            if let (Some(next), None) = (&next, &read_basis) {
                // The camera went away during the read-back: never write a late SET blind through
                // a dropped link. It (and anything queued behind it) is dropped and logged; this
                // caller's own SET was written and gets its answer.
                tracing::warn!(dropped = %summarize_set_request(next), "shading SET dropped: the camera went offline at the read-back");
                guard.disarm_and_abort("the camera went offline at the read-back");
                break state;
            }
            match next {
                Some(next) if over_answer_budget() => {
                    // Hand the rest of the FIFO to a thread that keeps the gate shut and answers
                    // nobody; this caller gets its answer now.
                    guard.armed = false;
                    tracing::info!(
                        next = %summarize_set_request(&next), budget_ms = self.drain_answer_budget_ms,
                        "write drain over its answer budget: the rest of the queue continues in the background"
                    );
                    let me = Arc::clone(self);
                    std::thread::spawn(move || match me.drain(next, false) {
                        Ok(_) => tracing::info!("background write drain finished"),
                        Err(e) => tracing::warn!(error = %e, "background write drain failed"),
                    });
                    break state;
                }
                Some(next) => {
                    // A SET arrived during the read-back: another round, planned on what was read.
                    tracing::info!(set = %summarize_set_request(&next), "shading SET arrived during the read-back; writing it next");
                    current = next;
                    if let Some(b) = read_basis {
                        basis = b;
                    }
                    before_stop = basis.aperture_stop;
                    log = DrainLog::default();
                    not_applied_extra.clear();
                }
                None => {
                    guard.armed = false;
                    break state;
                }
            }
        };
        if let Some(refused) = own_refusal {
            return Err(refused);
        }
        Ok(ApplyOutcome::Applied {
            count,
            state: Box::new(state),
        })
    }

    /// Plans and writes one SET. A frame-rate write moves the basis to the new rate, so a shutter
    /// angle in a later queued SET converts at it.
    fn apply_one(
        &self,
        link: &Link,
        basis: &mut WriteBasis,
        req: &SetRequest,
        log: &mut DrainLog,
    ) -> Result<()> {
        let writes = plan_writes(req, basis).map_err(|e| {
            tracing::warn!(set = %summarize_set_request(req), error = %e, "shading SET refused before any write");
            anyhow::Error::new(e)
        })?;
        tracing::info!(
            set = %summarize_set_request(req), writes = writes.len(), camera = %link.client.addr,
            "shading SET"
        );
        for w in &writes {
            if let Err(e) = link.client.write(w) {
                if matches!(e, RestError::Transport(_)) {
                    self.forget_link("a write got no answer");
                }
                let what = match e {
                    RestError::Transport(_) => "did not answer",
                    _ => "refused",
                };
                anyhow::bail!(
                    "camera {} {what} PUT {}{} {}: {e}",
                    link.client.addr,
                    bkshading_proto::rest::API_PREFIX,
                    w.path(),
                    rest::write_body(w).unwrap_or_default()
                );
            }
            log.count += 1;
            match w {
                RestWrite::FrameRate(label) => {
                    if let Some(f) = bkshading_proto::mapping::parse_frame_rate100(label) {
                        basis.fps100 = f;
                        basis.shutter_angles.clear();
                    }
                }
                RestWrite::ApertureStop(f) => {
                    log.aperture_written = true;
                    basis.aperture_stop = Some(*f);
                }
                _ => {}
            }
        }
        if let Some(dir) = req.aperture_step.filter(|d| *d != 0) {
            let landed = self.step_iris(link, basis, dir, log)?;
            if landed.is_some() {
                basis.aperture_stop = landed;
            }
        }
        merge_written(&mut log.written, req);
        Ok(())
    }

    /// One `apertureStep` press (shading#6, 0.2.1): from the camera's TRUE current f-stop, write a
    /// target [`APERTURE_PROBE_STOPS`] away, wait for the iris, and widen by the same increment
    /// until the iris lands on a different f-stop or the target reaches the lens range. The
    /// increment is below half of two adjacent native gaps, so the first landing is always the
    /// NEXT native step (1/3- or 1/6-stop lens, any zoom). Returns where the iris landed, `None`
    /// when the lens has no further step that way.
    fn step_iris(
        &self,
        link: &Link,
        basis: &WriteBasis,
        dir: i64,
        log: &mut DrainLog,
    ) -> Result<Option<f64>> {
        let range = basis.aperture_range;
        if basis.aperture_controllable == Some(false) {
            tracing::warn!(
                dir,
                "aperture step refused: lens/iris/description says controllable=false"
            );
            log.not_applied.push("apertureStep".into());
            return Ok(None);
        }
        // An absolute iris write earlier in this round is still travelling: let it land first, so
        // the step starts from where the iris IS, not from a value it is passing through.
        if log.aperture_written && self.aperture_readback_delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.aperture_readback_delay_ms));
        }
        let started = std::time::Instant::now();
        let start = self.read_iris(link)?;
        let mut probes = 0u32;
        for k in 1..=MAX_DEAD_APERTURE_PROBES {
            let Some(target) = aperture_probe_target(start, dir, k, range) else {
                tracing::info!(
                    dir,
                    from = start,
                    ?range,
                    probes,
                    duration_ms = started.elapsed().as_millis() as u64,
                    "aperture step: the lens has no further step this way"
                );
                return Ok(None);
            };
            let w = RestWrite::ApertureStop(target);
            if let Err(e) = link.client.write(&w) {
                if matches!(e, RestError::Transport(_)) {
                    self.forget_link("an aperture probe got no answer");
                }
                anyhow::bail!(
                    "camera {} refused the aperture step PUT {}{} {}: {e}",
                    link.client.addr,
                    bkshading_proto::rest::API_PREFIX,
                    w.path(),
                    rest::write_body(&w).unwrap_or_default()
                );
            }
            probes += 1;
            log.count += 1;
            if let Some(landed) = self.wait_iris_move(link, start)? {
                tracing::info!(
                    dir,
                    from = start,
                    to = landed,
                    last_target = target,
                    probes,
                    duration_ms = started.elapsed().as_millis() as u64,
                    "aperture step landed on the next lens step"
                );
                return Ok(Some(landed));
            }
            tracing::debug!(
                dir,
                from = start,
                target,
                probe = k,
                "aperture probe: the iris did not move; widening"
            );
        }
        log.not_applied.push("apertureStep".into());
        tracing::warn!(
            dir,
            from = start,
            probes,
            "aperture step: no move after the probe limit"
        );
        Ok(None)
    }

    /// The iris's true f-stop now (`GET lens/iris`), never a cached value.
    fn read_iris(&self, link: &Link) -> Result<f64> {
        let iris: bkshading_proto::rest::Iris = link
            .client
            .get_json(bkshading_proto::rest::IRIS)
            .map_err(|e| {
            if matches!(e, RestError::Transport(_)) {
                self.forget_link("an iris read got no answer");
            }
            anyhow::anyhow!("camera {} iris read failed: {e}", link.client.addr)
        })?;
        iris.aperture_stop
            .filter(|f| f.is_finite() && *f > 0.0)
            .ok_or_else(|| anyhow::anyhow!("camera {} reports no apertureStop", link.client.addr))
    }

    /// Waits up to `iris_probe_wait_ms` for the iris to leave `from`; once it has, until two reads in
    /// a row agree (it has landed). `None` = it never left `from` in time.
    fn wait_iris_move(&self, link: &Link, from: f64) -> Result<Option<f64>> {
        let poll = Duration::from_millis((self.iris_probe_wait_ms / 4).clamp(5, 100));
        let deadline = std::time::Instant::now() + Duration::from_millis(self.iris_probe_wait_ms);
        let mut last: Option<f64> = None;
        loop {
            std::thread::sleep(poll);
            let now = self.read_iris(link)?;
            let moved = (now - from).abs() > 0.005;
            if moved && last.is_some_and(|l| (l - now).abs() <= 0.005) {
                return Ok(Some(now));
            }
            last = moved.then_some(now);
            // A moving iris gets one more wait window to land; a still one gives up at the deadline.
            let limit = if moved {
                deadline + Duration::from_millis(self.iris_probe_wait_ms)
            } else {
                deadline
            };
            if std::time::Instant::now() >= limit {
                return Ok(last);
            }
        }
    }

    /// The authoritative read after a drain: it replaces the cache and carries `notApplied`.
    fn read_back(
        &self,
        log: &DrainLog,
        before_stop: Option<f64>,
        settle: bool,
    ) -> (RelayState, Option<WriteBasis>) {
        if log.aperture_written && self.aperture_readback_delay_ms > 0 {
            if settle {
                std::thread::sleep(Duration::from_millis(self.aperture_readback_delay_ms));
            } else {
                tracing::info!(
                    budget_ms = self.drain_settle_budget_ms,
                    "write drain over its settle budget: reading back without the iris wait"
                );
            }
        }
        let mut cache = lock(&self.read_cache);
        let reading = self.read_now();
        let mut state = reading.state;
        self.note_online_transition(&state);
        if state.online {
            let grid: Vec<f64> = state
                .caps
                .as_ref()
                .map(|c| c.fnumber_choices.clone())
                .unwrap_or_default();
            let mut not_applied = not_applied_keys(&log.written, &state.params, &grid);
            let after_stop = state.params.aperture_av.map(|av| 2f64.powf(av / 2.0));
            // A write that moved the iris is applied wherever the lens landed (its own step).
            let moved = iris_moved(before_stop, after_stop);
            if log.aperture_written && moved == Some(true) {
                not_applied.retain(|k| k != "apertureNorm");
            }
            if log.aperture_written {
                tracing::info!(
                    before = ?before_stop, after = ?after_stop, moved = ?moved,
                    "aperture read back after the write"
                );
            }
            if !not_applied.is_empty() {
                tracing::warn!(not_applied = ?not_applied, "shading write(s) NOT applied by the camera");
            }
            state.not_applied = not_applied;
        }
        *cache = Some(CachedRead {
            state: state.clone(),
            basis: reading.basis.clone(),
            read_at_ms: self.clock.now_ms(),
        });
        (state, reading.basis)
    }
}

/// Merges every present field of `req` into `acc` — a later SET's value wins — so the read-back
/// is compared with the LAST value asked for each key.
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
    if req.aperture_step.is_some() {
        acc.aperture_step = req.aperture_step;
    }
}
