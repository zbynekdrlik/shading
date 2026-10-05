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
# Source-only: the only top-level statements besides function defs source the sibling libs -- the
# ONE source-of-truth relay unit name (bkshading-relay-runtime.sh, bkshading_relay_unit_name), the ONE
# verified ro close (ro-window.sh) and the ONE EVENT Discord-note writer (event-mode-discord-confirm.sh)
# -- mirrors bkshading-e2e-pause.sh's own top-level sibling-lib source.
_BKSH_MODE_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bkshading-relay-runtime.sh
. "$_BKSH_MODE_HERE/bkshading-relay-runtime.sh"
# shellcheck source=scripts/lib/ro-window.sh
. "$_BKSH_MODE_HERE/ro-window.sh"   # ro_window_close_cmds -- the ONE verified ro close (issue 1407)
# shellcheck source=scripts/lib/event-mode-discord-confirm.sh
. "$_BKSH_MODE_HERE/event-mode-discord-confirm.sh"   # event_mode_discord_note_add, the ONE note writer

# _bkshading_relay_mode_close MODE CHANGE -> the shared verified close of the enable-state window
# (issue 1407): the ro remount, the root mode READ, and on a root left writable the FAIL lines
# naming the writers + `exit 1`. The box names itself at run time (one text serves every roster
# box). Its LAST FAIL line is the one bkshading_relay_mode_apply relays, so it says what to do.
_bkshading_relay_mode_close() {  # $1 = rig mode (test|event), $2 = what landed on disk
  local after=""
  [ "$1" = event ] && after=", so the relay is NOT started"
  ro_window_close_cmds "issue 1407" "\$(hostname 2>/dev/null || echo this box)" \
    "The $2 of $(bkshading_relay_unit_name) is on disk, but this box is left on a writable root$after." \
    "the root is still read-WRITE: stop that writer, put the root back read-only until 'findmnt -no OPTIONS /' reads ro, then re-run rig-mode.sh $1."
}

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
  # The persistent enable-state lives under /etc on the cambox's READ-ONLY root (issue 1311, live
  # on the source box 14.9.2026: the change failed 'Read-only file system' behind 2>/dev/null and
  # the unit stayed armed for the next boot). Mirror the painter's ro-persist helper: remount rw,
  # change, close the window with the ONE verified ro close (issue 1407: a root that does not read
  # ro again fails loud naming the writers), READ BACK, and exit non-zero when the state did not
  # land. A box without the unit (the painter box today) has nothing to persist ->
  # RELAY_ENABLED=not-found, exit 0. Every `systemctl` line still ENDS with `|| true` (the harness
  # guard): a failed change is captured into _rm_rc / _rm_missing on that same line and judged by
  # the `if` that follows -- the loud exit 1 never comes from a systemctl line itself.
  local close
  close="$(_bkshading_relay_mode_close test disable)"
  cat <<STOP
systemctl stop $unit 2>/dev/null || true
_rm_missing=0; systemctl cat $unit >/dev/null 2>&1 || _rm_missing=1 || true
if [ "\$_rm_missing" -eq 1 ]; then echo "RELAY_ENABLED=not-found"; exit 0; fi
_rm_rc=0
if mount -o remount,rw / 2>/dev/null; then
  systemctl disable $unit 2>/dev/null || _rm_rc=\$? || true
$close
else
  _rm_rc=98
fi
_rm_state="\$(systemctl is-enabled $unit 2>/dev/null)" || true
echo "RELAY_ENABLED=\${_rm_state:-unknown}"
if [ "\$_rm_state" = "enabled" ] || [ "\$_rm_rc" -ne 0 ]; then
  echo "FAIL: [issue 1311] $unit persist did not land (rc=\$_rm_rc is-enabled=\${_rm_state:-unknown}) -- a reboot would re-arm the relay" >&2
  exit 1
fi
STOP
}

