//! Write-burst session for immediate shading control (issue 1337).
//!
//! The owner's complaint (live shading, 17.9.2026): every panel click had a ~1-3 s response
//! because the relay ran a full `read_raw()` (3 gphoto2 spawns) BEFORE every write and one
//! `gphoto2 --set-config` spawn per param — ~4 processes per click, each a full USB-PTP
//! open/enumerate/close cycle. The fix keeps a persistent `gphoto2 --shell` child alive across a
//! BURST of rapid writes so the per-write USB re-enumeration is paid once, and plans each write
//! from the CACHED f-number choices (no pre-write read). Outside a burst nothing changes — reads
//! stay per-invocation CLI, floored (issue 1229 "quiet bus" doctrine).
//!
//! SAFETY (why the persistent `--shell` was rejected for READS in issue 1229 but is safe here for
//! WRITES): the shell is a best-effort OPTIMISATION with a hard CLI FALLBACK. It is opened only on
//! the first set of a burst, closed after [`WRITE_SESSION_IDLE_MS`] idle, and any error / no
//! response within [`WRITE_SESSION_WEDGE_MS`] KILLS the child and falls the current set back to a
//! per-invocation CLI `--set-config` (bounded by the existing 8 s `GPHOTO2_TIMEOUT`). So a broken
//! or slow shell never wedges the relay and never loses a write — it degrades to the (still
//! pre-read-free, ~1-spawn) CLI path. The `--shell` command grammar + prompt shape are UNVERIFIED
//! against a live camera in a Tier-0 lane (no gphoto2, no camera); the fallback is exactly why
//! that is acceptable — a shell that never completes simply always falls back to CLI, which the
//! supervisor's rig-acceptance step confirms/tunes. The pure pieces below (the state machine, the
//! idle-timing decision, the shell-line classifiers) ARE Tier-0 tested.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

/// After this long with NO set, the burst's persistent `gphoto2 --shell` child is closed and the
/// relay does ONE authoritative read (the "state read at burst close"). 5 s comfortably spans the
/// gaps between an operator's rapid clicks while releasing the shared USB bus promptly once they
/// stop (issue 1337; the issue-1229 quiet-bus doctrine resumes the moment the burst closes).
pub const WRITE_SESSION_IDLE_MS: u64 = 5_000;

/// Wedge bound: no completion from the shell child within this ⇒ the child is KILLED and the
/// current set falls back to a per-invocation CLI write (issue 1337, the wedge-watchdog pattern —
/// a bounded kill boundary, never an unbounded block). Well under the 8 s CLI `GPHOTO2_TIMEOUT`.
pub const WRITE_SESSION_WEDGE_MS: u64 = 3_000;

/// Quiet-gap that marks a shell command complete: after a `set-config` the shell prints its prompt
/// (which carries no trailing newline, so it is never delivered as a line) and waits. We conclude
/// the command finished when no new output line arrives for this long — bounded overall by
/// [`WRITE_SESSION_WEDGE_MS`]. `set-config` is fast (the camera applies immediately), so this is a
/// short, safe heuristic; the CLI fallback covers any case where it misjudges.
const SHELL_QUIET_GAP_MS: u64 = 300;

/// Whether an OPEN burst (last activity at `last_activity_ms`) is now idle-expired at `now_ms` and
/// its shell should be closed. Monotonic ms; a backwards step saturates to 0 (treated NOT expired —
/// keep the shell rather than churn it). Pure; Tier-0 tested + rustc-replicated.
pub fn burst_idle_expired(last_activity_ms: u64, now_ms: u64, idle_ms: u64) -> bool {
    now_ms.saturating_sub(last_activity_ms) >= idle_ms
}

/// The burst lifecycle state (issue 1337). `Idle` = no shell open; `Open` = a `gphoto2 --shell`
/// child is alive, last touched at `last_activity_ms`. Pure — the I/O layer stores this and drives
/// it through [`burst_step`], so every transition is exercised without a camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BurstState {
    #[default]
    Idle,
    Open {
        last_activity_ms: u64,
    },
}

