//! issue 1238 — the relay's honest focus signal: `ShadingParams.focus_distance` lifted from
//! the BMPCC's `d003` (manual focus DISTANCE). Pure, no IO, no camera. The BMPCC PTP space
//! exposes NO AF/MF focus-MODE selector and NO auto/manual exposure-MODE selector (STEP-0
//! research: the TalOrg control-point list + the MVP mapping), so `d003` distance is the only
//! honest focus property; these tests pin its parse, its `None` degradation when the camera
//! does not answer d003, and its back/forward-compatible camelCase wire shape.

use bkshading_proto::read::{params_and_caps, RawConfigs};
use bkshading_proto::wire::ShadingParams;

/// A full d003 RANGE block, mirroring gphoto2 `--get-config d003` on a BMPCC 4K.
const D003_BLOCK: &str =
    "Label: PTP Property 0xd003\nType: RANGE\nCurrent: 32768\nBottom: 0\nTop: 65536\nEND";

#[test]
fn params_and_caps_lifts_d003_focus_distance() {
    let raw = RawConfigs {
        focus_distance: D003_BLOCK.to_string(),
        ..Default::default()
    };
    let (params, _caps) = params_and_caps(&raw);
    assert_eq!(
        params.focus_distance,
        Some(32768),
        "d003 Current value lifts verbatim into focus_distance"
    );
}

#[test]
fn absent_d003_degrades_to_none_never_zero() {
    // The camera did not answer d003 this cycle (best-effort read -> empty block). This MUST be
    // a `None` (server-is-truth "not known"), never a fabricated 0 (= closest focus) that would
    // lie to a pre-run focus check.
    let raw = RawConfigs::default(); // every block empty
    let (params, _caps) = params_and_caps(&raw);
    assert_eq!(params.focus_distance, None);

    // A malformed block with no `Current:` line also degrades to None, not a crash.
    let raw_malformed = RawConfigs {
        focus_distance: "Label: PTP Property 0xd003\nType: RANGE\nBottom: 0\nEND".to_string(),
        ..Default::default()
    };
    let (params_malformed, _) = params_and_caps(&raw_malformed);
    assert_eq!(params_malformed.focus_distance, None);
}

#[test]
fn focus_distance_wire_is_camel_case_and_round_trips() {
    let params = ShadingParams {
        focus_distance: Some(32768),
        ..Default::default()
    };
    let json = serde_json::to_string(&params).unwrap();
    assert!(
        json.contains("\"focusDistance\":32768"),
        "focusDistance camelCase in wire: {json}"
    );
    let back: ShadingParams = serde_json::from_str(&json).unwrap();
    assert_eq!(back, params);
}

#[test]
fn older_relay_json_without_focus_distance_still_deserializes_as_none() {
    // Back-compat (`#[serde(default)]`): an older relay/service that predates issue 1238 sends
    // no `focusDistance` key — the new deserializer must accept it and default the field to None,
    // never error on the missing field.
    let old: ShadingParams = serde_json::from_str(r#"{"iso":400,"shutter":50}"#).unwrap();
    assert_eq!(old.focus_distance, None);
    assert_eq!(old.iso, Some(400));
}
