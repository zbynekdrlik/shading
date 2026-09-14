//! #1311 (Finding 2 -> Mitigations 2b) — pure-function + static-anchor guard for
//! `scripts/lib/bkshading-relay-mode.sh` and its `rig-mode.sh` wiring: scope the bkshading-relay
//! to EVENT mode.
//!
//! Root cause: `rig-mode.sh test` used to leave the relay running during development, so every
//! measurement carried the relay's gphoto2 PTP-session power toggles + the issue-1229 polling
//! noise on the shared xHCI root hub that also carries the boot stick (issue 1309/1311 Finding
//! 1/2 — two boot sticks died in 24h on exactly the two boxes with a shading camera on that hub).
//! `test` now STOPS + DISABLES the relay, `event` ENABLES + STARTS it; the #808 E2E pause then
//! finds `was-active=0` in TEST mode and toggles nothing (a true no-op).
//!
//! Same convention as `tests/harness_bkshading_e2e_pause_808.rs`: source the REAL lib
//! (source-only, no side effects) and exercise the PURE remote-text builders directly, plus a
//! fake-`sshpass`-on-PATH functional check of the thin orchestrator, plus static anchors on
//! `scripts/rig-mode.sh`'s do_test/do_event wiring.
//! RED before the lib + wiring exist (sourcing fails / anchors absent, every test fails); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/bkshading-relay-mode.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the REAL lib and run `body` against its pure functions. Returns (exit, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\nset +e\n{body}");
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
    out
}

// --- pure builders -------------------------------------------------------------------------

#[test]
fn stop_cmds_stops_then_disables_the_relay_unit() {
    let out = stdout_of("bkshading_relay_mode_stop_cmds");
    assert!(
        out.contains("systemctl stop bkshading-relay.service"),
        "TEST mode must STOP the relay. Got:\n{out}"
    );
    assert!(
        out.contains("systemctl disable bkshading-relay.service"),
        "TEST mode must DISABLE the relay (no return on reboot). Got:\n{out}"
    );
    assert!(
        !out.contains("systemctl enable ") && !out.contains("systemctl start "),
        "stop_cmds must never enable/start. Got:\n{out}"
    );
    // stop precedes disable (stop removes the live PTP power draw immediately).
    assert!(
        out.find("systemctl stop").unwrap() < out.find("systemctl disable").unwrap(),
        "stop must precede disable. Got:\n{out}"
    );
}

#[test]
fn start_cmds_enables_then_starts_the_relay_unit() {
    let out = stdout_of("bkshading_relay_mode_start_cmds");
    assert!(
        out.contains("systemctl enable bkshading-relay.service"),
        "EVENT mode must ENABLE the relay. Got:\n{out}"
    );
    assert!(
        out.contains("systemctl start bkshading-relay.service"),
        "EVENT mode must START the relay. Got:\n{out}"
    );
    assert!(
        !out.contains("systemctl stop ") && !out.contains("systemctl disable "),
        "start_cmds must never stop/disable. Got:\n{out}"
    );
    assert!(
        out.find("systemctl enable").unwrap() < out.find("systemctl start").unwrap(),
        "enable must precede start. Got:\n{out}"
    );
}

#[test]
fn builders_use_the_one_source_of_truth_unit_name() {
    // The unit name comes from bkshading_relay_unit_name (bkshading-relay-runtime.sh), never a
    // second hardcoded literal — proven by the value matching for BOTH builders.
    let unit = stdout_of("bkshading_relay_unit_name").trim().to_string();
    assert_eq!(unit, "bkshading-relay.service", "unit name drifted");
    let stop = stdout_of("bkshading_relay_mode_stop_cmds");
    let start = stdout_of("bkshading_relay_mode_start_cmds");
    assert!(stop.contains(&unit) && start.contains(&unit));
}

#[test]
fn every_stop_and_start_line_is_tolerant_of_a_missing_or_already_in_state_unit() {
    // Every systemctl line ends `|| true` so a box without the unit installed (or already in the
    // target state) is a clean no-op that never fails the caller.
    for body in [
        "bkshading_relay_mode_stop_cmds",
        "bkshading_relay_mode_start_cmds",
    ] {
        let out = stdout_of(body);
        for line in out.lines().filter(|l| l.contains("systemctl")) {
            assert!(
                line.trim_end().ends_with("|| true"),
                "every systemctl line must be `|| true`-tolerant: {line}"
            );
        }
    }
}

// --- thin orchestrator (fake sshpass on PATH) ----------------------------------------------

/// Build a tempdir holding a fake `sshpass` that logs its argv to $LOG and exits 0, and prepend it
/// to PATH. Returns (dir, log_path). Using tempfile::tempdir avoids the #975 pid+timestamp
/// collision.
fn fake_sshpass_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("sshpass.log");
    let bin = dir.path().join("sshpass");
    std::fs::write(
        &bin,
        "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> \"$FAKE_SSHPASS_LOG\"\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (dir, log)
}

