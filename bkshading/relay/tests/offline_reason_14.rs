//! shading#14: the relay says WHY its camera is offline (`offlineReason` in `/api/state`), so the
//! panel can tell the operator what to switch on. These run the real session, client and discovery
//! against the fake camera server (`support/`) and a fake sysfs + lease tree.

mod support;

use std::os::unix::fs::symlink;

use bkshading_proto::wire::OfflineReason;
use bkshading_relay::transport::{CameraAddr, CameraLocator, CameraSession, DiscoverPaths};
use support::{Cam, FakeCamera};

const VERSION: &str = "0.2.6-test";

fn fixed_port(port: u16) -> CameraLocator {
    CameraLocator::Fixed(CameraAddr {
        host: "127.0.0.1".into(),
        port,
    })
}

/// A session that reads the camera on every `read_state` (no read floor), so a test can change the
/// camera between two reads.
fn session(locator: CameraLocator) -> CameraSession {
    CameraSession::new(locator, VERSION).with_min_read_interval_ms(0)
}

#[test]
fn a_live_camera_reports_no_offline_reason_14() {
    let cam = FakeCamera::start(Cam::default());
    let st = session(fixed_port(cam.addr.port())).read_state();
    assert!(st.online);
    assert_eq!(st.offline_reason, None);
    let json = serde_json::to_value(&st).unwrap();
    assert_eq!(json["offlineReason"], serde_json::Value::Null, "{json}");
}

/// PP cam7 before the owner switched on Web media manager: `system/product` answered 307 (and the
/// redirect target 404). The camera is on the link; its REST API is off.
#[test]
fn a_redirect_from_the_camera_is_rest_off_14() {
    let mut c = Cam::default();
    c.get_override
        .insert("/system/product".into(), (307, String::new()));
    let cam = FakeCamera::start(c);
    let s = session(fixed_port(cam.addr.port()));
    let st = s.read_state();
    assert!(!st.online);
    assert_eq!(st.offline_reason, Some(OfflineReason::RestOff));
    let json = serde_json::to_value(&st).unwrap();
    assert_eq!(json["offlineReason"], "rest-off", "{json}");

    // Web media manager switched on: the next read finds the camera, the reason is gone.
    cam.cam().get_override.clear();
    let st = s.read_state();
    assert!(st.online, "camera found once REST answers");
    assert_eq!(st.offline_reason, None);
}

/// 404 on every probe (`system/product`, `lens/iris`, `/system`): something answers HTTP, but not
/// the camera REST API.
#[test]
fn not_found_everywhere_is_rest_off_14() {
    let c = Cam {
        not_found: vec![
            "/system/product".into(),
            "/lens/iris".into(),
            "/system".into(),
        ],
        ..Cam::default()
    };
    let cam = FakeCamera::start(c);
    let st = session(fixed_port(cam.addr.port())).read_state();
    assert!(!st.online);
    assert_eq!(st.offline_reason, Some(OfflineReason::RestOff));
}

/// A web page instead of the REST JSON is not a camera answer either.
#[test]
fn an_html_page_is_rest_off_14() {
    let mut c = Cam::default();
    c.get_override.insert(
        "/system/product".into(),
        (200, "<html><body>media manager</body></html>".into()),
    );
    let cam = FakeCamera::start(c);
    let st = session(fixed_port(cam.addr.port())).read_state();
    assert!(!st.online);
    assert_eq!(st.offline_reason, Some(OfflineReason::RestOff));
}

/// A camera still booting answers 503: it is there, not ready, never "REST off".
#[test]
fn a_booting_camera_is_not_answering_14() {
    let mut c = Cam::default();
    c.get_override
        .insert("/system/product".into(), (503, "booting".into()));
    let cam = FakeCamera::start(c);
    let st = session(fixed_port(cam.addr.port())).read_state();
    assert!(!st.online);
    assert_eq!(st.offline_reason, Some(OfflineReason::CameraNotAnswering));
}

/// Nothing listens at the camera's address (camera off, or nothing answers HTTP at all).
#[test]
fn a_refused_connection_is_not_answering_14() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let st = session(fixed_port(port)).read_state();
    assert!(!st.online);
    assert_eq!(st.offline_reason, Some(OfflineReason::CameraNotAnswering));
    let json = serde_json::to_value(&st).unwrap();
    assert_eq!(json["offlineReason"], "camera-not-answering", "{json}");
}

/// A fake `/sys/class/net` + networkd lease tree (the shape `rest_camera_6.rs` uses).
struct NetTree {
    root: std::path::PathBuf,
}