/// An event that moves the burst state machine (issue 1337).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurstEvent {
    /// A shading SET arrived.
    Set { now_ms: u64 },
    /// A shell write completed successfully (refresh the idle clock).
    WriteOk { now_ms: u64 },
    /// A shell write failed / wedged (kill the child, fall back to CLI, reset to Idle).
    WriteFailed,
    /// A periodic idle check (driven by the read path, which runs ~every 2 s).
    IdleCheck { now_ms: u64 },
}

/// The action the I/O layer must take for a [`burst_step`] transition (issue 1337).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurstAction {
    /// Idle + Set: open the shell (one read to plan if no cache), then write in it.
    OpenShellThenWrite,
    /// Open + Set: write in the already-open shell (no read, no re-open).
    WriteInShell,
    /// A successful write: nothing to do but the idle clock advanced (in the returned state).
    StayOpen,
    /// The burst went idle: close the shell and do ONE authoritative read at burst close.
    CloseShellFinalRead,
    /// A write failed: kill the shell child and fall this set back to CLI.
    KillShellFallback,
    /// No-op (e.g. an idle check while already Idle).
    Nothing,
}

/// The pure burst state machine (issue 1337): given the current [`BurstState`] and a
/// [`BurstEvent`], return the next state and the [`BurstAction`] the I/O layer performs. Total +
/// deterministic — Tier-0 tested + rustc-replicated. Nothing here touches a process or a camera.
pub fn burst_step(state: BurstState, event: BurstEvent, idle_ms: u64) -> (BurstState, BurstAction) {
    match (state, event) {
        (BurstState::Idle, BurstEvent::Set { now_ms }) => (
            BurstState::Open {
                last_activity_ms: now_ms,
            },
            BurstAction::OpenShellThenWrite,
        ),
        (BurstState::Open { .. }, BurstEvent::Set { now_ms }) => (
            BurstState::Open {
                last_activity_ms: now_ms,
            },
            BurstAction::WriteInShell,
        ),
        (BurstState::Open { .. }, BurstEvent::WriteOk { now_ms }) => (
            BurstState::Open {
                last_activity_ms: now_ms,
            },
            BurstAction::StayOpen,
        ),
        (_, BurstEvent::WriteFailed) => (BurstState::Idle, BurstAction::KillShellFallback),
        (BurstState::Open { last_activity_ms }, BurstEvent::IdleCheck { now_ms }) => {
            if burst_idle_expired(last_activity_ms, now_ms, idle_ms) {
                (BurstState::Idle, BurstAction::CloseShellFinalRead)
            } else {
                (BurstState::Open { last_activity_ms }, BurstAction::Nothing)
            }
        }
        // Idle + (WriteOk | IdleCheck): nothing to do.
        (BurstState::Idle, _) => (BurstState::Idle, BurstAction::Nothing),
    }
}

/// Whether a `gphoto2 --shell` output line reports an ERROR (issue 1337). gphoto2 prints error
/// diagnostics as `*** Error ***` / `*** Error (…): …` and various `Error`-bearing lines; any of
/// those means the set-config did NOT apply, so the caller kills the shell and falls back to CLI.
/// Pure — Tier-0 tested. Deliberately liberal (a false "error" only forces a safe CLI fallback).
pub fn shell_line_is_error(line: &str) -> bool {
    let l = line.trim();
    l.starts_with("***") || l.to_ascii_lowercase().contains("error")
}

/// Whether a `gphoto2 --shell` output line is the interactive PROMPT (issue 1337), e.g.
/// `gphoto2: {/} …` — a best-effort completion hint. NOTE the prompt usually carries no trailing
/// newline, so it is rarely delivered as a whole line; completion is primarily judged by the
/// quiet-gap heuristic (see [`Gphoto2Shell::set_config`]). Pure — Tier-0 tested.
pub fn shell_line_is_prompt(line: &str) -> bool {
    line.trim_start().starts_with("gphoto2:")
}

/// The `gphoto2 --shell` command that sets one config key (issue 1337). Mirrors the CLI
/// `--set-config <key>=<value>` flag shape, which the interactive shell's `set-config` accepts.
/// UNVERIFIED against a live camera in a Tier-0 lane — a wrong grammar simply makes every set
/// return an error line ⇒ the safe CLI fallback. Pure — pinned by a test so the shape is explicit.
pub fn shell_set_config_command(key: &str, value: &str) -> String {
    format!("set-config {key}={value}")
}

