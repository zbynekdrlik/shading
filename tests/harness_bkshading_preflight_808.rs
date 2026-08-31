//! #808 (bkshading epic) — pure-function guard for `scripts/lib/bkshading-preflight.sh`, the
//! automated #220 CAMERA PRE-RUN shutter-checklist preflight.
//!
//! Root cause: `scripts/recording-e2e.sh`'s #220 block prints a MANUAL human checklist ("SHUTTER
//! FAST: >= 1/500 s") because the harness reads only `/dev/video0` (the ShadowCast HDMI capture of
//! the BMPCC's monitor output) and cannot itself read the camera BODY's shutter. Now that the
//! `bkshading-relay` (issue 808 M1) runs on the cambox and reads the camera's shutter over
//! USB-PTP/gphoto2 (`GET /api/state` -> `RelayState{online,camera,params:{shutter},...}`,
//! `bkshading/relay/src/http.rs` + `bkshading/proto/src/wire.rs`), the harness can automate HALF of
//! that manual step: read the shutter back and WARN (never hard-fail — owner M3 decision recorded
//! on issue 808) when it is too slow.
//!
//! Same convention as `tests/harness_splitter_port_health_739.rs` / `tests/harness_audio_presence_preflight.rs`:
//! source the REAL lib (source-only, no side effects) and exercise the PURE functions directly
//! (parse/classify/message). The one I/O orchestrator (`bkshading_preflight_report`, curl + the
//! pure functions) is a thin caller, deliberately NOT unit-tested here — mirrors
//! `audio-presence-preflight.sh`'s own "the recording-e2e.sh step is a thin caller" convention.
//! RED before the lib exists (sourcing fails, every test fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/bkshading-preflight.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL lib and run `body` against its pure functions. Returns (exit, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}", body = body);
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn stdout_of(body: &str) -> String {
    let (rc, out, err) = run_sourced(body);
    assert_eq!(rc, 0, "body failed (rc={rc}): {body}\nstderr={err}");
    out.trim().to_string()
}

// ---------------------------------------------------------------------------------------------
// lib shape — the pure functions must be defined
// ---------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_pure_functions() {
    for f in [
        "bkshading_preflight_state_online",
        "bkshading_preflight_state_camera",
        "bkshading_preflight_state_shutter",
        "bkshading_preflight_classify",
        "bkshading_preflight_ok_message",
        "bkshading_preflight_warn_slow_message",
        "bkshading_preflight_warn_unknown_message",
        "bkshading_preflight_skip_offline_message",
        "bkshading_preflight_skip_unreachable_message",
        "bkshading_preflight_report",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by the lib");
    }
}

// ---------------------------------------------------------------------------------------------
// bkshading_preflight_state_online/_camera/_shutter <json> -> scalar extractors over the relay's
// GET /api/state response. Malformed/missing/null must yield EMPTY (online: "0"), never a
// fabricated value ("unreadable is never a silent pass").
// ---------------------------------------------------------------------------------------------
#[test]
fn state_online_true_when_online_field_is_true() {
    let json = r#"{"online":true,"camera":"USB PTP Class Camera","params":{"shutter":1000}}"#;
    assert_eq!(
        stdout_of(&format!("bkshading_preflight_state_online '{json}'")),
        "1"
    );
}

#[test]
fn state_online_false_when_offline() {
    let json = r#"{"online":false,"camera":null,"params":{}}"#;
    assert_eq!(
        stdout_of(&format!("bkshading_preflight_state_online '{json}'")),
        "0"
    );
}

#[test]
fn state_online_false_on_malformed_json() {
    // curl succeeded but the body is not valid JSON -- never crash, never fabricate "online".
    assert_eq!(
        stdout_of("bkshading_preflight_state_online 'not json at all'"),
        "0"
    );
    assert_eq!(stdout_of("bkshading_preflight_state_online ''"), "0");
}

#[test]
fn state_camera_extracts_the_model_string() {
    let json = r#"{"online":true,"camera":"USB PTP Class Camera","params":{"shutter":1000}}"#;
    assert_eq!(
        stdout_of(&format!("bkshading_preflight_state_camera '{json}'")),
        "USB PTP Class Camera"
    );
}

