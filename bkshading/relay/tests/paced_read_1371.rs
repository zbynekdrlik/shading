//! Issue 1371: the relay reads the camera through ONE persistent, PACED `gphoto2 --shell`
//! session (one key every `READ_PACING_MS`) instead of bursting the USB port with two gphoto2
//! processes per read cycle. These tests drive a fake `gphoto2 --shell` script: the shell's
//! `get-config` block reading, a full paced read through `CameraSession`, the session kept open
//! for the next cycle, and the CLI fallback when the shell fails.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Result};
use bkshading_relay::burst::{
    shell_get_config_command, shell_read_line, Gphoto2Shell, ShellReadLine,
};
use bkshading_relay::transport::{
    CameraSession, Gphoto2Runner, MonoClock, CORE_CONFIG_KEYS, PACED_READ_KEYS, READ_PACING_MS,
};

/// A scratch directory unique to this process AND this call (tests run in parallel threads).
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "bksh-1371-{tag}-{}-{}",
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

/// The blocks cam5's BMPCC answered on 14.10.2026 (off-grid f/4 lens: the `f-number` RADIO
/// reads `(null)`, the raw `5007` MENU reads 400).
const CAM5_BLOCKS: &[(&str, &str)] = &[
    ("iso", "Label: ISO Speed\nReadonly: 0\nType: RADIO\nCurrent: 800\nChoice: 0 400\nChoice: 1 800\nChoice: 2 1600\nEND"),
    ("f-number", "Label: F-Number\nReadonly: 0\nType: RADIO\nCurrent: (null)\nChoice: 0 f/0.19\nChoice: 1 f/0\nChoice: 2 f/4.5\nChoice: 3 f/4.8\nChoice: 4 f/5.6\nEND"),
    ("d002", "Label: PTP Property 0xd002\nReadonly: 0\nType: RANGE\nCurrent: 18000\nBottom: 173\nTop: 36000\nEND"),
    ("d004", "Label: PTP Property 0xd004\nReadonly: 0\nType: RANGE\nCurrent: 5600\nBottom: 2500\nTop: 10000\nEND"),
    ("d005", "Label: PTP Property 0xd005\nReadonly: 0\nType: MENU\nCurrent: 0\nEND"),
    ("d006", "Label: PTP Property 0xd006\nReadonly: 0\nType: MENU\nCurrent: 6000\nChoice: 0 2400\nChoice: 1 2500\nChoice: 2 3000\nChoice: 3 5000\nEND"),
    ("d007", "Label: PTP Property 0xd007\nReadonly: 0\nType: RANGE\nCurrent: 60\nBottom: 5\nTop: 60\nEND"),
    ("5007", "Label: F-Number\nReadonly: 0\nType: MENU\nCurrent: 400\nChoice: 0 19\nChoice: 1 0\nEND"),
    ("d003", "Label: PTP Property 0xd003\nReadonly: 0\nType: RANGE\nCurrent: 43913\nBottom: 0\nTop: 65535\nEND"),
];

/// Writes a fake `gphoto2 --shell`: it prints the prompt WITHOUT a newline (as the real shell
/// does), echoes each command line, answers `get-config <key>` with that key's block (or
/// `*** Error ***` for a key in `error_keys`), and logs every command to `cmds.log`. Each answer
/// ends with the prompt again, so a prompt fragment prefixes the next echoed command.
fn fake_shell(dir: &Path, error_keys: &[&str]) -> PathBuf {
    let mut cases = String::new();
    for (key, block) in CAM5_BLOCKS {
        if error_keys.contains(key) {
            cases.push_str(&format!(
                "    \"get-config {key}\") printf '*** Error (-2: Bad parameters) ***\\n' ;;\n"
            ));
        } else {
            cases.push_str(&format!(
                "    \"get-config {key}\") printf '%s\\n' '{block}' ;;\n"
            ));
        }
    }
    let log = dir.join("cmds.log");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{pids}'\nprintf 'gphoto2: {{/root}} /> '\nwhile IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> '{log}'\n  printf '%s\\n' \"$line\"\n  case \"$line\" in\n{cases}    quit) exit 0 ;;\n  esac\n  printf 'gphoto2: {{/root}} /> '\ndone\n",
        log = log.display(),
        pids = dir.join("pids.log").display()
    );
    let path = dir.join("gphoto2-shell.sh");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    wait_until_executable(&path);
    // The probe run above logged a pid; the tests count only the shells the relay starts.
    let _ = std::fs::remove_file(dir.join("pids.log"));
    path
}

