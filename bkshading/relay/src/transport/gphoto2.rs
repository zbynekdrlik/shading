//! The `gphoto2` CLI seam (moved out of `transport.rs`, issue 808, no behaviour change): the
//! [`Gphoto2Runner`] trait, the real [`Gphoto2Cli`] with its bounded, logged command runner (issue
//! 1309), and the pure parsers of its output. Re-exported from `transport`.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::reads::{build_get_config_many_args, split_config_blocks, FOCUS_DISTANCE_KEY};

/// Hard per-gphoto2-command timeout (issue 1309). A gphoto2 that hangs on a busy / unresponsive
/// USB-PTP device would otherwise hold the serialized camera lock (and its child) forever, wedging
/// the relay — the 2026-09-13/15 half-dead-cambox class. After this bound the child is KILLED and
/// reaped, and the command reports an error (a read then degrades to offline, a write returns 502).
pub const GPHOTO2_TIMEOUT: Duration = Duration::from_secs(8);

/// Last ~200 chars of a gphoto2 stderr (issue 1309), trimmed, for the error log line. Pure.
pub fn gphoto2_stderr_tail(stderr: &str) -> String {
    let trimmed = stderr.trim();
    let n = trimmed.chars().count();
    if n <= 200 {
        trimmed.to_string()
    } else {
        let tail: String = trimmed.chars().skip(n - 200).collect();
        format!("…{tail}")
    }
}

/// Runs a prepared command with a hard `timeout`, KILLING + reaping the child on overrun (issue
/// 1309). Returns `(output, elapsed, timed_out)`. stdout/stderr are drained on their own threads
/// so a large output can never deadlock the wait; on the deadline the child is `kill()`ed and
/// `wait()`ed (reaped — no zombie/orphan), and the reader threads finish as the pipes close.
fn run_with_timeout(
    mut cmd: std::process::Command,
    timeout: Duration,
) -> std::io::Result<(std::process::Output, Duration, bool)> {
    use std::io::Read;
    use std::process::Stdio;
    let start = Instant::now();
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let th_o = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(s) = so.as_mut() {
            let _ = s.read_to_end(&mut b);
        }
        b
    });
    let th_e = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(s) = se.as_mut() {
            let _ = s.read_to_end(&mut b);
        }
        b
    });
    let mut timed_out = false;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            timed_out = true;
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = th_o.join().unwrap_or_default();
    let stderr = th_e.join().unwrap_or_default();
    Ok((
        std::process::Output {
            status,
            stdout,
            stderr,
        },
        start.elapsed(),
        timed_out,
    ))
}

/// The seam over the `gphoto2` CLI. Blocking (std process); handlers call it via
/// `spawn_blocking`. `Send + Sync` so it can live behind an `Arc` shared across tasks.
pub trait Gphoto2Runner: Send + Sync {
    /// `gphoto2 --auto-detect` stdout.
    fn auto_detect(&self) -> Result<String>;
    /// `gphoto2 --get-config <key>` stdout.
    fn get_config(&self, key: &str) -> Result<String>;
    /// Reads MANY config keys in ONE gphoto2 invocation (issue 1229): the real transport spawns a
    /// single `gphoto2 --get-config k1 --get-config k2 …` process — ONE USB open/enumerate/close
    /// cycle for all keys, instead of one per key — and returns one stdout block per key, in the
    /// order of `keys`. On success the returned `Vec` has EXACTLY `keys.len()` elements; a spawn
    /// failure, a non-zero gphoto2 exit, or a block/key count mismatch is an `Err` (the read then
    /// degrades to offline, the same as a failed single `get_config`).
    ///
    /// The default impl loops `get_config` (one USB session PER key) — correct behaviour, wrong
    /// bus footprint; it exists so test fakes get the read semantics for free. The real
    /// [`Gphoto2Cli`] OVERRIDES it with the single-process batch that this issue is about.
    fn get_config_many(&self, keys: &[&str]) -> Result<Vec<String>> {
        keys.iter().map(|k| self.get_config(k)).collect()
    }
    /// `gphoto2 --set-config <key>=<value>`.
    fn set_config(&self, key: &str, value: &str) -> Result<()>;

    /// Reads the best-effort focus distance (`d003`) AND the camera `--summary` in ONE gphoto2
    /// invocation (issue 1306): `(d003_block, summary_text)`. Folding `--summary` into the existing
    /// best-effort d003 call keeps the per-read USB-PTP session count at 3 (issue 1229 doctrine) —
    /// no new session. The summary carries the raw current aperture (`F-Number(0x5007) … (400)`)
    /// that libgphoto2 omits from the `f-number` RADIO `Current:` when the lens is open below the
    /// camera's first enumerated stop. The default impl (test fakes) reads d003 alone with an empty
    /// summary; the real [`Gphoto2Cli`] OVERRIDES it with the single `--get-config d003 --summary`
    /// process, split at the first `END` line.
    fn get_focus_and_summary(&self) -> Result<(String, String)> {
        Ok((
            self.get_config(FOCUS_DISTANCE_KEY).unwrap_or_default(),
            String::new(),
        ))
    }
}

/// Splits the combined stdout of `gphoto2 --get-config d003 --summary` into `(d003_block,
/// summary_text)` at the FIRST `END` line: everything up to and including that `END` is the d003
/// config block, the remainder is the `--summary` text. Pure + fail-safe — if there is no `END`
/// line (an unexpected shape), the whole output is treated as the d003 block and the summary is
/// empty (aperture then just falls back to the RADIO `Current:` path). Tier-0 testable (issue 1306).
pub fn split_focus_and_summary(combined: &str) -> (String, String) {
    let mut block = String::new();
    let mut lines = combined.lines();
    for line in lines.by_ref() {
        block.push_str(line);
        block.push('\n');
        if line.trim() == "END" {
            let rest: Vec<&str> = lines.collect();
            return (block, rest.join("\n"));
        }
    }
    (block, String::new())
}

