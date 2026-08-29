#!/usr/bin/env bash
# airuleset:script-ok source-only lib (functions only; sourced into a caller that owns its own
# shell options) -- mirrors the sibling scripts/lib/*.sh convention (splitter-health.sh,
# measurement-eq.sh, audio-presence-preflight.sh): deliberately NOT `set -euo pipefail` here, since
# sourcing this file runs it in the CALLER's shell and strict mode here would leak into whichever
# caller sources it.
#
# scripts/lib/bkshading-preflight.sh -- issue 808 (bkshading epic): automates the #220 CAMERA
# PRE-RUN shutter checklist recording-e2e.sh has printed as a MANUAL human step since #220 landed
# (the harness reads only /dev/video0 -- the ShadowCast HDMI capture of the BMPCC's monitor output
# -- and cannot itself read or set the camera BODY's shutter/focus/exposure). Now that the
# bkshading-relay (issue 808 M1, .claude/rules/bkshading.md) runs on the cambox and talks to the
# camera body over USB-PTP/gphoto2, the harness CAN read the shutter back and turn HALF of that
# manual checklist into an automated REPORT-ONLY preflight check -- the M3 line item the design
# comment on issue 808 recorded.
#
# REPORT-ONLY BY DESIGN (owner intent recorded on issue 808, M3 discussion): a WARN, never a hard
# gate, and ABSENCE of the relay/camera is NOT an error -- the physical shading camera is ONE
# portable BMPCC that today is cabled to exactly one cambox; every OTHER active cambox's relay
# answers `online:false, camera:null`, which must be a quiet skip, never an abort. A future ticket
# can flip this to a hard gate once the fleet-wide cabling story is settled; this milestone only
# adds the automated READ + WARN.
#
# Split (mirrors splitter-health.sh / measurement-eq.sh): pure parse/classify/message functions
# below (no I/O, unit-testable via `run_sourced` in tests/harness_bkshading_preflight_808.rs, or
# directly via `bash -c '. scripts/lib/bkshading-preflight.sh; ...'`), plus ONE I/O orchestrator
# (`bkshading_preflight_report`, curl + the pure functions) at the bottom -- the single call site
# recording-e2e.sh actually invokes (the #675 sourced-helper anchor-safety pattern: appended as a
# brand-new line after the #220 checklist block, never editing its existing anchored text).
#
# Source-only: no top-level statements besides sourcing bkshading-relay-runtime.sh for the ONE
# source-of-truth relay port constant (bkshading_relay_port) -- mirrors camera-box-parity-align.sh's
# own top-level sibling-lib source.
_BKSH_PREFLIGHT_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bkshading-relay-runtime.sh
. "$_BKSH_PREFLIGHT_HERE/bkshading-relay-runtime.sh"

# --- pure JSON scalar extractors (arg1 = the relay's GET /api/state JSON string) ---------------
# python-only (no jq dependency), mirrors scripts/lib/measurement-eq.sh's "Scalar extractors"
# convention. A malformed/empty JSON string, or a missing/null field, prints EMPTY ("0" for the
# online flag) -- never a fabricated value ("unreadable is never a silent pass",
# audio-presence-preflight.sh's own rule).

bkshading_preflight_state_online() {
  python3 -c 'import json,sys
try:
    d = json.loads(sys.argv[1])
except Exception:
    print("0"); sys.exit(0)
print("1" if d.get("online") is True else "0")' "${1:-}"
}

bkshading_preflight_state_camera() {
  python3 -c 'import json,sys
try:
    d = json.loads(sys.argv[1])
except Exception:
    print(""); sys.exit(0)
c = d.get("camera")
print(c if isinstance(c, str) and c else "")' "${1:-}"
}

bkshading_preflight_state_shutter() {
  python3 -c 'import json,sys
try:
    d = json.loads(sys.argv[1])
except Exception:
    print(""); sys.exit(0)
s = (d.get("params") or {}).get("shutter")
print(s if isinstance(s, int) else "")' "${1:-}"
}

# --- pure classifier -----------------------------------------------------------------------------
# bkshading_preflight_classify <online 0|1> <camera> <shutter> [min_denom=500] -> one of:
#   ok | warn-slow | warn-unknown | skip-offline
# skip-offline: relay answered but no camera is attached (or the relay reports offline) -- the
#   EXPECTED common case (a portable camera cabled to only one box at a time); never a warning.
# warn-unknown: a camera IS attached/online but the shutter field is absent/unreadable -- cannot
#   auto-confirm the checklist, so warn loudly rather than silently pass.
# warn-slow: shutter denominator below the minimum (a SLOWER shutter, e.g. 1/60 < 1/500) -- the
#   exact #216 failure mode.
# ok: shutter denominator at/above the minimum (exactly-at-minimum counts as ok, mirrors
#   audio_preflight_is_silent's own "exactly at the boundary counts as the healthier side").
bkshading_preflight_classify() {
  local online="${1:-0}" camera="${2:-}" shutter="${3:-}" min="${4:-500}"
  if [ "$online" != "1" ] || [ -z "$camera" ]; then
    printf 'skip-offline\n'
    return 0
  fi
  case "$shutter" in
    ''|*[!0-9]*)
      printf 'warn-unknown\n'
      return 0
      ;;
  esac
  if [ "$shutter" -lt "$min" ]; then
    printf 'warn-slow\n'
  else
    printf 'ok\n'
  fi
}

