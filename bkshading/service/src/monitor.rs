//! FPS-sync telemetry (issue 809, remainder item 1).
//!
//! The aggregator computes each camera's [`FpsSync`] verdict and a config-vs-capture
//! `grab_fps_desync` flag, but on its own it only SETS those fields — nothing surfaces a
//! mismatch to the operator's log. This module turns those verdicts into log lines, gated on a
//! STATE TRANSITION so a chronic mismatch is logged ONCE (when it starts), not on every ~2 s
//! pump cycle — a per-cycle WARN would be exactly the repeated-chronic-state noise the fleet
//! notification discipline forbids.
//!
//! The transition decision is pure (this function), so it is unit-testable without the pump or a
//! running service; the pump task owns the `prev` map and does the actual `tracing::warn!`.
//! The mismatch note itself cross-references the appliance's grabber-side capture-rate health
//! (`capture_rate_health`, issues 656/685) + duplicate-frame analysis (issue 674) — the SAME
//! source beat, seen downstream on `/dev/videoN` — see [`bkshading_proto::wire::fps_mismatch_note`].

use std::collections::HashMap;

use bkshading_proto::wire::{fps_mismatch_note, grab_desync_note, CameraView, FpsSync};

/// Per-camera alert state tracked across pump cycles: `(last fps-sync verdict, last desync flag)`.
type AlertState = (FpsSync, bool);

/// Given the previous per-camera alert state and the current views, returns the log lines to
/// emit THIS cycle — one for a camera that just ENTERED [`FpsSync::Mismatch`], and one for a
/// camera whose config just went out of sync with the box's live capture rate. Updates `prev`
/// in place so a state that persists is not re-logged. Cameras that dropped out of the config
/// are pruned from `prev` (so re-appearing later logs afresh).
pub fn fps_alert_transitions(
    prev: &mut HashMap<String, AlertState>,
    views: &[CameraView],
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for v in views {
        seen.insert(v.id.clone());
        let (was_sync, was_desync) = prev
            .get(&v.id)
            .copied()
            .unwrap_or((FpsSync::Unknown, false));
        // Log the mismatch note only on the transition INTO Mismatch (chronic = logged once).
        if v.fps_sync == FpsSync::Mismatch && was_sync != FpsSync::Mismatch {
            let cam_fps100 = v.state.as_ref().and_then(|s| s.params.fps100);
            lines.push(fps_mismatch_note(&v.id, cam_fps100, v.grab_fps));
        }
        // Log the config-vs-capture desync only on the transition INTO desync.
        if v.grab_fps_desync && !was_desync {
            lines.push(grab_desync_note(&v.id, v.grab_fps));
        }
        prev.insert(v.id.clone(), (v.fps_sync, v.grab_fps_desync));
    }
    // Prune cameras no longer present so a later reappearance is treated as a fresh transition.
    prev.retain(|id, _| seen.contains(id));
    lines
}

/// Per-camera reachability, tracked across pump cycles (issue 1309): `true` == the last snapshot
/// reached this camera's relay.
type ReachState = std::collections::HashMap<String, bool>;

/// Reachability TRANSITION tracker (issue 1309) — the reachability sibling of
/// [`fps_alert_transitions`], and the reason `poll_state`'s per-poll `relay unreachable` WARN was
/// downgraded to `debug`. Given the previous per-camera reachable flag and the current views,
/// returns ONE log line per camera whose reachability just FLIPPED (reachable -> unreachable and
/// back), plus a one-time line for a camera first seen already down — so the strih log records a
/// relay going away / coming back ONCE per transition instead of every ~2 s (the 365 KB/run spam).
/// Updates `prev` in place and prunes cameras that dropped out of the config.
pub fn reach_transitions(prev: &mut ReachState, views: &[CameraView]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for v in views {
        seen.insert(v.id.clone());
        match prev.get(&v.id).copied() {
            Some(was) if was != v.reachable => {
                lines.push(if v.reachable {
                    format!("relay {} reachable again", v.id)
                } else {
                    format!("relay {} unreachable", v.id)
                });
            }
            // First sighting already down: record it once (a relay that starts the service down).
            None if !v.reachable => lines.push(format!("relay {} unreachable", v.id)),
            _ => {}
        }
        prev.insert(v.id.clone(), v.reachable);
    }
    prev.retain(|id, _| seen.contains(id));
    lines
}

/// A periodic reachability HEARTBEAT summary (issue 1309): `N/M relays reachable` (plus the
/// down ids), emitted every ~5 min by the pump so a chronically-down set stays visible without the
/// per-poll spam the transition tracker replaced.
pub fn reach_heartbeat_line(views: &[CameraView]) -> String {
    let total = views.len();
    let up = views.iter().filter(|v| v.reachable).count();
    let down: Vec<&str> = views
        .iter()
        .filter(|v| !v.reachable)
        .map(|v| v.id.as_str())
        .collect();
    if down.is_empty() {
        format!("relay reachability heartbeat: {up}/{total} reachable")
    } else {
        format!(
            "relay reachability heartbeat: {up}/{total} reachable (down: {})",
            down.join(", ")
        )
    }
}