/// A persistent `gphoto2 --shell` child used ONLY inside a write burst (issue 1337). Best-effort:
/// every method that can fail returns `Err`, and the caller (`CameraSession`) kills the child and
/// falls the current set back to a per-invocation CLI write. Never holds a lock across an unbounded
/// wait — reads are bounded by [`WRITE_SESSION_WEDGE_MS`].
pub struct Gphoto2Shell {
    child: Child,
    stdin: ChildStdin,
    /// Combined stdout+stderr lines from the shell child, delivered by a reader thread so a read
    /// never blocks the caller past the wedge deadline.
    lines: Receiver<String>,
    binary: String,
}

impl Gphoto2Shell {
    /// Spawns `gphoto2 --shell` (issue 1337). The child auto-detects the camera on its first
    /// command. Returns `Err` if the process cannot be spawned (⇒ the caller uses CLI for the
    /// whole burst). A reader thread drains stdout so a chatty shell can never deadlock the wait.
    pub fn open(binary: &str) -> Result<Self> {
        let mut child = Command::new(binary)
            .arg("--shell")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn {binary} --shell"))?;
        let stdin = child.stdin.take().context("shell child has no stdin")?;
        let stdout = child.stdout.take().context("shell child has no stdout")?;
        let (tx, rx) = mpsc::channel::<String>();
        // ONE reader thread over stdout (line-buffered). stderr is drained too so a large error
        // burst cannot fill the pipe and block the child; it is merged into the same channel.
        let tx_out = tx.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut buf = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf) {
                    Ok(0) => break, // EOF (child exited)
                    Ok(_) => {
                        if tx_out.send(buf.trim_end().to_string()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stderr);
                let mut buf = String::new();
                loop {
                    buf.clear();
                    match reader.read_line(&mut buf) {
                        Ok(0) => break,
                        Ok(_) => {
                            if tx.send(buf.trim_end().to_string()).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
        Ok(Gphoto2Shell {
            child,
            stdin,
            lines: rx,
            binary: binary.to_string(),
        })
    }

    /// Applies ONE `set-config key=value` through the persistent shell (issue 1337). Writes the
    /// command, then reads output lines until either an error line (→ `Err`), a quiet gap of
    /// [`SHELL_QUIET_GAP_MS`] with no error (→ `Ok`, the command completed), or the overall
    /// [`WRITE_SESSION_WEDGE_MS`] wedge deadline (→ `Err`, the caller kills + falls back). A single
    /// gphoto2 `set-config` is fast, so the quiet-gap resolves quickly; the wedge bounds any hang.
    pub fn set_config(&mut self, key: &str, value: &str) -> Result<()> {
        let cmd = shell_set_config_command(key, value);
        writeln!(self.stdin, "{cmd}")
            .with_context(|| format!("write to {} --shell", self.binary))?;
        self.stdin
            .flush()
            .with_context(|| format!("flush {} --shell", self.binary))?;
        let deadline = Instant::now() + Duration::from_millis(WRITE_SESSION_WEDGE_MS);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("gphoto2 --shell set-config {key}={value} wedged (no completion in {WRITE_SESSION_WEDGE_MS} ms)");
            }
            let gap = remaining.min(Duration::from_millis(SHELL_QUIET_GAP_MS));
            match self.lines.recv_timeout(gap) {
                Ok(line) => {
                    if shell_line_is_error(&line) {
                        bail!("gphoto2 --shell set-config {key}={value} error: {line}");
                    }
                    // A prompt line (rare — usually no newline) means done with no error.
                    if shell_line_is_prompt(&line) {
                        return Ok(());
                    }
                    // Otherwise keep reading; a fresh line resets the quiet-gap window.
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Quiet for a full gap with no error seen ⇒ the set-config completed.
                    return Ok(());
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("gphoto2 --shell exited during set-config {key}={value}");
                }
            }
        }
    }

    /// Kills + reaps the shell child (issue 1337). Called on close (idle) or on any error before
    /// the CLI fallback. Best-effort — a kill/wait error is swallowed (the child is going away).
    pub fn close(mut self) {
        let _ = self.stdin.write_all(b"quit\n");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
