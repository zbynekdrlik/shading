//! One shading write inside a write burst (issue 1337) — [`CameraSession::burst_apply`], moved out
//! of `transport.rs` (issue 1452) because that file is over its size budget, together with the
//! burst's published state (`publish_projection` / `busy_burst_state`, issue 1452). A child of
//! `transport`, so it reaches the session's private burst state without widening it.

use anyhow::{Context, Result};
use bkshading_proto::aperture::landed_aperture;
use bkshading_proto::mapping::{norm_to_choice_index, parse_fnumber_labels, DEFAULT_FPS100};
use bkshading_proto::read::{current_fnumber_x100, fps_supported, params_and_caps, plan_writes};
use bkshading_proto::wire::{summarize_set_request, RelayState, SetRequest};

use super::{merge_written, project_shading, BurstSession, CameraSession, PlanBasis};
use crate::burst::{
    burst_step, BurstAction, BurstEvent, BurstState, Gphoto2Shell, WRITE_SESSION_IDLE_MS,
};

impl CameraSession {
    /// Applies ONE shading write inside an open (or opening) burst (issue 1337): plan the writes
    /// from the choices read ONCE at burst open (NO pre-write read), then run them through the
    /// persistent `gphoto2 --shell` — falling any shell error back to a per-invocation CLI write for
    /// the rest of the burst so a broken/slow shell never loses a write and never wedges the relay.
    /// Returns the number of gphoto2 writes performed. Called only from
    /// [`submit`](Self::submit) under the burst lock. A CLI-path write failure is a real camera
    /// error and propagates (→ 502).
    pub(super) fn burst_apply(&self, burst: &mut BurstSession, req: &SetRequest) -> Result<usize> {
        // 1. Plan basis: read the camera ONCE at burst open (detect + core batch + focus/summary =
        //    the normal 3-session read, NOT one per write), caching the PlanBasis (f-number labels,
        //    fps100, the d006 fps choices) AND the full state to project the response onto.
        //    Subsequent writes in the burst pay NO read. A read failure (absent/busy camera)
        //    propagates -> 502, exactly like `apply`.
        if burst.plan.is_none() {
            let camera = self.camera_for_read();
            let raw = self
                .read_raw(&mut burst.shell)
                .inspect_err(|_| self.forget_detected())?;
            let (params, caps) = params_and_caps(&raw);
            let basis = PlanBasis {
                fnumber_labels: parse_fnumber_labels(&raw.fnumber),
                fps100: params.fps100.unwrap_or(DEFAULT_FPS100),
                // issue 1402: the d006 project-fps choices a `fps` write must hit exactly.
                fps_choices: caps.fps_choices.clone(),
            };
            let open_state = RelayState {
                online: camera.is_some(),
                camera,
                params,
                caps: Some(caps),
                fps_supported: fps_supported(&raw),
                capture_fps: self.capture_fps,
                version: self.version.clone(),
                not_applied: Vec::new(),
            };
            burst.plan = Some(basis);
            burst.open_state = Some(open_state);
            // issue 1452: where the iris is before this burst's first aperture write.
            burst.aperture_raw = current_fnumber_x100(&raw);
            burst.aperture_run_start = None;
            burst.aperture_moved = None;
            // issue 1343: a fresh burst starts a fresh write accumulation (the previous burst's
            // writes were consumed at its idle-close). Reset here on the FIRST set of a burst.
            burst.written = SetRequest::default();
            burst.projected = None; // item 0: no projection until the first write of this burst
            self.publish_projection(burst); // issue 1452: a busy-burst status read sees the open state
        }
        let basis = burst
            .plan
            .clone()
            .expect("burst.plan set immediately above");
        let writes = match plan_writes(req, &basis.fnumber_labels, basis.fps100, &basis.fps_choices)
        {
            Ok(writes) => writes,
            Err(refused) => {
                // issue 1402: a `fps` the camera's d006 does not list refuses the whole SET before
                // ANY write or burst-state change — never rounded to a neighbour, never sent to
                // the off-speed d007. Inside an open burst the plan stays (the camera is
                // untouched). If this SET would have OPENED the burst, drop the basis read for it,
                // so no plan outlives a burst that never opened (the next burst re-reads).
                tracing::warn!(
                    error = %refused,
                    set = %summarize_set_request(req),
                    "shading SET refused before any write (issue 1402)"
                );
                if !matches!(burst.state, BurstState::Open { .. }) {
                    burst.plan = None;
                    burst.open_state = None;
                    self.publish_projection(burst);
                }
                return Err(anyhow::Error::new(refused));
            }
        };
        let n = writes.len();
        // 2. Burst state machine: this SET arrived (opens the burst on the first set).
        let now = self.clock.now_ms();
        let (state_after_set, action) = burst_step(
            burst.state,
            BurstEvent::Set { now_ms: now },
            WRITE_SESSION_IDLE_MS,
        );
        burst.state = state_after_set;
        // 3. Open the persistent shell on the first set of a burst (unless disabled this burst, or
        //    no gphoto2 binary was configured — then the CLI write path is used, still pre-read-free).
        if matches!(action, BurstAction::OpenShellThenWrite)
            && burst.shell.is_none()
            && !burst.shell_disabled
            && !self.gphoto2_binary.is_empty()
        {
            match Gphoto2Shell::open(&self.gphoto2_binary) {
                Ok(sh) => {
                    burst.shell = Some(sh);
                    tracing::info!("shading write-burst shell opened");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "shading write-burst shell open failed; CLI for this burst");
                    burst.shell_disabled = true;
                }
            }
        }
        // 4. Run the writes: shell fast-path, CLI fallback on any shell error. `as_mut().map(...)`
        //    runs the shell write (if a shell is open) and RELEASES the borrow before the match, so
        //    the error arm can `take()` the shell to kill it.
        for (key, value) in &writes {
            match burst.shell.as_mut().map(|sh| sh.set_config(key, value)) {
                Some(Ok(())) => {} // shell write applied
                Some(Err(e)) => {
                    // Shell wedged/errored: kill it, disable the shell for the rest of this burst,
                    // and RETRY this write on the CLI (never lose the write).
                    tracing::warn!(error = %e, key = %key, "write-burst shell error; kill shell + CLI fallback");
                    if let Some(sh) = burst.shell.take() {
                        sh.close();
                    }
                    burst.shell_disabled = true;
                    if let Err(e2) = self.runner.set_config(key, value) {
                        // The CLI FALLBACK also failed -> a real camera error; invalidate the plan
                        // basis (same as the no-shell CLI path below) so the next burst re-reads the
                        // now-uncertain camera, then propagate (-> 502).
                        burst.forget_camera_state();
                        self.publish_projection(burst);
                        return Err(e2)
                            .with_context(|| format!("CLI fallback set-config {key}={value}"));
                    }
                }
                None => {
                    // No shell (disabled / not configured): CLI. A failure is a real camera error —
                    // invalidate the plan so the next burst re-reads the now-uncertain camera.
                    if let Err(e) = self.runner.set_config(key, value) {
                        burst.forget_camera_state();
                        self.publish_projection(burst);
                        return Err(e);
                    }
                }
            }
        }
        // 4b. issue 1452: the requested aperture listed index, when this SET wrote the f-number. The
        //     iris position before it leaves `aperture_raw` until the read-back below re-measures
        //     it, so the projection published meanwhile shows the request, not the old position.
        //     When another aperture click is already queued behind this one, the read-back is
        //     deferred to the run's LAST click (which converges from where the run started), so a
        //     quick run pays one settle and the first click's PUT never waits for N of them.
        let aperture_write = match req.aperture_norm {
            Some(norm) if writes.iter().any(|(key, _)| key == "f-number") => {
                let choice_count = basis.fnumber_labels.len() as i64;
                let before = burst
                    .aperture_raw
                    .take()
                    .or_else(|| burst.aperture_run_start.take());
                if self.aperture_click_queued() {
                    burst.aperture_run_start = before;
                    burst.aperture_moved = None;
                    None
                } else {
                    Some((norm_to_choice_index(norm, choice_count) as usize, before))
                }
            }
            // An aperture SET that planned no write (no f-number list): nothing moved or measured.
            Some(_) => {
                burst.aperture_moved = None;
                None
            }
            None => None,
        };
        // 5. Writes applied — record what this SET wrote (issue 1343: the merged `written` is
        //    compared against the authoritative readback at the burst idle-close), refresh the
        //    burst's PROJECTED state (item 0: open-read + every merged write projected onto it, so a
        //    pump read while the burst is open confirms the clicks instead of reverting them), and
        //    refresh the burst idle clock.
        merge_written(&mut burst.written, req);
        // issue 1402: an accepted fps write moved the camera's project rate. The rest of this burst
        // converts a shutter to the d002 ANGLE at the NEW rate the camera now runs, never at the
        // burst-open rate. (plan_writes accepted `fps`, so `fps * 100` is a listed choice.)
        if let (Some(fps), Some(plan)) = (req.fps, burst.plan.as_mut()) {
            if let Some(fps100) = fps.checked_mul(100) {
                plan.fps100 = fps100;
            }
        }
        refresh_projection(burst);
        self.publish_projection(burst);
        // 5b. issue 1452: read the aperture back through the shell until the iris settles, correct
        //     an overshoot by ONE listed step, then project + publish where the iris really is.
        //     A status read meanwhile is answered from the projection just published.
        if let Some((requested, before)) = aperture_write {
            self.converge_aperture(burst, before, requested, &basis.fnumber_labels);
            refresh_projection(burst);
            self.publish_projection(burst);
        }
        let now2 = self.clock.now_ms();
        let (state_after_ok, _) = burst_step(
            burst.state,
            BurstEvent::WriteOk { now_ms: now2 },
            WRITE_SESSION_IDLE_MS,
        );
        burst.state = state_after_ok;
        Ok(n)
    }

    /// Whether another aperture click is queued behind the running SET (issue 1452). Lock order
    /// burst -> set_queue, the same order `submit` already uses.
    fn aperture_click_queued(&self) -> bool {
        self.set_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .has_pending_aperture()
    }

    /// Publishes the burst's current representation (issue 1452): the projected state, else the
    /// burst-open state, else nothing. Called by the burst drain whenever either changes, and at
    /// the idle-close. `live_projection` is a leaf lock.
    pub(super) fn publish_projection(&self, burst: &BurstSession) {
        *self
            .live_projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            burst.projected.clone().or_else(|| burst.open_state.clone());
    }

    /// What `read_state` answers while the burst drain holds the burst lock (issue 1452): the
    /// published burst state, else the last cached read, else a benign offline snapshot. Touches
    /// no camera — the shell owns it.
    pub(super) fn busy_burst_state(&self, cached: Option<&RelayState>) -> RelayState {
        self.live_projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .or_else(|| cached.cloned())
            .unwrap_or_else(|| RelayState {
                capture_fps: self.capture_fps,
                ..RelayState::offline(self.version.clone())
            })
    }
}

