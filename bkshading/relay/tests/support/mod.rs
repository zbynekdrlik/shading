//! A fake Blackmagic camera REST server for the relay tests (shading#6): the ONE external endpoint
//! the tests replace. Everything on the relay side — discovery, the HTTP client, the session, the
//! mapping — runs for real against it over a real TCP socket.
//!
//! It answers the documented `/control/api/v1/...` endpoints from a shared [`Cam`] state, records
//! every request, and behaves like the live cam5 reads: a `PUT lens/iris {"apertureStop": x}`
//! snaps to the lens's own step (204), while `apertureNumber` / `normalised` writes answer 204 and
//! do not move the iris.

#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde_json::{json, Value};

/// The camera the fake serves. Tests change it through [`FakeCamera::cam`].
#[derive(Debug, Clone)]
pub struct Cam {
    pub product: Value,
    pub iso: i64,
    pub isos: Vec<i64>,
    /// `Some` = the camera measures shutter as a speed.
    pub shutter_speed: Option<i64>,
    /// `Some` = the camera measures shutter as an angle (x100).
    pub shutter_angle: Option<i64>,
    pub speeds: Vec<i64>,
    pub angles: Vec<i64>,
    pub kelvin: i64,
    pub tint: i64,
    pub aperture: f64,
    /// The lens's own stops; an `apertureStop` write lands on the nearest.
    pub lens_stops: Vec<f64>,
    /// What `lens/iris/description` reports as `apertureStop.min/max`; `None` = the ends of
    /// `lens_stops`. A zoom lens reports its wide-end min (4.1 on cam5 at 108 mm) while it lands on
    /// steps anchored elsewhere (4.4).
    pub iris_range: Option<(f64, f64)>,
    /// The iris ACKs a write and does not move.
    pub iris_ignores_writes: bool,
    /// `lens/iris/description controllable`.
    pub iris_controllable: bool,
    /// What `lens/iris` answers as `normalised` (shading#6, 0.2.3: the iris's position inside the
    /// lens's range, 0 = wide open, 1 = closed).
    pub iris_position: IrisPosition,
    /// How long after an `apertureStop` PUT the iris reports its new position (a moving iris).
    pub iris_latency: Duration,
    /// An accepted iris write still travelling: the landing and when it was written.
    pub iris_travel: Option<(f64, std::time::Instant)>,
    /// `lens/zoom focalLength` in mm (shading#6, 0.2.5); `None` = the endpoint answers 404.
    pub focal_length: Option<f64>,
    pub focus: f64,
    pub frame_rate: String,
    pub frame_rates: Vec<String>,
    /// A status answered to every PUT on this path instead of applying it.
    pub refuse: HashMap<String, (u16, String)>,
    /// Paths that answer 501 to a GET (not implemented on this body).
    pub not_implemented: Vec<String>,
    /// Paths that answer 404 to a GET (not served by this firmware at all).
    pub not_found: Vec<String>,
    /// A pause before answering a PUT on this path.
    pub put_delay: HashMap<String, Duration>,
    /// A fixed answer `(status, body)` to a GET on this path, instead of the modelled one.
    pub get_override: HashMap<String, (u16, String)>,
    /// A pause before answering a GET on this path (a camera that stops answering).
    pub get_delay: HashMap<String, Duration>,
    pub auto_wb_kelvin: i64,
}