# bkshading_relay_mode_start_cmds -> ENABLE + start the relay (EVENT mode). `enable` re-arms it for
# reboot; `start` brings the shading capability up for the broadcast. Only the `enable` runs inside
# the rw window; the start runs AFTER the verified close (issue 1407), so a root that does not read
# ro again starts nothing.
bkshading_relay_mode_start_cmds() {
  local unit close
  unit="$(bkshading_relay_unit_name)"
  close="$(_bkshading_relay_mode_close event enable)"
  cat <<START
_rm_missing=0; systemctl cat $unit >/dev/null 2>&1 || _rm_missing=1 || true
if [ "\$_rm_missing" -eq 1 ]; then echo "RELAY_ENABLED=not-found"; exit 0; fi
_rm_rc=0
if mount -o remount,rw / 2>/dev/null; then
  systemctl enable $unit 2>/dev/null || _rm_rc=\$? || true
$close
else
  _rm_rc=98
fi
systemctl start $unit 2>/dev/null || true
_rm_state="\$(systemctl is-enabled $unit 2>/dev/null)" || true
echo "RELAY_ENABLED=\${_rm_state:-unknown}"
if [ "\$_rm_state" != "enabled" ] || [ "\$_rm_rc" -ne 0 ]; then
  echo "FAIL: [issue 1311] $unit persist did not land (rc=\$_rm_rc is-enabled=\${_rm_state:-unknown}) -- the relay would not survive a reboot" >&2
  exit 1
fi
START
}

# --- thin best-effort ssh orchestrator (the ONE call site rig-mode.sh invokes) --------------
# bkshading_relay_mode_apply <action:test|event> <cam_pw> [label=ip ...] -> apply the mode's relay
# state to every box in the roster. Per box (a malformed pair is skipped; every box is tried), one
# status line each. Returns NON-ZERO when any box failed (issue 1311: never a claimed
# "stopped+disabled" that did not land; issue 1407: a box whose root did not go back read-only is a
# failure too). rig-mode.sh records that rc and goes on through every remaining step of the mode
# switch (the issue-868 pattern, issue 1407 design addendum item 1: under `set -e` a bare call left
# a measurement burn ON going into a production), then folds it into its exit status at the end.
# It also sets two lists in the CALLER's shell, both empty when every box landed:
# BKSHADING_RELAY_MODE_FAILED = one `label (ip)[ [writers: ...]]` entry per failed box, `, `-joined
# (the writers a failed ro close named), and BKSHADING_RELAY_MODE_FAILED_BOXES = the same boxes as
# `label (ip)` only. A third, BKSHADING_RELAY_MODE_WINDOW_FAILED, holds one `ip<TAB>kind<TAB>entry`
# line per box whose rw window itself failed, for the TEST painter-box stop below: kind `root-rw`
# (its own verified ro close failed, the root stayed read-WRITE) or `rw-refused` (the box refused
# the rw remount, rc=98). The report helpers name the boxes long after their FAIL lines scrolled by.
# `sshpass` is the OUTER command with
# `timeout` INSIDE it (issue 1290: a driver test that stubs `sshpass` as a shell function must be
# able to intercept it -- `timeout sshpass ...` would exec the real binary and bypass the stub).
bkshading_relay_mode_apply() {
  BKSHADING_RELAY_MODE_FAILED=""
  BKSHADING_RELAY_MODE_FAILED_BOXES=""
  BKSHADING_RELAY_MODE_WINDOW_FAILED=""
  local _failed=0 _out _rc _state _holders _entry
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
    _out=""; _rc=0
    _out="$(sshpass -p "$cam_pw" timeout 12 ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 \
      root@"$ip" "$cmds" 2>&1)" || _rc=$?
    _state="$(printf '%s\n' "$_out" | grep -oE '^RELAY_ENABLED=.*' | tail -n 1)"
    if [ "$_rc" -ne 0 ]; then
      # issue 1407: a failed ro close lists its writers on the box; this ONE line names them too.
      _holders="$(ro_window_holders "$_out")"
      echo "    [issue 1311] bkshading-relay $verb on $label ($ip): FAIL (rc=$_rc ${_state:-read-back missing}) -- $(printf '%s\n' "$_out" | grep -E '^FAIL' | tail -n 1)${_holders:+ (holders: $_holders)}" >&2
      _entry="$label ($ip)${_holders:+ [writers: $_holders]}"
      BKSHADING_RELAY_MODE_FAILED="${BKSHADING_RELAY_MODE_FAILED:+$BKSHADING_RELAY_MODE_FAILED, }$_entry"
      BKSHADING_RELAY_MODE_FAILED_BOXES="${BKSHADING_RELAY_MODE_FAILED_BOXES:+$BKSHADING_RELAY_MODE_FAILED_BOXES, }$label ($ip)"
      if ro_window_close_failed "$_out"; then
        BKSHADING_RELAY_MODE_WINDOW_FAILED+="$ip"$'\t'"root-rw"$'\t'"$_entry"$'\n'
      elif _bkshading_relay_mode_rw_refused "$_out"; then
        BKSHADING_RELAY_MODE_WINDOW_FAILED+="$ip"$'\t'"rw-refused"$'\t'"$_entry"$'\n'
      fi
      _failed=1
    else
      echo "    [issue 1311] bkshading-relay $verb on $label ($ip) [${_state:-read-back n/a}]"
    fi
  done
  [ "${_failed:-0}" -eq 0 ]
}

