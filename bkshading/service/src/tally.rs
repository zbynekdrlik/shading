//! PGM/PVW tally from the strih tally service (shading#25).
//!
//! camera-box's tally service (camera-box#1417, `strih-tally.service`) follows the strih OBS and
//! answers `GET http://127.0.0.1:9240/state` with `{"pgm": N, "pvw": N, "obs_connected": bool,
//! ...}`; N is a camera NUMBER (0 = no single camera), so `camN` <- N. It is the ONE tally rule
//! (`strih_tally_decision.py`): this service never reads OBS itself.
//!
//! Structure: the pure half ([`TallyState`] parsing, [`apply_tally`], the [`TallyTracker`] state
//! machine that logs once per transition) and one tokio task ([`run`]) that polls the configured
//! url every [`TALLY_POLL_INTERVAL`] with reqwest. On a change the task stores the tally in the
//! [`Aggregator`] (so every pump snapshot and `GET /api/cameras` carries it) and republishes the
//! live aggregate at once. No `[tally] url` = off: no task, no error, no log spam.

use std::sync::Arc;
use std::time::Duration;

use bkshading_proto::wire::{Aggregate, Tally};
use serde::Deserialize;
use tokio::sync::watch;

use crate::aggregator::Aggregator;

/// How often the tally service is polled. The ticket asks for a tint within 1 s of a cut; the
/// tally service itself follows OBS within one event round-trip (< 100 ms).
pub const TALLY_POLL_INTERVAL: Duration = Duration::from_millis(300);

/// One poll's total budget (a local service: a slower answer is a fault, and the tally clears).
pub const TALLY_POLL_TIMEOUT: Duration = Duration::from_millis(1000);

/// The optional `[tally]` config table. `url` absent or empty = tally off.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TallyConfig {
    #[serde(default)]
    pub url: Option<String>,
}

impl TallyConfig {
    /// The url to poll, or `None` when the tally is off.
    pub fn poll_url(&self) -> Option<&str> {
        self.url.as_deref().map(str::trim).filter(|u| !u.is_empty())
    }
}

/// The part of the tally service's `/state` the panel needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TallyState {
    /// The camera number in program, 0 = none.
    pub pgm: u64,
    /// The camera number in preview, 0 = none.
    pub pvw: u64,
    /// Whether the tally service is connected to OBS; false = it knows nothing (reads 0/0).
    pub obs_connected: bool,
}

/// The `/state` body as sent: unknown fields are ignored; a `pgm`/`pvw` is read as any JSON value
/// ([`tally_value`]), so a missing, `null`, negative or unreadable one means "no single camera"
/// (0) and one odd value never fails the body (a failed poll would clear a live tally).
/// `obs_connected` is required: without it the service cannot tell whether the numbers mean
/// anything.
#[derive(Deserialize)]
struct RawState {
    #[serde(default)]
    pgm: serde_json::Value,
    #[serde(default)]
    pvw: serde_json::Value,
    obs_connected: bool,
}

/// A tally number from any JSON value: a non-negative whole number, given as a number (`3`,
/// `3.0`) or a numeric string (`"3"`, `" 3.0 "`); anything else (null, a fraction, text, an
/// array) is 0, "no single camera".
pub fn tally_value(v: &serde_json::Value) -> u64 {
    let whole = |f: f64| {
        if f.is_finite() && f >= 0.0 && f.fract() == 0.0 && f <= u64::MAX as f64 {
            f as u64
        } else {
            0
        }
    };
    match v {
        serde_json::Value::Number(n) => n.as_u64().unwrap_or_else(|| n.as_f64().map_or(0, whole)),
        serde_json::Value::String(s) => {
            let s = s.trim();
            s.parse::<u64>()
                .unwrap_or_else(|_| s.parse::<f64>().map_or(0, whole))
        }
        _ => 0,
    }
}

/// Parses a `/state` body.
pub fn parse_tally(body: &str) -> Result<TallyState, String> {
    let raw: RawState = serde_json::from_str(body).map_err(|e| format!("bad /state body: {e}"))?;
    Ok(TallyState {
        pgm: tally_value(&raw.pgm),
        pvw: tally_value(&raw.pvw),
        obs_connected: raw.obs_connected,
    })
}

