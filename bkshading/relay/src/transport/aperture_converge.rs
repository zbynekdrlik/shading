//! Aperture write convergence inside a write burst (issue 1452) — the [`CameraSession`] half that
//! executes the pure `bkshading_proto::aperture` decisions through the burst's open
//! `gphoto2 --shell`. A child of `transport` so it reaches the session's private burst state
//! without widening it; split out because `transport.rs` is over its size budget.

use std::time::Duration;

use anyhow::Result;
use bkshading_proto::aperture::{
    aperture_correction, aperture_moved, aperture_settle, fnumber_label_index,
    fnumber_raw_from_block,
};

use super::{BurstSession, CameraSession, APERTURE_SETTLE_MAX_READS, FNUMBER_RAW_KEY};
use crate::burst::Gphoto2Shell;

/// What one aperture write's read-back saw (issue 1452).
struct Landing {
    /// Every settled value (x100), oldest first: the write's landing, then its correction's.
    seen: Vec<i64>,
    /// Where the iris is now; `None` when the last read-back answered no readable value.
    now: Option<i64>,
}

impl Landing {
    /// Nothing was read back (no shell, an unreadable value, a failed shell).
    fn unknown() -> Self {
        Landing {
            seen: Vec::new(),
            now: None,
        }
    }
}

/// Where an aperture write starts from (issue 1452).
pub(super) struct ApertureStart {
    /// The iris position (x100) before the write — for the last click of a deferred run, before
    /// the run. The landings are compared with it (`aperture_moved`).
    pub(super) raw: Option<i64>,
    /// `Some(index)` when this click ends a deferred run of aperture clicks: the listed index the
    /// run's previous click asked for, where the iris was heading — the overshoot direction's
    /// reference. `None` for a single click (the direction comes from `raw`).
    pub(super) run_last_request: Option<usize>,
}

impl CameraSession {
    /// Converges an aperture write onto the requested listed step (issue 1452): reads 0x5007 back
    /// through the burst's open shell until the iris settles, writes ONE correcting listed step
    /// when the camera overshot ([`aperture_correction`]), and records where the iris is now
    /// (`aperture_raw`, the next write's "before") and whether it moved (`aperture_moved`, the
    /// idle-close report: ANY landing away from `start.raw` is a move).
    ///
    /// `requested` is the listed index the write asked for. A single click to the position the
    /// iris already holds asks for no move and is not read back; the last click of a run always is
    /// (its deferred clicks already moved the iris, wherever its request points). Best-effort: it
    /// never fails the SET (the write itself already succeeded). A write that went over the CLI
    /// has no shell to read through, so its landing stays unknown (no new gphoto2 process per
    /// read, the issue-1229 bus doctrine); a shell error closes the shell for the rest of the
    /// burst, exactly like a failed shell write.
    pub(super) fn converge_aperture(
        &self,
        burst: &mut BurstSession,
        start: ApertureStart,
        requested: usize,
        labels: &[String],
    ) {
        let ends_run = start.run_last_request.is_some();
        let direction_from = start
            .run_last_request
            .or_else(|| start.raw.and_then(|raw| fnumber_label_index(labels, raw)));
        if !ends_run && direction_from == Some(requested) {
            burst.aperture_raw = start.raw;
            burst.aperture_moved = None;
            return;
        }
        // The settle waits for a reading away from the measured pre-write position; a run's last
        // click has none (the iris is somewhere along the run), so two equal reads settle it.
        let settle_from = if ends_run { None } else { start.raw };
        let outcome = match burst.shell.as_mut() {
            Some(sh) => self.measure_and_correct_aperture(
                sh,
                settle_from,
                direction_from,
                requested,
                labels,
            ),
            None => Ok(Landing::unknown()),
        };
        let landing = outcome.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "aperture read-back failed; shell closed, landing unknown");
            if let Some(sh) = burst.shell.take() {
                sh.close();
            }
            burst.shell_disabled = true;
            Landing::unknown()
        });
        burst.aperture_raw = landing.now;
        burst.aperture_moved = aperture_moved(start.raw, &landing.seen);
    }

    /// The shell half of [`converge_aperture`](Self::converge_aperture): the settled read-back
    /// (waiting for a reading away from `before`, when known), the at-most-one correcting write
    /// (direction judged from the listed index `direction_from`), and the settled read-back after
    /// it. `Err` = the shell failed (the caller closes it).
    fn measure_and_correct_aperture(
        &self,
        sh: &mut Gphoto2Shell,
        before: Option<i64>,
        direction_from: Option<usize>,
        requested: usize,
        labels: &[String],
    ) -> Result<Landing> {
        let requested_label = labels.get(requested).map(String::as_str).unwrap_or("?");
        let Some(landed) = self.settle_aperture(sh, before)? else {
            tracing::info!(
                requested = requested_label,
                "aperture write: 0x5007 not readable, landing unknown"
            );
            return Ok(Landing::unknown());
        };
        let settled = Landing {
            seen: vec![landed],
            now: Some(landed),
        };
        let correction = aperture_correction(
            requested,
            direction_from,
            fnumber_label_index(labels, landed),
            labels.len(),
        )
        .and_then(|fix| labels.get(fix));
        let Some(fix_label) = correction else {
            tracing::info!(
                requested = requested_label,
                before_x100 = ?before,
                direction_from = ?direction_from,
                landed_x100 = landed,
                "aperture write settled"
            );
            return Ok(settled);
        };
        tracing::info!(
            requested = requested_label,
            before_x100 = ?before,
            landed_x100 = landed,
            correction = %fix_label,
            "aperture write overshot; one correcting step back toward the request"
        );
        sh.set_config("f-number", fix_label)?;
        let corrected = self.settle_aperture(sh, Some(landed))?;
        tracing::info!(
            requested = requested_label,
            corrected_x100 = ?corrected,
            "aperture correction settled"
        );
        Ok(Landing {
            seen: settled.seen.into_iter().chain(corrected).collect(),
            now: corrected,
        })
    }

    /// Reads 0x5007 through the shell every `aperture_settle_poll_ms` until [`aperture_settle`]
    /// says the iris settled — at most [`APERTURE_SETTLE_MAX_READS`] reads (issue 1452).
    /// `Ok(None)` = the camera answered no readable value.
    fn settle_aperture(&self, sh: &mut Gphoto2Shell, before: Option<i64>) -> Result<Option<i64>> {
        let mut reads: Vec<i64> = Vec::with_capacity(APERTURE_SETTLE_MAX_READS);
        loop {
            std::thread::sleep(Duration::from_millis(self.aperture_settle_poll_ms));
            let Some(raw) = sh
                .get_config(FNUMBER_RAW_KEY)?
                .as_deref()
                .and_then(fnumber_raw_from_block)
            else {
                return Ok(None);
            };
            reads.push(raw);
            if let Some(settled) = aperture_settle(before, &reads, APERTURE_SETTLE_MAX_READS) {
                return Ok(Some(settled));
            }
        }
    }
}
