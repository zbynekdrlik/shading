//! Shared wire types for the bkshading shading-control protocol.
//!
//! One source of truth for the JSON exchanged between the browser web panel, the
//! `bkshading` aggregation service, and each `bkshading-relay`. Keeping these types in
//! `bkshading-proto` (depended on by BOTH the service and the relay) prevents the exact
//! "duplicate truth" drift the owner flagged in the MVP (Python `mapping.py` vs Kotlin
//! `PtpMapping.kt`). Field names use `camelCase` to stay compatible with the verified
//! dev2 MVP web-UI wire shape (`apertureAv`/`apertureNorm`/`iso`/...).

use serde::{Deserialize, Serialize};

/// How a camera's shading is reached (owner decision 2026-08-20: transports are USB via
/// a relay, or USB-Ethernet REST; Bluetooth is not used at all).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    /// Camera USB -> cambox PC, controlled by a `bkshading-relay` on that cambox.
    CamboxRelay,
    /// Camera USB -> a separately powered zero-class arm64 SBC with WiFi on the cage
    /// (device-agnostic: Pi Zero 2 W / Radxa ZERO 3W / Orange Pi Zero 2W), running the
    /// SAME relay — a "mini-cambox without video". Handheld path.
    SbcRelay,
    /// Camera in REST mode via a USB-C->Ethernet adapter (Camera OS >= 8.6). Future
    /// alternative transport; the service treats it as another relay-shaped endpoint.
    EthernetRest,
}

/// Live shading state of a single camera. `None` fields mean "not yet known this poll
/// cycle" (never "zero") — the server is the single source of truth.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShadingParams {
    /// Aperture value: `AV = 2*log2(fNumber)`.
    pub aperture_av: Option<f64>,
    /// Aperture normalised into `[0,1]` across the camera's f-number choices.
    pub aperture_norm: Option<f64>,
    pub iso: Option<i64>,
    pub kelvin: Option<i64>,
    pub tint: Option<i64>,
    /// Shutter speed as a denominator (e.g. `500` == 1/500 s).
    pub shutter: Option<i64>,
    /// PROJECT fps x100 — `system/format frameRate` (shading#6; the d006 MENU under PTP, issue
    /// 1402). The rate the camera records and outputs; it drives the shutter angle <->
    /// denominator conversion and the issue-809 comparison with the grab. Settable only to one
    /// of [`CameraCaps::fps_choices`]. `None` when the format is not read — never the off-speed
    /// rate.
    pub fps100: Option<i64>,
    /// OFF-SPEED (sensor) fps x100 — `system/format offSpeedFrameRate` x100. Readback /
    /// diagnostic only: never compared with the grab, never written.
    pub sensor_fps100: Option<i64>,
    /// Manual focus DISTANCE as the raw PTP `d003` (issue 1238). The REST relay (shading#6) does
    /// not report it: REST's `lens/focus` gives a normalised position, which is
    /// [`ShadingParams::focus_norm`]. Kept so an older relay still deserializes; `None` from a
    /// REST relay. `#[serde(default)]`.
    #[serde(default)]
    pub focus_distance: Option<i64>,
    /// Lens focus position normalised to `[0,1]` — `lens/focus normalised` (shading#6). READ-ONLY
    /// (never in [`SetRequest`]: a focus write during a take is unsafe, issue 1238). Its presence
    /// confirms the lens reports focus; a value STABLE across reads spaced beyond the relay's read
    /// floor is a no-AF-hunt proxy. It is not a focus MODE. `None` when the lens does not answer.
    /// `#[serde(default)]` so an older relay still deserializes.
    #[serde(default)]
    pub focus_norm: Option<f64>,
}

