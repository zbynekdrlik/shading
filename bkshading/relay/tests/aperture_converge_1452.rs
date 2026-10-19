//! Issue 1452: the relay converges an aperture write onto the requested listed f-number and
//! reports where the iris really landed. Live 19.10.2026: cam6 (BMPCC 6K, fw 8.1) lands ONE listed
//! step past the request (f/4.0 -> request f/4.5 -> f/4.8), cam7 (fw 8.6) does not move at all.
//! These tests drive a STATEFUL fake `gphoto2 --shell` that moves its 0x5007 value like those
//! bodies, through the real `CameraSession` write burst: the write, the 0x5007 read-back through
//! the same shell, the ONE correcting write, and the idle-close report.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};
use bkshading_proto::mapping::fnumber_to_av;
use bkshading_proto::wire::SetRequest;
use bkshading_relay::transport::{
    ApplyOutcome, CameraSession, Gphoto2Runner, MonoClock, APERTURE_SETTLE_MAX_READS,
};

/// A scratch directory unique to this process AND this call (tests run in parallel threads).
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "bksh-1452-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// How the fake body's iris answers an `f-number` write.
#[derive(Clone, Copy)]
enum Body {
    /// cam6: an upward write lands ONE listed step past the request; a downward write lands on it.
    OvershootsUp,
    /// cam7: every write is ACKed and the iris stays where it was.
    NeverMoves,
}

/// The five listed f-numbers of the fake body (x100), ascending: f/4 f/4.5 f/4.8 f/5.2 f/5.6.
const LIST: &str =
    "Choice: 0 f/4\\nChoice: 1 f/4.5\\nChoice: 2 f/4.8\\nChoice: 3 f/5.2\\nChoice: 4 f/5.6";

/// Writes the fake `gphoto2 --shell`. It keeps the iris value `AP` (x100, starting at `start`),
/// answers `get-config 5007` / `get-config f-number` from it, moves it on `set-config f-number=`
/// per `body`, answers the other paced-read keys with static blocks, and logs every command.
fn fake_body(dir: &Path, start: i64, body: Body) -> PathBuf {
    let mv = match body {
        Body::OvershootsUp => "if [ \"$v\" -gt \"$AP\" ]; then AP=$(next \"$v\"); else AP=$v; fi",
        Body::NeverMoves => ":",
    };
    let log = dir.join("cmds.log");
    let script = format!(
        r#"#!/bin/sh
AP={start}
label() {{ case "$1" in 400) echo f/4 ;; 450) echo f/4.5 ;; 480) echo f/4.8 ;; 520) echo f/5.2 ;; 560) echo f/5.6 ;; *) echo '(null)' ;; esac; }}
value() {{ case "$1" in f/4) echo 400 ;; f/4.5) echo 450 ;; f/4.8) echo 480 ;; f/5.2) echo 520 ;; f/5.6) echo 560 ;; *) echo 0 ;; esac; }}
next() {{ case "$1" in 400) echo 450 ;; 450) echo 480 ;; 480) echo 520 ;; *) echo 560 ;; esac; }}
printf 'gphoto2: {{/root}} /> '
while IFS= read -r line; do
  printf '%s\n' "$line" >> '{log}'
  printf '%s\n' "$line"
  case "$line" in
    "get-config 5007") printf 'Label: F-Number\nReadonly: 0\nType: MENU\nCurrent: %s\nEND\n' "$AP" ;;
    "get-config f-number") printf 'Label: F-Number\nReadonly: 0\nType: RADIO\nCurrent: %s\n{list}\nEND\n' "$(label "$AP")" ;;
    "get-config iso") printf 'Label: ISO Speed\nReadonly: 0\nType: RADIO\nCurrent: 800\nChoice: 0 400\nChoice: 1 800\nEND\n' ;;
    "get-config d002") printf 'Label: PTP Property 0xd002\nReadonly: 0\nType: RANGE\nCurrent: 18000\nBottom: 173\nTop: 36000\nEND\n' ;;
    "get-config d004") printf 'Label: PTP Property 0xd004\nReadonly: 0\nType: RANGE\nCurrent: 5600\nBottom: 2500\nTop: 10000\nEND\n' ;;
    "get-config d005") printf 'Label: PTP Property 0xd005\nReadonly: 0\nType: MENU\nCurrent: 0\nEND\n' ;;
    "get-config d006") printf 'Label: PTP Property 0xd006\nReadonly: 0\nType: MENU\nCurrent: 2500\nChoice: 0 2500\nEND\n' ;;
    "get-config d007") printf 'Label: PTP Property 0xd007\nReadonly: 0\nType: RANGE\nCurrent: 25\nBottom: 5\nTop: 60\nEND\n' ;;
    "get-config d003") printf 'Label: PTP Property 0xd003\nReadonly: 0\nType: RANGE\nCurrent: 100\nBottom: 0\nTop: 65535\nEND\n' ;;
    "set-config f-number="*) v=$(value "${{line#set-config f-number=}}"); {mv} ;;
    quit) exit 0 ;;
  esac
  printf 'gphoto2: {{/root}} /> '