/// Runs the freshly written script once (stdin closed, so it exits at once) until exec stops failing
/// with ETXTBSY: a child another test thread forked while this file was open for writing holds the
/// write handle until it execs, and an exec in that window fails "Text file busy" (CI, 14.10.2026:
/// the relay then fell back to the CLI read and the paced-read assertions failed).
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

#[test]
fn shell_read_lines_are_classified_1371() {
    assert_eq!(shell_read_line("Label: ISO Speed"), ShellReadLine::Start);
    assert_eq!(shell_read_line("END"), ShellReadLine::End);
    assert_eq!(shell_read_line("  END  "), ShellReadLine::End);
    assert_eq!(
        shell_read_line("*** Error (-2: 'Bad parameters') ***"),
        ShellReadLine::Error
    );
    // The echo, the prompt and a body line are all "other" (a body line may say anything).
    assert_eq!(shell_read_line("get-config d002"), ShellReadLine::Other);
    assert_eq!(
        shell_read_line("gphoto2: {/root} /> get-config iso"),
        ShellReadLine::Other
    );
    assert_eq!(
        shell_read_line("Choice: 3 Error-free"),
        ShellReadLine::Other
    );
    assert_eq!(shell_get_config_command("5007"), "get-config 5007");
}

#[cfg(unix)]
#[test]
fn the_shell_reads_one_block_per_key_and_reports_an_error_key_1371() {
    let scratch = Scratch::new("block");
    let bin = fake_shell(&scratch.0, &["d003"]);
    let mut shell = Gphoto2Shell::open(&bin.to_string_lossy()).expect("open the fake shell");
    let iso = shell
        .get_config("iso")
        .expect("shell alive")
        .expect("iso answered");
    assert_eq!(
        iso, CAM5_BLOCKS[0].1,
        "exactly the Label..END block, no echo, no prompt"
    );
    let raw = shell
        .get_config("5007")
        .expect("shell alive")
        .expect("5007 answered");
    assert!(raw.contains("Current: 400"), "{raw}");
    assert_eq!(
        shell
            .get_config("d003")
            .expect("an error answer leaves the shell alive"),
        None
    );
    // The shell still answers after an error.
    assert!(shell
        .get_config("d004")
        .unwrap()
        .unwrap()
        .contains("Current: 5600"));
    shell.close();
}

/// The runner the CLI path uses (the shell is a separate process). It counts its calls, so a
/// test can tell whether a cycle went through the shell or fell back to the CLI.
struct CountingCli {
    detects: Arc<AtomicUsize>,
    cli_reads: Arc<AtomicUsize>,
}

impl Gphoto2Runner for CountingCli {
    fn auto_detect(&self) -> Result<String> {
        self.detects.fetch_add(1, Ordering::SeqCst);
        Ok("Model  Port\n----\nBlackmagic Design Pocket Cinema Camera 4K usb:004,002".into())
    }
    fn get_config(&self, key: &str) -> Result<String> {
        match CAM5_BLOCKS.iter().find(|(k, _)| *k == key) {
            Some((_, block)) => Ok((*block).to_string()),
            None => bail!("no such key {key}"),
        }
    }
    fn get_config_many(&self, keys: &[&str]) -> Result<Vec<String>> {
        self.cli_reads.fetch_add(1, Ordering::SeqCst);
        keys.iter().map(|k| self.get_config(k)).collect()
    }
    fn set_config(&self, _key: &str, _value: &str) -> Result<()> {
        Ok(())
    }
}

struct FakeClock(Arc<AtomicU64>);