/// The camera number a tally number refers to: exactly `cam<N>` (a handheld or any other id has
/// no tally number).
pub fn tally_number(id: &str) -> Option<u64> {
    let digits = id.strip_prefix("cam")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The tally of camera `id` under `tally` (`None` = no tally known: off, an error, OBS away).
pub fn camera_tally(id: &str, tally: Option<&TallyState>) -> Option<Tally> {
    let t = tally.filter(|t| t.obs_connected)?;
    let n = tally_number(id)?;
    if n == 0 {
        None
    } else if t.pgm == n {
        // A camera in program AND preview is live: program wins.
        Some(Tally::Pgm)
    } else if t.pvw == n {
        Some(Tally::Pvw)
    } else {
        None
    }
}

/// `agg` with every camera's `tally` set from `tally` (pure; used by the tally task and by every
/// snapshot, so no push ever drops it).
pub fn apply_tally(agg: &Aggregate, tally: Option<&TallyState>) -> Aggregate {
    let mut out = agg.clone();
    for cam in &mut out.cameras {
        cam.tally = camera_tally(&cam.id, tally);
    }
    out
}

/// The tally source's health, for one log line per transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Health {
    Live,
    ObsAway,
    Failed,
}

/// What one poll changed: the tally to store/publish (when it changed) and the log lines.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TallyStep {
    /// `Some(new)` when the effective tally changed (`new` = `None` clears it).
    pub changed: Option<Option<TallyState>>,
    /// Info lines (a cut, a recovery).
    pub info: Vec<String>,
    /// Warning lines (OBS away, the service failing), once per transition.
    pub warn: Vec<String>,
}

/// The poll loop's state machine (pure): the effective tally and the source's health.
#[derive(Debug, Default)]
pub struct TallyTracker {
    health: Option<Health>,
    current: Option<TallyState>,
}

impl TallyTracker {
    /// Folds one poll outcome in: an OK body while OBS is connected is the tally; OBS away or an
    /// error clears it. Health changes are logged once; a changed tally is reported.
    pub fn observe(&mut self, outcome: Result<TallyState, String>) -> TallyStep {
        let mut step = TallyStep::default();
        let (health, effective) = match &outcome {
            Ok(t) if t.obs_connected => (Health::Live, Some(*t)),
            Ok(_) => (Health::ObsAway, None),
            Err(_) => (Health::Failed, None),
        };
        if self.health != Some(health) {
            match (health, &outcome) {
                (Health::Live, _) => step.info.push("tally: live (OBS connected)".to_string()),
                (Health::ObsAway, _) => step
                    .warn
                    .push("tally: the tally service has no OBS connection; tally cleared".into()),
                (Health::Failed, Err(e)) => step
                    .warn
                    .push(format!("tally: poll failed ({e}); tally cleared")),
                (Health::Failed, Ok(_)) => {}
            }
            self.health = Some(health);
        }
        if effective != self.current {
            if let Some(t) = &effective {
                step.info
                    .push(format!("tally: pgm {} pvw {}", t.pgm, t.pvw));
            }
            self.current = effective;
            step.changed = Some(effective);
        }
        step
    }
}

/// One poll of the tally service.
async fn poll(client: &reqwest::Client, url: &str) -> Result<TallyState, String> {
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    let body = resp.text().await.map_err(|e| e.to_string())?;
    parse_tally(&body)
}

/// The tally task: polls `url` every `interval` forever; on a change stores the tally in `agg`
/// and republishes the live aggregate with it. Never returns while the service runs.
pub async fn run(
    url: String,
    interval: Duration,
    agg: Arc<Aggregator>,
    live_tx: Arc<watch::Sender<Arc<Aggregate>>>,
) {
    let client = match reqwest::Client::builder()
        .timeout(TALLY_POLL_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "tally: cannot build the HTTP client; tally off");
            return;
        }
    };
    tracing::info!(%url, interval_ms = interval.as_millis() as u64, "tally: polling");
    let mut tracker = TallyTracker::default();
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let step = tracker.observe(poll(&client, &url).await);
        for line in &step.warn {
            tracing::warn!("{line}");
        }
        for line in &step.info {
            tracing::info!("{line}");
        }
        if let Some(tally) = step.changed {
            agg.set_tally(tally);
            live_tx.send_modify(|current| {
                *current = Arc::new(apply_tally(current, tally.as_ref()));
            });
        }
    }
}
