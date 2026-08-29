#!/usr/bin/env bash
# airuleset:script-ok source-only lib (functions only; sourced into a caller that owns its own
# shell options) -- mirrors the sibling scripts/lib/*.sh convention (bkshading-preflight.sh,
# splitter-health.sh, measurement-eq.sh): deliberately NOT `set -euo pipefail` here, since
# sourcing this file runs it in the CALLER's shell and strict mode here would leak into whichever
# caller sources it.
#
# scripts/lib/bkshading-e2e-pause.sh -- issue 808 (bkshading epic): PAUSE the bkshading-relay on
# the measurement-critical camboxes for the duration of an E2E recording run, and RESTORE it in
# cleanup(). The relay is a fleet-standby service (owner directive: it runs on ALL camboxes so
# any camera can be shaded on demand -- see .claude/rules/bkshading.md), but its gphoto2 USB-PTP
# polling causally degrades measurement quality on the two boxes that matter most to the E2E
# harness: the SOURCE camera (USB-bus contention with the Cam Link 4K capture device -- cam1
# measured 58.6 vs 60.0 fps, stop/start isolation) and cam2/painter (a 3-core box already running
# camera-box RT + the painter, where the extra CPU/jitter load correlates with worse dual-QR
# window quality). Evidence: issue 808 comments 2026-08-29T09:59:31Z / 2026-08-29T15:54:47Z.
#
# The interim mitigation was a MANUAL `systemctl stop bkshading-relay` on both boxes (unit left
# `enabled` -- comes back on reboot). This lib makes that pause DURABLE and harness-enforced: the
# [0/8]-era preflight pauses it (recording the prior active state), and cleanup() restores it --
# but ONLY on a box where the pause step actually found it running beforehand, so a box someone
# deliberately silenced (like the current interim mitigation) is never woken back up by a run.
#
# Split (mirrors bkshading-preflight.sh): pure remote-text builders + a pure parser below (no I/O,
# unit-testable via `run_sourced` in tests/harness_bkshading_e2e_pause_808.rs, or directly via
# `bash -c '. scripts/lib/bkshading-e2e-pause.sh; ...'`), plus TWO thin I/O orchestrators
# (`bkshading_e2e_pause_stop`/`bkshading_e2e_pause_restore`, ssh + the pure functions) at the
# bottom -- the two call sites recording-e2e.sh actually invokes, added as brand-NEW lines after
# the existing `bkshading_preflight_report` call and at the very end of cleanup() (the #675
# additive-only-lines pattern: never edit an existing anchored ssh command string).
#
# Source-only: no top-level statements besides sourcing bkshading-relay-runtime.sh for the ONE
# source-of-truth relay unit name (bkshading_relay_unit_name) -- mirrors bkshading-preflight.sh's
# own top-level sibling-lib source.
_BKSH_PAUSE_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bkshading-relay-runtime.sh
. "$_BKSH_PAUSE_HERE/bkshading-relay-runtime.sh"

# The structured marker line bkshading_e2e_pause_stop_cmds echoes and
# bkshading_e2e_pause_parse_state extracts -- ONE source of truth so the print side and the parse
# side can never drift on the literal prefix.
bkshading_e2e_pause_marker_prefix() { printf '%s\n' BKSHADING_PAUSE_STATE; }

# --- pure remote-text builders --------------------------------------------------------------
# Every remote-side `$`/`$(...)` in each heredoc is backslash-escaped so THIS function's own
# LOCAL evaluation (on dev1, where recording-e2e.sh calls it) never expands them -- only real
# local bash parameters (e.g. $label, the embedded unit name) are meant to be substituted here,
# baked in as literal text. The unescaped result is literal shell text the REMOTE host evaluates
# when it receives the full command string over ssh. Mirrors camera_box_verify_active_cmds's own
# shape exactly (scripts/lib/camera-box-restart-verify.sh).

# bkshading_e2e_pause_stop_cmds LABEL -> REMOTE bash text: probe whether the relay unit is
# currently active, echo a `BKSHADING_PAUSE_STATE:LABEL:0|1` marker line recording that prior
# state, then stop the unit -- tolerant of it not being installed or already stopped (never fails
# the caller, `|| true` throughout). Meant to be embedded via `$(bkshading_e2e_pause_stop_cmds
# "$LABEL")` as the WHOLE remote command string of a brand-new ssh call.
bkshading_e2e_pause_stop_cmds() {
  local label="$1"
  local unit
  unit="$(bkshading_relay_unit_name)"
  cat <<PAUSE
_bksh_was_active=0
systemctl is-active --quiet $unit 2>/dev/null && _bksh_was_active=1 || true
systemctl stop $unit 2>/dev/null || true
echo "$(bkshading_e2e_pause_marker_prefix):$label:\$_bksh_was_active"
PAUSE
}

