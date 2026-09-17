//! Write-burst session tests (issue 1337): the pure state machine + shell parsers, the projected
//! PUT-response state, the CLI-fallback write path (no per-write read, FIFO plan), the
//! plan-cached-across-a-burst behaviour, and the real `Gphoto2Shell` child against a fake
//! `gphoto2 --shell` script (no camera). Mirrors the fake-runner model of `tests/relay.rs`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use bkshading_proto::wire::{SetRequest, ShadingParams};
use bkshading_relay::burst::{
    burst_idle_expired, burst_step, shell_line_is_error, shell_line_is_prompt,
    shell_set_config_command, BurstAction, BurstEvent, BurstState, Gphoto2Shell,
};
use bkshading_relay::transport::{project_shading, ApplyOutcome, CameraSession, Gphoto2Runner};

// --- pure state machine + parsers -----------------------------------------------------------

#[test]
fn burst_state_machine_open_write_idleclose_1337() {
    // idle + set -> open + open-shell
    let (s, a) = burst_step(BurstState::Idle, BurstEvent::Set { now_ms: 100 }, 5_000);
    assert_eq!(
        s,
        BurstState::Open {
            last_activity_ms: 100
        }
    );
    assert_eq!(a, BurstAction::OpenShellThenWrite);
    // open + set -> write in shell, activity advances
    let (s, a) = burst_step(s, BurstEvent::Set { now_ms: 200 }, 5_000);
    assert_eq!(
        s,
        BurstState::Open {
            last_activity_ms: 200
        }
    );
    assert_eq!(a, BurstAction::WriteInShell);
    // write ok advances the idle clock
    let (s, a) = burst_step(s, BurstEvent::WriteOk { now_ms: 250 }, 5_000);
    assert_eq!(
        s,
        BurstState::Open {
            last_activity_ms: 250
        }
    );
    assert_eq!(a, BurstAction::StayOpen);
    // idle check not yet expired
    let (s, a) = burst_step(s, BurstEvent::IdleCheck { now_ms: 300 }, 5_000);
    assert_eq!(
        s,
        BurstState::Open {
            last_activity_ms: 250
        }
    );
    assert_eq!(a, BurstAction::Nothing);
    // idle check expired -> close + one authoritative read
    let (s, a) = burst_step(s, BurstEvent::IdleCheck { now_ms: 5_251 }, 5_000);
    assert_eq!(s, BurstState::Idle);
    assert_eq!(a, BurstAction::CloseShellFinalRead);
}

#[test]
fn burst_write_failed_and_idle_events_1337() {
    let open = BurstState::Open {
        last_activity_ms: 100,
    };
    let (s, a) = burst_step(open, BurstEvent::WriteFailed, 5_000);
    assert_eq!(s, BurstState::Idle);
    assert_eq!(a, BurstAction::KillShellFallback);
    // idle + any event is a no-op
    let (s, a) = burst_step(BurstState::Idle, BurstEvent::IdleCheck { now_ms: 9 }, 5_000);
    assert_eq!((s, a), (BurstState::Idle, BurstAction::Nothing));
    let (s, a) = burst_step(BurstState::Idle, BurstEvent::WriteOk { now_ms: 9 }, 5_000);
    assert_eq!((s, a), (BurstState::Idle, BurstAction::Nothing));
}

#[test]
fn burst_idle_expiry_boundary_1337() {
    assert!(!burst_idle_expired(0, 4_999, 5_000));
    assert!(burst_idle_expired(0, 5_000, 5_000));
    assert!(!burst_idle_expired(1_000, 500, 5_000)); // backwards step -> not expired
}

#[test]
fn shell_line_classifiers_1337() {
    assert!(shell_line_is_error("*** Error ***"));
    assert!(shell_line_is_error("Could not set config: PTP Error"));
    assert!(!shell_line_is_error(
        "Setting new value 'f/4.5' for f-number"
    ));
    assert!(shell_line_is_prompt("gphoto2: {/} "));
    assert!(!shell_line_is_prompt("Setting new value"));
    assert_eq!(
        shell_set_config_command("f-number", "f/4.5"),
        "set-config f-number=f/4.5"
    );
}