/// The camera's own fine-grained value lists/ranges — the web UI rebuilds its ISO and
/// shutter button groups from these when present.
///
/// Note: no `Eq` derive — [`CameraCaps::fnumber_choices`] is `Vec<f64>` and `f64` has no
/// total order, so `Eq` is impossible. `PartialEq` is enough (and is all the whole-state
/// `assert_eq!` round-trip tests need — `Eq` is never required as a bound anywhere).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraCaps {
    pub iso_choices: Vec<i64>,
    /// The camera's f-number choices as plain numbers (e.g. `[2.8, 4.0, 5.2, 8.0]`), ordered
    /// like the RADIO `f-number` choices the relay reads. The panel uses the COUNT for the
    /// aperture +/- step (`idx = round(norm*(n-1))`, step to `clamp(idx±1)`, send the absolute
    /// `apertureNorm = idx'/(n-1)` — the inverse of [`crate::mapping::norm_to_choice_index`],
    /// which the relay applies to the SAME choice list) and the VALUES to label the target
    /// f-number (issue 1304). `#[serde(default)]` so an older relay that does not send it still
    /// deserializes (empty `Vec`) — the panel then DISABLES the aperture +/- buttons rather
    /// than fabricating a step.
    // `rename_all = "camelCase"` would emit `fnumberChoices` (one word); the panel (app.js) and
    // the E2E stub relay speak `fNumberChoices`, so pin the wire key explicitly.
    #[serde(default, rename = "fNumberChoices")]
    pub fnumber_choices: Vec<f64>,
    pub shutter_choices: Vec<i64>,
    /// The camera's PROJECT-fps choices x100 — the frame rates `system/supportedFormats` lists
    /// for the current codec and resolution, ascending (shading#6; under PTP the d006 MENU, issue
    /// 1402). A
    /// `fps` write is accepted only for one of these ([`crate::mapping::fps_settable`]); the
    /// service offers "align to grab" only when `grab * 100` is one of them.
    /// `#[serde(default)]`: an older relay that does not send it reads as empty, so nothing is
    /// settable and no align is offered (an old PTP relay wrote `fps` to the off-speed d007).
    #[serde(default)]
    pub fps_choices: Vec<i64>,
    /// The OFF-SPEED range (`system/format min/maxOffSpeedFrameRate`) — not the settable project rates (those are
    /// [`CameraCaps::fps_choices`]).
    pub fps_min: i64,
    pub fps_max: i64,
    pub kelvin_min: i64,
    pub kelvin_max: i64,
}

/// A relay's report about the one camera it owns. Served at the relay's `GET /api/state`
/// and forwarded by the service into each web-UI camera block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayState {
    /// Camera found and answering its REST API this cycle (shading#6).
    pub online: bool,
    /// Camera model string (`system/product productName`), when known.
    pub camera: Option<String>,
    pub params: ShadingParams,
    pub caps: Option<CameraCaps>,
    /// Whether the camera exposes its PROJECT fps (`system/format`, issue 1402). Exposed is not
    /// "settable to any rate": a `fps` write is accepted only for a value in
    /// [`CameraCaps::fps_choices`].
    pub fps_supported: bool,
    /// The box's own capture-mode fps, reported by the relay from its `CAMERA_BOX_CAPTURE_FPS`
    /// environment (the relay runs on the cambox, so it reads the SAME rate the appliance
    /// requests). `None` when the relay's environment does not set it (issue 809). This is the
    /// box's ACTUAL grab rate — the service derives/validates the per-camera `grab_fps` against
    /// it so a stale static config can't silently mis-compare. `#[serde(default)]` so an older
    /// relay that doesn't send it still deserializes (as `None`).
    #[serde(default)]
    pub capture_fps: Option<i64>,
    /// The relay binary's own version (for diagnostics; the service shows its own).
    pub version: String,
    /// The WIRE field names (`apertureNorm`, `iso`, `shutter`, `kelvin`, `tint`, `fps`) whose
    /// write the camera did NOT apply, filled after a write by comparing every key it wrote
    /// against the read-back that follows it (issue 1343, shading#6). Empty after a plain read,
    /// or when every write landed. The panel renders each listed value with a `.not-applied`
    /// style so a silently-refused write is visible instead of the optimistic value just
    /// reverting. Cleared on the next real read. `#[serde(default)]` so
    /// an older relay that does not send it still deserializes (empty `Vec`), mirroring
    /// [`RelayState::capture_fps`]; `rename_all = "camelCase"` emits it as `notApplied`.
    #[serde(default)]
    pub not_applied: Vec<String>,
}

