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

use anyhow::{bail, Context, Result};
use bkshading_proto::mapping::{
    choices_to_norm, fnumber_to_av, norm_to_choice_index, parse_fnumber, parse_fnumber_labels,
    DEFAULT_FPS100,
};
use bkshading_proto::read::{fps_supported, params_and_caps, plan_writes, RawConfigs};
use bkshading_proto::wire::{
    summarize_set_request, RelayState, SetQueue, SetRequest, ShadingParams, SubmitAction,
};

use crate::burst::{
    burst_step, BurstAction, BurstEvent, BurstState, Gphoto2Shell, WRITE_SESSION_IDLE_MS,
};

/// Hard per-gphoto2-command timeout (issue 1309). A gphoto2 that hangs on a busy / unresponsive
/// USB-PTP device would otherwise hold the serialized camera lock (and its child) forever, wedging
/// the relay — the 2026-09-13/15 half-dead-cambox class. After this bound the child is KILLED and
/// reaped, and the command reports an error (a read then degrades to offline, a write returns 502).
pub const GPHOTO2_TIMEOUT: Duration = Duration::from_secs(8);

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

/// Last ~200 chars of a gphoto2 stderr (issue 1309), trimmed, for the error log line. Pure.
pub fn gphoto2_stderr_tail(stderr: &str) -> String {
    let trimmed = stderr.trim();
    let n = trimmed.chars().count();
    if n <= 200 {
        trimmed.to_string()
    } else {
        let tail: String = trimmed.chars().skip(n - 200).collect();
        format!("…{tail}")
    }
}

/// Runs a prepared command with a hard `timeout`, KILLING + reaping the child on overrun (issue
/// 1309). Returns `(output, elapsed, timed_out)`. stdout/stderr are drained on their own threads
/// so a large output can never deadlock the wait; on the deadline the child is `kill()`ed and
/// `wait()`ed (reaped — no zombie/orphan), and the reader threads finish as the pipes close.
fn run_with_timeout(
    mut cmd: std::process::Command,
    timeout: Duration,
) -> std::io::Result<(std::process::Output, Duration, bool)> {
    use std::io::Read;
    use std::process::Stdio;
    let start = Instant::now();
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let th_o = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(s) = so.as_mut() {
            let _ = s.read_to_end(&mut b);
        }
        b
    });
    let th_e = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(s) = se.as_mut() {
            let _ = s.read_to_end(&mut b);
        }
        b
    });
    let mut timed_out = false;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            timed_out = true;
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = th_o.join().unwrap_or_default();
    let stderr = th_e.join().unwrap_or_default();
    Ok((
        std::process::Output {
            status,
            stdout,
            stderr,
        },
        start.elapsed(),
        timed_out,
    ))
}

/// The seam over the `gphoto2` CLI. Blocking (std process); handlers call it via
/// `spawn_blocking`. `Send + Sync` so it can live behind an `Arc` shared across tasks.
pub trait Gphoto2Runner: Send + Sync {
    /// `gphoto2 --auto-detect` stdout.
    fn auto_detect(&self) -> Result<String>;
    /// `gphoto2 --get-config <key>` stdout.
    fn get_config(&self, key: &str) -> Result<String>;
    /// Reads MANY config keys in ONE gphoto2 invocation (issue 1229): the real transport spawns a
    /// single `gphoto2 --get-config k1 --get-config k2 …` process — ONE USB open/enumerate/close
    /// cycle for all keys, instead of one per key — and returns one stdout block per key, in the
    /// order of `keys`. On success the returned `Vec` has EXACTLY `keys.len()` elements; a spawn
    /// failure, a non-zero gphoto2 exit, or a block/key count mismatch is an `Err` (the read then
    /// degrades to offline, the same as a failed single `get_config`).
    ///
    /// The default impl loops `get_config` (one USB session PER key) — correct behaviour, wrong
    /// bus footprint; it exists so test fakes get the read semantics for free. The real
    /// [`Gphoto2Cli`] OVERRIDES it with the single-process batch that this issue is about.
    fn get_config_many(&self, keys: &[&str]) -> Result<Vec<String>> {
        keys.iter().map(|k| self.get_config(k)).collect()
    }
    /// `gphoto2 --set-config <key>=<value>`.
    fn set_config(&self, key: &str, value: &str) -> Result<()>;