impl MonoClock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(unix)]
#[test]
fn a_read_cycle_sends_each_key_once_paced_through_one_kept_shell_1371() {
    let scratch = Scratch::new("cycle");
    let bin = fake_shell(&scratch.0, &[]);
    let detects = Arc::new(AtomicUsize::new(0));
    let cli_reads = Arc::new(AtomicUsize::new(0));
    let clock = Arc::new(AtomicU64::new(0));
    let session = CameraSession::new(
        Box::new(CountingCli {
            detects: detects.clone(),
            cli_reads: cli_reads.clone(),
        }),
        "1.7.0-dev.516",
    )
    .with_min_read_interval_ms(10_000)
    .with_clock(Box::new(FakeClock(clock.clone())))
    .with_gphoto2_binary(bin.to_string_lossy().to_string());

    let started = Instant::now();
    let st = session.read_state();
    let took = started.elapsed();
    assert!(st.online, "{st:?}");
    assert_eq!(st.params.iso, Some(800));
    assert_eq!(st.params.kelvin, Some(5600));
    assert_eq!(st.params.fps100, Some(6000));
    assert_eq!(st.params.focus_distance, Some(43913));
    let av = st.params.aperture_av.expect("aperture from the 5007 raw");
    assert!((av - 4.0).abs() < 1e-9, "f/4.0 -> AV 4.0, got {av}");
    assert_eq!(
        cli_reads.load(Ordering::SeqCst),
        0,
        "no CLI read: the shell answered"
    );
    let paced_floor_ms = READ_PACING_MS * (PACED_READ_KEYS.len() as u64 - 1);
    assert!(
        took.as_millis() as u64 >= paced_floor_ms,
        "{} keys {READ_PACING_MS} ms apart take at least {paced_floor_ms} ms, took {took:?}",
        PACED_READ_KEYS.len()
    );
    let want: Vec<String> = PACED_READ_KEYS
        .iter()
        .map(|k| format!("get-config {k}"))
        .collect();
    assert_eq!(shell_commands(&scratch.0), want, "each key once, in order");

    // The next cycle reuses the SAME shell process (the fake logs its pid once per start).
    clock.store(10_000, Ordering::SeqCst);
    assert!(session.read_state().online);
    assert_eq!(shell_commands(&scratch.0).len(), 2 * PACED_READ_KEYS.len());
    assert_eq!(cli_reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        detects.load(Ordering::SeqCst),
        1,
        "the camera is detected once"
    );
    let pids = std::fs::read_to_string(scratch.0.join("pids.log")).unwrap();
    assert_eq!(
        pids.lines().count(),
        1,
        "one shell for both cycles: {pids:?}"
    );
    // The core keys are a prefix of the paced keys, in the same order (the read maps by position).
    assert_eq!(
        &PACED_READ_KEYS[..CORE_CONFIG_KEYS.len()],
        &CORE_CONFIG_KEYS[..]
    );
}

#[cfg(unix)]
#[test]
fn a_core_key_error_falls_back_to_the_cli_read_and_stays_online_1371() {
    let scratch = Scratch::new("fallback");
    let bin = fake_shell(&scratch.0, &["d004"]);
    let cli_reads = Arc::new(AtomicUsize::new(0));
    let session = CameraSession::new(
        Box::new(CountingCli {
            detects: Arc::new(AtomicUsize::new(0)),
            cli_reads: cli_reads.clone(),
        }),
        "1.7.0-dev.516",
    )
    .with_gphoto2_binary(bin.to_string_lossy().to_string());
    let st = session.read_state();
    assert!(st.online, "the CLI read answered this cycle: {st:?}");
    assert_eq!(st.params.kelvin, Some(5600));
    assert_eq!(
        cli_reads.load(Ordering::SeqCst),
        1,
        "one CLI read after the shell failed"
    );
}

#[cfg(unix)]
#[test]
fn a_missing_best_effort_key_is_empty_not_a_failed_read_1371() {
    let scratch = Scratch::new("besteffort");
    let bin = fake_shell(&scratch.0, &["5007", "d003"]);
    let cli_reads = Arc::new(AtomicUsize::new(0));
    let session = CameraSession::new(
        Box::new(CountingCli {
            detects: Arc::new(AtomicUsize::new(0)),
            cli_reads: cli_reads.clone(),
        }),
        "1.7.0-dev.516",
    )
    .with_gphoto2_binary(bin.to_string_lossy().to_string());
    let st = session.read_state();
    assert!(st.online);
    assert_eq!(
        cli_reads.load(Ordering::SeqCst),
        0,
        "still the shell, no CLI read"
    );
    assert_eq!(st.params.focus_distance, None);
    assert_eq!(
        st.params.aperture_av, None,
        "no raw and a (null) current: no aperture"
    );
}