impl RelayState {
    /// The state a relay reports when it cannot see its camera this cycle. `capture_fps` is
    /// `None` here (the box rate is filled in by the relay, which overrides this default with
    /// its known env value even on a camera-offline cycle).
    pub fn offline(version: impl Into<String>) -> Self {
        RelayState {
            online: false,
            camera: None,
            params: ShadingParams::default(),
            caps: None,
            fps_supported: false,
            capture_fps: None,
            version: version.into(),
            not_applied: Vec::new(),
        }
    }
}

/// A write request from the web panel — every field optional; only present fields are
/// applied. Aperture is set as a normalised `[0,1]` position (the panel's slider), the
/// relay maps it back to an f-number choice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetRequest {
    pub aperture_norm: Option<f64>,
    pub iso: Option<i64>,
    pub kelvin: Option<i64>,
    pub tint: Option<i64>,
    /// Shutter denominator (e.g. `500` == 1/500 s).
    pub shutter: Option<i64>,
    /// PROJECT fps, plain (e.g. `50`). Written as the camera's own frame rate, only when `fps * 100` is
    /// one of the camera's [`CameraCaps::fps_choices`]; any other value refuses the whole
    /// request (issue 1402). Never rounded to a neighbour.
    pub fps: Option<i64>,
    /// `true` runs the camera's auto white balance (`video/whiteBalance/doAuto`, shading#6).
    pub auto_wb: Option<bool>,
}

/// Whether a camera's reported frame rate matches the box's grab mode (issue 809). A
/// mismatch produces duplicate/beat artefacts at the source of the capture chain (#674
/// duplicate-frame analysis, #685 capture-rate health), so the panel surfaces it as a
/// visible warning and offers an explicit "align to grab" action (never an auto-write —
/// a camera-side format change can interrupt recording).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FpsSync {
    /// Can't judge: the camera's fps isn't known this cycle (offline / not read) or no
    /// grab mode is configured for this camera.
    #[default]
    Unknown,
    /// The camera's project fps equals the box's grab fps — no beat.
    Synced,
    /// The camera's project fps differs from the box's grab fps — the warning state.
    Mismatch,
}

impl FpsSync {
    /// Classifies a camera's reported project fps (`ShadingParams.fps100`, i.e. fps x100)
    /// against the box's configured grab fps (plain fps, e.g. `60`). Pure — the single
    /// source of truth for the sync verdict, shared by the service and covered by unit
    /// tests. Compares the PROJECT fps (d006, issue 1402), because that is exactly what the
    /// "align to grab" write changes and what the camera's HDMI output follows;
    /// `sensor_fps100` (off-speed d007) stays a separate diagnostic. A non-positive value on
    /// either side (an unread property, a misconfigured `grab_fps = 0`) is treated as "not
    /// known" and yields [`FpsSync::Unknown`] rather than a false mismatch.
    pub fn classify(camera_fps100: Option<i64>, grab_fps: Option<i64>) -> FpsSync {
        match (camera_fps100, grab_fps) {
            (Some(cam), Some(grab)) if cam > 0 && grab > 0 => {
                if cam == grab * 100 {
                    FpsSync::Synced
                } else {
                    FpsSync::Mismatch
                }
            }
            _ => FpsSync::Unknown,
        }
    }
}

