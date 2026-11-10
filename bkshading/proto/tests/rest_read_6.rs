//! shading#6: the relay drives the camera through Blackmagic's REST API. These pin the pure half:
//! the JSON shapes the camera answers (from Blackmagic's REST API manual and the live cam5 reads),
//! their assembly into the wire `ShadingParams`/`CameraCaps`, and the planned writes.

use bkshading_proto::mapping::{fnumber_grid, parse_frame_rate100, THIRD_STOP_FNUMBERS};
use bkshading_proto::read::{
    fps_supported, frame_rate_choices, iris_moved, params_and_caps, plan_writes, RestWrite,
    ShutterMeasure, WriteBasis,
};
use bkshading_proto::rest::{self, Format, Product, RestRead, SupportedFormats};
use bkshading_proto::wire::SetRequest;

fn de<T: serde::de::DeserializeOwned>(json: &str) -> T {
    serde_json::from_str(json).unwrap_or_else(|e| panic!("{json}: {e}"))
}

/// The supported-format list of a 4K body: two resolutions, BRAW and ProRes.
const SUPPORTED_FORMATS: &str = r#"{"supportedFormats":[
  {"codecs":["BRaw:Q0","BRaw:5_1"],"frameRates":["23.98","24","25","29.97","30","50","59.94","60"],
   "maxOffSpeedFrameRate":60,"minOffSpeedFrameRate":5,
   "recordResolution":{"width":3840,"height":2160},"sensorResolution":{"width":4096,"height":2160}},
  {"codecs":["ProRes:HQ"],"frameRates":["23.98","24","25","120"],
   "recordResolution":{"width":3840,"height":2160}},
  {"codecs":["BRaw:Q0"],"frameRates":["23.98","24","25","29.97","30","50","59.94","60","120"],
   "recordResolution":{"width":1920,"height":1080}}
]}"#;

