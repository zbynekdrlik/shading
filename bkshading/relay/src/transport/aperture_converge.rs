//! Aperture write convergence inside a write burst (issue 1452) — the [`CameraSession`] half that
//! executes the pure `bkshading_proto::aperture` decisions through the burst's open
//! `gphoto2 --shell`. A child of `transport` so it reaches the session's private burst state
//! without widening it; split out so `transport.rs` does not grow past its size budget.

use std::time::Duration;

use anyhow::{Context, Result};
use bkshading_proto::aperture::{
    aperture_correction, aperture_moved, aperture_settle, fnumber_label_index,
    fnumber_raw_from_block,
};

use super::{BurstSession, CameraSession, APERTURE_SETTLE_MAX_READS, FNUMBER_RAW_KEY};
use crate::burst::Gphoto2Shell;

impl CameraSession {
    /// Converges an aperture write onto the requested listed step (issue 1452): reads 0x5007 back
    /// through the burst's open shell until the iris settles, writes ONE correcting listed step
    /// when the camera overshot ([`aperture_correction`]), and records where the iris settled
    /// (`aperture_raw`, the next write's "before") and whether it moved (`aperture_moved`, the
    /// idle-close report). Best-effort: it never fails the SET (the write itself already
    /// succeeded). A write that went over the CLI has no shell to read through, so its landing stays
    /// unknown (no new gphoto2 process per read, the issue-1229 bus doctrine); a shell error closes
    /// the shell for the rest of the burst, exactly like a failed shell write.
    pub(super) fn converge_aperture(
        &self,
        burst: &mut BurstSession,
        requested: usize,
        labels: &[String],
    ) {
        let before = burst.aperture_raw;
        let outcome = match burst.shell.as_mut() {
            Some(sh) => self.measure_and_correct_aperture(sh, before, requested, labels),
            None => Ok(None),
        };
        let landed = match outcome {
            Ok(landed) => landed,
            Err(e) => {
                tracing::warn!(error = %e, "aperture read-back failed; shell closed, landing unknown");
                if let Some(sh) = burst.shell.take() {
                    sh.close();
                }
                burst.shell_disabled = true;
                None
            }
        };
        burst.aperture_raw = landed;
        burst.aperture_moved = aperture_moved(before, landed.as_slice());
    }

    /// The shell half of [`converge_aperture`](Self::converge_aperture): the settled read-back,
    /// the at-most-one correcting write, and the settled read-back after it. `Ok(Some(raw))` = where
    /// the iris settled (x100), `Ok(None)` = 0x5007 not readable, `Err` = the shell failed.
    fn measure_and_correct_aperture(
        &self,
        sh: &mut Gphoto2Shell,
        before: Option<i64>,
        requested: usize,
        labels: &[String],
    ) -> Result<Option<i64>> {
        let requested_label = labels.get(requested).map(String::as_str).unwrap_or("?");
        let Some(landed) = self.settle_aperture(sh, before)? else {
            tracing::info!(
                requested = requested_label,
                "aperture write: 0x5007 not readable, landing unknown"
            );
            return Ok(None);
        };
        let correction = aperture_correction(
            requested,
            before.and_then(|raw| fnumber_label_index(labels, raw)),
            fnumber_label_index(labels, landed),
            labels.len(),
        );
        let Some(fix) = correction else {
            tracing::info!(
                requested = requested_label,
                before_x100 = ?before,
                landed_x100 = landed,
                "aperture write settled"
            );
            return Ok(Some(landed));
        };
        let fix_label = labels
            .get(fix)
            .context("aperture correction index outside the f-number list")?;
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
        Ok(corrected)
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
