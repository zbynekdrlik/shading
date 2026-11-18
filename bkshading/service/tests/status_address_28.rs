//! shading#28: the view carries the address the service dials without a registration, so the
//! panel's status line can show `address · bez ohlásenia` for a camera that never announced.

use bkshading::aggregator::camera_view;
use bkshading::config::ServiceConfig;

const CONFIG: &str = "\
[[camera]]
id = \"cam1\"
label = \"Cam 1\"
transport = \"cambox-relay\"
address = \"register:cam1\"
fallback_address = \"10.77.8.61:8771\"

[[camera]]
id = \"cam4\"
label = \"Cam 4\"
transport = \"cambox-relay\"
address = \"register:cam4\"

[[camera]]
id = \"cam6\"
label = \"Cam 6\"
transport = \"cambox-relay\"
address = \"10.77.8.66:8771\"
";

#[test]
fn the_view_names_the_static_address_the_service_dials_28() {
    let cfg = ServiceConfig::from_toml_str(CONFIG).unwrap();
    let addr = |i: usize| camera_view(&cfg.cameras[i], None, false).address;
    assert_eq!(
        addr(0).as_deref(),
        Some("10.77.8.61:8771"),
        "register: + fallback"
    );
    assert_eq!(
        addr(1),
        None,
        "register: without a fallback has no static address"
    );
    assert_eq!(
        addr(2).as_deref(),
        Some("10.77.8.66:8771"),
        "a plain address"
    );
}

#[test]
fn an_older_service_view_without_the_address_still_parses_28() {
    let json = serde_json::json!({
        "id": "cam1", "label": "Cam 1", "transport": "cambox-relay", "hasPreview": false,
        "reachable": false, "grabFps": null, "fpsSync": "unknown", "state": null
    });
    let view: bkshading_proto::wire::CameraView = serde_json::from_value(json).unwrap();
    assert_eq!(view.address, None);
}