fn run_apply(
    action: &str,
    pairs: &[&str],
    log: &PathBuf,
    dir: &tempfile::TempDir,
) -> (i32, String) {
    let joined = pairs
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(" ");
    let body = format!("bkshading_relay_mode_apply {action} pw {joined}");
    let harness = format!("set -uo pipefail\n. \"$LIB\"\nset +e\n{body}");
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib())
        .env("PATH", path)
        .env("FAKE_SSHPASS_LOG", log)
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run apply harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn apply_test_stops_and_disables_each_roster_box() {
    let (dir, log) = fake_sshpass_dir();
    let (rc, stdout) = run_apply("test", &["cam1=10.0.0.1", "cam2=10.0.0.2"], &log, &dir);
    assert_eq!(rc, 0, "apply must never fail the caller");
    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(
        logged.matches("root@10.0.0.1").count() + logged.matches("root@10.0.0.2").count(),
        2,
        "both roster boxes must be contacted. log:\n{logged}"
    );
    assert!(logged.contains("systemctl stop bkshading-relay.service"));
    assert!(logged.contains("systemctl disable bkshading-relay.service"));
    assert!(!logged.contains("systemctl enable"));
    assert!(stdout.contains("cam1") && stdout.contains("cam2"));
}

#[test]
fn apply_event_enables_and_starts_each_roster_box() {
    let (dir, log) = fake_sshpass_dir();
    let (rc, _stdout) = run_apply("event", &["cam1=10.0.0.1", "cam2=10.0.0.2"], &log, &dir);
    assert_eq!(rc, 0);
    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(logged.contains("systemctl enable bkshading-relay.service"));
    assert!(logged.contains("systemctl start bkshading-relay.service"));
    assert!(!logged.contains("systemctl stop"));
}

#[test]
fn apply_skips_malformed_pairs_and_an_unknown_action_never_fails() {
    let (dir, log) = fake_sshpass_dir();
    // empty label/ip and a bare/equal pair are skipped; no box contacted.
    let (rc, _) = run_apply("test", &["", "=", "cam2="], &log, &dir);
    assert_eq!(rc, 0);
    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        logged.is_empty() || !logged.contains("root@"),
        "malformed pairs must contact no box. log:\n{logged}"
    );
    // an unknown action returns 0 and contacts nothing.
    let (dir2, log2) = fake_sshpass_dir();
    let (rc2, _) = run_apply("bogus", &["cam1=10.0.0.1"], &log2, &dir2);
    assert_eq!(rc2, 0, "unknown action must not fail the caller");
    let logged2 = std::fs::read_to_string(&log2).unwrap_or_default();
    assert!(
        !logged2.contains("root@"),
        "unknown action must contact nothing"
    );
}

// --- rig-mode.sh static anchors ------------------------------------------------------------

fn rig_mode_src() -> String {
    std::fs::read_to_string(manifest_dir().join("scripts/rig-mode.sh")).unwrap()
}

fn do_test_body(src: &str) -> String {
    src.split("\ndo_test() {")
        .nth(1)
        .and_then(|s| s.split("\ndo_event() {").next())
        .expect("rig-mode.sh must define do_test() then do_event()")
        .to_string()
}

fn do_event_body(src: &str) -> String {
    src.split("\ndo_event() {")
        .nth(1)
        .and_then(|s| s.split("\nmain() {").next())
        .expect("rig-mode.sh must define do_event() then main()")
        .to_string()
}

#[test]
fn rig_mode_sources_the_relay_mode_lib() {
    let src = rig_mode_src();
    assert!(
        src.contains("/lib/bkshading-relay-mode.sh"),
        "rig-mode.sh must source scripts/lib/bkshading-relay-mode.sh"
    );
}

#[test]
fn do_test_stops_and_disables_the_relay_via_the_helper() {
    let src = rig_mode_src();
    let body = do_test_body(&src);
    assert!(
        body.contains("bkshading_relay_mode_apply test"),
        "do_test (TEST mode) must stop+disable the relay via bkshading_relay_mode_apply test"
    );
    assert!(
        !body.contains("bkshading_relay_mode_apply event"),
        "do_test must NOT start the relay"
    );
}

#[test]
fn do_event_enables_and_starts_the_relay_via_the_helper() {
    let src = rig_mode_src();
    let body = do_event_body(&src);
    assert!(
        body.contains("bkshading_relay_mode_apply event"),
        "do_event (EVENT mode) must enable+start the relay via bkshading_relay_mode_apply event"
    );
    assert!(
        !body.contains("bkshading_relay_mode_apply test"),
        "do_event must NOT stop the relay"
    );
}