/// One camera as the aggregation service presents it to the web panel. A camera with no
/// NDI preview (a handheld, or M1 where preview is not wired yet) has `has_preview =
/// false` and the panel renders a params-only block (no preview area).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraView {
    pub id: String,
    pub label: String,
    pub transport: Transport,
    /// The camera has an NDI preview CONFIGURED (`ndi_preview` set): the panel renders a preview
    /// block for it. Says nothing about whether a picture is arriving — that is `preview_live`.
    pub has_preview: bool,
    /// The preview has a FRESH frame right now (issue 808): the service's preview store holds a
    /// frame younger than the freshness bound. The panel loads a frame only while this is set and
    /// shows its placeholder otherwise, so an absent or stopped feed is never polled into console
    /// errors and never shown frozen as if live. Always `false` without `has_preview`.
    /// `#[serde(default)]`: a view from an older service that does not send it reads as not live.
    #[serde(default)]
    pub preview_live: bool,
    /// The camera's relay was reachable this cycle.
    pub reachable: bool,
    /// The EFFECTIVE grab-mode fps this camera is compared against (issue 809) — the box's live
    /// reported capture rate when the relay reports one ([`resolve_grab`] DERIVES it from the
    /// actual capture mode), else the static per-camera config value. `None` when neither is
    /// known — then `fps_sync` is [`FpsSync::Unknown`] and the panel shows no grab comparison.
    pub grab_fps: Option<i64>,
    /// The static config `grab_fps` disagrees with the box's live reported capture rate (issue
    /// 809): a silent desync (e.g. the box's capture mode changed but the TOML wasn't updated).
    /// The panel then compares against the LIVE rate (`grab_fps` above) and surfaces this so the
    /// stale config gets fixed. `#[serde(default)]` for older-service wire compat.
    #[serde(default)]
    pub grab_fps_desync: bool,
    /// FPS-vs-grab sync verdict (issue 809) — [`FpsSync::classify`] of the camera's
    /// reported project fps against the EFFECTIVE `grab_fps`. Drives the panel's warning +
    /// align button.
    pub fps_sync: FpsSync,
    /// The explicit "align to grab" write can be offered (issue 1402): the camera reports its
    /// d006 project-fps choices and `grab_fps * 100` is EXACTLY one of them
    /// ([`crate::mapping::fps_settable`], the same rule the relay's write refusal uses). False
    /// with no grab, no caps, or a grab the camera does not list (a 60 grab on a BMPCC: `6000`
    /// is not a d006 choice) — the relay would refuse that write, so the panel offers no button.
    /// `#[serde(default)]`: a view from an older service reads as false (no button).
    #[serde(default)]
    pub fps_align_settable: bool,
    /// Live state from the relay, when reachable.
    pub state: Option<RelayState>,
    /// The self-registration of a `register:<id>` camera (issue 808): the relay id, the address
    /// its last announcement came from, how long ago that was and how long the relay has announced
    /// without a gap. `None` for a camera at a fixed address, or one whose relay never announced.
    /// `#[serde(default)]`: a view from an older service reads as `None`.
    #[serde(default)]
    pub registration: Option<RegistrationView>,
}

/// A self-registering relay as the panel shows it (issue 808).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationView {
    /// The relay id the camera config names (`register:<id>`).
    pub relay_id: String,
    /// The `host:port` the relay's last announcement came from.
    pub address: String,
    /// Seconds since that announcement.
    pub age_s: u64,
    /// Seconds the relay has announced without a gap longer than the registration TTL.
    pub announcing_s: u64,
    /// The announcement is within the TTL: the service reaches the relay at `address`.
    pub fresh: bool,
}

/// Reconciliation of a camera's configured grab fps with the box's live reported capture rate
/// (issue 809). See [`resolve_grab`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrabResolution {
    /// The grab fps to compare the camera against: the box's live reported capture rate when
    /// known (the ACTUAL capture mode), otherwise the static config value.
    pub effective: Option<i64>,
    /// The static config grab fps disagrees with the box's live reported capture rate — a
    /// silent desync that would otherwise mis-compare against a stale reference.
    pub desync: bool,
}

