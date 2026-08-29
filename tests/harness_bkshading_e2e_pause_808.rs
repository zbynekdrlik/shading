//! #808 (bkshading epic) — pure-function guard for `scripts/lib/bkshading-e2e-pause.sh`, the
//! E2E-harness-managed PAUSE + RESTORE of `bkshading-relay` on the two measurement-critical
//! camboxes (the SOURCE camera + cam2/painter).
//!
//! Root cause: the relay's gphoto2 USB-PTP polling causally degrades measurement quality on both
//! boxes it needs to be paused on — cam1 Cam Link capture drops 60.0 -> 58.3-58.9 fps (USB-bus
//! contention, proven by stop/start isolation) and cam2's dual-QR window quality correlates with
//! relay state (a 3-core box already running camera-box RT + the painter). Evidence: issue 808
//! comments 2026-08-29T09:59:31Z / 2026-08-29T15:54:47Z. The interim mitigation was a MANUAL
//! `systemctl stop bkshading-relay` on both boxes (unit left `enabled`) — this lib makes that
//! durable and harness-enforced: pause at `[0/8]`, restore in `cleanup()`, but ONLY on a box the
//! pause step found genuinely ACTIVE beforehand (never re-activate a box someone deliberately
//! silenced).
//!
//! Same convention as `tests/harness_bkshading_preflight_808.rs`: source the REAL lib
//! (source-only, no side effects) and exercise the PURE functions directly (the two remote-text
//! builders + the parser). The two thin I/O orchestrators (`bkshading_e2e_pause_stop`/
//! `bkshading_e2e_pause_restore`, ssh + the pure functions) are deliberately NOT unit-tested here
//! beyond an existence check — mirrors `bkshading_preflight_report`'s own "the recording-e2e.sh
//! step is a thin caller" convention.
//! RED before the lib exists (sourcing fails, every test fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/bkshading-e2e-pause.sh");
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
// lib shape — the pure functions (AND the two thin orchestrators) must be defined
// ---------------------------------------------------------------------------------------------
#[test]
fn lib_defines_the_pure_functions_and_orchestrators() {
    for f in [
        "bkshading_e2e_pause_marker_prefix",
        "bkshading_e2e_pause_stop_cmds",
        "bkshading_e2e_pause_restore_cmds",
        "bkshading_e2e_pause_parse_state",
        "bkshading_e2e_pause_stop",
        "bkshading_e2e_pause_restore",
    ] {
        let out = stdout_of(&format!("type {f} >/dev/null 2>&1 && echo DEFINED"));
        assert_eq!(out, "DEFINED", "{f} is not defined by the lib");
    }
}

// ---------------------------------------------------------------------------------------------
// bkshading_e2e_pause_marker_prefix — the ONE source of truth the print + parse sides share.
// ---------------------------------------------------------------------------------------------
#[test]
fn marker_prefix_is_the_expected_literal() {
    assert_eq!(
        stdout_of("bkshading_e2e_pause_marker_prefix"),
        "BKSHADING_PAUSE_STATE"
    );
}

// ---------------------------------------------------------------------------------------------
// bkshading_e2e_pause_stop_cmds LABEL -> REMOTE bash text: probes is-active, stops the unit,
// echoes the marker line. Every remote `$` must stay ESCAPED (literal) in the printed text — a
// leak here would mean this function's own local generation accidentally evaluated something
// meant to run only on the remote host.
// ---------------------------------------------------------------------------------------------
#[test]
fn stop_cmds_names_the_relay_unit_and_stops_it() {
    let out = stdout_of("bkshading_e2e_pause_stop_cmds cam1");
    assert!(
        out.contains("systemctl stop bkshading-relay.service"),
        "{out}"
    );
    assert!(
        out.contains("systemctl is-active --quiet bkshading-relay.service"),
        "{out}"
    );
    assert!(out.contains("|| true"), "must be tolerant: {out}");
}