# bkshading_e2e_pause_restore_cmds WAS_ACTIVE -> REMOTE bash text: restart the relay unit ONLY
# when WAS_ACTIVE is exactly "1" (the pause step found it active beforehand) -- never on a box
# where it was already stopped (e.g. the #808 interim manual mitigation), so cleanup() never
# re-activates a relay the operator deliberately silenced. WAS_ACTIVE is a LOCAL (dev1-side) bash
# parameter baked in as a literal "0"/"1" (or anything else, treated the same as "0") at
# generation time -- this is a pure LOCAL decision, never a remote conditional, so an unset/empty
# value (the pause step never ran, e.g. an early abort before [0/8]) safely takes the do-nothing
# branch with no remote-side comparison at all. Tolerant of the unit not being installed (never
# fails the caller).
bkshading_e2e_pause_restore_cmds() {
  local was_active="${1:-0}"
  local unit
  unit="$(bkshading_relay_unit_name)"
  if [ "$was_active" = "1" ]; then
    printf 'systemctl start %s 2>/dev/null || true\n' "$unit"
  else
    printf '%s\n' 'true  # bkshading-relay was not active before this run -- leave it stopped (issue 808)'
  fi
}

# --- pure parser -----------------------------------------------------------------------------
# bkshading_e2e_pause_parse_state <label> <ssh_stdout> -> "0" or "1": extracts the LAST
# `BKSHADING_PAUSE_STATE:<label>:0|1` marker line bkshading_e2e_pause_stop_cmds prints, out of the
# combined ssh stdout (which may also carry unrelated noise on the same call). FAIL-SAFE default
# "0" (treat as "was not active") on ANYTHING else -- a missing marker, a malformed line, an
# unreachable/timed-out ssh call -- must never be misread as "restore it": that could re-activate
# a relay the operator deliberately silenced (the current #808 interim manual mitigation on cam1
# and cam2). Fixed-string match (grep -F), never a regex, so a label containing no special
# characters (every real label here is a plain camera/box name) can never be misparsed as one.
bkshading_e2e_pause_parse_state() {
  local label="$1" output="${2:-}"
  local prefix line
  prefix="$(bkshading_e2e_pause_marker_prefix)"
  line="$(printf '%s\n' "$output" | grep -F "${prefix}:${label}:" | tail -1)" || true
  case "$line" in
    *:1) printf '%s\n' 1 ;;
    *) printf '%s\n' 0 ;;
  esac
}

# --- thin I/O orchestrators (the TWO call sites recording-e2e.sh actually invokes) -----------
# Deliberately NOT unit tested beyond "the pure functions above are exercised directly" --
# mirrors bkshading_preflight_report's own "the recording-e2e.sh step is a thin caller" note.

# bkshading_e2e_pause_stop <label> <ip> <pw> [timeout_s=8] -> prints "0" or "1" (the box's PRIOR
# active state) on stdout; NEVER fails the caller (always returns 0, even on an unreachable box --
# the fail-safe "0" default flows straight through bkshading_e2e_pause_parse_state).
bkshading_e2e_pause_stop() {
  local label="$1" ip="$2" pw="$3" timeout_s="${4:-8}"
  local out
  out="$(timeout "$timeout_s" sshpass -p "$pw" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$ip" \
    "$(bkshading_e2e_pause_stop_cmds "$label")" 2>/dev/null)" || true
  bkshading_e2e_pause_parse_state "$label" "$out"
  return 0
}

# bkshading_e2e_pause_restore <label> <ip> <pw> <was_active> [timeout_s=8] -> best-effort
# restart; NEVER fails the caller (cleanup()'s trap must always run to completion, #328/#649).
bkshading_e2e_pause_restore() {
  local label="$1" ip="$2" pw="$3" was_active="$4" timeout_s="${5:-8}"
  timeout "$timeout_s" sshpass -p "$pw" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$ip" \
    "$(bkshading_e2e_pause_restore_cmds "$was_active")" >/dev/null 2>&1 || true
  return 0
}