/// Reconciles a per-camera configured grab fps with the box's live reported capture rate
/// (`RelayState::capture_fps`), for issue 809. DERIVES the effective grab from the box's ACTUAL
/// capture mode when the relay reports it (a box-side mode change is then followed automatically
/// instead of silently desyncing a static config), falling back to the config when the relay
/// reports nothing. VALIDATES the two against each other: when both are known and differ,
/// `desync = true` and the LIVE capture rate wins (it is the real grab). A non-positive value on
/// either side is treated as "not known" (never a false desync).
pub fn resolve_grab(config_grab: Option<i64>, reported_capture_fps: Option<i64>) -> GrabResolution {
    let config = config_grab.filter(|&g| g > 0);
    let reported = reported_capture_fps.filter(|&c| c > 0);
    match (config, reported) {
        (Some(cfg), Some(cap)) => GrabResolution {
            effective: Some(cap),
            desync: cfg != cap,
        },
        (None, Some(cap)) => GrabResolution {
            effective: Some(cap),
            desync: false,
        },
        (Some(cfg), None) => GrabResolution {
            effective: Some(cfg),
            desync: false,
        },
        (None, None) => GrabResolution {
            effective: None,
            desync: false,
        },
    }
}

/// The one-line telemetry note the service logs when a camera's project fps does NOT match the
/// box's grab mode (issue 809). Names the concrete rates AND cross-references the appliance's
/// grabber-side capture-rate health (`capture_rate_health`, issues 656/685 — captured-vs-
/// negotiated rate on `/dev/videoN`) plus the duplicate-frame analysis (issue 674): a camera
/// off-grab is the SAME source beat those downstream checks see, so aligning the camera fps to
/// the grab removes it at the source. Pure so the exact wording is pinned by a test.
pub fn fps_mismatch_note(id: &str, camera_fps100: Option<i64>, grab_fps: Option<i64>) -> String {
    let cam = camera_fps100
        .map(|f| format!("{:.2}", f as f64 / 100.0))
        .unwrap_or_else(|| "?".into());
    let grab = grab_fps
        .map(|g| g.to_string())
        .unwrap_or_else(|| "?".into());
    format!(
        "issue-809 fps mismatch on '{id}': camera project fps {cam} != box grab {grab} \
         — source beat/duplicate class (cross-ref capture_rate_health 656/685 grabber rate, \
         674 duplicate-frame analysis); align camera fps to grab to remove it at the source"
    )
}

/// The one-line telemetry note logged when the static per-camera `grab_fps` config disagrees
/// with the box's live reported capture rate (issue 809) — the "mode change silently desynced
/// the config" case. `effective_grab` is the LIVE capture rate now being used.
pub fn grab_desync_note(id: &str, effective_grab: Option<i64>) -> String {
    let g = effective_grab
        .map(|g| g.to_string())
        .unwrap_or_else(|| "?".into());
    format!(
        "issue-809 grab config desync on '{id}': box live capture rate is {g} fps but the \
         static grab_fps config differs — comparing against the live rate; update grab_fps in \
         the TOML to match the box's capture mode"
    )
}

/// The whole aggregate the service serves at `GET /api/cameras`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Aggregate {
    /// The service's own version (shown in the panel header — version-on-dashboard).
    pub version: String,
    pub cameras: Vec<CameraView>,
}

/// A server -> client WebSocket message for the issue-808 live state push (`/ws`).
///
/// Internally tagged (`#[serde(tag = "type")]`), so the payload flattens the aggregate's
/// own fields alongside a `type` discriminator — the exact `{"type":"state", ...}` shape the
/// verified dev2 MVP web UI already speaks. That lets the browser reuse its existing
/// `render(agg)` directly on the flattened message (`msg.version` / `msg.cameras`) instead of
/// unwrapping a nested object. Push-only: the panel still WRITES over the HTTP
/// `PUT /api/cameras/:id/params` surface (robust through the cloudflare remote), so no
/// client->server variant is needed yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ServerMsg {
    /// The full aggregate, pushed on connect and again whenever it changes.
    State(Aggregate),
}

// --- issue 1309 / 1337: shading-write single-flight FIFO queue + a shared SET summary -----------