impl Default for Cam {
    fn default() -> Self {
        Cam {
            product: json!({
                "deviceName": "cam5",
                "productName": "Blackmagic Pocket Cinema Camera 4K",
                "softwareVersion": "9.8"
            }),
            iso: 400,
            isos: vec![100, 200, 400, 800, 1600, 3200],
            shutter_speed: Some(50),
            shutter_angle: None,
            speeds: vec![25, 50, 60, 100, 125],
            angles: vec![9000, 18000, 36000],
            kelvin: 5600,
            tint: 0,
            aperture: 7.3,
            // 1/3-stop steps (cam5's lens has only 1/3 steps), displayed off the nominal values.
            lens_stops: vec![
                4.4, 4.9, 5.6, 6.3, 7.3, 8.0, 9.0, 10.0, 11.0, 13.0, 14.0, 16.0, 18.0, 20.0, 22.0,
            ],
            iris_range: None,
            iris_ignores_writes: false,
            iris_controllable: true,
            iris_position: IrisPosition::Fixed(0.3158),
            iris_latency: Duration::ZERO,
            iris_travel: None,
            focal_length: None,
            focus: 0.42,
            frame_rate: "25".into(),
            frame_rates: ["23.98", "24", "25", "29.97", "30", "50", "59.94", "60"]
                .map(String::from)
                .to_vec(),
            refuse: HashMap::new(),
            not_implemented: Vec::new(),
            not_found: Vec::new(),
            put_delay: HashMap::new(),
            get_delay: HashMap::new(),
            get_override: HashMap::new(),
            auto_wb_kelvin: 4800,
        }
    }
}

/// How the fake reports `lens/iris normalised`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IrisPosition {
    /// A fixed value whatever the iris does (the live cam5 read, 0.3158).
    Fixed(f64),
    /// The iris's place between the ends of `lens_stops`, in stops: 0.0 at the first (widest),
    /// 1.0 at the last (closed), fractional between (cam3 at 24 mm read 0.0 wide open at f/4.0).
    FromLens,
    /// The iris's place between the ends of `iris_range` (`lens/iris/description`), in stops: the
    /// Pocket 4K at fw 9.8 measures against the description, so at 108 mm its widest stop 4.4 reads
    /// above 0 against a description min of 4.1 (live 10.11.2026), and 0 is never reached there.
    FromRange,
    /// No `normalised` key at all.
    Absent,
}

/// `normalised` for an iris at `f` on a lens whose ends are `stops[0]` and `stops[last]`.
pub fn lens_position(stops: &[f64], f: f64) -> f64 {
    let (lo, hi) = (*stops.first().unwrap(), *stops.last().unwrap());
    ((f.log2() - lo.log2()) / (hi.log2() - lo.log2())).clamp(0.0, 1.0)
}

/// One request the fake received.
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub body: Option<Value>,
}

#[derive(Default)]
pub struct Shared {
    pub cam: Mutex<Cam>,
    pub seen: Mutex<Vec<Seen>>,
}

/// A running fake camera. The server lives on its own runtime thread for the test's life.
pub struct FakeCamera {
    pub addr: SocketAddr,
    pub shared: Arc<Shared>,
}

impl FakeCamera {
    pub fn start(cam: Cam) -> FakeCamera {
        let shared = Arc::new(Shared {
            cam: Mutex::new(cam),
            seen: Mutex::new(Vec::new()),
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let state = shared.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("fake camera runtime");
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                let app = Router::new().fallback(handle).with_state(state);
                axum::serve(listener, app).await.unwrap();
            });
        });
        let addr = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("fake camera bound");
        FakeCamera { addr, shared }
    }

    pub fn cam(&self) -> std::sync::MutexGuard<'_, Cam> {
        self.shared.cam.lock().unwrap()
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.shared.seen.lock().unwrap().clone()
    }

    /// The PUTs received, as `(path, body)`.
    pub fn puts(&self) -> Vec<(String, Option<Value>)> {
        self.seen()
            .into_iter()
            .filter(|s| s.method == "PUT")
            .map(|s| (s.path, s.body))
            .collect()
    }

    pub fn gets_of(&self, path: &str) -> usize {
        self.seen()
            .iter()
            .filter(|s| s.method == "GET" && s.path == path)
            .count()
    }

    pub fn clear_seen(&self) {
        self.shared.seen.lock().unwrap().clear();
    }
}