    /// Reads the best-effort focus distance (`d003`) AND the camera `--summary` in ONE gphoto2
    /// invocation (issue 1306): `(d003_block, summary_text)`. Folding `--summary` into the existing
    /// best-effort d003 call keeps the per-read USB-PTP session count at 3 (issue 1229 doctrine) —
    /// no new session. The summary carries the raw current aperture (`F-Number(0x5007) … (400)`)
    /// that libgphoto2 omits from the `f-number` RADIO `Current:` when the lens is open below the
    /// camera's first enumerated stop. The default impl (test fakes) reads d003 alone with an empty
    /// summary; the real [`Gphoto2Cli`] OVERRIDES it with the single `--get-config d003 --summary`
    /// process, split at the first `END` line.
    fn get_focus_and_summary(&self) -> Result<(String, String)> {
        Ok((
            self.get_config(FOCUS_DISTANCE_KEY).unwrap_or_default(),
            String::new(),
        ))
    }
}

/// Splits the combined stdout of `gphoto2 --get-config d003 --summary` into `(d003_block,
/// summary_text)` at the FIRST `END` line: everything up to and including that `END` is the d003
/// config block, the remainder is the `--summary` text. Pure + fail-safe — if there is no `END`
/// line (an unexpected shape), the whole output is treated as the d003 block and the summary is
/// empty (aperture then just falls back to the RADIO `Current:` path). Tier-0 testable (issue 1306).
pub fn split_focus_and_summary(combined: &str) -> (String, String) {
    let mut block = String::new();
    let mut lines = combined.lines();
    for line in lines.by_ref() {
        block.push_str(line);
        block.push('\n');
        if line.trim() == "END" {
            let rest: Vec<&str> = lines.collect();
            return (block, rest.join("\n"));
        }
    }
    (block, String::new())
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

impl Gphoto2Cli {
    /// Runs ONE gphoto2 command through the single centralised seam (issue 1309): a hard
    /// timeout+kill ([`run_with_timeout`]) plus ONE structured log line per command — at info on
    /// success (kind/param/value/argv/rc/duration_ms), at error on failure (adds the stderr tail +
    /// whether it timed out). `kind` is `detect`/`read`/`set`; `param` names the key(s); `value`
    /// is the write value (set only). Returns the child's stdout bytes on success, an `Err` on a
    /// non-zero exit or a timeout (the caller degrades a read to offline / returns 502 for a write).
    /// Centralising here is why the fork/kill/log lives in ONE place, not 5 copies, and why the
    /// `Gphoto2Runner` trait (and every fake-runner test) is untouched.
    fn run(&self, kind: &str, param: &str, value: Option<&str>, args: &[&str]) -> Result<Vec<u8>> {
        let mut cmd = std::process::Command::new(&self.binary);
        cmd.args(args);
        let argv = std::iter::once(self.binary.as_str())
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        let (out, elapsed, timed_out) =
            run_with_timeout(cmd, GPHOTO2_TIMEOUT).with_context(|| format!("spawn {argv}"))?;
        let duration_ms = elapsed.as_millis() as u64;
        let rc = out.status.code();
        if out.status.success() && !timed_out {
            tracing::info!(
                kind,
                param,
                value,
                argv = %argv,
                rc = ?rc,
                duration_ms,
                "gphoto2 command"
            );
            Ok(out.stdout)
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let tail = gphoto2_stderr_tail(&stderr);
            tracing::error!(
                kind,
                param,
                value,
                argv = %argv,
                rc = ?rc,
                timed_out,
                duration_ms,
                stderr = %tail,
                "gphoto2 command failed"
            );
            if timed_out {
                bail!("gphoto2 command timed out after {duration_ms} ms and was killed: {argv}");
            }
            bail!("gphoto2 command failed ({argv}): {tail}");
        }
    }
}

impl Gphoto2Runner for Gphoto2Cli {
    fn auto_detect(&self) -> Result<String> {
        let out = self.run("detect", "auto-detect", None, &["--auto-detect"])?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn get_config(&self, key: &str) -> Result<String> {
        let out = self.run("read", key, None, &["--get-config", key])?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn get_config_many(&self, keys: &[&str]) -> Result<Vec<String>> {
        // issue 1229: read every key in ONE gphoto2 process = ONE USB open/enumerate/close on the
        // shared xHCI bus (vs one session PER key with the default `get_config` loop), which is the
        // per-read footprint reduction this issue is about. `--get-config` repeats in the same
        // invocation, so gphoto2 opens the camera once and reads all keys in that one PTP session.
        if keys.is_empty() {
            // An argument-less `gphoto2` would print usage + exit non-zero; keep the pub fn honest.
            return Ok(Vec::new());
        }
        let args = build_get_config_many_args(keys);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.run("read", &keys.join(","), None, &arg_refs)?;
        let stdout = String::from_utf8_lossy(&out);
        // Fail-safe: a block/key count mismatch (a key errored mid-batch, a truncated block) must
        // NOT mis-assign a block to the wrong key — it degrades to a failed read (-> offline), the
        // same as a failed single `get_config`.
        split_config_blocks(&stdout, keys.len()).ok_or_else(|| {
            anyhow::anyhow!(
                "gphoto2 multi --get-config returned an unexpected END-block count (expected {})",
                keys.len()
            )
        })
    }

    fn set_config(&self, key: &str, value: &str) -> Result<()> {
        let kv = format!("{key}={value}");
        self.run("set", key, Some(value), &["--set-config", &kv])?;
        Ok(())
    }

    fn get_focus_and_summary(&self) -> Result<(String, String)> {
        // issue 1306: read d003 AND --summary in ONE gphoto2 process = ONE USB open/enumerate/close
        // on the shared xHCI bus, so the per-read session count stays 3 (issue 1229). Split the
        // combined stdout at the first `END` line: d003 config block, then the summary text.
        let out = self.run(
            "read",
            "d003+summary",
            None,
            &["--get-config", FOCUS_DISTANCE_KEY, "--summary"],
        )?;
        let stdout = String::from_utf8_lossy(&out);
        Ok(split_focus_and_summary(&stdout))
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
/// matches the field order they are assigned to in [`RawConfigs`]. The best-effort `d003` focus
/// distance (issue 1238) is deliberately NOT here — it is read as a separate call so a camera that
/// does not answer it can never abort the core batch (which would wrongly degrade the read to
/// offline). `d001`-style undiscovered keys are not read.
pub const CORE_CONFIG_KEYS: [&str; 7] = ["iso", "f-number", "d002", "d004", "d005", "d006", "d007"];

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

/// The relay's write-burst session state (issue 1337) — behind ONE mutex on the [`CameraSession`].
/// Holds the burst lifecycle [`BurstState`], the persistent `gphoto2 --shell` child while a burst
/// is open, and the plan basis (f-number labels + fps100) captured by the ONE read at burst open so
/// every subsequent write in the burst is planned WITHOUT a pre-write read.
#[derive(Default)]
struct BurstSession {
    state: BurstState,
    shell: Option<Gphoto2Shell>,
    /// `(fnumber_labels, fps100)` read once at burst open; `None` between bursts (re-read next open).
    plan: Option<(Vec<String>, i64)>,
    /// The full [`RelayState`] read at burst open — the base the PUT-response state projects the
    /// burst's writes onto (so the panel gets an immediate confirmation with the camera's real
    /// caps/model). `None` between bursts.
    open_state: Option<RelayState>,
    /// A shell open/write failed this burst ⇒ use the CLI write path for the REST of the burst
    /// (never re-open a broken shell per write). Cleared when the burst idle-closes.
    shell_disabled: bool,
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
            set_queue: Mutex::new(SetQueue::default()),
            burst: Mutex::new(BurstSession::default()),
            gphoto2_binary: String::new(),
        }
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

    fn read_raw(&self) -> Result<RawConfigs> {
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
        let [iso, fnumber, shutter_angle, kelvin, tint, sensor_fps, project_fps]: [String; 7] =
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
            sensor_fps,
            project_fps,
            focus_distance,
            summary,
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
        let mut burst = self
            .burst
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (next_state, action) = burst_step(
            burst.state,
            BurstEvent::IdleCheck { now_ms },
            WRITE_SESSION_IDLE_MS,
        );
        let force_read = match action {
            BurstAction::CloseShellFinalRead => {
                burst.state = next_state;
                if let Some(shell) = burst.shell.take() {
                    shell.close();
                }
                burst.plan = None;
                burst.open_state = None;
                burst.shell_disabled = false;
                tracing::info!("shading write-burst idle-closed; taking one authoritative read");
                true
            }
            _ if matches!(burst.state, BurstState::Open { .. }) => {
                // The shell owns the camera this instant — serve the last snapshot, never a
                // second concurrent gphoto2 read. The burst-open read populated the cache; if a
                // burst somehow opened before any read, a benign offline snapshot is returned.
                return cache
                    .as_ref()
                    .map(|c| c.state.clone())
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
        let state = self.read_state_uncached();
        *cache = Some(CachedRead {
            state: state.clone(),
            read_at_ms: now_ms,
        });
        state
    }

    /// The real (un-throttled) read cycle (issue 1229): `detect()` (one `gphoto2 --auto-detect`) +
    /// `read_raw()` (ONE batched multi `--get-config` for the seven shading keys + one best-effort
    /// `--get-config d003`) = THREE USB-PTP sessions, coalesced down from the pre-fix nine.
    /// A detect miss or a gphoto2 read error degrades to an offline [`RelayState`], the
    /// server-is-truth model. Reached only through [`read_state`](Self::read_state)'s floor.
    fn read_state_uncached(&self) -> RelayState {
        let state = self.compute_state();
        self.note_online_transition(&state);
        state
    }

    /// Builds the current [`RelayState`] from one real read cycle (the old `read_state_uncached`
    /// body), WITHOUT the transition-log side effect (issue 1309 kept the two concerns separate).
    fn compute_state(&self) -> RelayState {
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
        let raw = self.read_raw()?;
        // issue 1304: the write index basis is the PARSEABLE-only f-number list — the SAME basis
        // `params_and_caps` uses for the readback `aperture_norm` and the caps `fnumber_choices`
        // the panel steps over. Using the unfiltered `parse_choices` here would desync the count.
        let fnumber_choices = parse_fnumber_labels(&raw.fnumber);
        let (params, _) = params_and_caps(&raw);
        let fps100 = params.fps100.unwrap_or(DEFAULT_FPS100);
        let writes = plan_writes(req, &fnumber_choices, fps100);
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
        let mut projected: Option<ShadingParams> = None;
        loop {
            match self.burst_apply(&mut burst, &current) {
                Err(e) => {
                    // A CLI-write failure (real camera error): drop the in-flight state AND the
                    // whole queued FIFO — writes planned against a now-uncertain camera must not run
                    // blind. The handler maps this to a 502.
                    guard.disarm_and_abort();
                    return Err(e);
                }
                Ok(applied) => {
                    total_applied += applied;
                    if let Some((labels, _)) = burst.plan.as_ref() {
                        let base = projected
                            .take()
                            .or_else(|| burst.open_state.as_ref().map(|s| s.params.clone()))
                            .unwrap_or_default();
                        projected = Some(project_shading(&base, &current, labels));
                    }
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
            }
        }
        // Build the PUT-response state from the burst-open read with the writes projected onto it —
        // no fresh USB read (the authoritative read happens at burst idle-close). `open_state` is
        // Some in every Ok path (set alongside `plan` at burst open).
        let open = burst.open_state.clone();
        drop(burst);
        let state = match open {
            Some(open) => {
                let params = projected.unwrap_or_else(|| open.params.clone());
                RelayState { params, ..open }
            }
            None => RelayState {
                capture_fps: self.capture_fps,
                ..RelayState::offline(self.version.clone())
            },
        };
        Ok(ApplyOutcome::Applied {
            count: total_applied,
            state: Box::new(state),
        })
    }

    /// Applies ONE shading write inside an open (or opening) burst (issue 1337): plan the writes
    /// from the choices read ONCE at burst open (NO pre-write read), then run them through the
    /// persistent `gphoto2 --shell` — falling any shell error back to a per-invocation CLI write for
    /// the rest of the burst so a broken/slow shell never loses a write and never wedges the relay.
    /// Returns the number of gphoto2 writes performed. Called only from [`submit`] under the burst
    /// lock. A CLI-path write failure is a real camera error and propagates (→ 502).
    fn burst_apply(&self, burst: &mut BurstSession, req: &SetRequest) -> Result<usize> {
        // 1. Plan basis: read the camera ONCE at burst open (detect + core batch + focus/summary =
        //    the normal 3-session read, NOT one per write), caching the f-number choices + fps100
        //    AND the full state to project the response onto. Subsequent writes in the burst pay NO
        //    read. A read failure (absent/busy camera) propagates -> 502, exactly like `apply`.
        if burst.plan.is_none() {
            let camera = self.detect();
            let raw = self.read_raw()?;
            let (params, caps) = params_and_caps(&raw);
            let fps100 = params.fps100.unwrap_or(DEFAULT_FPS100);
            let labels = parse_fnumber_labels(&raw.fnumber);
            let open_state = RelayState {
                online: camera.is_some(),
                camera,
                params,
                caps: Some(caps),
                fps_supported: fps_supported(&raw),
                capture_fps: self.capture_fps,
                version: self.version.clone(),
            };
            burst.plan = Some((labels, fps100));
            burst.open_state = Some(open_state);
        }
        let (labels, fps100) = burst
            .plan
            .clone()
            .expect("burst.plan set immediately above");
        let writes = plan_writes(req, &labels, fps100);
        let n = writes.len();
        // 2. Burst state machine: this SET arrived (opens the burst on the first set).
        let now = self.clock.now_ms();
        let (state_after_set, action) = burst_step(
            burst.state,
            BurstEvent::Set { now_ms: now },
            WRITE_SESSION_IDLE_MS,
        );
        burst.state = state_after_set;
        // 3. Open the persistent shell on the first set of a burst (unless disabled this burst, or
        //    no gphoto2 binary was configured — then the CLI write path is used, still pre-read-free).
        if matches!(action, BurstAction::OpenShellThenWrite)
            && burst.shell.is_none()
            && !burst.shell_disabled
            && !self.gphoto2_binary.is_empty()
        {
            match Gphoto2Shell::open(&self.gphoto2_binary) {
                Ok(sh) => {
                    burst.shell = Some(sh);
                    tracing::info!("shading write-burst shell opened");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "shading write-burst shell open failed; CLI for this burst");
                    burst.shell_disabled = true;
                }
            }
        }
        // 4. Run the writes: shell fast-path, CLI fallback on any shell error.
        for (key, value) in &writes {
            if burst.shell.is_some() {
                let res = burst.shell.as_mut().unwrap().set_config(key, value);
                if let Err(e) = res {
                    tracing::warn!(error = %e, key = %key, "write-burst shell error; kill shell + CLI fallback");
                    if let Some(sh) = burst.shell.take() {
                        sh.close();
                    }
                    burst.shell_disabled = true;
                    self.runner
                        .set_config(key, value)
                        .with_context(|| format!("CLI fallback set-config {key}={value}"))?;
                }
            } else if let Err(e) = self.runner.set_config(key, value) {
                // A CLI-path failure is a real camera error — invalidate the plan basis so the next
                // burst re-reads against the now-uncertain camera, then propagate.
                burst.plan = None;
                burst.open_state = None;
                return Err(e);
            }
        }
        // 5. Writes applied — refresh the burst idle clock.
        let now2 = self.clock.now_ms();
        let (state_after_ok, _) = burst_step(
            burst.state,
            BurstEvent::WriteOk { now_ms: now2 },
            WRITE_SESSION_IDLE_MS,
        );
        burst.state = state_after_ok;
        Ok(n)
    }
}