/// One-line human summary of the non-`None` params in a SET (issue 1309), for the relay + service
/// logs (so a "crash after two SETs" is reconstructible from the log alone). Pure — unit-tested.
pub fn summarize_set_request(req: &SetRequest) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(v) = req.aperture_norm {
        parts.push(format!("aperture_norm={v}"));
    }
    if let Some(v) = req.iso {
        parts.push(format!("iso={v}"));
    }
    if let Some(v) = req.kelvin {
        parts.push(format!("kelvin={v}"));
    }
    if let Some(v) = req.tint {
        parts.push(format!("tint={v}"));
    }
    if let Some(v) = req.shutter {
        parts.push(format!("shutter={v}"));
    }
    if let Some(v) = req.fps {
        parts.push(format!("fps={v}"));
    }
    if let Some(v) = req.auto_wb {
        parts.push(format!("auto_wb={v}"));
    }
    if parts.is_empty() {
        "(none)".to_string()
    } else {
        parts.join(" ")
    }
}

/// The action a [`SetQueue`] decided for an incoming SET (issue 1309/1337).
#[derive(Debug, Clone, PartialEq)]
pub enum SubmitAction {
    /// No write is in flight — run THIS request now (the caller then drives the camera and, while it
    /// runs, drains any SETs that queued behind it, in ORDER).
    RunNow(SetRequest),
    /// A write is already in flight — this request was appended to the FIFO queue and will be
    /// applied IN ORDER by the in-flight worker. `total` is the running count of queued SETs.
    Queued { total: u64 },
}

/// Single-flight FIFO queue for shading writes (issue 1309 single-flight; issue 1337 FIFO). The
/// relay wraps ONE of these in a `Mutex`: the FIRST SET flips `in_flight` and writes to the camera; any SET
/// arriving while a write is in flight is APPENDED to `pending` (never a second parallel camera write —
/// the issue-1309 guarantee); when the in-flight worker finishes it pops the FRONT of
/// `pending` and runs it, else clears `in_flight`. FIFO (not the former latest-wins coalesce) so a
/// burst of 20 rapid clicks becomes 20 in-order camera moves — the owner's issue-1337 requirement.
/// Pure state machine — the camera I/O lives in the relay; this is exhaustively rustc-testable.
#[derive(Debug, Default, Clone)]
pub struct SetQueue {
    in_flight: bool,
    pending: std::collections::VecDeque<SetRequest>,
    queued_total: u64,
}

impl SetQueue {
    /// Decide what to do with an incoming SET. `RunNow` when idle (marks in-flight); `Queued`
    /// (appended to the FIFO tail) when a write is already running.
    pub fn submit(&mut self, req: SetRequest) -> SubmitAction {
        if self.in_flight {
            self.pending.push_back(req);
            self.queued_total += 1;
            SubmitAction::Queued {
                total: self.queued_total,
            }
        } else {
            self.in_flight = true;
            SubmitAction::RunNow(req)
        }
    }

    /// Called by the in-flight worker after each write completes: `Some(next)` = a queued
    /// request is next (FIFO front), stay in-flight and run it; `None` = queue empty, clear
    /// in-flight (idle again).
    pub fn finish(&mut self) -> Option<SetRequest> {
        match self.pending.pop_front() {
            Some(next) => Some(next),
            None => {
                self.in_flight = false;
                None
            }
        }
    }

    /// The next queued SET, FIFO, WITHOUT ending the flight when the queue is empty (shading#6): the
    /// relay pops with this while it writes, keeps the gate shut through its read-back, and only
    /// then calls [`SetQueue::finish`] — so a SET that arrives during the read-back queues instead
    /// of starting a second drain whose writes would land inside the first one's read-back.
    pub fn pop_pending(&mut self) -> Option<SetRequest> {
        self.pending.pop_front()
    }

    /// Called on a write error (or an unwind guard): drop the in-flight state AND the WHOLE queued
    /// FIFO (writes planned against a now-uncertain camera must not run blind), returning every
    /// dropped follow-up so the caller can log the loss — those SETs were already acknowledged to
    /// the client. The next SET starts fresh. Idempotent: a second `abort()` returns an empty Vec.
    pub fn abort(&mut self) -> Vec<SetRequest> {
        self.in_flight = false;
        self.pending.drain(..).collect()
    }

    /// Total number of SETs queued over this queue's lifetime (for diagnostics).
    pub fn queued_total(&self) -> u64 {
        self.queued_total
    }
}
