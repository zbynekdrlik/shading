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
    camera_links, parse_camera_addr, parse_capture_fps_env, parse_lease,
    parse_min_read_interval_env, read_is_fresh, ApplyOutcome, CameraAddr, CameraLocator,
    CameraSession, DiscoverPaths, MonoClock, CAMERA_HTTP_PORT,
};
use serde_json::json;
use support::{Cam, FakeCamera};

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
    assert!(st.caps.unwrap().iso_choices.is_empty());
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
    let s = CameraSession::new(
        CameraLocator::Fixed(CameraAddr {
            host: "127.0.0.1".into(),
            port,
        }),
        VERSION,
    )
    .with_capture_fps(Some(60));
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
    let s = session(&cam)
        .with_min_read_interval_ms(10_000)
        .with_clock(Box::new(clock.clone()));
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
    let s = session(&cam);
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
    let s = session(&cam);
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
    let s = session(&cam);
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
    let s = session(&cam);
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
    let s = session(&cam).with_clock(Box::new(clock));
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
    let s = session(&cam);
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
    let s = session(&cam);
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
        .insert("/video/iso".into(), Duration::from_millis(400));
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
    // Let the first SET reach its slow PUT, then send two more.
    std::thread::sleep(Duration::from_millis(150));
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

    fn iface(&self, name: &str, driver: &str, ifindex: u32) {
        let dir = self.root.join("net").join(name);
        std::fs::create_dir_all(dir.join("device")).unwrap();
        symlink(
            self.root.join("drivers").join(driver),
            dir.join("device/driver"),
        )
        .unwrap();
        std::fs::write(dir.join("ifindex"), format!("{ifindex}\n")).unwrap();
    }

    fn lease(&self, ifindex: u32, text: &str) {
        std::fs::write(self.root.join("leases").join(ifindex.to_string()), text).unwrap();
    }

    fn paths(&self) -> DiscoverPaths {
        DiscoverPaths {
            sysfs_net: self.root.join("net"),
            leases_dir: self.root.join("leases"),
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
    let links = camera_links(&net.paths());
    assert_eq!(links.len(), 1, "only the cdc_ncm interface: {links:?}");
    assert_eq!(links[0].iface, "enx7c2e0d123456");
    assert_eq!(links[0].camera_ip, Some("127.0.0.1".parse().unwrap()));
    let s = CameraSession::new(
        CameraLocator::Discover {
            paths: net.paths(),
            port: cam.addr.port(),
        },
        VERSION,
    );
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
    let s = CameraSession::new(
        CameraLocator::Discover {
            paths: net.paths(),
            port: CAMERA_HTTP_PORT,
        },
        VERSION,
    );
    assert!(!s.read_state().online, "no cdc_ncm interface at all");
    net.iface("enx7c2e0d123456", "cdc_ncm", 9);
    let links = camera_links(&net.paths());
    assert_eq!(
        links[0].camera_ip, None,
        "the link is there, DHCP has not run"
    );
    let s = CameraSession::new(
        CameraLocator::Discover {
            paths: net.paths(),
            port: CAMERA_HTTP_PORT,
        },
        VERSION,
    );
    assert!(!s.read_state().online);
    // A missing sysfs dir reads as no links, never a panic.
    let gone = DiscoverPaths {
        sysfs_net: net.root.join("nope"),
        leases_dir: net.root.join("nope"),
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
