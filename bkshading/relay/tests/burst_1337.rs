//! Write-burst session tests (issue 1337): the pure state machine + shell parsers, the projected
//! PUT-response state, the CLI-fallback write path (no per-write read, FIFO plan), the
//! plan-cached-across-a-burst behaviour, and the real `Gphoto2Shell` child against a fake
//! `gphoto2 --shell` script (no camera). Mirrors the fake-runner model of `tests/relay.rs`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use bkshading_proto::wire::{SetRequest, ShadingParams};
use bkshading_relay::burst::{
    burst_idle_expired, burst_step, shell_line_is_error, shell_line_is_prompt,
    shell_set_config_command, BurstAction, BurstEvent, BurstState, Gphoto2Shell,
};
use bkshading_relay::transport::{
    project_shading, ApplyOutcome, CameraSession, Gphoto2Runner, MonoClock,
};

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

// --- issue 1343: write-not-applied surfacing at the burst idle-close ---------------------------

/// A [`MonoClock`] a test drives directly, so the burst idle-close (issue 1337) fires without a
/// real 5 s sleep.
struct FakeClock(Arc<AtomicU64>);

impl MonoClock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[test]
fn burst_idle_close_flags_a_dropped_aperture_write_1343() {
    // The BurstFakeRunner RECORDS writes but never mutates its config map — so from the camera's
    // point of view every write is DROPPED (it ACKs + ignores it, exactly cam1's BMPCC today). We
    // write the aperture to a DIFFERENT choice index than the current f/5.2 (idx 2 -> idx 3, f/8.0)
    // and ISO to its CURRENT value (400). At the burst idle-close the relay takes ONE authoritative
    // read and compares each written key against it: the aperture never moved (dropped) -> flagged;
    // the ISO already equals the readback (applied) -> NOT flagged. So `not_applied == ["apertureNorm"]`.
    let (runner, _reads, _writes) = BurstFakeRunner::full();
    let clock = Arc::new(AtomicU64::new(0));
    // No gphoto2 binary -> the burst uses the CLI write path (the fake runner records the writes).
    let session = CameraSession::new(Box::new(runner), "1.7.0-dev.643")
        .with_clock(Box::new(FakeClock(clock.clone())));

    // t=0: open the burst and write aperture -> idx 3 (norm 1.0) + iso -> 400 (its current value).
    let req = SetRequest {
        aperture_norm: Some(1.0),
        iso: Some(400),
        ..Default::default()
    };
    session.submit(&req).expect("submit ok");

    // Advance past the 5 s idle window and read: this fires the burst idle-close authoritative read,
    // which fills `not_applied` from the written-vs-readback comparison.
    clock.store(6_000, Ordering::SeqCst);
    let state = session.read_state();

    assert!(state.online, "the authoritative read saw the camera");
    assert_eq!(
        state.not_applied,
        vec!["apertureNorm".to_string()],
        "the dropped aperture write is flagged; the applied ISO is not"
    );
}

#[test]
fn burst_idle_close_flags_nothing_when_the_write_matches_the_readback_1343() {
    // Write ISO to the readback's OWN current value (400) — the "camera applied it" case. The
    // aperture is left untouched (None), so the only written key equals the readback and
    // `not_applied` is empty.
    let (runner, _reads, _writes) = BurstFakeRunner::full();
    let clock = Arc::new(AtomicU64::new(0));
    let session = CameraSession::new(Box::new(runner), "1.7.0-dev.643")
        .with_clock(Box::new(FakeClock(clock.clone())));

    let req = SetRequest {
        iso: Some(400),
        ..Default::default()
    };
    session.submit(&req).expect("submit ok");

    clock.store(6_000, Ordering::SeqCst);
    let state = session.read_state();

    assert!(state.online);
    assert!(
        state.not_applied.is_empty(),
        "a write that matches the readback flags nothing: {:?}",
        state.not_applied
    );
}

// --- issue 1343 item 0: while a burst is open, read_state serves the PROJECTED state -----------

#[test]
fn read_state_serves_the_projected_state_while_the_burst_is_open_1343() {
    // The owner's "the number changes then reverts after half a second": a click's optimistic value
    // was reconciled back to the PRE-BURST value by the very next service-pump /api/state tick,
    // because read_state's Open arm returned the pre-burst read_cache snapshot. It must instead
    // serve the burst's PROJECTED state (open-read + every write applied so far) until the burst
    // idle-closes, when the authoritative read wins.
    let (runner, _reads, _writes) = BurstFakeRunner::full(); // current iso 400, kelvin 5600
    let clock = Arc::new(AtomicU64::new(0));
    let session = CameraSession::new(Box::new(runner), "1.7.0-dev.643")
        .with_clock(Box::new(FakeClock(clock.clone())));

    // Seed read_cache with the PRE-BURST value (iso 400) via one real read, so the mid-burst read
    // below proves the projection beats a POPULATED stale cache — the owner's exact "reverts to 400"
    // symptom, not merely an empty cache.
    clock.store(0, Ordering::SeqCst);
    assert_eq!(
        session.read_state().params.iso,
        Some(400),
        "pre-burst read caches iso 400"
    );

    // t=0: raise ISO to 10000. The camera's real current is 400; the burst opens.
    session
        .submit(&SetRequest {
            iso: Some(10000),
            ..Default::default()
        })
        .expect("submit ok");

    // t=100 (burst still Open): a pump read must report the PROJECTED iso 10000, NOT the pre-burst
    // 400 — this is the anti-revert fix.
    clock.store(100, Ordering::SeqCst);
    let mid = session.read_state();
    assert_eq!(
        mid.params.iso,
        Some(10000),
        "an open-burst read serves the projected iso, not the pre-burst value"
    );

    // t=200: a SECOND write in the SAME burst (kelvin) — the projection accumulates both.
    clock.store(200, Ordering::SeqCst);
    session
        .submit(&SetRequest {
            kelvin: Some(6500),
            ..Default::default()
        })
        .expect("submit ok");
    clock.store(300, Ordering::SeqCst);
    let mid2 = session.read_state();
    assert_eq!(mid2.params.iso, Some(10000), "iso still projected");
    assert_eq!(mid2.params.kelvin, Some(6500), "kelvin projected too");

    // t=6000: past the 5 s idle window -> the authoritative read wins. This fake runner never
    // applies a write, so the readback is the ORIGINAL iso 400 (and not_applied flags it).
    clock.store(6000, Ordering::SeqCst);
    let after = session.read_state();
    assert_eq!(
        after.params.iso,
        Some(400),
        "after idle-close the authoritative read replaces the projection"
    );
    assert!(
        after.not_applied.contains(&"iso".to_string()),
        "the dropped iso is flagged at close: {:?}",
        after.not_applied
    );
}
