//! shading#6: the relay drives the camera through Blackmagic's REST API. These run the real
//! session, client and discovery against a local fake camera server (`support/`), the only part
//! replaced: no camera, no gphoto2.

mod support;

use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use bkshading_proto::read::RestWrite;
use bkshading_proto::wire::SetRequest;
use bkshading_relay::http::set_error_status;
use bkshading_relay::transport::rest::{
    build_request, decode_chunked, parse_response, write_body, HttpResponse,
};
use bkshading_relay::transport::{
    camera_links, parse_camera_addr, parse_capture_fps_env, parse_inet, parse_lease,
    parse_min_read_interval_env, peer_of, read_is_fresh, ApplyOutcome, CameraAddr, CameraLocator,
    CameraSession, DiscoverPaths, MonoClock, CAMERA_HTTP_PORT, IRIS_PROBE_WAIT_MS,
};
use serde_json::json;
use support::{Cam, FakeCamera, IrisPosition};

const VERSION: &str = "0.2.0-test";

fn fixed(cam: &FakeCamera) -> CameraLocator {
    CameraLocator::Fixed(CameraAddr {
        host: "127.0.0.1".into(),
        port: cam.addr.port(),
    })
}

fn session(cam: &FakeCamera) -> CameraSession {
    CameraSession::new(fixed(cam), VERSION).with_aperture_readback_delay_ms(0)
}

/// A clock the test moves by hand (the read floor without real sleeps).
#[derive(Clone, Default)]
struct FakeClock(Arc<AtomicU64>);