impl NetTree {
    fn new(tag: &str) -> NetTree {
        let root = std::env::temp_dir().join(format!(
            "bkshading-relay-offline14-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("net")).unwrap();
        std::fs::create_dir_all(root.join("leases")).unwrap();
        std::fs::create_dir_all(root.join("drivers/cdc_ncm")).unwrap();
        NetTree { root }
    }

    /// A Blackmagic (`1edb`) `cdc_ncm` interface, like the camera's USB-C link.
    fn camera_iface(&self, name: &str, ifindex: u32) {
        let usb_dev = self.root.join("usb").join(name);
        let usb_if = usb_dev.join(format!("{name}:1.0"));
        std::fs::create_dir_all(&usb_if).unwrap();
        symlink(self.root.join("drivers/cdc_ncm"), usb_if.join("driver")).unwrap();
        let dir = self.root.join("net").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        symlink(&usb_if, dir.join("device")).unwrap();
        std::fs::write(dir.join("ifindex"), format!("{ifindex}\n")).unwrap();
        std::fs::write(usb_dev.join("idVendor"), "1edb\n").unwrap();
    }

    fn paths(&self) -> DiscoverPaths {
        DiscoverPaths {
            sysfs_net: self.root.join("net"),
            leases_dir: self.root.join("leases"),
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
fn no_usb_link_is_no_camera_link_then_a_link_without_dhcp_is_not_answering_14() {
    let net = NetTree::new("links");
    let discover = || CameraLocator::Discover {
        paths: net.paths(),
        port: 80,
    };
    let st = session(discover()).read_state();
    assert!(!st.online);
    assert_eq!(
        st.offline_reason,
        Some(OfflineReason::NoCameraLink),
        "no Blackmagic cdc_ncm interface at all"
    );
    let json = serde_json::to_value(&st).unwrap();
    assert_eq!(json["offlineReason"], "no-camera-link", "{json}");

    // The camera is on USB, its link is up, but it has served no DHCP address yet.
    net.camera_iface("enx7c2e0d123456", 9);
    let st = session(discover()).read_state();
    assert_eq!(st.offline_reason, Some(OfflineReason::CameraNotAnswering));
}

#[test]
fn a_link_whose_camera_redirects_is_rest_off_14() {
    let mut c = Cam::default();
    c.get_override
        .insert("/system/product".into(), (307, String::new()));
    let cam = FakeCamera::start(c);
    let net = NetTree::new("redirect");
    net.camera_iface("enx7c2e0d123456", 7);
    std::fs::write(
        net.root.join("leases/7"),
        "ADDRESS=127.0.0.2\nSERVER_ADDRESS=127.0.0.1\n",
    )
    .unwrap();
    let st = session(CameraLocator::Discover {
        paths: net.paths(),
        port: cam.addr.port(),
    })
    .read_state();
    assert!(!st.online);
    assert_eq!(st.offline_reason, Some(OfflineReason::RestOff));
}

/// A 6K at fw 8.6 serves neither `system/product` nor `lens/iris/description`; while it boots,
/// `/system` answers 503. That camera is not ready yet, never "REST off".
#[test]
fn a_booting_camera_without_product_is_not_answering_14() {
    let mut c = Cam {
        not_found: vec!["/system/product".into(), "/lens/iris".into()],
        ..Cam::default()
    };
    c.get_override
        .insert("/system".into(), (503, "booting".into()));
    let cam = FakeCamera::start(c);
    let st = session(fixed_port(cam.addr.port())).read_state();
    assert!(!st.online);
    assert_eq!(st.offline_reason, Some(OfflineReason::CameraNotAnswering));
}

/// A camera found before stops answering in the middle of a read (a GET times out): the relay
/// keeps its name and says it is not answering.
#[test]
fn a_held_camera_that_stops_answering_is_not_answering_14() {
    let cam = FakeCamera::start(Cam::default());
    let s = session(fixed_port(cam.addr.port()));
    assert!(s.read_state().online);
    cam.cam().get_delay.insert(
        "/video/iso".into(),
        bkshading_relay::transport::REST_TIMEOUT + std::time::Duration::from_millis(500),
    );
    let st = s.read_state();
    assert!(!st.online);
    assert_eq!(st.offline_reason, Some(OfflineReason::CameraNotAnswering));
    assert_eq!(
        st.camera.as_deref(),
        Some("Blackmagic Pocket Cinema Camera 4K")
    );
}

/// Web media manager switched OFF while the relay holds the camera: every GET answers 307. The
/// camera must not stay "online" with no values; the relay reports REST off at once.
#[test]
fn web_media_manager_switched_off_on_a_held_camera_is_rest_off_14() {
    let cam = FakeCamera::start(Cam::default());
    let s = session(fixed_port(cam.addr.port()));
    assert!(s.read_state().online);
    {
        let mut c = cam.cam();
        for p in bkshading_proto::rest::READ_ENDPOINTS
            .iter()
            .chain([&bkshading_proto::rest::PRODUCT, &"/system"])
        {
            c.get_override.insert(p.to_string(), (307, String::new()));
        }
    }
    let st = s.read_state();
    assert!(!st.online, "no endpoint answered: not controllable");
    assert_eq!(st.offline_reason, Some(OfflineReason::RestOff));
    // The next read looks the camera up again and still finds REST off.
    let st = s.read_state();
    assert!(!st.online);
    assert_eq!(st.offline_reason, Some(OfflineReason::RestOff));
    // Switched back on: online again.
    cam.cam().get_override.clear();
    assert!(s.read_state().online);
}

/// Two camera links: one address refuses, the other redirects. The redirect says more (a camera is
/// there, its REST is off), whatever order the links are tried in.
#[test]
fn of_two_links_the_redirect_wins_14() {
    let mut c = Cam::default();
    c.get_override
        .insert("/system/product".into(), (307, String::new()));
    let cam = FakeCamera::start(c);
    let net = NetTree::new("two");
    net.camera_iface("enxa00000000001", 7);
    std::fs::write(net.root.join("leases/7"), "SERVER_ADDRESS=127.0.0.5\n").unwrap();
    net.camera_iface("enxb00000000002", 8);
    std::fs::write(net.root.join("leases/8"), "SERVER_ADDRESS=127.0.0.1\n").unwrap();
    let st = session(CameraLocator::Discover {
        paths: net.paths(),
        port: cam.addr.port(),
    })
    .read_state();
    assert!(!st.online);
    assert_eq!(st.offline_reason, Some(OfflineReason::RestOff));
}