#[test]
fn project_shading_folds_writes_1337() {
    let base = ShadingParams {
        aperture_av: Some(4.0),
        aperture_norm: Some(0.0),
        iso: Some(400),
        ..Default::default()
    };
    let labels: Vec<String> = ["f/4.5", "f/5.0", "f/5.6", "f/8.0"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    // step to idx 1 (f/5.0): norm = 1/3
    let req = SetRequest {
        aperture_norm: Some(1.0 / 3.0),
        iso: Some(800),
        fps: Some(30),
        ..Default::default()
    };
    let p = project_shading(&base, &req, &labels);
    assert_eq!(p.iso, Some(800));
    assert_eq!(p.fps100, Some(3000));
    assert!((p.aperture_av.unwrap() - 2.0 * (5.0_f64).log2()).abs() < 1e-9);
    assert!((p.aperture_norm.unwrap() - 1.0 / 3.0).abs() < 1e-9);
    // the base is untouched (clone), and a None field leaves the base value.
    assert_eq!(base.aperture_av, Some(4.0));
    assert_eq!(
        project_shading(&base, &SetRequest::default(), &labels).iso,
        Some(400)
    );
}

// --- burst-driven submit (CLI-fallback path, no gphoto2 binary configured) -------------------

const AUTO_DETECT: &str = "\
Model                          Port
----------------------------------------------------------
Blackmagic Design Pocket Cinema Camera 4K usb:002,005";

/// A fake runner that COUNTS read-path USB sessions (auto_detect + the batched core read + the
/// focus/summary read) and RECORDS every `set_config`, answering reads from a full-camera config.
/// Lets a burst test assert: (a) a multi-param SET reads ONCE (no per-write read); (b) a second
/// SET in the same burst re-uses the cached plan (zero further reads); (c) the writes recorded.
/// Recorded `(key, value)` writes, shared with the test body.
type RecordedWrites = Arc<Mutex<Vec<(String, String)>>>;

struct BurstFakeRunner {
    configs: HashMap<String, String>,
    reads: Arc<AtomicUsize>,
    writes: RecordedWrites,
}

impl BurstFakeRunner {
    fn full() -> (Self, Arc<AtomicUsize>, RecordedWrites) {
        let mut configs = HashMap::new();
        configs.insert(
            "iso".into(),
            "Current: 400\nChoice: 0 100\nChoice: 1 200\nChoice: 2 400\nChoice: 3 800\nEND".into(),
        );
        configs.insert(
            "f-number".into(),
            "Current: f/5.2\nChoice: 0 f/2.8\nChoice: 1 f/4.0\nChoice: 2 f/5.2\nChoice: 3 f/8.0\nEND".into(),
        );
        configs.insert(
            "d002".into(),
            "Current: 18000\nBottom: 173\nTop: 36000\nEND".into(),
        );
        configs.insert(
            "d004".into(),
            "Current: 5600\nBottom: 2500\nTop: 10000\nEND".into(),
        );
        configs.insert("d005".into(), "Current: 0\nEND".into());
        configs.insert("d006".into(), "Current: 2500\nEND".into());
        configs.insert("d007".into(), "Current: 25\nBottom: 5\nTop: 60\nEND".into());
        let reads = Arc::new(AtomicUsize::new(0));
        let writes = Arc::new(Mutex::new(Vec::new()));
        (
            BurstFakeRunner {
                configs,
                reads: reads.clone(),
                writes: writes.clone(),
            },
            reads,
            writes,
        )
    }
}

impl Gphoto2Runner for BurstFakeRunner {
    fn auto_detect(&self) -> Result<String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(AUTO_DETECT.to_string())
    }
    fn get_config(&self, key: &str) -> Result<String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.configs
            .get(key)
            .cloned()
            .ok_or_else(|| anyhow!("no such config key: {key}"))
    }
    fn get_config_many(&self, keys: &[&str]) -> Result<Vec<String>> {
        // ONE USB session regardless of key count.
        self.reads.fetch_add(1, Ordering::SeqCst);
        keys.iter()
            .map(|k| {
                self.configs
                    .get(*k)
                    .cloned()
                    .ok_or_else(|| anyhow!("no such config key: {k}"))
            })
            .collect()
    }
    fn get_focus_and_summary(&self) -> Result<(String, String)> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok((String::new(), String::new()))
    }
    fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.writes
            .lock()
            .unwrap()
            .push((key.to_string(), value.to_string()));
        Ok(())
    }
}