done
"#,
        log = log.display(),
        list = LIST,
    );
    let path = dir.join("gphoto2-shell.sh");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    wait_until_executable(&path);
    let _ = std::fs::remove_file(&log);
    path
}

/// Runs the freshly written script once (stdin closed, it exits at once) until exec stops failing
/// with ETXTBSY — the same parallel-test race the issue-1371 paced-read tests guard against.
fn wait_until_executable(path: &Path) {
    for _ in 0..100 {
        match std::process::Command::new(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Ok(_) => return,
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            Err(e) => panic!("cannot run the fake shell {}: {e}", path.display()),
        }
    }
    panic!(
        "the fake shell {} stayed busy (ETXTBSY) for 2 s",
        path.display()
    );
}

fn shell_commands(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("cmds.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// The CLI runner: only the camera detect answers. Every read and write in these tests must go
/// through the shell; a CLI fallback reads as an offline camera and fails the test loudly.
struct DetectOnly;

impl Gphoto2Runner for DetectOnly {
    fn auto_detect(&self) -> Result<String> {
        Ok("Model  Port\n----\nBlackmagic Design Pocket Cinema Camera 6K usb:004,002".into())
    }
    fn get_config(&self, key: &str) -> Result<String> {
        bail!("CLI get-config {key} used; the shell should have answered")
    }
    fn set_config(&self, key: &str, _value: &str) -> Result<()> {
        bail!("CLI set-config {key} used; the shell should have written it")
    }
}

struct FakeClock(Arc<AtomicU64>);

impl MonoClock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn session(bin: &Path, clock: &Arc<AtomicU64>) -> CameraSession {
    CameraSession::new(Box::new(DetectOnly), "1.7.0-dev.810")
        .with_clock(Box::new(FakeClock(clock.clone())))
        .with_gphoto2_binary(bin.to_string_lossy().to_string())
        .with_aperture_settle_poll_ms(5)
}

/// `apertureNorm` of listed index `idx` on the five-value list.
fn norm(idx: usize) -> f64 {
    idx as f64 / 4.0
}

fn av(f: f64) -> f64 {
    fnumber_to_av(f).unwrap()
}

fn aperture_writes(cmds: &[String]) -> Vec<String> {
    cmds.iter()
        .filter(|c| c.starts_with("set-config f-number="))
        .cloned()
        .collect()
}

#[cfg(unix)]
#[test]
fn an_overshoot_gets_one_correcting_step_and_lands_on_the_request_1452() {
    // cam6: f/4.0 -> request f/4.5 -> the body lands f/4.8 -> ONE step back -> f/4.5.
    let scratch = Scratch::new("overshoot");
    let bin = fake_body(&scratch.0, 400, Body::OvershootsUp);
    let clock = Arc::new(AtomicU64::new(0));
    let s = session(&bin, &clock);

    let outcome = s
        .submit(&SetRequest {
            aperture_norm: Some(norm(1)),
            ..Default::default()
        })
        .expect("submit ok");
    let ApplyOutcome::Applied { state, .. } = outcome else {
        panic!("an idle queue runs the SET")
    };
    let cmds = shell_commands(&scratch.0);
    assert_eq!(
        aperture_writes(&cmds),
        vec![
            "set-config f-number=f/4.5".to_string(),
            "set-config f-number=f/4.5".to_string()
        ],
        "the write, then ONE correcting write back to the request: {cmds:?}"
    );
    let reads_after_write = cmds
        .iter()
        .skip_while(|c| !c.starts_with("set-config f-number="))
        .filter(|c| *c == "get-config 5007")
        .count();
    assert!(
        (2..=2 * APERTURE_SETTLE_MAX_READS).contains(&reads_after_write),
        "the read-back goes through the shell and stays bounded: {reads_after_write} reads"
    );
    // The immediate confirmation carries the real landed value.
    let got = state.params.aperture_av.expect("aperture reported");
    assert!((got - av(4.5)).abs() < 1e-9, "landed f/4.5, got AV {got}");

    // The idle-close authoritative read agrees, and nothing is flagged.
    clock.store(6_000, Ordering::SeqCst);
    let closed = s.read_state();
    assert!(closed.online, "{closed:?}");
    assert!(closed.not_applied.is_empty(), "{:?}", closed.not_applied);
    let got = closed.params.aperture_av.expect("aperture read");
    assert!(
        (got - av(4.5)).abs() < 1e-9,
        "camera at f/4.5, got AV {got}"
    );
}

#[cfg(unix)]
#[test]
fn a_write_that_moved_but_landed_elsewhere_is_not_reported_as_ignored_1452() {
    // f/4.8 -> request f/5.2 -> the body lands f/5.6, the LAST listed value (the lens's own stop):
    // no correction, and the iris moved, so the report must not say "ignored".
    let scratch = Scratch::new("elsewhere");
    let bin = fake_body(&scratch.0, 480, Body::OvershootsUp);
    let clock = Arc::new(AtomicU64::new(0));
    let s = session(&bin, &clock);

    let outcome = s
        .submit(&SetRequest {
            aperture_norm: Some(norm(3)),
            ..Default::default()
        })
        .expect("submit ok");
    let ApplyOutcome::Applied { state, .. } = outcome else {
        panic!("an idle queue runs the SET")
    };
    assert_eq!(
        aperture_writes(&shell_commands(&scratch.0)),
        vec!["set-config f-number=f/5.2".to_string()],
        "no correction from a list end"
    );
    let got = state.params.aperture_av.expect("aperture reported");
    assert!(
        (got - av(5.6)).abs() < 1e-9,
        "the confirmation shows where the iris really is (f/5.6), got AV {got}"
    );

    clock.store(6_000, Ordering::SeqCst);
    let closed = s.read_state();
    assert!(closed.online, "{closed:?}");
    assert!(
        !closed.not_applied.contains(&"apertureNorm".to_string()),
        "a write that moved the iris is applied, not ignored: {:?}",
        closed.not_applied
    );
    let got = closed.params.aperture_av.expect("aperture read");
    assert!(
        (got - av(5.6)).abs() < 1e-9,
        "the real value f/5.6, got AV {got}"
    );
}

#[cfg(unix)]
#[test]
fn a_write_the_body_ignores_is_never_corrected_and_stays_flagged_1452() {
    // cam7: the write is ACKed, the iris stays at f/4.0. The read-back ends at its bound, no
    // correcting write runs, and the idle-close keeps the issue-1343 flag.
    let scratch = Scratch::new("ignored");
    let bin = fake_body(&scratch.0, 400, Body::NeverMoves);
    let clock = Arc::new(AtomicU64::new(0));
    let s = session(&bin, &clock);

    s.submit(&SetRequest {
        aperture_norm: Some(norm(1)),
        ..Default::default()
    })
    .expect("submit ok");
    let cmds = shell_commands(&scratch.0);
    assert_eq!(
        aperture_writes(&cmds),
        vec!["set-config f-number=f/4.5".to_string()],
        "an unmoved iris is never corrected: {cmds:?}"
    );
    let reads_after_write = cmds
        .iter()
        .skip_while(|c| !c.starts_with("set-config f-number="))
        .filter(|c| *c == "get-config 5007")
        .count();
    assert_eq!(
        reads_after_write, APERTURE_SETTLE_MAX_READS,
        "an unmoved iris is read until the bound, never longer"
    );

    clock.store(6_000, Ordering::SeqCst);
    let closed = s.read_state();
    assert!(closed.online, "{closed:?}");
    assert_eq!(
        closed.not_applied,
        vec!["apertureNorm".to_string()],
        "an ignored write is still reported as ignored"
    );
}
