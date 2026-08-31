#!/usr/bin/env bash
# airuleset:script-ok source-only lib (functions only; sourced into a caller that owns its own
# shell options) -- mirrors the sibling scripts/lib/*.sh convention (splitter-health.sh,
# measurement-eq.sh, audio-presence-preflight.sh): deliberately NOT `set -euo pipefail` here, since
# sourcing this file runs it in the CALLER's shell and strict mode here would leak into whichever
# caller sources it.
#
# scripts/lib/bkshading-preflight.sh -- issue 808 (bkshading epic) + issue 1237: automates the #220
# CAMERA PRE-RUN checklist recording-e2e.sh has printed as a MANUAL human step since #220 landed
# (the harness reads only /dev/video0 -- the ShadowCast HDMI capture of the BMPCC's monitor output
# -- and cannot itself read or set the camera BODY's shutter/focus/exposure). Now that the
# bkshading-relay (issue 808 M1, .claude/rules/bkshading.md) runs on the cambox and talks to the
# camera body over USB-PTP/gphoto2, the harness CAN read those settings back and turn the checklist
# into an automated REPORT-ONLY preflight check:
#   - SHUTTER >= 1/500 (issue 808): the #216 slow-shutter smear guard.
#   - EXPOSURE/gain (issue 1237): iso (gain) + aperture (apertureAv) reported as concrete FIXED
#     values -- the measurable half of the "EXPOSURE: FIXED / manual gain" line.
#   - FOCUS mode + auto/manual EXPOSURE mode (issue 1237): the relay's /api/state does NOT expose
#     these (relay/src/transport.rs reads iso/f-number/d002/d004/d005/d006/d007 only, no focus/mode
#     config), so they are surfaced HONESTLY as a NOTE -- never a fabricated pass (the LOUD-UNKNOWN
#     doctrine, .claude/rules/imag-ssh-remote-tool-preflight.md) -- and a follow-up extends the
#     relay so they become auto-checkable. The #220 manual checklist still owns those two lines.
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

# The #220 checklist's own minimum ("SHUTTER FAST: >= 1/500 s") -- ONE source of truth reused by
# both bkshading_preflight_classify's default and bkshading_preflight_report's default, mirroring
# bkshading_relay_port()'s single-source-of-truth convention (review finding, issue 808).
bkshading_preflight_min_shutter_denom() { printf '%s\n' 500; }

# --- pure JSON scalar extractors (arg1 = the relay's GET /api/state JSON string) ---------------
# python-only (no jq dependency), mirrors scripts/lib/measurement-eq.sh's "Scalar extractors"
# convention. A malformed/empty JSON string, or a missing/null field, prints EMPTY ("0" for the
# online flag) -- never a fabricated value ("unreadable is never a silent pass",
# audio-presence-preflight.sh's own rule).

bkshading_preflight_state_online() {
  python3 -c 'import json,sys
try:
    d = json.loads(sys.argv[1])
    online = d.get("online")
except Exception:
    online = None
print("1" if online is True else "0")' "${1:-}"
}

bkshading_preflight_state_camera() {
  python3 -c 'import json,sys
try:
    d = json.loads(sys.argv[1])
    c = d.get("camera")
except Exception:
    c = None
print(c if isinstance(c, str) and c else "")' "${1:-}"
}

bkshading_preflight_state_shutter() {
  python3 -c 'import json,sys
try:
    d = json.loads(sys.argv[1])
    s = (d.get("params") or {}).get("shutter")
except Exception:
    s = None
print(s if isinstance(s, int) and not isinstance(s, bool) else "")' "${1:-}"
}

# issue 1237: params.iso is the camera's ISO/gain (Option<i64> in bkshading/proto/src/wire.rs,
# set from the gphoto2 `iso` config by relay/src/transport.rs). Same non-dict/bool guards as the
# shutter extractor: a malformed/non-dict body, a non-dict params, a JSON null/bool, or an absent
# field all print EMPTY -- never a fabricated value.
bkshading_preflight_state_iso() {
  python3 -c 'import json,sys
try:
    d = json.loads(sys.argv[1])
    v = (d.get("params") or {}).get("iso")
except Exception:
    v = None
print(v if isinstance(v, int) and not isinstance(v, bool) else "")' "${1:-}"
}