#[test]
fn state_camera_empty_when_null_or_absent() {
    assert_eq!(
        stdout_of(r#"bkshading_preflight_state_camera '{"online":false,"camera":null}'"#),
        ""
    );
    assert_eq!(
        stdout_of(r#"bkshading_preflight_state_camera '{"online":false}'"#),
        ""
    );
    assert_eq!(stdout_of("bkshading_preflight_state_camera 'garbage'"), "");
}

#[test]
fn state_shutter_extracts_the_denominator() {
    let json = r#"{"online":true,"camera":"x","params":{"shutter":1000}}"#;
    assert_eq!(
        stdout_of(&format!("bkshading_preflight_state_shutter '{json}'")),
        "1000"
    );
}

// ---------------------------------------------------------------------------------------------
// Review finding (issue 808 diff review): a VALID-JSON-but-non-dict top-level body (or a
// non-dict `params`) must NEVER crash the extractor with an uncaught AttributeError -- it must
// degrade to the same safe default as malformed/empty JSON. A relay/proxy could plausibly answer
// with a bare `null`/list/string/number under a transient failure, and the whole POINT of this
// preflight is to never abort the E2E run over it.
// ---------------------------------------------------------------------------------------------
#[test]
fn state_online_false_on_non_dict_top_level_json() {
    for body in ["null", "[1,2,3]", "\"a string\"", "42"] {
        let out = stdout_of(&format!("bkshading_preflight_state_online '{body}'"));
        assert_eq!(
            out, "0",
            "non-dict top-level JSON {body:?} must yield online=0, not crash"
        );
    }
}

#[test]
fn state_camera_empty_on_non_dict_top_level_json() {
    for body in ["null", "[1,2,3]", "\"a string\"", "42"] {
        let out = stdout_of(&format!("bkshading_preflight_state_camera '{body}'"));
        assert_eq!(
            out, "",
            "non-dict top-level JSON {body:?} must yield empty camera, not crash"
        );
    }
}

#[test]
fn state_shutter_empty_on_non_dict_top_level_json() {
    for body in ["null", "[1,2,3]", "\"a string\"", "42"] {
        let out = stdout_of(&format!("bkshading_preflight_state_shutter '{body}'"));
        assert_eq!(
            out, "",
            "non-dict top-level JSON {body:?} must yield empty shutter, not crash"
        );
    }
}

#[test]
fn state_shutter_empty_when_params_is_not_a_dict() {
    // params: [1,2] -- (d.get("params") or {}).get("shutter") crashes with AttributeError on a
    // non-dict params unless the whole access is guarded.
    let out = stdout_of(
        r#"bkshading_preflight_state_shutter '{"online":true,"camera":"x","params":[1,2]}'"#,
    );
    assert_eq!(
        out, "",
        "a non-dict params must yield empty shutter, not crash"
    );
}

#[test]
fn state_shutter_empty_when_shutter_is_a_json_bool() {
    // Python bool is an int subclass -- isinstance(True, int) is True, so a naive check would
    // print "True"/"False" instead of treating it as absent.
    let out = stdout_of(
        r#"bkshading_preflight_state_shutter '{"online":true,"params":{"shutter":true}}'"#,
    );
    assert_eq!(
        out, "",
        "a JSON boolean shutter must not be treated as a valid int"
    );
}

// ---------------------------------------------------------------------------------------------
// report must never crash the CALLER even when the relay is reachable but answers with a
// non-dict body -- the exact review-found gap (a bare command-substitution assignment inside
// bkshading_preflight_report, unguarded by any `if`/`&&`, propagates a python crash straight
// through the caller's `set -e`).
// ---------------------------------------------------------------------------------------------
#[test]
fn report_never_fails_the_caller_on_a_non_dict_relay_body() {
    for body in ["null", "[1,2,3]", "\"a string\"", "42"] {
        let (rc, out, _err) = run_sourced(&format!(
            "set -e\ncurl() {{ printf '%s' '{body}'; }}\nbkshading_preflight_report cam1 x 1 1 500\necho AFTER"
        ));
        assert_eq!(rc, 0, "must never fail the caller on relay body {body:?}");
        assert!(
            out.contains("AFTER"),
            "must return control to the caller on relay body {body:?}: {out}"
        );
    }
}

#[test]
fn state_shutter_empty_when_null_absent_or_non_integer() {
    assert_eq!(
        stdout_of(
            r#"bkshading_preflight_state_shutter '{"online":true,"params":{"shutter":null}}'"#
        ),
        ""
    );
    assert_eq!(
        stdout_of(r#"bkshading_preflight_state_shutter '{"online":true,"params":{}}'"#),
        ""
    );
    assert_eq!(
        stdout_of(r#"bkshading_preflight_state_shutter '{"online":true}'"#),
        ""
    );
    assert_eq!(stdout_of("bkshading_preflight_state_shutter 'garbage'"), "");
}

// ---------------------------------------------------------------------------------------------
// bkshading_preflight_classify <online 0|1> <camera> <shutter> [min_denom=500]
//   -> ok | warn-slow | warn-unknown | skip-offline
// ---------------------------------------------------------------------------------------------
fn classify(online: &str, camera: &str, shutter: &str) -> String {
    stdout_of(&format!(
        "bkshading_preflight_classify {online} \"{camera}\" \"{shutter}\""
    ))
}

#[test]
fn classify_offline_is_skip_regardless_of_shutter() {
    // the EXPECTED common case (a portable camera cabled to only one box) -- never a warning.
    assert_eq!(classify("0", "", ""), "skip-offline");
    assert_eq!(classify("0", "", "1000"), "skip-offline");
}

#[test]
fn classify_online_but_no_camera_name_is_skip_defensively() {
    assert_eq!(classify("1", "", "1000"), "skip-offline");
}

#[test]
fn classify_online_camera_present_shutter_missing_is_warn_unknown() {
    assert_eq!(classify("1", "USB PTP Class Camera", ""), "warn-unknown");
}

#[test]
fn classify_online_camera_present_shutter_non_numeric_is_warn_unknown() {
    // defensive: a garbage value must never be numerically compared.
    assert_eq!(classify("1", "USB PTP Class Camera", "abc"), "warn-unknown");
}

#[test]
fn classify_shutter_below_minimum_is_warn_slow() {
    // the exact #216 failure mode: 1/60 is far below the required >= 1/500.
    assert_eq!(classify("1", "USB PTP Class Camera", "60"), "warn-slow");
    assert_eq!(classify("1", "USB PTP Class Camera", "499"), "warn-slow");
}

#[test]
fn classify_shutter_at_or_above_minimum_is_ok() {
    // boundary: exactly at the minimum counts as OK (not slow), mirrors audio_preflight_is_silent's
    // own "exactly at the boundary counts as the healthier side" convention.
    assert_eq!(classify("1", "USB PTP Class Camera", "500"), "ok");
    assert_eq!(classify("1", "USB PTP Class Camera", "1000"), "ok");
}

#[test]
fn classify_respects_a_custom_minimum_denom() {
    let out = stdout_of("bkshading_preflight_classify 1 \"cam\" \"400\" 250");
    assert_eq!(out, "ok", "400 >= custom min 250 must be ok: {out}");
    let out2 = stdout_of("bkshading_preflight_classify 1 \"cam\" \"200\" 250");
    assert_eq!(out2, "warn-slow", "200 < custom min 250 must warn: {out2}");
}

// ---------------------------------------------------------------------------------------------
// message formatters -- pure string builders, must name the issue, the camera/box, and the values.
// ---------------------------------------------------------------------------------------------
#[test]
fn ok_message_names_camera_and_shutter() {
    let m = stdout_of(
        "bkshading_preflight_ok_message cam1 10.77.9.1 \"USB PTP Class Camera\" 1000 500",
    );
    assert!(m.contains("cam1"), "{m}");
    assert!(m.contains("1000"), "{m}");
    assert!(m.contains("500"), "{m}");
    assert!(m.contains("USB PTP Class Camera"), "{m}");
}

#[test]
fn warn_slow_message_is_loud_and_names_the_issue_and_values() {
    let m = stdout_of(
        "bkshading_preflight_warn_slow_message cam1 10.77.9.1 \"USB PTP Class Camera\" 60 500",
    );
    assert!(m.contains("WARNING"), "must be loud: {m}");
    assert!(m.contains("#808"), "must cite the epic: {m}");
    assert!(m.contains("60"), "must name the measured shutter: {m}");
    assert!(m.contains("500"), "must name the required minimum: {m}");
    assert!(m.contains("cam1") && m.contains("10.77.9.1"), "{m}");
}

#[test]
fn warn_unknown_message_is_loud() {
    let m = stdout_of(
        "bkshading_preflight_warn_unknown_message cam1 10.77.9.1 \"USB PTP Class Camera\" 500",
    );
    assert!(m.contains("WARNING"), "{m}");
    assert!(m.contains("#808"), "{m}");
    assert!(m.contains("cam1"), "{m}");
}

#[test]
fn skip_offline_message_is_not_a_warning_and_names_the_box() {
    let m = stdout_of("bkshading_preflight_skip_offline_message cam3 10.77.9.3 8771");
    assert!(
        !m.contains("WARNING"),
        "an absent camera must not be a WARNING: {m}"
    );
    assert!(m.contains("cam3"), "{m}");
    assert!(m.contains("8771"), "{m}");
}

#[test]
fn skip_unreachable_message_is_not_a_warning_and_names_the_box() {
    let m = stdout_of("bkshading_preflight_skip_unreachable_message cam4 10.77.9.4 8771");
    assert!(
        !m.contains("WARNING"),
        "an unreachable relay must not be a WARNING: {m}"
    );
    assert!(m.contains("cam4"), "{m}");
    assert!(m.contains("8771"), "{m}");
}

// ---------------------------------------------------------------------------------------------
// bkshading_preflight_report never aborts the caller -- even pointed at a port nothing listens on
// (an unreachable relay), it must return 0 (the harness runs under `set -euo pipefail`).
// ---------------------------------------------------------------------------------------------
#[test]
fn report_never_fails_the_caller_on_an_unreachable_relay() {
    // port 1 is a reserved/unassigned TCP port -- curl will refuse/timeout quickly, never listen.
    let (rc, out, _err) =
        run_sourced("set -e; bkshading_preflight_report cam9 127.0.0.1 1 1 500; echo AFTER");
    assert_eq!(rc, 0, "must never fail the caller under set -e");
    assert!(
        out.contains("AFTER"),
        "must return control to the caller: {out}"
    );
    assert!(
        out.contains("cam9"),
        "must name the box in its output: {out}"
    );
}

// =============================================================================================
// #1237 — complete the pre-run auto-check: the EXPOSURE/gain half (iso + aperture, measurable
// from the same GET /api/state) + honest surfacing of the FOCUS + auto/manual EXPOSURE-MODE that
// the relay does NOT expose. REPORT-ONLY, same as the shutter half (owner M3 decision on #808 +
// the ticket's report-only-first default). The relay's read plan (bkshading/relay/src/transport.rs)
// reads iso + f-number(aperture) but NO focus/exposure-mode config, so those stay a NOTE + a
// filed follow-up, never a fabricated pass (imag-ssh-remote-tool-preflight.md LOUD-UNKNOWN).
// =============================================================================================
#[test]
fn lib_defines_the_1237_exposure_functions() {
    for f in [
        "bkshading_preflight_state_iso",
        "bkshading_preflight_state_aperture",
        "bkshading_preflight_classify_exposure",
        "bkshading_preflight_exposure_ok_message",
        "bkshading_preflight_warn_exposure_message",
        "bkshading_preflight_focus_note_message",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by the lib");
    }
}

// --- state_iso: params.iso (gain) integer extractor, same guards as the shutter extractor -------
#[test]
fn state_iso_extracts_the_integer_gain() {
    let json = r#"{"online":true,"camera":"x","params":{"iso":400,"shutter":1000}}"#;
    assert_eq!(
        stdout_of(&format!("bkshading_preflight_state_iso '{json}'")),
        "400"
    );
}

#[test]
fn state_iso_empty_when_null_absent_bool_or_non_dict() {
    assert_eq!(
        stdout_of(r#"bkshading_preflight_state_iso '{"online":true,"params":{"iso":null}}'"#),
        ""
    );
    assert_eq!(
        stdout_of(r#"bkshading_preflight_state_iso '{"online":true,"params":{}}'"#),
        ""
    );
    // a JSON bool must not be treated as a valid int (python bool is an int subclass).
    assert_eq!(
        stdout_of(r#"bkshading_preflight_state_iso '{"online":true,"params":{"iso":true}}'"#),
        ""
    );
    assert_eq!(
        stdout_of(r#"bkshading_preflight_state_iso '{"online":true,"params":[1,2]}'"#),
        ""
    );
    for body in ["null", "[1,2,3]", "\"a string\"", "42", "garbage"] {
        assert_eq!(
            stdout_of(&format!("bkshading_preflight_state_iso '{body}'")),
            "",
            "non-dict/garbage {body:?} must yield empty iso, not crash"
        );
    }
}

// --- state_aperture: params.apertureAv (float or int) presence extractor -----------------------
#[test]
fn state_aperture_extracts_the_av_value() {
    let json = r#"{"online":true,"camera":"x","params":{"apertureAv":4.0}}"#;
    assert_eq!(
        stdout_of(&format!("bkshading_preflight_state_aperture '{json}'")),
        "4.0"
    );
    // an integer AV must also be accepted (isinstance int).
    let json2 = r#"{"online":true,"camera":"x","params":{"apertureAv":5}}"#;
    assert_eq!(
        stdout_of(&format!("bkshading_preflight_state_aperture '{json2}'")),
        "5"
    );
}

#[test]
fn state_aperture_empty_when_null_absent_bool_or_non_dict() {
    assert_eq!(
        stdout_of(
            r#"bkshading_preflight_state_aperture '{"online":true,"params":{"apertureAv":null}}'"#
        ),
        ""
    );
    assert_eq!(
        stdout_of(r#"bkshading_preflight_state_aperture '{"online":true,"params":{}}'"#),
        ""
    );
    assert_eq!(
        stdout_of(
            r#"bkshading_preflight_state_aperture '{"online":true,"params":{"apertureAv":true}}'"#
        ),
        ""
    );
    for body in ["null", "[1,2,3]", "\"a string\"", "42", "garbage"] {
        assert_eq!(
            stdout_of(&format!("bkshading_preflight_state_aperture '{body}'")),
            "",
            "non-dict/garbage {body:?} must yield empty aperture, not crash"
        );
    }
}

// --- classify_exposure <online> <camera> <iso> <aperture> -------------------------------------
//   skip-offline | ok | warn-iso | warn-aperture | warn-both
fn classify_exposure(online: &str, camera: &str, iso: &str, aperture: &str) -> String {
    stdout_of(&format!(
        "bkshading_preflight_classify_exposure {online} \"{camera}\" \"{iso}\" \"{aperture}\""
    ))
}

#[test]
fn classify_exposure_offline_or_no_camera_is_skip() {
    assert_eq!(classify_exposure("0", "", "", ""), "skip-offline");
    assert_eq!(classify_exposure("0", "", "400", "4.0"), "skip-offline");
    assert_eq!(classify_exposure("1", "", "400", "4.0"), "skip-offline");
}

#[test]
fn classify_exposure_both_present_is_ok() {
    assert_eq!(classify_exposure("1", "cam", "400", "4.0"), "ok");
}

#[test]
fn classify_exposure_names_the_missing_param() {
    assert_eq!(classify_exposure("1", "cam", "", "4.0"), "warn-iso");
    assert_eq!(classify_exposure("1", "cam", "400", ""), "warn-aperture");
    assert_eq!(classify_exposure("1", "cam", "", ""), "warn-both");
}

// --- message formatters -----------------------------------------------------------------------
#[test]
fn exposure_ok_message_names_camera_iso_and_aperture() {
    let m = stdout_of(
        "bkshading_preflight_exposure_ok_message cam1 10.77.9.1 \"USB PTP Class Camera\" 400 4.0",
    );
    assert!(
        !m.contains("WARNING"),
        "an OK exposure must not be a WARNING: {m}"
    );
    assert!(m.contains("cam1"), "{m}");
    assert!(m.contains("400"), "must name the ISO/gain: {m}");
    assert!(m.contains("4.0"), "must name the aperture AV: {m}");
    assert!(m.contains("USB PTP Class Camera"), "{m}");
}

#[test]
fn warn_exposure_message_is_loud_and_names_the_missing_param() {
    // The message keys off the classifier STATUS (single source of truth for the missing set), so
    // the classifier and the message can never disagree about which parameter is absent.
    // warn-iso -> names ISO/gain
    let m = stdout_of(
        "bkshading_preflight_warn_exposure_message cam1 10.77.9.1 \"USB PTP Class Camera\" warn-iso",
    );
    assert!(m.contains("WARNING"), "must be loud: {m}");
    assert!(m.contains("#1237"), "must cite the ticket: {m}");
    assert!(
        m.contains("ISO") || m.contains("gain"),
        "must name the missing ISO/gain: {m}"
    );
    assert!(m.contains("cam1") && m.contains("10.77.9.1"), "{m}");
    // warn-aperture -> names aperture
    let m2 = stdout_of(
        "bkshading_preflight_warn_exposure_message cam1 10.77.9.1 \"USB PTP Class Camera\" warn-aperture",
    );
    assert!(m2.contains("WARNING"), "{m2}");
    assert!(
        m2.contains("aperture"),
        "must name the missing aperture: {m2}"
    );
    // warn-both -> names both
    let m3 = stdout_of(
        "bkshading_preflight_warn_exposure_message cam1 10.77.9.1 \"USB PTP Class Camera\" warn-both",
    );
    assert!(
        (m3.contains("ISO") || m3.contains("gain")) && m3.contains("aperture"),
        "warn-both must name both: {m3}"
    );
}

#[test]
fn focus_note_is_a_note_not_a_warning_and_names_focus_and_the_box() {
    let m = stdout_of("bkshading_preflight_focus_note_message cam1");
    assert!(
        !m.contains("WARNING"),
        "an unmeasurable-by-design note must NOT be a WARNING (never a fabricated pass either): {m}"
    );
    assert!(m.to_lowercase().contains("focus"), "must name focus: {m}");
    assert!(m.contains("cam1"), "must name the box: {m}");
}

// --- report integration: a fully-good online camera prints shutter OK + exposure OK + focus NOTE
#[test]
fn report_full_online_camera_prints_all_three_lines() {
    let json = r#"{"online":true,"camera":"USB PTP Class Camera","params":{"iso":400,"shutter":1000,"apertureAv":4.0}}"#;
    let (rc, out, err) = run_sourced(&format!(
        "set -e\ncurl() {{ printf '%s' '{json}'; }}\nbkshading_preflight_report cam1 x 1 1 500\necho AFTER"
    ));
    let all = format!("{out}\n{err}");
    assert_eq!(rc, 0, "report must never fail the caller: {all}");
    assert!(
        out.contains("AFTER"),
        "must return control to the caller: {out}"
    );
    assert!(all.contains("1000"), "shutter OK line must appear: {all}");
    assert!(
        all.contains("400"),
        "exposure OK line must name the ISO/gain: {all}"
    );
    assert!(
        all.to_lowercase().contains("focus"),
        "focus honesty NOTE must appear: {all}"
    );
    // a fully-good camera never emits a WARNING.
    assert!(
        !all.contains("WARNING"),
        "a fully-good camera must not WARN: {all}"
    );
}

// a slow shutter AND a missing iso: shutter warns AND exposure warns, still returns 0.
#[test]
fn report_slow_shutter_and_missing_iso_warns_both_but_never_fails() {
    let json = r#"{"online":true,"camera":"USB PTP Class Camera","params":{"shutter":60,"apertureAv":4.0}}"#;
    let (rc, out, err) = run_sourced(&format!(
        "set -e\ncurl() {{ printf '%s' '{json}'; }}\nbkshading_preflight_report cam1 x 1 1 500\necho AFTER"
    ));
    let all = format!("{out}\n{err}");
    assert_eq!(rc, 0, "must never fail the caller: {all}");
    assert!(out.contains("AFTER"), "must return control: {out}");
    assert!(
        all.contains("WARNING"),
        "a slow shutter + missing iso must WARN: {all}"
    );
}