# --- the rig-mode caller's report of a failed relay step (issue 1407 design addendum item 1) ---
# rig-mode.sh keeps going after a failed apply (the issue-868 pattern) and reports it in three
# places, all naming the boxes the apply recorded. Each helper is a report, never a gate: it prints
# nothing for rc 0 and always returns 0; the caller folds the rc into its own exit.

# _bkshading_relay_mode_failed_boxes -> the failed boxes with the writers a failed ro close named,
# or a pointer to the per-box FAIL lines when the apply recorded none.
_bkshading_relay_mode_failed_boxes() {
  printf '%s' "${BKSHADING_RELAY_MODE_FAILED:-a relay box (see its [issue 1311] FAIL line above)}"
}

# bkshading_relay_mode_warn_continue MODE RC -> the loud WARNING (stderr) right after a failed apply:
# which boxes failed, that the switch goes on through its remaining steps, and that it will still
# exit non-zero.
bkshading_relay_mode_warn_continue() {  # $1 = rig mode (test|event), $2 = the apply's rc
  local mode="${1:-?}" rc="${2:-0}" rest="its remaining steps"
  [ "$rc" = 0 ] && return 0
  case "$mode" in
    event) rest="the burn-OFF, the strih NDI mapping and the EVENT contract, so no measurement burn is left ON" ;;
    test) rest="the painter, the burns and the chain checks" ;;
  esac
  echo "WARNING [issue 1407]: the bkshading relay step FAILED (rc=$rc) on $(_bkshading_relay_mode_failed_boxes) -- continuing through $rest; rig-mode $mode will still exit non-zero." >&2
  return 0
}

# bkshading_relay_mode_result MODE RC -> the RESULT line (stderr) at the end of a mode whose relay
# step failed: the boxes and their writers, what that leaves, and what to do.
bkshading_relay_mode_result() {  # $1 = rig mode (test|event), $2 = the apply's rc
  local mode="${1:-?}" rc="${2:-0}" state="its relay state did not land"
  [ "$rc" = 0 ] && return 0
  # A failed box can still have the relay running (the start runs after a failed enable) or still
  # armed, so the line says what is NOT confirmed, never more.
  case "$mode" in
    event) state="its shading relay may not be running for the broadcast, or not armed for a reboot" ;;
    test) state="its shading relay may still be running, or armed for a reboot" ;;
  esac
  echo "RESULT: rig-mode $mode -- the bkshading relay step FAILED (issue 1407, rc=$rc) on $(_bkshading_relay_mode_failed_boxes): $state. A box listed with writers still has a read-WRITE root (stop the writer, put the root back read-only until 'findmnt -no OPTIONS /' reads ro). Every other step of the switch ran. Fix the box (its [issue 1311] FAIL line above says why), then re-run rig-mode.sh $mode." >&2
  return 0
}