/// One full read cycle of cam5 (Pocket 4K, fw 9.8). The iris is the live read from the ticket.
fn cam5() -> RestRead {
    RestRead {
        iso: Some(de(r#"{"iso":400}"#)),
        supported_isos: Some(de(r#"{"supportedISOs":[1600,100,200,400,800,3200]}"#)),
        shutter: Some(de(
            r#"{"continuousShutterAutoExposure":false,"shutterSpeed":50}"#,
        )),
        supported_shutters: Some(de(r#"{"shutterSpeeds":[60,50,100,25,2000]}"#)),
        white_balance: Some(de(r#"{"whiteBalance":5600}"#)),
        white_balance_description: Some(de(r#"{"whiteBalance":{"min":2500,"max":10000}}"#)),
        tint: Some(de(r#"{"whiteBalanceTint":-3}"#)),
        iris: Some(de(
            r#"{"apertureNumber":432,"apertureStop":7.3,"continuousApertureAutoExposure":false,"normalised":0.3158}"#,
        )),
        iris_description: Some(de(
            r#"{"controllable":true,"apertureStop":{"min":4.4,"max":22}}"#,
        )),
        focus: Some(de(r#"{"normalised":0.42}"#)),
        format: Some(de(
            r#"{"codec":"BRaw:Q0","frameRate":"25","offSpeedEnabled":false,"offSpeedFrameRate":50,
                "minOffSpeedFrameRate":5,"maxOffSpeedFrameRate":60,
                "recordResolution":{"width":3840,"height":2160},
                "sensorResolution":{"width":4096,"height":2160}}"#,
        )),
        supported_formats: Some(de(SUPPORTED_FORMATS)),
    }
}

#[test]
fn every_endpoint_lives_under_the_v1_prefix_6() {
    assert_eq!(rest::API_PREFIX, "/control/api/v1");
    assert_eq!(rest::IRIS, "/lens/iris");
    assert_eq!(rest::ISO, "/video/iso");
    assert_eq!(rest::SHUTTER, "/video/shutter");
    assert_eq!(rest::WHITE_BALANCE, "/video/whiteBalance");
    assert_eq!(rest::TINT, "/video/whiteBalanceTint");
    assert_eq!(rest::FOCUS, "/lens/focus");
    assert_eq!(rest::FORMAT, "/system/format");
    assert_eq!(rest::PRODUCT, "/system/product");
    assert_eq!(rest::READ_ENDPOINTS.len(), 12);
    assert!(
        !rest::READ_ENDPOINTS.contains(&rest::PRODUCT),
        "product is read first, once"
    );
}

#[test]
fn a_full_cam5_read_maps_onto_the_wire_6() {
    let read = cam5();
    let (p, caps) = params_and_caps(&read);
    // Aperture: the true displayed f-number, as AV, and the nearest grid position.
    let av = p.aperture_av.unwrap();
    assert!((av - 2.0 * 7.3f64.log2()).abs() < 1e-9, "{av}");
    let grid = &caps.fnumber_choices;
    assert_eq!(
        grid.first(),
        Some(&4.4),
        "the lens's own wide-open stop starts the grid"
    );
    assert_eq!(grid.last(), Some(&22.0));
    let idx7_1 = grid.iter().position(|f| *f == 7.1).unwrap();
    let norm = p.aperture_norm.unwrap();
    assert!(
        (norm - idx7_1 as f64 / (grid.len() - 1) as f64).abs() < 1e-9,
        "7.3 sits nearest 7.1 on the 1/3-stop grid"
    );
    assert_eq!(p.iso, Some(400));
    assert_eq!(p.kelvin, Some(5600));
    assert_eq!(p.tint, Some(-3));
    assert_eq!(p.shutter, Some(50));
    assert_eq!(p.fps100, Some(2500));
    assert_eq!(p.sensor_fps100, Some(5000));
    assert_eq!(p.focus_norm, Some(0.42));
    assert_eq!(
        p.focus_distance, None,
        "the PTP d003 distance is not a REST value"
    );
    assert_eq!(
        caps.iso_choices,
        vec![100, 200, 400, 800, 1600, 3200],
        "sorted"
    );
    assert_eq!(caps.shutter_choices, vec![25, 50, 60, 100, 2000], "sorted");
    assert_eq!(
        caps.fps_choices,
        vec![2398, 2400, 2500, 2997, 3000, 5000, 5994, 6000],
        "only the BRAW Q0 rates at the current 3840x2160 (no ProRes 120, no 1080p 120)"
    );
    assert_eq!((caps.fps_min, caps.fps_max), (5, 60));
    assert_eq!((caps.kelvin_min, caps.kelvin_max), (2500, 10000));
    assert!(fps_supported(&read));
}

#[test]
fn an_empty_read_degrades_to_unknown_never_a_panic_6() {
    let read = RestRead::default();
    let (p, caps) = params_and_caps(&read);
    assert_eq!(p.aperture_av, None);
    assert_eq!(p.iso, None);
    assert_eq!(p.shutter, None);
    assert_eq!(p.fps100, None, "never the fallback rate");
    // 0.2.2 (coordinator, live 6K fw 8.6): no lens range -> the default 2.8..22 slider range.
    assert_eq!(caps.fnumber_choices.first(), Some(&2.8));
    assert_eq!(caps.fnumber_choices.last(), Some(&22.0));
    assert_eq!(
        caps.iso_choices,
        bkshading_proto::mapping::POCKET_ISO_CHOICES.to_vec(),
        "no supportedISOs -> the standard Pocket list"
    );
    assert!(caps.fps_choices.is_empty());
    assert!(
        !caps.shutter_choices.is_empty(),
        "the standard denominators at 25 fps"
    );
    assert!(!fps_supported(&read));
}

#[test]
fn a_shutter_angle_converts_at_the_project_rate_6() {
    let mut read = cam5();
    read.shutter = Some(de(
        r#"{"continuousShutterAutoExposure":false,"shutterAngle":18000}"#,
    ));
    read.supported_shutters = Some(de(r#"{"shutterAngles":[9000,17280,18000,36000]}"#));
    let (p, caps) = params_and_caps(&read);
    assert_eq!(p.shutter, Some(50), "180 deg at 25 fps = 1/50");
    // 360 -> 1/25, 180 -> 1/50, 172.8 -> 1/52, 90 -> 1/100.
    assert_eq!(caps.shutter_choices, vec![25, 50, 52, 100]);
    let basis = WriteBasis::from_read(&read);
    assert_eq!(basis.shutter_measure, ShutterMeasure::Angle);
    let req = SetRequest {
        shutter: Some(52),
        ..Default::default()
    };
    assert_eq!(
        plan_writes(&req, &basis).unwrap(),
        vec![RestWrite::ShutterAngle(17280)],
        "a listed denominator writes the camera's exact angle back"
    );
}

#[test]
fn fps_values_parse_from_the_cameras_strings_6() {
    assert_eq!(parse_frame_rate100("23.98"), Some(2398));
    assert_eq!(parse_frame_rate100("24"), Some(2400));
    assert_eq!(parse_frame_rate100("24.00"), Some(2400));
    assert_eq!(parse_frame_rate100(" 59.94 "), Some(5994));
    assert_eq!(parse_frame_rate100("119.88"), Some(11988));
    assert_eq!(parse_frame_rate100("120"), Some(12000));
    for junk in ["", "0", "-25", "25p", "abc", "1e2"] {
        assert_eq!(parse_frame_rate100(junk), None, "{junk:?}");
    }
}

#[test]
fn the_frame_rate_list_falls_back_to_the_resolution_when_no_entry_names_the_codec_6() {
    let format: Format = de(
        r#"{"codec":"BRaw:12_1","frameRate":"25","recordResolution":{"width":1920,"height":1080}}"#,
    );
    let supported: SupportedFormats = de(SUPPORTED_FORMATS);
    let rates: Vec<i64> = frame_rate_choices(&format, &supported)
        .into_iter()
        .map(|c| c.fps100)
        .collect();
    assert_eq!(
        rates.last(),
        Some(&12000),
        "the 1080p entry, whatever its codec"
    );
    let other: Format =
        de(r#"{"codec":"BRaw:Q0","recordResolution":{"width":6144,"height":3456}}"#);
    assert!(
        frame_rate_choices(&other, &supported).is_empty(),
        "no entry for the resolution -> nothing settable, never a rate that also changes it"
    );
}

#[test]
fn the_aperture_grid_spans_the_lens_and_never_crowds_its_ends_6() {
    let g = fnumber_grid(Some(4.4), Some(22.0));
    assert_eq!(
        g,
        vec![4.4, 5.0, 5.6, 6.3, 7.1, 8.0, 9.0, 10.0, 11.0, 13.0, 14.0, 16.0, 18.0, 20.0, 22.0],
        "4.5 is within 1/6 stop of the lens's 4.4 and is left out"
    );
    assert_eq!(fnumber_grid(Some(2.8), Some(2.8)), vec![2.8]);
    assert_eq!(
        fnumber_grid(Some(0.0), Some(22.0)),
        THIRD_STOP_FNUMBERS.to_vec()
    );
    assert_eq!(
        fnumber_grid(Some(8.0), Some(4.0)),
        THIRD_STOP_FNUMBERS.to_vec()
    );
    assert_eq!(fnumber_grid(None, Some(22.0)), THIRD_STOP_FNUMBERS.to_vec());
    let g = fnumber_grid(Some(2.8), Some(22.0));
    assert_eq!(
        g[..3],
        [2.8, 3.2, 3.5],
        "a lens opening on a nominal stop keeps it once"
    );
    for w in g.windows(2) {
        assert!(w[0] < w[1], "ascending: {g:?}");
    }
}

#[test]
fn every_wire_field_plans_its_rest_write_in_order_6() {
    let basis = WriteBasis::from_read(&cam5());
    let last = basis.fnumber_choices.len() - 1;
    let req = SetRequest {
        aperture_norm: Some(1.0),
        iso: Some(800),
        kelvin: Some(6500),
        tint: Some(10),
        shutter: Some(60),
        fps: Some(50),
        auto_wb: Some(true),
        aperture_step: None,
    };
    let w = plan_writes(&req, &basis).expect("50 is listed");
    assert_eq!(
        w,
        vec![
            RestWrite::FrameRate("50".into()),
            RestWrite::ApertureStop(basis.fnumber_choices[last]),
            RestWrite::Iso(800),
            RestWrite::ShutterSpeed(60),
            RestWrite::WhiteBalance(6500),
            RestWrite::Tint(10),
            RestWrite::AutoWhiteBalance,
        ]
    );
    let paths: Vec<&str> = w.iter().map(RestWrite::path).collect();
    assert_eq!(
        paths,
        vec![
            "/system/format",
            "/lens/iris",
            "/video/iso",
            "/video/shutter",
            "/video/whiteBalance",
            "/video/whiteBalanceTint",
            "/video/whiteBalance/doAuto",
        ]
    );
    let none = plan_writes(&SetRequest::default(), &basis).unwrap();
    assert!(none.is_empty());
    let off = SetRequest {
        auto_wb: Some(false),
        ..Default::default()
    };
    assert!(
        plan_writes(&off, &basis).unwrap().is_empty(),
        "autoWb false writes nothing"
    );
}

#[test]
fn an_unlisted_fps_refuses_the_whole_request_before_anything_is_planned_6() {
    let basis = WriteBasis::from_read(&cam5());
    for fps in [0, -25, 48, 120, i64::MAX] {
        let req = SetRequest {
            iso: Some(800),
            fps: Some(fps),
            ..Default::default()
        };
        let err = plan_writes(&req, &basis).unwrap_err();
        assert_eq!(err.fps, fps);
        assert_eq!(err.choices, basis.fps_choices());
        assert!(err.to_string().starts_with("fps-not-settable: "), "{err}");
    }
    // A listed rate writes the camera's own string, not a reformatted number.
    let req = SetRequest {
        fps: Some(24),
        ..Default::default()
    };
    assert_eq!(
        plan_writes(&req, &basis).unwrap(),
        vec![RestWrite::FrameRate("24".into())]
    );
}

#[test]
fn a_shutter_angle_set_with_a_new_rate_converts_at_that_rate_6() {
    let mut read = cam5();
    read.shutter = Some(de(r#"{"shutterAngle":18000}"#));
    read.supported_shutters = Some(de(r#"{"shutterAngles":[18000]}"#));
    let basis = WriteBasis::from_read(&read);
    let req = SetRequest {
        fps: Some(30),
        shutter: Some(50),
        ..Default::default()
    };
    assert_eq!(
        plan_writes(&req, &basis).unwrap(),
        vec![
            RestWrite::FrameRate("30".into()),
            RestWrite::ShutterAngle(21600),
        ],
        "1/50 at the new 30 fps is 216 deg, not the 180 deg listed at 25"
    );
}

#[test]
fn iris_motion_is_judged_beyond_display_rounding_6() {
    assert_eq!(iris_moved(Some(7.3), Some(8.0)), Some(true));
    assert_eq!(iris_moved(Some(7.3), Some(7.3)), Some(false));
    assert_eq!(iris_moved(Some(7.3), Some(7.32)), Some(false));
    assert_eq!(iris_moved(None, Some(7.3)), None);
    assert_eq!(iris_moved(Some(7.3), None), None);
}

#[test]
fn the_product_names_the_camera_6() {
    let p: Product = de(
        r#"{"deviceName":"cam5","productName":"Blackmagic Pocket Cinema Camera 4K","softwareVersion":"9.8"}"#,
    );
    assert_eq!(
        p.model().as_deref(),
        Some("Blackmagic Pocket Cinema Camera 4K")
    );
    let p: Product = de(r#"{"deviceName":"  cam5 "}"#);
    assert_eq!(p.model().as_deref(), Some("cam5"));
    let p: Product = de(r#"{"productName":"","softwareVersion":"9.8"}"#);
    assert_eq!(p.model(), None);
}

#[test]
fn unknown_fields_and_missing_ones_still_decode_6() {
    let iris: rest::Iris = de(r#"{"apertureStop":2.8,"somethingNew":1}"#);
    assert_eq!(iris.aperture_stop, Some(2.8));
    let fmt: Format = de("{}");
    assert_eq!(fmt, Format::default());
}

#[test]
fn integer_fields_accept_a_float_the_camera_might_send_6() {
    let iso: rest::Iso = de(r#"{"iso":400.0}"#);
    assert_eq!(iso.iso, Some(400));
    let isos: rest::SupportedIsos = de(r#"{"supportedISOs":[100,200.0]}"#);
    assert_eq!(isos.supported_isos, vec![100, 200]);
    let fmt: Format = de(r#"{"recordResolution":{"width":3840.0,"height":2160}}"#);
    assert_eq!(fmt.record_resolution.unwrap().width, Some(3840));
    let wb: rest::WhiteBalanceDescription = de(r#"{"whiteBalance":{"min":2500.4,"max":1e4}}"#);
    assert_eq!(wb.white_balance.unwrap().max, Some(10000));
}

#[test]
fn aperture_probes_creep_out_from_the_true_f_stop_and_stop_at_the_range_6() {
    use bkshading_proto::mapping::{aperture_probe_target, APERTURE_PROBE_STOPS};
    let r = Some((4.1, 22.0));
    // Each probe is 1/16 stop further: f = 4.4 * 2^(k/32).
    let p1 = aperture_probe_target(4.4, 1, 1, r).unwrap();
    let p2 = aperture_probe_target(4.4, 1, 2, r).unwrap();
    assert_eq!(p1, 4.50);
    assert_eq!(p2, 4.59);
    assert!((2f64.powf(APERTURE_PROBE_STOPS / 2.0) - 1.021_897).abs() < 1e-6);
    assert_eq!(aperture_probe_target(4.4, -1, 1, r), Some(4.31));
    // Downward from 4.4 the range min 4.1 is reached at the 4th probe; the 5th makes no progress.
    assert_eq!(aperture_probe_target(4.4, -1, 4, r), Some(4.1));
    assert_eq!(aperture_probe_target(4.4, -1, 5, r), None);
    // At an end there is nothing to probe.
    assert_eq!(aperture_probe_target(22.0, 1, 1, r), None);
    assert_eq!(aperture_probe_target(4.1, -1, 1, r), None);
    // Without a range the probes never stop on their own (the relay caps the count).
    assert!(aperture_probe_target(22.0, 1, 1, None).unwrap() > 22.0);
    // Degenerate input.
    assert_eq!(aperture_probe_target(0.0, 1, 1, r), None);
    assert_eq!(aperture_probe_target(4.4, 0, 1, r), None);
    assert_eq!(aperture_probe_target(4.4, 1, 0, r), None);
    assert_eq!(aperture_probe_target(f64::NAN, 1, 1, r), None);
    // Any positive direction is +1, any negative -1.
    assert_eq!(aperture_probe_target(4.4, 7, 1, r), Some(4.50));
}

#[test]
fn a_probe_increment_never_crosses_past_the_next_native_step_6() {
    // The safety argument behind APERTURE_PROBE_STOPS: the first probe that lands somewhere else
    // lands on the NEXT step. Simulated over both live lenses with the camera's nearest-step snap.
    use bkshading_proto::mapping::aperture_probe_target;
    let snap = |stops: &[f64], f: f64| {
        *stops
            .iter()
            .min_by(|a, b| {
                ((*a / f).log2().abs())
                    .partial_cmp(&(*b / f).log2().abs())
                    .unwrap()
            })
            .unwrap()
    };
    let cam5 = [4.4, 4.8, 5.2, 5.6, 6.2, 6.7, 7.3, 8.0];
    let pp6k = [2.8, 2.9, 3.1, 3.2, 3.3, 3.5, 3.7, 3.9, 4.0, 4.2];
    for stops in [&cam5[..], &pp6k[..]] {
        let range = Some((stops[0], *stops.last().unwrap()));
        for (i, &c) in stops.iter().enumerate() {
            for dir in [1i64, -1] {
                let next = stops.get((i as i64 + dir) as usize).copied();
                let mut landed = None;
                for k in 1..=64 {
                    let Some(t) = aperture_probe_target(c, dir, k, range) else {
                        break;
                    };
                    let s = snap(stops, t);
                    if s != c {
                        landed = Some(s);
                        break;
                    }
                }
                assert_eq!(landed, next, "from {c} dir {dir}");
            }
        }
    }
}

/// shading#6 (0.2.3): `lens/iris normalised` (the iris's place in the lens's range, 0 = wide open,
/// 1 = closed) is the read param `irisRangePos`; absent when the camera does not give it.
#[test]
fn the_iris_range_position_maps_onto_the_wire_6() {
    let pos = |read: &RestRead| {
        let (p, _) = params_and_caps(read);
        serde_json::to_value(&p).unwrap()["irisRangePos"].clone()
    };
    assert_eq!(pos(&cam5()), serde_json::json!(0.3158));
    let mut wide = cam5();
    wide.iris = Some(de(
        r#"{"apertureNumber":320,"apertureStop":4.0,"continuousApertureAutoExposure":false,"normalised":0.0}"#,
    ));
    assert_eq!(
        pos(&wide),
        serde_json::json!(0.0),
        "cam3 at 24 mm, wide open"
    );
    let mut none = cam5();
    none.iris = Some(de(r#"{"apertureStop":4.0}"#));
    assert_eq!(pos(&none), serde_json::Value::Null);
    let mut over = cam5();
    over.iris = Some(de(r#"{"apertureStop":22.0,"normalised":1.0000001}"#));
    assert_eq!(pos(&over), serde_json::json!(1.0), "clamped into 0..1");
}