#[test]
fn submit_burst_plans_without_per_write_read_and_returns_state_1337() {
    let (runner, reads, writes) = BurstFakeRunner::full();
    // No gphoto2 binary configured -> the burst uses the CLI write path (still pre-read-free).
    let session = CameraSession::new(Box::new(runner), "1.7.0-dev.516");
    let req = SetRequest {
        iso: Some(800),
        kelvin: Some(6500),
        tint: Some(10),
        ..Default::default()
    };
    match session.submit(&req).expect("submit ok") {
        ApplyOutcome::Applied { count, state } => {
            assert_eq!(count, 3, "iso + kelvin + tint = 3 writes");
            // the returned state projects the writes onto the burst-open read (immediate confirm).
            assert_eq!(state.params.iso, Some(800));
            assert_eq!(state.params.kelvin, Some(6500));
            assert_eq!(state.params.tint, Some(10));
            assert!(state.online, "state came from the burst-open read");
        }
        ApplyOutcome::Queued => panic!("an idle queue must RUN the SET"),
    }
    // The three writes were recorded (planned from the cached choices, no per-write read).
    let w = writes.lock().unwrap().clone();
    assert!(w.contains(&("iso".into(), "800".into())));
    assert!(w.contains(&("d004".into(), "6500".into())));
    assert!(w.contains(&("d005".into(), "10".into())));
    // ONE read cycle for the whole 3-param SET (detect + core batch + focus = 3 sessions), NOT
    // one read per write — the responsiveness win. The pre-fix per-write `apply` read 3x here.
    assert_eq!(
        reads.load(Ordering::SeqCst),
        3,
        "the whole burst SET reads the camera ONCE (3 USB sessions), not per write"
    );
}

#[test]
fn submit_burst_reuses_cached_plan_across_sets_1337() {
    let (runner, reads, _writes) = BurstFakeRunner::full();
    let session = CameraSession::new(Box::new(runner), "1.7.0-dev.516");
    let req = SetRequest {
        iso: Some(800),
        ..Default::default()
    };
    // First SET opens the burst and reads once (3 sessions).
    session.submit(&req).expect("submit ok");
    assert_eq!(reads.load(Ordering::SeqCst), 3);
    // A second SET while the burst is still open (no idle-close between) re-uses the cached plan
    // and reads NOTHING more — the persistent-burst benefit.
    session.submit(&req).expect("submit ok");
    assert_eq!(
        reads.load(Ordering::SeqCst),
        3,
        "the second SET in the burst plans from the cached choices (no further read)"
    );
}

// --- the real Gphoto2Shell child against a fake `gphoto2 --shell` script (unix) ---------------

#[cfg(unix)]
#[test]
fn gphoto2_shell_set_config_ok_and_error_1337() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("bksh-1337-shell-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // A fake `gphoto2 --shell`: reads a command line, prints a prompt line (fast completion).
    let ok = dir.join("gphoto2-shell-ok.sh");
    std::fs::write(
        &ok,
        "#!/bin/sh\nwhile IFS= read -r line; do printf 'gphoto2: {/} \\n'; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&ok, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut shell = Gphoto2Shell::open(&ok.to_string_lossy()).expect("open ok shell");
    shell
        .set_config("f-number", "f/4.5")
        .expect("set-config ok");
    shell
        .set_config("iso", "800")
        .expect("second set-config ok");
    shell.close();

    // A fake shell that prints an error line -> set_config returns Err (caller falls back to CLI).
    let bad = dir.join("gphoto2-shell-err.sh");
    std::fs::write(
        &bad,
        "#!/bin/sh\nwhile IFS= read -r line; do printf '*** Error ***\\n'; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut shell = Gphoto2Shell::open(&bad.to_string_lossy()).expect("open err shell");
    assert!(
        shell.set_config("f-number", "f/4.5").is_err(),
        "an error line makes set_config fail -> CLI fallback"
    );
    shell.close();

    // A missing binary -> open fails (the burst then uses CLI for the whole burst).
    assert!(Gphoto2Shell::open("/nonexistent/gphoto2-xyz").is_err());

    let _ = std::fs::remove_dir_all(&dir);
}