/// Real transport: spawns the `gphoto2` binary.
pub struct Gphoto2Cli {
    pub binary: String,
}

impl Default for Gphoto2Cli {
    fn default() -> Self {
        Gphoto2Cli {
            binary: "gphoto2".to_string(),
        }
    }
}

impl Gphoto2Cli {
    /// Runs ONE gphoto2 command through the single centralised seam (issue 1309): a hard
    /// timeout+kill ([`run_with_timeout`]) plus ONE structured log line per command — at info on
    /// success (kind/param/value/argv/rc/duration_ms), at error on failure (adds the stderr tail +
    /// whether it timed out). `kind` is `detect`/`read`/`set`; `param` names the key(s); `value`
    /// is the write value (set only). Returns the child's stdout bytes on success, an `Err` on a
    /// non-zero exit or a timeout (the caller degrades a read to offline / returns 502 for a write).
    /// Centralising here is why the fork/kill/log lives in ONE place, not 5 copies, and why the
    /// `Gphoto2Runner` trait (and every fake-runner test) is untouched.
    fn run(&self, kind: &str, param: &str, value: Option<&str>, args: &[&str]) -> Result<Vec<u8>> {
        let mut cmd = std::process::Command::new(&self.binary);
        cmd.args(args);
        let argv = std::iter::once(self.binary.as_str())
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        let (out, elapsed, timed_out) =
            run_with_timeout(cmd, GPHOTO2_TIMEOUT).with_context(|| format!("spawn {argv}"))?;
        let duration_ms = elapsed.as_millis() as u64;
        let rc = out.status.code();
        if out.status.success() && !timed_out {
            tracing::info!(
                kind,
                param,
                value,
                argv = %argv,
                rc = ?rc,
                duration_ms,
                "gphoto2 command"
            );
            Ok(out.stdout)
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let tail = gphoto2_stderr_tail(&stderr);
            tracing::error!(
                kind,
                param,
                value,
                argv = %argv,
                rc = ?rc,
                timed_out,
                duration_ms,
                stderr = %tail,
                "gphoto2 command failed"
            );
            if timed_out {
                bail!("gphoto2 command timed out after {duration_ms} ms and was killed: {argv}");
            }
            bail!("gphoto2 command failed ({argv}): {tail}");
        }
    }
}

impl Gphoto2Runner for Gphoto2Cli {
    fn auto_detect(&self) -> Result<String> {
        let out = self.run("detect", "auto-detect", None, &["--auto-detect"])?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn get_config(&self, key: &str) -> Result<String> {
        let out = self.run("read", key, None, &["--get-config", key])?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn get_config_many(&self, keys: &[&str]) -> Result<Vec<String>> {
        // issue 1229: read every key in ONE gphoto2 process = ONE USB open/enumerate/close on the
        // shared xHCI bus (vs one session PER key with the default `get_config` loop), which is the
        // per-read footprint reduction this issue is about. `--get-config` repeats in the same
        // invocation, so gphoto2 opens the camera once and reads all keys in that one PTP session.
        if keys.is_empty() {
            // An argument-less `gphoto2` would print usage + exit non-zero; keep the pub fn honest.
            return Ok(Vec::new());
        }
        let args = build_get_config_many_args(keys);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.run("read", &keys.join(","), None, &arg_refs)?;
        let stdout = String::from_utf8_lossy(&out);
        // Fail-safe: a block/key count mismatch (a key errored mid-batch, a truncated block) must
        // NOT mis-assign a block to the wrong key — it degrades to a failed read (-> offline), the
        // same as a failed single `get_config`.
        split_config_blocks(&stdout, keys.len()).ok_or_else(|| {
            anyhow::anyhow!(
                "gphoto2 multi --get-config returned an unexpected END-block count (expected {})",
                keys.len()
            )
        })
    }

    fn set_config(&self, key: &str, value: &str) -> Result<()> {
        let kv = format!("{key}={value}");
        self.run("set", key, Some(value), &["--set-config", &kv])?;
        Ok(())
    }

    fn get_focus_and_summary(&self) -> Result<(String, String)> {
        // issue 1306: read d003 AND --summary in ONE gphoto2 process = ONE USB open/enumerate/close
        // on the shared xHCI bus, so the per-read session count stays 3 (issue 1229). Split the
        // combined stdout at the first `END` line: d003 config block, then the summary text.
        let out = self.run(
            "read",
            "d003+summary",
            None,
            &["--get-config", FOCUS_DISTANCE_KEY, "--summary"],
        )?;
        let stdout = String::from_utf8_lossy(&out);
        Ok(split_focus_and_summary(&stdout))
    }
}

/// Parses the first camera model out of `gphoto2 --auto-detect` table output. The table is
/// `Model<ws>Port` with a `---` separator line; each data row ends in a `usb:BBB,DDD` port
/// token, so the model is the row with that token stripped. Returns `None` if no camera row
/// is present (camera absent / unplugged). Pure — unit-tested without a camera.
pub fn parse_first_model(output: &str) -> Option<String> {
    for raw in output.lines() {
        let line = raw.trim_end();
        // A data row ends in a port token; the header/separator rows do not.
        if let Some(idx) = line.rfind("usb:") {
            let model = line[..idx].trim();
            if !model.is_empty() {
                return Some(model.to_string());
            }
        }
    }
    None
}