# --- pure message formatters ----------------------------------------------------------------------
# All take the box label + ip first; the rest match what each status needs to report.

bkshading_preflight_ok_message() {
  local label="$1" ip="$2" camera="$3" shutter="$4" min="$5"
  printf "    bkshading relay check (%s, %s): camera '%s' shutter 1/%s -- OK (>= 1/%s, #220 checklist satisfied automatically)\n" \
    "$label" "$ip" "$camera" "$shutter" "$min"
}

bkshading_preflight_warn_slow_message() {
  local label="$1" ip="$2" camera="$3" shutter="$4" min="$5"
  printf "WARNING #808: bkshading relay on %s (%s) reports camera '%s' shutter 1/%s -- SLOWER than the required >= 1/%s (ideally 1/1000). A slow shutter integrates a full 60Hz monitor refresh and smears the dual-QR Vernier (the #216 ~175s optical-read gap). Fix the camera's shutter, THEN run.\n" \
    "$label" "$ip" "$camera" "$shutter" "$min"
}

bkshading_preflight_warn_unknown_message() {
  local label="$1" ip="$2" camera="$3" min="$4"
  printf "WARNING #808: bkshading relay on %s (%s) reports camera '%s' online but no shutter value -- cannot automatically confirm the #220 pre-run checklist. Verify the shutter manually (>= 1/%s, ideally 1/1000).\n" \
    "$label" "$ip" "$camera" "$min"
}

bkshading_preflight_skip_offline_message() {
  local label="$1" ip="$2" port="$3"
  printf "    NOTE: bkshading relay reachable at %s:%s but reports no camera attached/offline -- skipping the automated #220 shutter check for %s; the manual checklist above still applies.\n" \
    "$ip" "$port" "$label"
}

bkshading_preflight_skip_unreachable_message() {
  local label="$1" ip="$2" port="$3"
  printf "    NOTE: bkshading relay unreachable at %s:%s (not provisioned or down) -- skipping the automated #220 shutter check for %s; the manual checklist above still applies.\n" \
    "$ip" "$port" "$label"
}

# --- I/O orchestrator (the ONE call site recording-e2e.sh invokes) -------------------------------
# bkshading_preflight_report <label> <ip> [port] [max_time_s=5] [min_denom=500]
# Never fails the run (always returns 0): an unreachable relay or an absent camera are the
# EXPECTED common case (see skip-offline above), and even a genuinely slow shutter is a WARN, not
# a gate, per the owner's report-only M3 decision recorded on issue 808. Deliberately not unit
# tested beyond "never fails the caller" (tests/harness_bkshading_preflight_808.rs) -- it is a thin
# curl-then-dispatch caller over the pure functions above, mirroring
# audio-presence-preflight.sh's own "the recording-e2e.sh step is a thin caller" convention.
bkshading_preflight_report() {
  local label="$1" ip="$2" port="${3:-$(bkshading_relay_port)}" max_time="${4:-5}" min="${5:-500}"
  local raw status camera shutter online
  if raw="$(curl -fsS --max-time "$max_time" "http://${ip}:${port}/api/state" 2>/dev/null)" && [ -n "$raw" ]; then
    online="$(bkshading_preflight_state_online "$raw")"
    camera="$(bkshading_preflight_state_camera "$raw")"
    shutter="$(bkshading_preflight_state_shutter "$raw")"
    status="$(bkshading_preflight_classify "$online" "$camera" "$shutter" "$min")"
    case "$status" in
      ok)           bkshading_preflight_ok_message "$label" "$ip" "$camera" "$shutter" "$min" ;;
      warn-slow)    bkshading_preflight_warn_slow_message "$label" "$ip" "$camera" "$shutter" "$min" >&2 ;;
      warn-unknown) bkshading_preflight_warn_unknown_message "$label" "$ip" "$camera" "$min" >&2 ;;
      *)            bkshading_preflight_skip_offline_message "$label" "$ip" "$port" ;;
    esac
  else
    bkshading_preflight_skip_unreachable_message "$label" "$ip" "$port"
  fi
  return 0
}
