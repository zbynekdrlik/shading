//! Service config-parsing + camera-view assembly tests — pure, no HTTP, no relay.

use bkshading::aggregator::camera_view;
use bkshading::config::ServiceConfig;
use bkshading_proto::wire::{RelayState, ShadingParams, Transport};

const EXAMPLE: &str = "\
bind = \"0.0.0.0:8770\"

[[camera]]
id = \"cam1\"
label = \"Cam 1\"
transport = \"cambox-relay\"
address = \"cam1.lan:8771\"
ndi_preview = \"CAM1 (usb)\"

[[camera]]
id = \"handheld-1\"
label = \"Handheld 1\"
transport = \"sbc-relay\"
address = \"10.77.9.60:8771\"
";

#[test]
fn parses_camera_list_with_transports() {
    let cfg = ServiceConfig::from_toml_str(EXAMPLE).expect("parse config");
    assert_eq!(cfg.bind, "0.0.0.0:8770");
    assert_eq!(cfg.cameras.len(), 2);

    let cam1 = &cfg.cameras[0];
    assert_eq!(cam1.id, "cam1");
    assert_eq!(cam1.transport, Transport::CamboxRelay);
    assert_eq!(cam1.address, "cam1.lan:8771");
    assert_eq!(cam1.ndi_preview.as_deref(), Some("CAM1 (usb)"));

    let handheld = &cfg.cameras[1];
    assert_eq!(handheld.transport, Transport::SbcRelay);
    assert_eq!(handheld.ndi_preview, None); // no preview -> params-only block
}

#[test]
fn empty_config_starts_clean() {
    let cfg = ServiceConfig::from_toml_str("").expect("empty parse");
    assert!(cfg.cameras.is_empty());
    assert_eq!(cfg.bind, "0.0.0.0:8770"); // default bind applies
}

#[test]
fn camera_with_ndi_preview_has_preview_block() {
    let cfg = ServiceConfig::from_toml_str(EXAMPLE).unwrap();
    let state = RelayState {
        online: true,
        camera: Some("Blackmagic Design Pocket Cinema Camera 4K".into()),
        params: ShadingParams {
            iso: Some(400),
            ..Default::default()
        },
        caps: None,
        fps_supported: true,
        version: "1.7.0-dev.516".into(),
    };
    let view = camera_view(&cfg.cameras[0], Some(state));
    assert!(
        view.has_preview,
        "cam1 has an NDI preview name -> preview block"
    );
    assert!(view.reachable);
    assert_eq!(view.state.unwrap().params.iso, Some(400));
}

#[test]
fn handheld_without_preview_is_params_only_and_offline_when_unreachable() {
    let cfg = ServiceConfig::from_toml_str(EXAMPLE).unwrap();
    let view = camera_view(&cfg.cameras[1], None);
    assert!(
        !view.has_preview,
        "handheld without NDI preview -> params-only block"
    );
    assert!(!view.reachable);
    assert!(view.state.is_none());
}

#[test]
fn served_index_injects_the_version() {
    // The panel header must carry the compiled version straight in the DOM
    // (version-on-dashboard), not a placeholder.
    let html = bkshading::http::rendered_index();
    assert!(
        html.contains(concat!("v", env!("CARGO_PKG_VERSION"))),
        "served index must show v{}",
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        !html.contains("{{VERSION}}"),
        "placeholder must be replaced"
    );
    assert!(html.contains("data-testid=\"version\""));
}