#[test]
fn stop_cmds_echoes_the_marker_line_with_the_label_baked_in() {
    let out = stdout_of("bkshading_e2e_pause_stop_cmds cam3");
    assert!(
        out.contains("BKSHADING_PAUSE_STATE:cam3:"),
        "label must be baked in locally: {out}"
    );
}

#[test]
fn stop_cmds_never_leaks_a_local_dollar_sign_meant_for_the_remote_side() {
    // The remote-side `$_bksh_was_active` reference must survive as a LITERAL `$` in the
    // generated text -- if the escaping were dropped, local bash would try to expand an unset
    // local variable of that name into an empty string instead, silently breaking the remote
    // script (the marker line would read "BKSHADING_PAUSE_STATE:cam1:" with nothing after the
    // final colon instead of a real 0/1).
    let out = stdout_of("bkshading_e2e_pause_stop_cmds cam1");
    assert!(
        out.contains("$_bksh_was_active"),
        "the remote variable reference must survive as a literal $ in the output: {out}"
    );
}

// ---------------------------------------------------------------------------------------------
// bkshading_e2e_pause_restore_cmds WAS_ACTIVE -> a PURE LOCAL decision: "1" starts the unit,
// anything else is a documented no-op. Never a remote conditional.
// ---------------------------------------------------------------------------------------------
#[test]
fn restore_cmds_starts_the_unit_when_was_active_is_1() {
    let out = stdout_of("bkshading_e2e_pause_restore_cmds 1");
    assert!(
        out.contains("systemctl start bkshading-relay.service"),
        "{out}"
    );
    assert!(out.contains("|| true"), "must be tolerant: {out}");
}

#[test]
fn restore_cmds_is_a_noop_when_was_active_is_0() {
    let out = stdout_of("bkshading_e2e_pause_restore_cmds 0");
    assert!(
        !out.contains("systemctl start"),
        "must never start the unit when it was not active before: {out}"
    );
}

#[test]
fn restore_cmds_is_a_noop_on_any_non_1_value_defensively() {
    // garbage / empty / unset must all take the safe "do nothing" branch -- never a remote-side
    // comparison that could misparse.
    for v in ["", "0", "garbage", "true", "01"] {
        let out = stdout_of(&format!("bkshading_e2e_pause_restore_cmds '{v}'"));
        assert!(
            !out.contains("systemctl start"),
            "value {v:?} must be a no-op: {out}"
        );
    }
}

#[test]
fn restore_cmds_defaults_to_noop_with_no_argument() {
    let out = stdout_of("bkshading_e2e_pause_restore_cmds");
    assert!(
        !out.contains("systemctl start"),
        "no argument must default to the safe no-op branch: {out}"
    );
}

// ---------------------------------------------------------------------------------------------
// bkshading_e2e_pause_parse_state <label> <ssh_output> -> "0" or "1", FAIL-SAFE "0" default.
// ---------------------------------------------------------------------------------------------
#[test]
fn parse_state_extracts_1_when_the_marker_says_so() {
    let out = stdout_of("bkshading_e2e_pause_parse_state cam1 'BKSHADING_PAUSE_STATE:cam1:1'");
    assert_eq!(out, "1");
}

#[test]
fn parse_state_extracts_0_when_the_marker_says_so() {
    let out = stdout_of("bkshading_e2e_pause_parse_state cam1 'BKSHADING_PAUSE_STATE:cam1:0'");
    assert_eq!(out, "0");
}

#[test]
fn parse_state_defaults_to_0_on_missing_marker() {
    // never fails, never fabricates a "1" -- an ssh timeout/empty output is common and expected.
    assert_eq!(stdout_of("bkshading_e2e_pause_parse_state cam1 ''"), "0");
    assert_eq!(
        stdout_of("bkshading_e2e_pause_parse_state cam1 'some unrelated ssh noise'"),
        "0"
    );
}