# issue 1237: params.apertureAv is the camera's aperture value (AV = 2*log2(fNumber); Option<f64>
# in wire.rs, derived from the gphoto2 `f-number` config). A float OR int is accepted (isinstance
# int/float, excluding bool); null/absent/non-dict print EMPTY. Its PRESENCE is the aperture half
# of a fixed-exposure read (read.rs only populates it when the camera reports a valid f-number).
bkshading_preflight_state_aperture() {
  python3 -c 'import json,sys
try:
    d = json.loads(sys.argv[1])
    v = (d.get("params") or {}).get("apertureAv")
except Exception:
    v = None
print(v if isinstance(v, (int, float)) and not isinstance(v, bool) else "")' "${1:-}"
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
  local online="${1:-0}" camera="${2:-}" shutter="${3:-}" min="${4:-$(bkshading_preflight_min_shutter_denom)}"
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

# issue 1237: the EXPOSURE/gain classifier -- the measurable half of the #220 "EXPOSURE: FIXED /
# manual gain" line. The relay's /api/state exposes iso (gain) + apertureAv (aperture), so a fixed
# exposure is EVIDENCED when both are reported as concrete values (read.rs only fills them from a
# valid gphoto2 read). This does NOT prove the auto/manual exposure MODE is off (the relay does not
# expose that -- see bkshading_preflight_focus_note_message), only that concrete exposure values
# are readable.
# bkshading_preflight_classify_exposure <online 0|1> <camera> <iso> <aperture> -> one of:
#   skip-offline | ok | warn-iso | warn-aperture | warn-both
# skip-offline: no camera on this box (the portable-camera common case) -- gated exactly like the
#   shutter classifier so the two never disagree about whether a camera is present.
# warn-*: a camera IS online but iso and/or aperture is absent/unreadable -- cannot confirm a fixed
#   exposure, so warn loudly (naming which), never silently pass.
# ok: both iso and aperture reported.
bkshading_preflight_classify_exposure() {
  local online="${1:-0}" camera="${2:-}" iso="${3:-}" aperture="${4:-}"
  if [ "$online" != "1" ] || [ -z "$camera" ]; then
    printf 'skip-offline\n'
    return 0
  fi
  local iso_missing=0 ap_missing=0
  [ -z "$iso" ] && iso_missing=1
  [ -z "$aperture" ] && ap_missing=1
  if [ "$iso_missing" = 1 ] && [ "$ap_missing" = 1 ]; then
    printf 'warn-both\n'
  elif [ "$iso_missing" = 1 ]; then
    printf 'warn-iso\n'
  elif [ "$ap_missing" = 1 ]; then
    printf 'warn-aperture\n'
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

# issue 1237: exposure/gain OK line -- names the fixed ISO/gain + aperture AV the relay reported.
bkshading_preflight_exposure_ok_message() {
  local label="$1" ip="$2" camera="$3" iso="$4" aperture="$5"
  printf "    bkshading relay check (%s, %s): camera '%s' exposure fixed -- ISO/gain %s, aperture AV %s reported (#220 exposure/gain line satisfied automatically)\n" \
    "$label" "$ip" "$camera" "$iso" "$aperture"
}

# issue 1237: exposure/gain WARNING -- names whichever of ISO/gain or aperture the relay could not
# read for an online camera (self-derives the missing set from the empty value(s), so the ONE
# message covers warn-iso / warn-aperture / warn-both). REPORT-ONLY (never a hard gate).
bkshading_preflight_warn_exposure_message() {
  local label="$1" ip="$2" camera="$3" iso="$4" aperture="$5" missing=""
  [ -z "$iso" ] && missing="ISO/gain"
  if [ -z "$aperture" ]; then
    if [ -n "$missing" ]; then missing="$missing + aperture"; else missing="aperture"; fi
  fi
  printf "WARNING #1237: bkshading relay on %s (%s) reports camera '%s' online but %s not read -- cannot confirm a FIXED exposure/gain (#220 checklist: EXPOSURE FIXED / manual gain, no auto-exposure drift). Verify the camera's exposure manually, THEN run.\n" \
    "$label" "$ip" "$camera" "$missing"
}

# issue 1237: FOCUS + auto/manual EXPOSURE-MODE honesty NOTE. The relay's GET /api/state does NOT
# expose the camera's focus mode or its auto/manual exposure mode (relay/src/transport.rs reads only
# iso, f-number, d002/d004/d005/d006/d007 -- no focus/exposure-mode config). Per the LOUD-UNKNOWN
# doctrine (.claude/rules/imag-ssh-remote-tool-preflight.md) an unmeasurable signal is NEVER a silent
# pass: surface it as a NOTE so the #220 manual checklist still visibly owns FOCUS: MANUAL and
# no-auto-exposure-drift, and a follow-up extends the relay to make them auto-checkable.
bkshading_preflight_focus_note_message() {
  local label="$1"
  printf "    NOTE: bkshading relay does not expose FOCUS mode or the auto/manual EXPOSURE mode for %s -- the #220 manual checklist still owns 'FOCUS: MANUAL' and 'no auto-exposure drift'. Auto-checking those needs the relay to read those camera configs (tracked as a follow-up).\n" \
    "$label"
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
  local label="$1" ip="$2" port="${3:-$(bkshading_relay_port)}" max_time="${4:-5}" min="${5:-$(bkshading_preflight_min_shutter_denom)}"
  local raw status camera shutter online iso aperture exp_status
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
    # issue 1237: the EXPOSURE/gain half + the FOCUS honesty NOTE -- only when a camera is present
    # on this box (status != skip-offline). skip-offline (the portable-camera common case) stays as
    # quiet as the shutter path -- no exposure/focus lines for a box with no camera attached.
    if [ "$status" != skip-offline ]; then
      iso="$(bkshading_preflight_state_iso "$raw")"
      aperture="$(bkshading_preflight_state_aperture "$raw")"
      exp_status="$(bkshading_preflight_classify_exposure "$online" "$camera" "$iso" "$aperture")"
      case "$exp_status" in
        ok)     bkshading_preflight_exposure_ok_message "$label" "$ip" "$camera" "$iso" "$aperture" ;;
        warn-*) bkshading_preflight_warn_exposure_message "$label" "$ip" "$camera" "$iso" "$aperture" >&2 ;;
        *)      : ;;
      esac
      bkshading_preflight_focus_note_message "$label"
    fi
  else
    bkshading_preflight_skip_unreachable_message "$label" "$ip" "$port"
  fi
  return 0
}