# _bkshading_relay_mode_rw_refused OUTPUT -> 0 when a box's relay output says the box REFUSED the rw
# remount: the stop/start text sets _rm_rc=98 when `mount -o remount,rw /` fails and prints
# `... persist did not land (rc=98 ...)` (issue 1407, decision 5997211658 Q3). A plain pattern match,
# no pipe, so it is safe in a condition under the caller's pipefail.
_bkshading_relay_mode_rw_refused() {  # $1 = the box's captured relay output
  case "${1:-}" in
    *"persist did not land (rc=98 "*) return 0 ;;
    *) return 1 ;;
  esac
}

# bkshading_relay_mode_painter_window_stop MODE PAINTER_IP -> 0, with the RESULT line on stderr, when
# the last apply found the PAINTER box's rw window itself broken; 1 and nothing printed otherwise.
# Two cases, both decided (issue 1407): its root stayed read-WRITE, i.e. its own verified ro close
# failed (decision 5996845165 Q1 = B, writers named when fuser/lsof found any), or it REFUSED the rw
# remount, rc=98, e.g. a filesystem forced read-only on errors or a failing stick (decision 5997211658
# Q3). rig-mode's TEST stops right there, before the painter launch: the painter handoff needs a
# working rw window on that same root, so it can only fail, and going on would stop the running
# painter and leave cam2 dark with no dead-man. TEST is development, so stopping strands nothing on
# air. Every other relay failure (the source box, an unreachable box) keeps the record-and-fold;
# EVENT never stops here (a burn left on air is the worse fault).
bkshading_relay_mode_painter_window_stop() {  # $1 = rig mode, $2 = the painter box ip
  local mode="${1:-?}" ip="${2:-}" row_ip="" row_kind="" row_entry="" kind="" entry="" what="" fix=""
  [ -n "$ip" ] || return 1
  while IFS=$'\t' read -r row_ip row_kind row_entry; do
    if [ "$row_ip" = "$ip" ]; then kind="$row_kind"; entry="$row_entry"; fi
  done <<<"${BKSHADING_RELAY_MODE_WINDOW_FAILED:-}"
  case "$kind" in
    root-rw)
      what="whose root stayed read-WRITE"
      fix="Put that root back read-only (stop the writer until 'findmnt -no OPTIONS /' reads ro)"
      ;;
    rw-refused)
      what="which REFUSED the read-write remount (rc=98: a filesystem forced read-only on errors, or a failing stick)"
      fix="Check that box first (dmesg for I/O errors, 'findmnt -no OPTIONS /'; never reboot a cambox remotely)"
      ;;
    *) return 1 ;;
  esac
  echo "RESULT: rig-mode $mode -- STOPPED before the painter launch: the bkshading relay step FAILED (issue 1407) on the painter box $entry, $what. The painter handoff needs a working rw window on that root, so it would fail and leave cam2 dark with no dead-man: the painter, the burns and the chain checks did not run, and the running painter was left as it is (decisions 5996845165, 5997211658). $fix, then re-run rig-mode.sh $mode. The relay step failed on: $(_bkshading_relay_mode_failed_boxes)." >&2
  return 0
}

# bkshading_relay_mode_discord_note MSG_FILE RC -> on a failed relay step, put one plain-Slovak
# warning line at the TOP of the EVENT Discord confirmation (MSG_FILE, the issue-724 message), so the
# owner's phone never reads a clean confirmation alone while the run exits non-zero. It names the
# boxes only (no process names) and claims nothing the contract below it decides. Written by the ONE
# shared event_mode_discord_note_add (scripts/lib/event-mode-discord-confirm.sh, sourced above);
# never fails the caller.
bkshading_relay_mode_discord_note() {  # $1 = the confirmation message file, $2 = the apply's rc
  local msg="${1:-}" rc="${2:-0}"
  [ "$rc" = 0 ] && return 0
  [ -n "$msg" ] && [ -f "$msg" ] || return 0
  event_mode_discord_note_add "$msg" "⚠️ Shading sa nepodarilo nastaviť na: ${BKSHADING_RELAY_MODE_FAILED_BOXES:-jednom z camboxov}. Shading tejto kamery počas vysielania nemusí fungovať. Ostatné kroky prepnutia prebehli, výsledok kontroly je nižšie. Napíš Claudovi, nech box skontroluje." top
  return 0
}