/// cam5's zoom lens at 108 mm, read live: 1/3-stop steps anchored on the zoom's wide end (4.4), not
/// on the standard marks, while `lens/iris/description` says min 4.1.
pub fn zoom_third_stop_lens() -> Cam {
    Cam {
        aperture: 4.4,
        lens_stops: vec![
            4.4, 4.8, 5.2, 5.6, 6.2, 6.7, 7.3, 8.0, 8.7, 9.5, 10.4, 11.3, 12.3, 13.5, 14.6, 16.0,
            17.4, 19.0, 20.7, 22.0,
        ],
        iris_range: Some((4.1, 22.0)),
        ..Cam::default()
    }
}

/// SNV cam5 read live 10.11.2026 (shading#6, 0.2.5): Pocket 4K fw 9.8, zoom lens at 108 mm.
/// The native ladder there is 4.4 5.2 5.6 6.2 6.7 7.3 8.0 (no 4.8: writes of 4.6/4.8 stay at 4.4,
/// 5.0 lands 5.2), continued here in 1/3 stops to 22 (the description's max). The description says
/// 4.1 .. 22 and `normalised` is measured against it (0.0387 live at 4.4, never 0 at tele).
/// `lens/zoom` answers `focalLength` 108. The iris reports a move ~300 ms after a PUT (measured
/// <= 340 ms).
pub fn pocket_4k_zoom_108mm() -> Cam {
    Cam {
        aperture: 6.7,
        lens_stops: vec![
            4.4, 5.2, 5.6, 6.2, 6.7, 7.3, 8.0, 8.7, 9.5, 10.4, 11.3, 12.3, 13.5, 14.6, 16.0, 17.4,
            19.0, 20.7, 22.0,
        ],
        iris_range: Some((4.1, 22.0)),
        iris_position: IrisPosition::FromRange,
        iris_latency: Duration::from_millis(300),
        focal_length: Some(108.0),
        ..Cam::default()
    }
}

/// SNV cam3, the original Pocket 6K at fw 8.6, read live 9.11.2026 (shading#6): REST is served
/// (Web media manager on), but this firmware has no `system/product`, no capability lists and no
/// descriptions. Its lens at 24 mm, walked live with 1/16-stop targets: 1/6-stop steps from 4.0
/// (the end of the lens) up to 11.0, starting at 9.5. `lens/iris normalised` is the iris's place in
/// the lens's range: 0.0 wide open at 4.0 (read live 10.11.2026), 1.0 closed.
pub fn pocket_6k_fw86() -> Cam {
    Cam {
        product: json!(null),
        iso: 1000,
        shutter_speed: Some(100),
        kelvin: 4850,
        tint: 0,
        aperture: 9.5,
        lens_stops: vec![
            4.0, 4.4, 4.6, 4.8, 5.0, 5.2, 5.4, 5.6, 5.9, 6.2, 6.45, 6.7, 7.0, 7.3, 7.65, 8.0, 8.35,
            8.7, 9.1, 9.5, 10.0, 10.5, 11.0,
        ],
        iris_position: IrisPosition::FromLens,
        focus: 0.0,
        frame_rate: "60".into(),
        not_found: [
            "/system/product",
            "/video/supportedISOs",
            "/video/supportedShutters",
            "/video/whiteBalance/description",
            "/lens/iris/description",
            "/video/shutter/description",
        ]
        .map(String::from)
        .to_vec(),
        not_implemented: [
            "/video/ndFilter",
            "/system/supportedCodecFormats",
            "/system/supportedVideoFormats",
        ]
        .map(String::from)
        .to_vec(),
        ..Cam::default()
    }
}

/// The PP Pocket 6K lens: 1/6-stop steps near open (2.8 2.9 3.1 3.2 3.3 3.5, read live), with some
/// gaps under 0.1 stop (3.9 -> 4.0).
pub fn sixth_stop_lens() -> Cam {
    Cam {
        aperture: 2.8,
        lens_stops: vec![
            2.8, 2.9, 3.1, 3.2, 3.3, 3.5, 3.7, 3.9, 4.0, 4.2, 4.5, 4.8, 5.0, 5.3, 5.6, 6.0, 6.3,
            6.7, 7.1, 7.6, 8.0, 8.5, 9.0, 9.5, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0,
            18.0, 19.0, 20.0, 22.0,
        ],
        iris_range: Some((2.8, 22.0)),
        ..Cam::default()
    }
}