#[test]
fn parse_state_defaults_to_0_on_malformed_marker() {
    assert_eq!(
        stdout_of("bkshading_e2e_pause_parse_state cam1 'BKSHADING_PAUSE_STATE:cam1:'"),
        "0"
    );
    assert_eq!(
        stdout_of("bkshading_e2e_pause_parse_state cam1 'BKSHADING_PAUSE_STATE:cam1:maybe'"),
        "0"
    );
}

#[test]
fn parse_state_ignores_a_different_labels_marker() {
    // a multi-box run's combined ssh output must never cross-contaminate boxes.
    let combined = "BKSHADING_PAUSE_STATE:cam2:1\nBKSHADING_PAUSE_STATE:cam1:0";
    assert_eq!(
        stdout_of(&format!(
            "bkshading_e2e_pause_parse_state cam1 '{combined}'"
        )),
        "0"
    );
    assert_eq!(
        stdout_of(&format!(
            "bkshading_e2e_pause_parse_state cam2 '{combined}'"
        )),
        "1"
    );
}

#[test]
fn parse_state_takes_the_last_matching_marker_line() {
    // defensive against a retried/duplicated ssh call embedding two marker lines for the same box.
    let combined = "BKSHADING_PAUSE_STATE:cam1:1\nBKSHADING_PAUSE_STATE:cam1:0";
    assert_eq!(
        stdout_of(&format!(
            "bkshading_e2e_pause_parse_state cam1 '{combined}'"
        )),
        "0"
    );
}

// ---------------------------------------------------------------------------------------------
// end-to-end pure round trip: stop_cmds' own echoed marker (with the remote-side variable
// literally SUBSTITUTED, simulating what the REMOTE shell would actually produce) must parse
// back through parse_state to the value that was substituted -- proves the print/parse contract
// agrees with itself without needing a real remote host.
// ---------------------------------------------------------------------------------------------
#[test]
fn stop_cmds_marker_round_trips_through_parse_state() {
    for was_active in ["0", "1"] {
        // Simulate remote execution: substitute the escaped remote variable the way the REAL
        // remote shell would, by sourcing the generated text with _bksh_was_active pre-seeded to
        // the value under test and capturing only the LAST line (the echoed marker).
        let body = format!(
            "_bksh_was_active={was_active}\n\
             out=\"$(bkshading_e2e_pause_stop_cmds cam1 | tail -1)\"\n\
             eval \"$out\""
        );
        let out = stdout_of(&body);
        let parsed = stdout_of(&format!("bkshading_e2e_pause_parse_state cam1 '{out}'"));
        assert_eq!(
            parsed, was_active,
            "round trip failed for {was_active}: marker={out}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// bkshading_e2e_pause_stop / bkshading_e2e_pause_restore never fail the caller even against an
// unreachable box (port 1 is a reserved/unassigned TCP port -- curl/ssh refuses instantly).
// ---------------------------------------------------------------------------------------------
#[test]
fn stop_orchestrator_never_fails_the_caller_on_an_unreachable_box() {
    let (rc, out, _err) = run_sourced(
        "set -e; ssh() { return 255; }; sshpass() { return 255; }; \
         r=\"$(bkshading_e2e_pause_stop cam9 127.0.0.1 fakepw 1)\"; echo \"RESULT:$r\"; echo AFTER",
    );
    assert_eq!(rc, 0, "must never fail the caller under set -e");
    assert!(
        out.contains("RESULT:0"),
        "an unreachable box must fail-safe to 0: {out}"
    );
    assert!(out.contains("AFTER"), "must return control: {out}");
}

#[test]
fn restore_orchestrator_never_fails_the_caller_on_an_unreachable_box() {
    let (rc, out, _err) = run_sourced(
        "set -e; ssh() { return 255; }; sshpass() { return 255; }; \
         bkshading_e2e_pause_restore cam9 127.0.0.1 fakepw 1 1; echo AFTER",
    );
    assert_eq!(rc, 0, "must never fail the caller under set -e");
    assert!(out.contains("AFTER"), "must return control: {out}");
}
