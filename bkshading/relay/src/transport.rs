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

use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
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

pub use discover::{camera_links, parse_lease, CameraLink, DiscoverPaths, CAMERA_LINK_DRIVER};
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

/// One cached read (issue 1229): the state and when it was read.
struct CachedRead {
    state: RelayState,
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
            read_at_ms: now_ms,
        });
        reading.state
    }

    /// Submits a shading write through the single-flight FIFO gate (issue 1309/1337) — the entry
    /// point of `PUT /api/params`. With no write in flight this call reads the camera once (the
    /// write basis), writes this SET and every SET that queues behind it IN ORDER, then reads the
    /// camera back (after [`APERTURE_READBACK_DELAY_MS`] when the iris was written) and returns that
    /// state with `notApplied` filled. With a write in flight the SET is queued and `Queued`
    /// returned at once.
    pub fn submit(&self, req: &SetRequest) -> Result<ApplyOutcome> {
        let mut current = match lock(&self.set_queue).submit(req.clone()) {
            SubmitAction::Queued { total } => {
                tracing::info!(
                    set = %summarize_set_request(req), queued_total = total,
                    "shading SET queued behind an in-flight write (FIFO, applied in order)"
                );
                return Ok(ApplyOutcome::Queued);
            }
            SubmitAction::RunNow(r) => r,
        };
        let mut guard = FlightGuard {
            queue: &self.set_queue,
            armed: true,
        };
        let opened = self.read_now();
        let (Some(mut basis), Some(link)) = (opened.basis, lock(&self.link).clone()) else {
            guard.disarm_and_abort("the camera is offline");
            *lock(&self.read_cache) = None;
            anyhow::bail!("camera offline: no camera answers its REST API, nothing written");
        };
        let before_stop = basis.aperture_stop;
        let mut log = DrainLog::default();
        let mut own_refusal: Option<anyhow::Error> = None;
        let mut own_set = true;
        loop {
            match self.apply_one(&link, &mut basis, &current, &mut log) {
                Ok(()) => {}
                Err(e) if e.is::<FpsNotSettable>() => {
                    // The CLIENT's error, decided before any write: the camera is untouched, so the
                    // queued follow-ups (other clients' SETs, already acknowledged) still run.
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
            match lock(&self.set_queue).finish() {
                Some(next) => {
                    tracing::info!(set = %summarize_set_request(&next), "draining queued shading SET (FIFO, in order)");
                    current = next;
                }
                None => {
                    guard.armed = false;
                    break;
                }
            }
        }
        let state = self.read_back(&log, before_stop);
        if let Some(refused) = own_refusal {
            return Err(refused);
        }
        Ok(ApplyOutcome::Applied {
            count: log.count,
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
                anyhow::bail!(
                    "camera {} refused PUT {}{} {}: {e}",
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
        merge_written(&mut log.written, req);
        Ok(())
    }

    /// The authoritative read after a drain: it replaces the cache and carries `notApplied`.
    fn read_back(&self, log: &DrainLog, before_stop: Option<f64>) -> RelayState {
        if log.aperture_written && self.aperture_readback_delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.aperture_readback_delay_ms));
        }
        let mut cache = lock(&self.read_cache);
        let mut state = self.read_now().state;
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
            read_at_ms: self.clock.now_ms(),
        });
        state
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
}