fn nearest(stops: &[f64], want: f64) -> f64 {
    stops
        .iter()
        .copied()
        .min_by(|a, b| {
            (a / want)
                .log2()
                .abs()
                .partial_cmp(&(b / want).log2().abs())
                .unwrap()
        })
        .unwrap_or(want)
}

fn ok(v: Value) -> Response {
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        v.to_string(),
    )
        .into_response()
}

async fn handle(State(sh): State<Arc<Shared>>, method: Method, uri: Uri, body: Bytes) -> Response {
    let full = uri.path().to_string();
    let Some(path) = full.strip_prefix("/control/api/v1") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = path.to_string();
    let parsed: Option<Value> = if body.is_empty() {
        None
    } else {
        serde_json::from_slice(&body).ok()
    };
    sh.seen.lock().unwrap().push(Seen {
        method: method.to_string(),
        path: path.clone(),
        body: parsed.clone(),
    });
    if method == Method::PUT {
        let delay = sh.cam.lock().unwrap().put_delay.get(&path).copied();
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        return put(&sh, &path, parsed);
    }
    if method != Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let delay = sh.cam.lock().unwrap().get_delay.get(&path).copied();
    if let Some(d) = delay {
        tokio::time::sleep(d).await;
    }
    {
        // A travelling iris arrives once its latency has passed.
        let mut cam = sh.cam.lock().unwrap();
        if let Some((to, at)) = cam.iris_travel {
            if at.elapsed() >= cam.iris_latency {
                cam.aperture = to;
                cam.iris_travel = None;
            }
        }
    }
    let cam = sh.cam.lock().unwrap().clone();
    if cam.not_implemented.contains(&path) {
        return (StatusCode::NOT_IMPLEMENTED, "not implemented").into_response();
    }
    if let Some((status, body)) = cam.get_override.get(&path) {
        return (StatusCode::from_u16(*status).unwrap(), body.clone()).into_response();
    }
    if cam.not_found.contains(&path) {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }
    match path.as_str() {
        "/system" => StatusCode::NO_CONTENT.into_response(),
        "/system/product" => ok(cam.product.clone()),
        "/video/iso" => ok(json!({ "iso": cam.iso })),
        "/video/supportedISOs" => ok(json!({ "supportedISOs": cam.isos })),
        "/video/shutter" => match (cam.shutter_speed, cam.shutter_angle) {
            (Some(s), _) => {
                ok(json!({ "continuousShutterAutoExposure": false, "shutterSpeed": s }))
            }
            (None, Some(a)) => {
                ok(json!({ "continuousShutterAutoExposure": false, "shutterAngle": a }))
            }
            _ => ok(json!({})),
        },
        "/video/supportedShutters" => {
            if cam.shutter_speed.is_some() {
                ok(json!({ "shutterSpeeds": cam.speeds }))
            } else {
                ok(json!({ "shutterAngles": cam.angles }))
            }
        }
        "/video/whiteBalance" => ok(json!({ "whiteBalance": cam.kelvin })),
        "/video/whiteBalance/description" => {
            ok(json!({ "whiteBalance": { "min": 2500, "max": 10000 } }))
        }
        "/video/whiteBalanceTint" => ok(json!({ "whiteBalanceTint": cam.tint })),
        "/lens/iris" => {
            let mut v = json!({
                "apertureNumber": 432,
                "apertureStop": cam.aperture,
                "continuousApertureAutoExposure": false,
            });
            let norm = match cam.iris_position {
                IrisPosition::Fixed(n) => Some(n),
                IrisPosition::FromLens => Some(lens_position(&cam.lens_stops, cam.aperture)),
                IrisPosition::FromRange => {
                    let (lo, hi) = cam.iris_range.expect("FromRange needs iris_range");
                    Some(lens_position(&[lo, hi], cam.aperture))
                }
                IrisPosition::Absent => None,
            };
            if let Some(n) = norm {
                v["normalised"] = json!(n);
            }
            ok(v)
        }
        "/lens/iris/description" => {
            let (min, max) = cam.iris_range.unwrap_or((
                *cam.lens_stops.first().unwrap(),
                *cam.lens_stops.last().unwrap(),
            ));
            ok(
                json!({ "controllable": cam.iris_controllable, "apertureStop": { "min": min, "max": max } }),
            )
        }
        "/lens/focus" => ok(json!({ "normalised": cam.focus })),
        "/lens/zoom" => match cam.focal_length {
            Some(f) => ok(json!({ "focalLength": f, "normalised": 0.0 })),
            None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
        },
        "/system/format" => ok(json!({
            "codec": "BRaw:Q0",
            "frameRate": cam.frame_rate,
            "offSpeedEnabled": false,
            "offSpeedFrameRate": 60,
            "minOffSpeedFrameRate": 5,
            "maxOffSpeedFrameRate": 60,
            "recordResolution": { "width": 3840, "height": 2160 },
            "sensorResolution": { "width": 4096, "height": 2160 }
        })),
        "/system/supportedFormats" => ok(json!({ "supportedFormats": [{
            "codecs": ["BRaw:Q0"],
            "frameRates": cam.frame_rates,
            "maxOffSpeedFrameRate": 60,
            "minOffSpeedFrameRate": 5,
            "recordResolution": { "width": 3840, "height": 2160 },
            "sensorResolution": { "width": 4096, "height": 2160 }
        }]})),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn put(sh: &Shared, path: &str, body: Option<Value>) -> Response {
    let mut cam = sh.cam.lock().unwrap();
    if let Some((status, text)) = cam.refuse.get(path).cloned() {
        return (StatusCode::from_u16(status).unwrap(), text).into_response();
    }
    let b = body.unwrap_or(Value::Null);
    let int = |k: &str| b.get(k).and_then(Value::as_i64);
    match path {
        "/video/iso" => match int("iso") {
            Some(v) => cam.iso = v,
            None => return StatusCode::BAD_REQUEST.into_response(),
        },
        "/video/shutter" => {
            if let Some(v) = int("shutterSpeed") {
                cam.shutter_speed = Some(v);
            } else if let Some(v) = b.get("shutterAngle").and_then(Value::as_f64) {
                cam.shutter_angle = Some(v.round() as i64);
            } else {
                return StatusCode::BAD_REQUEST.into_response();
            }
        }
        "/video/whiteBalance" => match int("whiteBalance") {
            Some(v) => cam.kelvin = v,
            None => return StatusCode::BAD_REQUEST.into_response(),
        },
        "/video/whiteBalance/doAuto" => cam.kelvin = cam.auto_wb_kelvin,
        "/video/whiteBalanceTint" => match int("whiteBalanceTint") {
            Some(v) => cam.tint = v,
            None => return StatusCode::BAD_REQUEST.into_response(),
        },
        "/lens/iris" => {
            // Live cam5: only apertureStop moves the iris; the other two keys are ACKed and ignored.
            if let Some(f) = b.get("apertureStop").and_then(Value::as_f64) {
                if !cam.iris_ignores_writes {
                    let to = nearest(&cam.lens_stops, f);
                    if cam.iris_latency.is_zero() {
                        cam.aperture = to;
                    } else {
                        cam.iris_travel = Some((to, std::time::Instant::now()));
                    }
                }
            }
        }
        "/system/format" => match b.get("frameRate").and_then(Value::as_str) {
            Some(fr) if cam.frame_rates.iter().any(|r| r == fr) => cam.frame_rate = fr.to_string(),
            _ => return StatusCode::BAD_REQUEST.into_response(),
        },
        _ => return StatusCode::NOT_FOUND.into_response(),
    }
    StatusCode::NO_CONTENT.into_response()
}
