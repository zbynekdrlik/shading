#!/usr/bin/env bash
# airuleset:script-ok source-only lib (functions only; sourced into a caller that owns its own
# shell options) -- mirrors the sibling scripts/lib/bkshading-e2e-pause.sh convention: deliberately
# NOT `set -euo pipefail` here, since sourcing this file runs it in the CALLER's shell and strict
# mode would leak into whichever caller (rig-mode.sh, or a test harness) sources it.
#
# scripts/lib/bkshading-relay-mode.sh -- issue 1311 (Finding 2 -> Mitigations 2b): scope the
# bkshading-relay to EVENT mode. During DEVELOPMENT (rig-mode.sh test) the shading panel is not
# needed, and every relay start/stop is a PTP-session power change on the shared xHCI root hub that
# also carries the boot stick + the grabber (issue 1309/1311 Finding 1/2). Two boot sticks died in
# 24h on exactly the two boxes with a shading camera on that hub. So:
#   - rig-mode.sh test  -> STOP + DISABLE the relay on the relay boxes (no PTP session, no
#     issue-1229 polling noise during measurement; disable keeps it from returning on reboot).
#   - rig-mode.sh event -> ENABLE + START it (shading available for the broadcast).
# The E2E's #808 pause/restore (scripts/lib/bkshading-e2e-pause.sh) then finds `was-active=0` in
# TEST mode (unit inactive, no /run marker) and toggles nothing -- a true no-op; no change needed
# there.
#
# Split (mirrors bkshading-e2e-pause.sh): pure remote-text builders (no I/O, unit-testable via
# `run_sourced` in tests/harness_bkshading_relay_mode_1311.rs) + a thin, best-effort ssh
# orchestrator at the bottom (the ONE call site rig-mode.sh's do_test/do_event invoke). The relay
# ROSTER is passed IN by the caller (rig-mode.sh derives the source box + cam2 -- the SAME two
# boxes the #808 E2E pause targets), NEVER a literal box list embedded here.
#
# Source-only: the only top-level statement besides function defs is sourcing
# bkshading-relay-runtime.sh for the ONE source-of-truth relay unit name (bkshading_relay_unit_name)
# -- mirrors bkshading-e2e-pause.sh's own top-level sibling-lib source.
_BKSH_MODE_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bkshading-relay-runtime.sh
. "$_BKSH_MODE_HERE/bkshading-relay-runtime.sh"

# --- pure remote-text builders --------------------------------------------------------------
# Each echoes REMOTE bash text (the WHOLE remote command string of an ssh call) with the ONE
# source-of-truth unit name baked in as a literal. Tolerant throughout (`|| true`): a box without
# the unit installed, or an already-stopped/started unit, is a clean no-op that never fails the
# caller. Meant to be embedded as `"$(bkshading_relay_mode_stop_cmds)"` -- it is the WHOLE remote
# command, so the #744/#746 trailing-newline-glue trap does not apply (nothing follows the $(...)).

# bkshading_relay_mode_stop_cmds -> stop + DISABLE the relay (TEST mode). `stop` removes the live
# PTP session power draw + the issue-1229 polling noise from the shared hub NOW; `disable` keeps
# the unit from returning on the next reboot. disable is best-effort (a read-only /etc could refuse
# the wants-symlink removal) -- the stop is the load-bearing half.
bkshading_relay_mode_stop_cmds() {
  local unit
  unit="$(bkshading_relay_unit_name)"
  cat <<STOP
systemctl stop $unit 2>/dev/null || true
systemctl disable $unit 2>/dev/null || true
STOP
}

# bkshading_relay_mode_start_cmds -> ENABLE + start the relay (EVENT mode). `enable` re-arms it for
# reboot; `start` brings the shading capability up for the broadcast.
bkshading_relay_mode_start_cmds() {
  local unit
  unit="$(bkshading_relay_unit_name)"
  cat <<START
systemctl enable $unit 2>/dev/null || true
systemctl start $unit 2>/dev/null || true
START
}

# --- thin best-effort ssh orchestrator (the ONE call site rig-mode.sh invokes) --------------
# bkshading_relay_mode_apply <action:test|event> <cam_pw> [label=ip ...] -> apply the mode's relay
# state to every box in the roster. Best-effort per box (a bad/unreachable box, or a malformed
# pair, is skipped and NEVER aborts the caller -- always returns 0). Prints one status line per
# box. `sshpass` is the OUTER command with `timeout` INSIDE it (issue 1290: a driver test that
# stubs `sshpass` as a shell function must be able to intercept it -- `timeout sshpass ...` would
# exec the real binary and bypass the stub).
bkshading_relay_mode_apply() {
  local action="$1" cam_pw="$2"
  shift 2 || return 0
  local cmds verb
  case "$action" in
    test)
      cmds="$(bkshading_relay_mode_stop_cmds)"
      verb="stopped+disabled (TEST mode -- EVENT-only relay, issue 1311)"
      ;;
    event)
      cmds="$(bkshading_relay_mode_start_cmds)"
      verb="enabled+started (EVENT mode)"
      ;;
    *)
      echo "bkshading_relay_mode_apply: unknown action '$action' (want test|event)" >&2
      return 0
      ;;
  esac
  local pair label ip
  for pair in "$@"; do
    label="${pair%%=*}"
    ip="${pair#*=}"
    if [ -z "$label" ] || [ -z "$ip" ] || [ "$label" = "$ip" ]; then
      continue
    fi
    sshpass -p "$cam_pw" timeout 12 ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
      root@"$ip" "$cmds" 2>/dev/null || true
    echo "    [issue 1311] bkshading-relay $verb on $label ($ip)"
  done
  return 0
}