impl BurstSession {
    /// Drops everything the burst knew about the camera after a failed write (issue 1343 / 1452):
    /// the plan basis and open state (the next burst re-reads the now-uncertain camera), the
    /// merged writes (no stale not-applied), the projection, and the aperture tracking.
    fn forget_camera_state(&mut self) {
        self.plan = None;
        self.open_state = None;
        self.written = SetRequest::default();
        self.projected = None;
        self.aperture_raw = None;
        self.aperture_run_start = None;
        self.aperture_moved = None;
    }
}

/// Recomputes the burst's PROJECTED state (issue 1343 item 0): the burst-open read with every
/// merged write projected onto it. Issue 1452: once an aperture write's landing was read back
/// (`aperture_raw` holds it), the aperture shows where the iris really is, not where it was asked
/// to go. Nothing to project before the burst-open read.
fn refresh_projection(burst: &mut BurstSession) {
    if let (Some(basis), Some(open)) = (burst.plan.as_ref(), burst.open_state.as_ref()) {
        let mut params = project_shading(&open.params, &burst.written, &basis.fnumber_labels);
        if let (Some(_), Some(raw)) = (burst.written.aperture_norm, burst.aperture_raw) {
            (params.aperture_av, params.aperture_norm) =
                landed_aperture(raw, &basis.fnumber_labels);
        }
        burst.projected = Some(RelayState {
            params,
            ..open.clone()
        });
    }
}