impl MonoClock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Waits (bounded) until the fake camera has received a PUT on `path` — a sync point instead of a
/// guessed sleep, so a loaded CI runner cannot reorder the test's threads.
fn wait_for_put(cam: &FakeCamera, path: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !cam.puts().iter().any(|(p, _)| p == path) {
        assert!(
            std::time::Instant::now() < deadline,
            "no PUT {path} within 10 s"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn applied(o: ApplyOutcome) -> (usize, bkshading_proto::wire::RelayState) {
    match o {
        ApplyOutcome::Applied { count, state } => (count, *state),
        ApplyOutcome::Queued => panic!("expected Applied, got Queued"),
    }
}

#[test]
fn a_live_camera_reads_onto_the_wire_6() {
    let cam = FakeCamera::start(Cam::default());
    let st = session(&cam).with_capture_fps(Some(60)).read_state();
    assert!(st.online);
    assert_eq!(
        st.camera.as_deref(),
        Some("Blackmagic Pocket Cinema Camera 4K")
    );
    assert_eq!(st.version, VERSION);
    assert_eq!(st.capture_fps, Some(60));
    let p = &st.params;
    assert_eq!(p.iso, Some(400));
    assert_eq!(p.kelvin, Some(5600));
    assert_eq!(p.tint, Some(0));
    assert_eq!(p.shutter, Some(50));
    assert_eq!(p.fps100, Some(2500));
    assert_eq!(p.sensor_fps100, Some(6000));
    assert_eq!(p.focus_norm, Some(0.42));
    let f = 2f64.powf(p.aperture_av.unwrap() / 2.0);
    assert!((f - 7.3).abs() < 1e-9, "{f}");
    let caps = st.caps.as_ref().unwrap();
    assert_eq!(caps.fnumber_choices.first(), Some(&4.4));
    assert_eq!(caps.fnumber_choices.last(), Some(&22.0));
    assert_eq!(caps.iso_choices, vec![100, 200, 400, 800, 1600, 3200]);
    assert_eq!(caps.shutter_choices, vec![25, 50, 60, 100, 125]);
    assert_eq!(
        caps.fps_choices,
        vec![2398, 2400, 2500, 2997, 3000, 5000, 5994, 6000]
    );
    assert!(st.fps_supported);
    assert!(st.not_applied.is_empty());
    // Every request went under the API prefix, as plain GETs (no write on a read).
    assert!(cam.puts().is_empty());
    assert_eq!(cam.gets_of("/system/product"), 1);
    assert_eq!(cam.gets_of("/lens/iris"), 1);
}

#[test]
fn an_endpoint_the_body_does_not_implement_is_just_unknown_6() {
    let cam = FakeCamera::start(Cam {
        not_implemented: vec!["/lens/focus".into(), "/video/supportedISOs".into()],
        ..Default::default()
    });
    let st = session(&cam).read_state();
    assert!(st.online, "a 501 endpoint never makes the camera offline");
    assert_eq!(st.params.focus_norm, None);
    // 0.2.2 (coordinator, live 6K fw 8.6): a missing ISO list falls back to the standard Pocket list.
    assert_eq!(
        st.caps.unwrap().iso_choices,
        bkshading_proto::mapping::POCKET_ISO_CHOICES.to_vec()
    );
    assert_eq!(st.params.iso, Some(400));
}

#[test]
fn no_camera_reads_offline_and_keeps_the_box_rate_6() {
    // A port nothing listens on: bind and drop.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let s = Arc::new(
        CameraSession::new(
            CameraLocator::Fixed(CameraAddr {
                host: "127.0.0.1".into(),
                port,
            }),
            VERSION,
        )
        .with_capture_fps(Some(60)),
    );
    let st = s.read_state();
    assert!(!st.online);
    assert_eq!(st.camera, None);
    assert_eq!(st.capture_fps, Some(60));
    assert_eq!(s.detect(), None);
    // A write to an absent camera is a 502, never a panic, and writes nothing.
    let err = s
        .submit(&SetRequest {
            iso: Some(800),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(set_error_status(&err), StatusCode::BAD_GATEWAY);
    assert!(err.to_string().contains("camera offline"), "{err}");
}

#[test]
fn the_read_floor_serves_the_cache_until_it_expires_1229() {
    let cam = FakeCamera::start(Cam::default());
    let clock = FakeClock::default();
    let s = Arc::new(
        session(&cam)
            .with_min_read_interval_ms(10_000)
            .with_clock(Box::new(clock.clone())),
    );
    s.read_state();
    s.read_state();
    clock.0.store(9_999, Ordering::SeqCst);
    s.read_state();
    assert_eq!(
        cam.gets_of("/video/iso"),
        1,
        "three polls inside the floor, one read"
    );
    assert_eq!(
        cam.gets_of("/system/product"),
        1,
        "the camera is probed once"
    );
    clock.0.store(10_000, Ordering::SeqCst);
    cam.cam().iso = 800;
    assert_eq!(s.read_state().params.iso, Some(800));
    assert_eq!(cam.gets_of("/video/iso"), 2);
    assert_eq!(
        cam.gets_of("/system/product"),
        1,
        "a found camera is not probed again"
    );
}

#[test]
fn every_param_is_one_put_and_the_answer_is_the_read_back_6() {
    let cam = FakeCamera::start(Cam::default());
    let s = Arc::new(session(&cam));
    let req = SetRequest {
        iso: Some(800),
        kelvin: Some(6500),
        tint: Some(-5),
        shutter: Some(100),
        fps: Some(50),
        ..Default::default()
    };
    let (count, st) = applied(s.submit(&req).unwrap());
    assert_eq!(count, 5);
    assert_eq!(
        cam.puts(),
        vec![
            ("/system/format".into(), Some(json!({"frameRate": "50"}))),
            ("/video/iso".into(), Some(json!({"iso": 800}))),
            ("/video/shutter".into(), Some(json!({"shutterSpeed": 100}))),
            (
                "/video/whiteBalance".into(),
                Some(json!({"whiteBalance": 6500}))
            ),
            (
                "/video/whiteBalanceTint".into(),
                Some(json!({"whiteBalanceTint": -5}))
            ),
        ]
    );
    assert!(st.online);
    assert_eq!(st.params.iso, Some(800));
    assert_eq!(st.params.kelvin, Some(6500));
    assert_eq!(st.params.tint, Some(-5));
    assert_eq!(st.params.shutter, Some(100));
    assert_eq!(st.params.fps100, Some(5000));
    assert!(st.not_applied.is_empty(), "{:?}", st.not_applied);
    // The read-back replaced the cache: the next poll inside the floor serves it, no new read.
    cam.clear_seen();
    assert_eq!(s.read_state().params.iso, Some(800));
    assert!(
        cam.seen().is_empty(),
        "served from the cache: {:?}",
        cam.seen()
    );
}

#[test]
fn an_aperture_write_sends_aperture_stop_and_reports_where_the_lens_landed_6() {
    let cam = FakeCamera::start(Cam::default());
    let s = Arc::new(session(&cam));
    let grid = s.read_state().caps.unwrap().fnumber_choices;
    let i8 = grid.iter().position(|f| *f == 8.0).unwrap();
    let norm = i8 as f64 / (grid.len() - 1) as f64;
    let (_, st) = applied(
        s.submit(&SetRequest {
            aperture_norm: Some(norm),
            ..Default::default()
        })
        .unwrap(),
    );
    assert_eq!(
        cam.puts(),
        vec![("/lens/iris".into(), Some(json!({"apertureStop": 8.0})))],
        "only apertureStop moves the iris (live cam5)"
    );
    let f = 2f64.powf(st.params.aperture_av.unwrap() / 2.0);
    assert!((f - 8.0).abs() < 1e-9, "landed {f}");
    assert!(st.not_applied.is_empty());

    // A request between the lens's own steps lands on the nearest step: moved, so applied.
    let i5 = grid.iter().position(|f| *f == 5.0).unwrap();
    let (_, st) = applied(
        s.submit(&SetRequest {
            aperture_norm: Some(i5 as f64 / (grid.len() - 1) as f64),
            ..Default::default()
        })
        .unwrap(),
    );
    let f = 2f64.powf(st.params.aperture_av.unwrap() / 2.0);
    assert!(
        (f - 4.9).abs() < 1e-9,
        "the lens's 4.9 step, not the nominal 5.0: {f}"
    );
    assert!(
        st.not_applied.is_empty(),
        "a write that moved the iris is applied wherever it landed: {:?}",
        st.not_applied
    );
}

#[test]
fn an_iris_that_ignores_the_write_is_flagged_not_applied_1343() {
    let cam = FakeCamera::start(Cam {
        iris_ignores_writes: true,
        ..Default::default()
    });
    let s = Arc::new(session(&cam));
    let (_, st) = applied(
        s.submit(&SetRequest {
            aperture_norm: Some(1.0),
            iso: Some(800),
            ..Default::default()
        })
        .unwrap(),
    );
    assert_eq!(st.not_applied, vec!["apertureNorm".to_string()]);
    assert_eq!(st.params.iso, Some(800));
}

#[test]
fn an_unlisted_fps_is_refused_422_and_writes_nothing_1402() {
    let cam = FakeCamera::start(Cam::default());
    let s = Arc::new(session(&cam));
    let err = s
        .submit(&SetRequest {
            fps: Some(48),
            iso: Some(800),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(set_error_status(&err), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        err.to_string().starts_with("fps-not-settable: fps 48"),
        "{err}"
    );
    assert!(cam.puts().is_empty(), "nothing reached the camera");
    // The gate is idle again: the next SET runs.
    let (count, _) = applied(
        s.submit(&SetRequest {
            fps: Some(30),
            ..Default::default()
        })
        .unwrap(),
    );
    assert_eq!(count, 1);
    assert_eq!(cam.cam().frame_rate, "30");
}

#[test]
fn a_refused_put_is_a_502_naming_the_camera_answer_and_drops_the_cache_6() {
    let mut c = Cam::default();
    c.refuse.insert(
        "/video/iso".into(),
        (403, "ISO cannot be changed in the current state".into()),
    );
    let cam = FakeCamera::start(c);
    let clock = FakeClock::default();
    let s = Arc::new(session(&cam).with_clock(Box::new(clock)));
    s.read_state();
    let err = s
        .submit(&SetRequest {
            iso: Some(800),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(set_error_status(&err), StatusCode::BAD_GATEWAY);
    let msg = err.to_string();
    assert!(msg.contains("PUT /control/api/v1/video/iso"), "{msg}");
    assert!(msg.contains("403"), "{msg}");
    assert!(msg.contains("ISO cannot be changed"), "{msg}");
    // The cache was dropped: the next poll reads the camera again even inside the floor.
    let before = cam.gets_of("/video/iso");
    s.read_state();
    assert_eq!(cam.gets_of("/video/iso"), before + 1);
    // And the gate is idle again.
    cam.cam().refuse.clear();
    assert!(s
        .submit(&SetRequest {
            iso: Some(800),
            ..Default::default()
        })
        .is_ok());
}

#[test]
fn auto_white_balance_runs_the_cameras_do_auto_6() {
    let cam = FakeCamera::start(Cam::default());
    let s = Arc::new(session(&cam));
    let (count, st) = applied(
        s.submit(&SetRequest {
            auto_wb: Some(true),
            ..Default::default()
        })
        .unwrap(),
    );
    assert_eq!(count, 1);
    assert_eq!(
        cam.puts(),
        vec![("/video/whiteBalance/doAuto".into(), None)]
    );
    assert_eq!(st.params.kelvin, Some(4800));
}

#[test]
fn a_shutter_angle_camera_gets_its_exact_angle_back_6() {
    let cam = FakeCamera::start(Cam {
        shutter_speed: None,
        shutter_angle: Some(18000),
        angles: vec![9000, 17280, 18000, 36000],
        ..Default::default()
    });
    let s = Arc::new(session(&cam));
    let st = s.read_state();
    assert_eq!(st.params.shutter, Some(50), "180 deg at 25 fps");
    assert_eq!(st.caps.unwrap().shutter_choices, vec![25, 50, 52, 100]);
    let (_, st) = applied(
        s.submit(&SetRequest {
            shutter: Some(52),
            ..Default::default()
        })
        .unwrap(),
    );
    assert_eq!(
        cam.puts(),
        vec![(
            "/video/shutter".into(),
            Some(json!({"shutterAngle": 17280}))
        )]
    );
    assert_eq!(st.params.shutter, Some(52));
    assert!(st.not_applied.is_empty());
}

#[test]
fn sets_that_arrive_during_a_write_queue_and_run_in_order_1337() {
    let mut c = Cam::default();
    c.put_delay
        .insert("/video/iso".into(), Duration::from_millis(600));
    let cam = FakeCamera::start(c);
    let s = Arc::new(session(&cam));
    let first = {
        let s = s.clone();
        std::thread::spawn(move || {
            s.submit(&SetRequest {
                iso: Some(800),
                ..Default::default()
            })
        })
    };
    // Once the first SET is inside its slow PUT, send two more.
    wait_for_put(&cam, "/video/iso");
    let q1 = s
        .submit(&SetRequest {
            iso: Some(1600),
            ..Default::default()
        })
        .unwrap();
    let q2 = s
        .submit(&SetRequest {
            kelvin: Some(3200),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(q1, ApplyOutcome::Queued);
    assert_eq!(q2, ApplyOutcome::Queued);
    let (count, st) = applied(first.join().unwrap().unwrap());
    assert_eq!(count, 3, "the in-flight worker drained both queued SETs");
    let puts: Vec<_> = cam
        .puts()
        .into_iter()
        .map(|(p, b)| (p, b.unwrap()))
        .collect();
    assert_eq!(
        puts,
        vec![
            ("/video/iso".to_string(), json!({"iso": 800})),
            ("/video/iso".to_string(), json!({"iso": 1600})),
            (
                "/video/whiteBalance".to_string(),
                json!({"whiteBalance": 3200})
            ),
        ],
        "FIFO, never two writes at once"
    );
    assert_eq!(st.params.iso, Some(1600));
    assert_eq!(st.params.kelvin, Some(3200));
    assert!(st.not_applied.is_empty(), "{:?}", st.not_applied);
}

/// A fake `/sys/class/net` + networkd lease tree.
struct NetTree {
    root: std::path::PathBuf,
}

impl NetTree {
    fn new(tag: &str) -> NetTree {
        let root = std::env::temp_dir().join(format!(
            "bkshading-relay-discover-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("net")).unwrap();
        std::fs::create_dir_all(root.join("leases")).unwrap();
        std::fs::create_dir_all(root.join("drivers/cdc_ncm")).unwrap();
        std::fs::create_dir_all(root.join("drivers/r8169")).unwrap();
        NetTree { root }
    }

    /// Like real sysfs: `net/<if>/device` is a SYMLINK to the USB interface dir
    /// (`usb/<dev>/<dev>:1.0`), whose parent, the USB device, holds `idVendor`.
    fn iface(&self, name: &str, driver: &str, ifindex: u32) {
        let usb_dev = self.root.join("usb").join(name);
        let usb_if = usb_dev.join(format!("{name}:1.0"));
        std::fs::create_dir_all(&usb_if).unwrap();
        symlink(
            self.root.join("drivers").join(driver),
            usb_if.join("driver"),
        )
        .unwrap();
        let dir = self.root.join("net").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        symlink(&usb_if, dir.join("device")).unwrap();
        std::fs::write(dir.join("ifindex"), format!("{ifindex}\n")).unwrap();
        self.vendor(name, "1edb");
    }

    fn vendor(&self, name: &str, vendor: &str) {
        let usb_dev = self.root.join("usb").join(name);
        std::fs::write(usb_dev.join("idVendor"), format!("{vendor}\n")).unwrap();
    }

    fn usb_product(&self, name: &str, product: &str) {
        let usb_dev = self.root.join("usb").join(name);
        std::fs::write(usb_dev.join("product"), format!("{product}\n")).unwrap();
    }

    fn lease(&self, ifindex: u32, text: &str) {
        std::fs::write(self.root.join("leases").join(ifindex.to_string()), text).unwrap();
    }

    fn paths(&self) -> DiscoverPaths {
        DiscoverPaths {
            sysfs_net: self.root.join("net"),
            leases_dir: self.root.join("leases"),
            // No `ip` here: the /30 fallback finds nothing, so the lease alone decides.
            ip_cmd: self.root.join("no-ip-binary"),
        }
    }
}

impl Drop for NetTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn the_camera_is_the_dhcp_server_on_the_cdc_ncm_link_6() {
    let cam = FakeCamera::start(Cam::default());
    let net = NetTree::new("found");
    net.iface("enp1s0", "r8169", 2);
    net.lease(2, "SERVER_ADDRESS=10.77.9.1\n");
    net.iface("enx7c2e0d123456", "cdc_ncm", 7);
    // A real networkd lease carries more lines; the camera is SERVER_ADDRESS.
    net.lease(
        7,
        "# This is private data. Do not parse.\nADDRESS=127.0.0.2\nNETMASK=255.255.255.252\n\
         ROUTER=127.0.0.9\nSERVER_ADDRESS=127.0.0.1\nLIFETIME=86400\n",
    );
    // A 2.5G USB dongle on the same driver is not the camera.
    net.iface("enx00e04c680001", "cdc_ncm", 8);
    net.vendor("enx00e04c680001", "0bda");
    net.lease(8, "SERVER_ADDRESS=127.0.0.3\n");
    let links = camera_links(&net.paths());
    assert_eq!(
        links.len(),
        1,
        "only the Blackmagic cdc_ncm interface: {links:?}"
    );
    assert_eq!(links[0].source, Some("lease"));
    assert_eq!(links[0].iface, "enx7c2e0d123456");
    assert_eq!(links[0].camera_ip, Some("127.0.0.1".parse().unwrap()));
    let s = Arc::new(CameraSession::new(
        CameraLocator::Discover {
            paths: net.paths(),
            port: cam.addr.port(),
        },
        VERSION,
    ));
    let st = s.read_state();
    assert!(
        st.online,
        "found through the lease and confirmed by system/product"
    );
    assert_eq!(
        s.detect().as_deref(),
        Some("Blackmagic Pocket Cinema Camera 4K")
    );
}

#[test]
fn no_link_or_no_lease_reads_offline_6() {
    let net = NetTree::new("none");
    let s = Arc::new(CameraSession::new(
        CameraLocator::Discover {
            paths: net.paths(),
            port: CAMERA_HTTP_PORT,
        },
        VERSION,
    ));
    assert!(!s.read_state().online, "no cdc_ncm interface at all");
    net.iface("enx7c2e0d123456", "cdc_ncm", 9);
    let links = camera_links(&net.paths());
    assert_eq!(
        links[0].camera_ip, None,
        "the link is there, DHCP has not run"
    );
    let s = Arc::new(CameraSession::new(
        CameraLocator::Discover {
            paths: net.paths(),
            port: CAMERA_HTTP_PORT,
        },
        VERSION,
    ));
    assert!(!s.read_state().online);
    // A missing sysfs dir reads as no links, never a panic.
    let gone = DiscoverPaths {
        sysfs_net: net.root.join("nope"),
        leases_dir: net.root.join("nope"),
        ip_cmd: net.root.join("nope"),
    };
    assert!(camera_links(&gone).is_empty());
}

#[test]
fn the_lease_names_the_server_else_the_router_6() {
    assert_eq!(
        parse_lease("ADDRESS=172.25.77.162\nSERVER_ADDRESS=172.25.77.161\n"),
        Some("172.25.77.161".parse().unwrap())
    );
    assert_eq!(
        parse_lease("ADDRESS=172.25.77.162\nROUTER=172.25.77.161 172.25.77.1\n"),
        Some("172.25.77.161".parse().unwrap())
    );
    assert_eq!(parse_lease("ADDRESS=172.25.77.162\n"), None);
    assert_eq!(parse_lease("SERVER_ADDRESS=not-an-ip\n"), None);
    assert_eq!(parse_lease(""), None);
}

#[test]
fn the_camera_flag_takes_host_and_port_6() {
    let a = parse_camera_addr("172.25.77.161").unwrap();
    assert_eq!((a.host.as_str(), a.port), ("172.25.77.161", 80));
    let a = parse_camera_addr("HTTP://172.25.77.161").unwrap();
    assert_eq!((a.host.as_str(), a.port), ("172.25.77.161", 80));
    let a = parse_camera_addr("http://cam5.local:8080/").unwrap();
    assert_eq!((a.host.as_str(), a.port), ("cam5.local", 8080));
    for bad in [
        "",
        "https://x",
        "x:0",
        "x:notaport",
        "a b",
        "x/control",
        "http://x/y",
    ] {
        assert!(parse_camera_addr(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn requests_and_answers_are_plain_http11_6() {
    let addr = CameraAddr {
        host: "172.25.77.161".into(),
        port: 80,
    };
    assert_eq!(
        build_request("GET", &addr, "/lens/iris", None),
        "GET /control/api/v1/lens/iris HTTP/1.1\r\nHost: 172.25.77.161:80\r\n\
         Accept: application/json\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    );
    let put = build_request("PUT", &addr, "/lens/iris", Some("{\"apertureStop\":8.0}"));
    assert!(put.starts_with("PUT /control/api/v1/lens/iris HTTP/1.1\r\n"));
    assert!(put.ends_with(
        "Content-Type: application/json\r\nContent-Length: 20\r\n\r\n{\"apertureStop\":8.0}"
    ));

    let ok = HttpResponse {
        status: 200,
        body: "{\"iso\":400}".into(),
    };
    let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"iso\":400}";
    assert_eq!(parse_response(raw, false).unwrap(), Some(ok.clone()));
    assert_eq!(
        parse_response(&raw[..30], false).unwrap(),
        None,
        "incomplete"
    );
    assert!(parse_response(&raw[..45], true).is_err(), "closed mid-body");
    let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{\"iso\r\n6\r\n\":400}\r\n0\r\n\r\n";
    assert_eq!(parse_response(chunked, false).unwrap(), Some(ok.clone()));
    let to_close = b"HTTP/1.0 200 OK\r\n\r\n{\"iso\":400}";
    assert_eq!(
        parse_response(to_close, false).unwrap(),
        None,
        "waits for the close"
    );
    assert_eq!(parse_response(to_close, true).unwrap(), Some(ok));
    assert_eq!(
        parse_response(b"HTTP/1.1 204 No Content\r\n\r\n", false).unwrap(),
        Some(HttpResponse {
            status: 204,
            body: String::new()
        })
    );
    assert!(parse_response(b"SSH-2.0-OpenSSH\r\n\r\n", true).is_err());
    // An interim 100 Continue is skipped; the real answer follows it.
    let interim = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 204 No Content\r\n\r\n";
    assert_eq!(parse_response(interim, false).unwrap().unwrap().status, 204);
    assert_eq!(
        parse_response(b"HTTP/1.1 100 Continue\r\n\r\n", false).unwrap(),
        None
    );
    // A chunk size a camera could never mean is refused, not an overflow panic.
    assert!(decode_chunked(b"ffffffffffffffff\r\nab\r\n").is_err());
    assert!(parse_response(b"HTTP/1.1 200", true).is_err());
    assert_eq!(decode_chunked(b"3\r\nabc\r\n").unwrap(), None);
    assert!(decode_chunked(b"zz\r\n").is_err());
}

#[test]
fn write_bodies_follow_the_api_6() {
    assert_eq!(
        write_body(&RestWrite::FrameRate("59.94".into())).unwrap(),
        r#"{"frameRate":"59.94"}"#
    );
    assert_eq!(
        write_body(&RestWrite::ApertureStop(5.6)).unwrap(),
        r#"{"apertureStop":5.6}"#
    );
    assert_eq!(
        write_body(&RestWrite::ShutterAngle(18000)).unwrap(),
        r#"{"shutterAngle":18000}"#
    );
    assert_eq!(write_body(&RestWrite::AutoWhiteBalance), None);
}

#[test]
fn env_parsers_keep_their_rules_809_1229() {
    assert_eq!(parse_capture_fps_env(Some("60".into())), Some(60));
    assert_eq!(parse_capture_fps_env(Some("59.94".into())), Some(60));
    assert_eq!(parse_capture_fps_env(Some("inf".into())), None);
    assert_eq!(parse_capture_fps_env(Some("0".into())), None);
    assert_eq!(parse_capture_fps_env(None), None);
    assert_eq!(
        parse_min_read_interval_env(Some("15000".into())),
        Some(15_000)
    );
    assert_eq!(parse_min_read_interval_env(Some("0".into())), None);
    assert_eq!(parse_min_read_interval_env(Some("3600001".into())), None);
    assert_eq!(parse_min_read_interval_env(None), None);
    assert!(!read_is_fresh(None, 0, 10_000));
    assert!(read_is_fresh(Some(0), 9_999, 10_000));
    assert!(!read_is_fresh(Some(0), 10_000, 10_000));
    assert!(read_is_fresh(Some(100), 50, 10_000));
}

#[test]
fn the_slash30_peer_is_the_fallback_address_6() {
    let out = "7: enx7c2e0d123456    inet 172.25.77.162/30 brd 172.25.77.163 scope global dynamic enx7c2e0d123456\\       valid_lft 86000sec preferred_lft 86000sec\n";
    assert_eq!(
        parse_inet(out),
        Some(("172.25.77.162".parse().unwrap(), 30))
    );
    assert_eq!(parse_inet(""), None);
    assert_eq!(parse_inet("7: enx inet6 fe80::1/64 scope link"), None);
    let p = |a: &str, n| peer_of(a.parse().unwrap(), n).map(|ip| ip.to_string());
    assert_eq!(p("172.25.77.162", 30).as_deref(), Some("172.25.77.161"));
    assert_eq!(p("172.25.77.161", 30).as_deref(), Some("172.25.77.162"));
    assert_eq!(p("172.25.77.160", 30), None, "the network address");
    assert_eq!(p("172.25.77.163", 30), None, "the broadcast address");
    assert_eq!(p("10.0.0.0", 31).as_deref(), Some("10.0.0.1"));
    assert_eq!(p("10.77.9.20", 24), None, "not a point-to-point subnet");
}

#[test]
fn a_camera_that_stops_answering_goes_offline_within_the_deadline_6() {
    let cam = FakeCamera::start(Cam::default());
    let clock = FakeClock::default();
    let s = Arc::new(session(&cam).with_clock(Box::new(clock.clone())));
    assert!(s.read_state().online);
    cam.cam()
        .get_delay
        .insert("/lens/iris".into(), Duration::from_secs(10));
    clock.0.store(20_000, Ordering::SeqCst);
    let started = std::time::Instant::now();
    let st = s.read_state();
    let took = started.elapsed();
    assert!(!st.online, "a read that gets no answer is offline");
    assert_eq!(
        st.camera.as_deref(),
        Some("Blackmagic Pocket Cinema Camera 4K")
    );
    assert!(
        took < Duration::from_secs(4),
        "bounded by the 1 s request deadline: {took:?}"
    );
    // The link was dropped: the next read probes the camera again, and finds it.
    cam.cam().get_delay.clear();
    clock.0.store(40_000, Ordering::SeqCst);
    assert!(s.read_state().online);
    assert_eq!(cam.gets_of("/system/product"), 2);
}

#[test]
fn a_set_that_arrives_during_the_read_back_waits_for_it_6() {
    let cam = FakeCamera::start(Cam::default());
    let s = Arc::new(session(&cam).with_aperture_readback_delay_ms(2000));
    let first = {
        let s = s.clone();
        std::thread::spawn(move || {
            s.submit(&SetRequest {
                aperture_norm: Some(1.0),
                ..Default::default()
            })
        })
    };
    // The iris PUT has landed; this arrives inside the 2 s read-back wait.
    wait_for_put(&cam, "/lens/iris");
    let q = s
        .submit(&SetRequest {
            iso: Some(1600),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        q,
        ApplyOutcome::Queued,
        "the gate stays shut through the read-back"
    );
    let (count, st) = applied(first.join().unwrap().unwrap());
    assert_eq!(count, 2, "the late SET was written in a second round");
    assert_eq!(st.params.iso, Some(1600));
    assert_eq!(
        cam.puts().into_iter().map(|(p, _)| p).collect::<Vec<_>>(),
        vec!["/lens/iris".to_string(), "/video/iso".to_string()]
    );
}

#[test]
fn a_late_set_is_never_written_through_a_camera_that_went_offline_6() {
    let cam = FakeCamera::start(Cam::default());
    let s = Arc::new(session(&cam).with_aperture_readback_delay_ms(1500));
    let first = {
        let s = s.clone();
        std::thread::spawn(move || {
            s.submit(&SetRequest {
                aperture_norm: Some(1.0),
                ..Default::default()
            })
        })
    };
    wait_for_put(&cam, "/lens/iris");
    let q = s
        .submit(&SetRequest {
            iso: Some(1600),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(q, ApplyOutcome::Queued);
    // The camera stops answering before the read-back runs.
    cam.cam()
        .get_delay
        .insert("/video/iso".into(), Duration::from_secs(10));
    let (count, st) = applied(first.join().unwrap().unwrap());
    assert_eq!(count, 1, "only the caller's own write");
    assert!(!st.online);
    assert_eq!(
        cam.puts().into_iter().map(|(p, _)| p).collect::<Vec<_>>(),
        vec!["/lens/iris".to_string()],
        "the late SET was dropped, not written blind"
    );
    // The gate is idle again.
    cam.cam().get_delay.clear();
    assert!(s
        .submit(&SetRequest {
            iso: Some(800),
            ..Default::default()
        })
        .is_ok());
}

#[test]
fn a_drain_over_its_settle_budget_skips_the_iris_wait_6() {
    let cam = FakeCamera::start(Cam::default());
    let s = Arc::new(
        session(&cam)
            .with_aperture_readback_delay_ms(3000)
            .with_drain_settle_budget_ms(0),
    );
    let started = std::time::Instant::now();
    let (_, st) = applied(
        s.submit(&SetRequest {
            aperture_norm: Some(1.0),
            ..Default::default()
        })
        .unwrap(),
    );
    assert!(
        started.elapsed() < Duration::from_millis(2500),
        "no 3 s iris wait past the budget: {:?}",
        started.elapsed()
    );
    assert!(st.online);
}

/// Presses +/- (`apertureStep`) from one end of the lens to the other and back, and checks every
/// press lands on EXACTLY the next native step: no step skipped, none repeated.
fn walk_the_lens(cam_model: Cam) {
    let stops = cam_model.lens_stops.clone();
    let cam = FakeCamera::start(cam_model);
    let s = Arc::new(session(&cam).with_iris_probe_wait_ms(30));
    let fnum = |st: &bkshading_proto::wire::RelayState| {
        let f = 2f64.powf(st.params.aperture_av.unwrap() / 2.0);
        (f * 100.0).round() / 100.0
    };
    assert_eq!(fnum(&s.read_state()), stops[0], "starts wide open");
    let press = |dir: i64| {
        let (_, st) = applied(
            s.submit(&SetRequest {
                aperture_step: Some(dir),
                ..Default::default()
            })
            .unwrap(),
        );
        assert!(st.not_applied.is_empty(), "{:?}", st.not_applied);
        fnum(&st)
    };
    for want in &stops[1..] {
        assert_eq!(press(1), *want, "+ from the previous step");
    }
    for want in stops[..stops.len() - 1].iter().rev() {
        assert_eq!(press(-1), *want, "- from the previous step");
    }
    // At the ends a press stays put (the lens has no further step), never an error.
    assert_eq!(press(-1), stops[0]);
}

#[test]
fn one_press_is_the_next_step_of_a_zoom_anchored_third_stop_lens_6() {
    walk_the_lens(support::zoom_third_stop_lens());
}

#[test]
fn one_press_is_the_next_step_of_a_sixth_stop_lens_6() {
    walk_the_lens(support::sixth_stop_lens());
}

fn press(s: &Arc<CameraSession>, dir: i64) -> bkshading_proto::wire::RelayState {
    applied(
        s.submit(&SetRequest {
            aperture_step: Some(dir),
            ..Default::default()
        })
        .unwrap(),
    )
    .1
}

fn fstop(st: &bkshading_proto::wire::RelayState) -> f64 {
    let f = 2f64.powf(st.params.aperture_av.unwrap() / 2.0);
    (f * 100.0).round() / 100.0
}

#[test]
fn an_iris_that_follows_no_probe_gives_up_bounded_and_says_so_6() {
    let cam = FakeCamera::start(Cam {
        iris_ignores_writes: true,
        ..support::zoom_third_stop_lens()
    });
    let s = Arc::new(session(&cam).with_iris_probe_wait_ms(30));
    let st = press(&s, 1);
    assert_eq!(fstop(&st), 4.4);
    assert_eq!(st.not_applied, vec!["apertureStep".to_string()]);
    assert_eq!(
        cam.puts().len(),
        6,
        "MAX_DEAD_APERTURE_PROBES writes, never the 64 of a whole lens"
    );
    // The refusal is in the cached state too, so the next /api/state poll still carries it.
    assert_eq!(s.read_state().not_applied, vec!["apertureStep".to_string()]);
}

#[test]
fn an_iris_the_lens_says_it_cannot_drive_is_not_probed_6() {
    let cam = FakeCamera::start(Cam {
        iris_controllable: false,
        ..support::zoom_third_stop_lens()
    });
    let s = Arc::new(session(&cam).with_iris_probe_wait_ms(30));
    let st = press(&s, 1);
    assert!(cam.puts().is_empty());
    assert_eq!(st.not_applied, vec!["apertureStep".to_string()]);
}

#[test]
fn a_slow_iris_is_waited_for_and_still_lands_on_the_next_step_6() {
    // The iris reports its new position 60 ms after the write; the probe waits up to 200 ms.
    let cam = FakeCamera::start(Cam {
        iris_latency: Duration::from_millis(60),
        ..support::sixth_stop_lens()
    });
    let s = Arc::new(session(&cam).with_iris_probe_wait_ms(200));
    for want in [2.9, 3.1, 3.2, 3.3, 3.5] {
        assert_eq!(fstop(&press(&s, 1)), want);
    }
    assert_eq!(fstop(&press(&s, -1)), 3.3);
}

#[test]
fn presses_queued_past_the_answer_budget_finish_in_the_background_in_order_6() {
    // The iris takes 300 ms to land (under the 1 s REST timeout), which keeps the first press in
    // flight long enough for the two queued submits on a loaded runner.
    let cam = FakeCamera::start(Cam {
        iris_latency: Duration::from_millis(300),
        ..support::zoom_third_stop_lens()
    });
    let s = Arc::new(
        session(&cam)
            .with_iris_probe_wait_ms(600)
            .with_min_read_interval_ms(1),
    );
    let first = {
        let s = s.clone();
        std::thread::spawn(move || press(&s, 1))
    };
    wait_for_put(&cam, "/lens/iris");
    for _ in 0..2 {
        let q = s
            .submit(&SetRequest {
                aperture_step: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(q, ApplyOutcome::Queued);
    }
    // The first caller is answered after its own press, not after all three.
    assert_eq!(fstop(&first.join().unwrap()), 4.8);
    // The two queued presses still run, in order, one native step each: 5.2, then 5.6.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let st = s.read_state();
        if st.online && fstop(&st) == 5.6 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the background drain never reached 5.6"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // And the gate is free again afterwards.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match s
            .submit(&SetRequest {
                iso: Some(800),
                ..Default::default()
            })
            .unwrap()
        {
            ApplyOutcome::Applied { .. } => break,
            ApplyOutcome::Queued => {
                assert!(std::time::Instant::now() < deadline, "gate never freed");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

#[test]
fn a_pocket_6k_at_fw_8_6_is_detected_read_set_and_stepped_6() {
    // The coordinator's live endpoint subset of SNV cam3 (no system/product, no lists, no
    // descriptions), found through discovery so the USB product string names it.
    let cam = FakeCamera::start(support::pocket_6k_fw86());
    let stops = cam.cam().lens_stops.clone();
    let net = NetTree::new("6k86");
    net.iface("enx7c2e0d6k0086", "cdc_ncm", 11);
    net.usb_product("enx7c2e0d6k0086", "Pocket Cinema Camera 6K");
    net.lease(11, "SERVER_ADDRESS=127.0.0.1\n");
    let s = Arc::new(
        CameraSession::new(
            CameraLocator::Discover {
                paths: net.paths(),
                port: cam.addr.port(),
            },
            VERSION,
        )
        .with_aperture_readback_delay_ms(0)
        .with_iris_probe_wait_ms(30),
    );
    // detect: confirmed by lens/iris, named by the USB product string.
    assert_eq!(s.detect().as_deref(), Some("Pocket Cinema Camera 6K"));
    // state: every value read, every missing list replaced by a sane default.
    let st = s.read_state();
    assert!(st.online);
    assert_eq!(st.params.iso, Some(1000));
    assert_eq!(st.params.shutter, Some(100));
    assert_eq!(st.params.kelvin, Some(4850));
    assert_eq!(st.params.fps100, Some(6000));
    assert_eq!(fstop(&st), 9.5);
    let caps = st.caps.clone().unwrap();
    assert!(
        caps.iso_choices.contains(&1000) && caps.iso_choices.len() > 10,
        "{:?}",
        caps.iso_choices
    );
    assert!(
        caps.shutter_choices.contains(&100),
        "{:?}",
        caps.shutter_choices
    );
    assert_eq!((caps.kelvin_min, caps.kelvin_max), (2500, 10000));
    assert!(caps.fnumber_choices.len() > 5, "the slider gets a range");
    assert!(caps.fps_choices.contains(&6000));
    // set: plain values land.
    let (_, st) = applied(
        s.submit(&SetRequest {
            iso: Some(800),
            kelvin: Some(5600),
            shutter: Some(50),
            ..Default::default()
        })
        .unwrap(),
    );
    assert_eq!(
        (st.params.iso, st.params.kelvin, st.params.shutter),
        (Some(800), Some(5600), Some(50))
    );
    assert!(st.not_applied.is_empty(), "{:?}", st.not_applied);
    // step: one press = the next native step, down to the end of the lens and back up, quietly
    // stopping at both ends although the lens range is unknown.
    let at = stops.iter().position(|f| *f == 9.5).unwrap();
    for want in stops[..at].iter().rev() {
        assert_eq!(fstop(&press(&s, -1)), *want);
    }
    let st = press(&s, -1);
    assert_eq!(fstop(&st), 4.0, "the end of the lens");
    assert!(
        st.not_applied.is_empty(),
        "the end of an unknown range is quiet: {:?}",
        st.not_applied
    );
    for want in &stops[1..] {
        assert_eq!(fstop(&press(&s, 1)), *want);
    }
}

#[test]
fn a_fixed_address_camera_without_system_product_is_a_blackmagic_camera_6() {
    let cam = FakeCamera::start(support::pocket_6k_fw86());
    let s = Arc::new(session(&cam));
    assert_eq!(s.detect().as_deref(), Some("Blackmagic camera"));
    assert!(s.read_state().online);
    assert_eq!(
        cam.gets_of("/system/product"),
        1,
        "probed once, then the link is kept"
    );
}

#[test]
fn a_probe_the_camera_refuses_as_out_of_range_is_the_end_of_the_lens_6() {
    let mut c = support::pocket_6k_fw86();
    c.refuse
        .insert("/lens/iris".into(), (400, "Bad Request".into()));
    let cam = FakeCamera::start(c);
    let s = Arc::new(session(&cam).with_iris_probe_wait_ms(30));
    let st = press(&s, 1);
    assert_eq!(fstop(&st), 9.5);
    assert!(st.not_applied.is_empty(), "{:?}", st.not_applied);
    assert_eq!(
        cam.puts().len(),
        1,
        "no widening after an out-of-range answer"
    );
}

#[test]
fn something_that_is_not_a_camera_is_not_taken_for_one_6() {
    let cam = FakeCamera::start(Cam {
        not_found: ["/system/product", "/lens/iris"].map(String::from).to_vec(),
        not_implemented: vec!["/system".into()],
        ..Cam::default()
    });
    let s = Arc::new(session(&cam));
    assert_eq!(s.detect(), None);
    assert!(!s.read_state().online);
}

#[test]
fn a_lens_iris_answer_without_an_f_stop_or_an_html_system_page_is_no_camera_6() {
    let mut c = support::pocket_6k_fw86();
    c.get_override
        .insert("/lens/iris".into(), (200, "{}".into()));
    c.get_override.insert(
        "/system".into(),
        (200, "<html><body>router login</body></html>".into()),
    );
    let cam = FakeCamera::start(c);
    let s = Arc::new(session(&cam));
    assert_eq!(s.detect(), None);
}

#[test]
fn a_booting_camera_answering_500_is_probed_again_not_named_by_default_6() {
    let mut c = Cam::default();
    c.get_override
        .insert("/system/product".into(), (503, "starting".into()));
    let cam = FakeCamera::start(c);
    let s = Arc::new(session(&cam));
    assert_eq!(
        s.detect(),
        None,
        "a 503 is not 'not served': no fallback name"
    );
    cam.cam().get_override.clear();
    assert_eq!(
        s.detect().as_deref(),
        Some("Blackmagic Pocket Cinema Camera 4K"),
        "the next probe names it properly"
    );
}

#[test]
fn without_a_lens_range_the_slider_keeps_every_f_stop_seen_6() {
    let mut c = support::pocket_6k_fw86();
    c.lens_stops = vec![1.8, 2.0, 2.2, 2.5, 2.8];
    c.aperture = 1.8;
    let cam = FakeCamera::start(c);
    let s = Arc::new(session(&cam).with_iris_probe_wait_ms(30));
    let grid0 = s.read_state().caps.unwrap().fnumber_choices;
    assert_eq!(grid0.first(), Some(&1.8));
    let st = press(&s, 1);
    assert_eq!(fstop(&st), 2.0);
    let grid1 = st.caps.unwrap().fnumber_choices;
    assert_eq!(
        grid1, grid0,
        "the grid does not shrink once the iris left 1.8"
    );
}

#[test]
fn a_slider_position_past_an_unknown_lens_end_is_not_applied_not_an_error_6() {
    let mut c = support::pocket_6k_fw86();
    c.refuse
        .insert("/lens/iris".into(), (400, "Bad Request".into()));
    let cam = FakeCamera::start(c);
    let s = Arc::new(session(&cam));
    let (_, st) = applied(
        s.submit(&SetRequest {
            aperture_norm: Some(0.0),
            iso: Some(800),
            ..Default::default()
        })
        .unwrap(),
    );
    assert_eq!(st.not_applied, vec!["apertureNorm".to_string()]);
    assert_eq!(st.params.iso, Some(800), "the rest of the SET still landed");
}

// --- shading#6 (0.2.3): the relay knows the lens's ends from `lens/iris normalised` -------------
// Live on cam3 (Pocket 6K, fw 8.6, 24 mm, 10.11.2026): `lens/iris/description` is 404, a "-" at the
// widest stop 4.0 was probed ~7.7 s ("no move after the probe limit and no lens range") and the
// panel snapped back. `GET lens/iris` answers `normalised` 0.0 there: the iris's place in the lens's
// range. The relay passes it on as `params.irisRangePos`, and a step toward an end the iris is
// already at is refused at once, with no probe.

/// `params.irisRangePos` on the wire (what the service and the panel read).
fn iris_range_pos(st: &bkshading_proto::wire::RelayState) -> serde_json::Value {
    serde_json::to_value(&st.params).unwrap()["irisRangePos"].clone()
}

fn iris_puts(cam: &FakeCamera) -> usize {
    cam.puts().iter().filter(|(p, _)| p == "/lens/iris").count()
}

#[test]
fn the_iris_range_position_is_a_read_param_6() {
    let cam = FakeCamera::start(support::pocket_6k_fw86());
    let s = Arc::new(session(&cam));
    let st = s.read_state();
    let want = support::lens_position(&cam.cam().lens_stops, 9.5);
    let got = iris_range_pos(&st)
        .as_f64()
        .expect("irisRangePos is a number");
    assert!((got - want).abs() < 1e-9, "{got} != {want}");
    assert!(got > 0.0 && got < 1.0, "9.5 lies between the ends: {got}");
    // A SET's read-back is a fresh camera read (a plain read may answer from the read floor's cache).
    let reread = |s: &Arc<CameraSession>| {
        applied(
            s.submit(&SetRequest {
                iso: Some(800),
                ..Default::default()
            })
            .unwrap(),
        )
        .1
    };
    cam.cam().aperture = 4.0;
    assert_eq!(iris_range_pos(&reread(&s)), json!(0.0), "wide open");
    cam.cam().aperture = 11.0;
    let st = reread(&s);
    assert_eq!(iris_range_pos(&st), json!(1.0), "closed");
}

#[test]
fn a_camera_without_normalised_reports_no_iris_range_position_6() {
    let mut c = support::pocket_6k_fw86();
    c.iris_position = IrisPosition::Absent;
    let cam = FakeCamera::start(c);
    let s = Arc::new(session(&cam));
    let st = s.read_state();
    assert!(st.online);
    assert_eq!(iris_range_pos(&st), serde_json::Value::Null);
}

/// A step toward the end the iris is at: refused at once (`notApplied: apertureStep`), no PUT to
/// `lens/iris`, and fast (the old way was 6 dead probes of 1.2 s each).
fn assert_end_refused_at_once(aperture: f64, dir: i64) {
    let mut c = support::pocket_6k_fw86();
    c.aperture = aperture;
    let cam = FakeCamera::start(c);
    // The production probe wait: a probe would cost >= 1.2 s, so the timing below catches one.
    let s = Arc::new(session(&cam).with_iris_probe_wait_ms(IRIS_PROBE_WAIT_MS));
    let _ = s.read_state();
    cam.clear_seen();
    let started = std::time::Instant::now();
    let st = press(&s, dir);
    let took = started.elapsed();
    assert_eq!(iris_puts(&cam), 0, "no probe written: {:?}", cam.puts());
    assert_eq!(st.not_applied, vec!["apertureStep".to_string()]);
    assert_eq!(fstop(&st), aperture, "the iris stays at the end");
    assert!(
        took < Duration::from_millis(IRIS_PROBE_WAIT_MS),
        "answered at once, not after a probe: {took:?}"
    );
}

#[test]
fn a_minus_press_at_the_wide_end_is_refused_at_once_without_a_probe_6() {
    assert_end_refused_at_once(4.0, -1);
}

#[test]
fn a_plus_press_at_the_closed_end_is_refused_at_once_without_a_probe_6() {
    assert_end_refused_at_once(11.0, 1);
}

#[test]
fn a_press_away_from_the_end_still_walks_the_lens_6() {
    let mut c = support::pocket_6k_fw86();
    c.aperture = 4.0;
    let cam = FakeCamera::start(c);
    let s = Arc::new(session(&cam).with_iris_probe_wait_ms(30));
    let st = press(&s, 1);
    assert_eq!(fstop(&st), 4.4, "+ from the wide end is the next step");
    assert!(st.not_applied.is_empty(), "{:?}", st.not_applied);
    let p = iris_range_pos(&st).as_f64().unwrap();
    assert!(p > 0.0 && p < 1.0, "{p}");
}

#[test]
fn without_normalised_a_minus_at_the_wide_end_falls_back_to_probing_6() {
    let mut c = support::pocket_6k_fw86();
    c.aperture = 4.0;
    c.iris_position = IrisPosition::Absent;
    let cam = FakeCamera::start(c);
    let s = Arc::new(session(&cam).with_iris_probe_wait_ms(30));
    let st = press(&s, -1);
    assert_eq!(fstop(&st), 4.0);
    assert!(
        iris_puts(&cam) > 0,
        "no position known: the probe finds the end"
    );
    assert!(st.not_applied.is_empty(), "{:?}", st.not_applied);
}
