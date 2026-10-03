#!/usr/bin/env bash
# scripts/bkshading-wifi-heal.sh — one pass of the handheld SBC's WiFi heal (issue 808).
# Extended header below `set -euo pipefail` (kept early for pre-write-script-check.sh).
set -euo pipefail

# ---------------------------------------------------------------------------------------------
# WHY (design 5972548198): on handheld-1 (3.10.2026) the board sat on a dead AP with
# wpa_state=COMPLETED and a DHCP lease, and no traffic passed; nothing in wpa_supplicant or
# systemd-networkd checks that a COMPLETED link carries traffic. A manual `wpa_cli reassociate`
# brought it back at once. This pass, run by bkshading-wifi-heal.timer every 20 s:
#   1. reads wpa_state; while it is not COMPLETED the reachability rungs (steps 2-4) do nothing
#      (the supplicant is working) -- the stuck rung (step 5) is the one exception;
#   2. reads the DHCP default gateway of wlan0 from the route table (never a hard-coded address)
#      and pings it over wlan0;
#   3. keeps the consecutive-miss count in /run (tmpfs: the root is read-only) and lets the pure
#      bkshading_sbc_wifi_heal_decide choose none / reassociate (N misses) / restart (2N misses);
#   4. writes ONE journal line per action naming the BSSID + signal before and after;
#   5. the STUCK rung, decided by the pure bkshading_sbc_wifi_heal_stuck_decide:
#      - a supplicant that does not answer while its unit is STOPPED (inactive / failed) is started
#        at once: no driver reload while wlan0 is there; with wlan0 gone the remembered module is
#        loaded, or reloaded, first;
#      - a pass whose wpa_state is not COMPLETED and whose supplicant journal shows a
#        driver-refused association since the last pass (read through a journal cursor in /run,
#        so each line counts once), or whose supplicant does not answer while its unit runs (hung),
#        is a stuck pass. After 3 in a row it stops the supplicant, reloads the WiFi driver module
#        and starts the supplicant again (live 3.10.2026: the uwe5622 driver refused every
#        association, only a sprdwl_ng reload revived it);
#      - the module name is remembered in /run, so a module unloaded by a reload whose load failed
#        is still loaded again, and a module that loaded but never brought wlan0 back is reloaded;
#      - a trap armed only between the stop and the start loads the module and queues the
#        supplicant start if the pass is ended in between.
#      Plain scanning out of range is never stuck.
# No reboot, no ifdown loop. A first miss, and the first answer after misses or an action, are
# logged once each. A pass that cannot judge the link (a tool missing, ip or ping failing for
# another reason than a lost reply) changes nothing, says which tool, and exits 1, so the unit
# shows failed -- never a self-made reassociate or restart on a link nobody measured.
#
# Installed by scripts/bkshading-provision-sbc.sh --install to /usr/local/lib/bkshading/ (both libs
# beside it in lib/), run by systemd/bkshading-wifi-heal.service. The tools come from PATH
# (wpa_cli, ip, ping, systemctl, timeout, plus dirname/mkdir/mv/rm/sleep; everything else is a
# bash builtin; journalctl + modprobe are optional: without journalctl the stuck rung sees only a
# supplicant that does not answer, without modprobe or with a built-in driver it restarts the
# supplicant alone). For tests: BKSHADING_WIFI_HEAL_STATE_DIR (default /run/bkshading-wifi-heal),
# BKSHADING_WIFI_HEAL_SETTLE_S, BKSHADING_WIFI_HEAL_TOOL_TIMEOUT_S,
# BKSHADING_WIFI_HEAL_IFACE_WAIT_S, BKSHADING_WIFI_HEAL_SYSFS_NET (default /sys/class/net, where
# wlan0 and its driver module link are read), BKSHADING_WIFI_HEAL_SYSFS_MODULE (default
# /sys/module, where a loaded module has its dir).
# Exit 0 after a judged pass (also one that acted); 1 when it could not judge, or an action failed
# (result=FAILED); 143 / 130 when a SIGTERM / SIGINT ended a driver reload or start, and the failing
# status when a command failed inside one -- each after the restore trap loaded the driver (when
# the plan has a load) and queued the supplicant start.
# ---------------------------------------------------------------------------------------------

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bkshading-sbc-runtime.sh
. "$HERE/lib/bkshading-sbc-runtime.sh"
# shellcheck source=scripts/lib/bkshading-sbc-wifi-probe.sh
. "$HERE/lib/bkshading-sbc-wifi-probe.sh"

# A non-negative integer from an env override, else the lib default ($2).
int_or() {
  if [[ "${1:-}" =~ ^[0-9]{1,3}$ ]]; then printf '%s\n' "$((10#$1))"; else printf '%s\n' "$2"; fi
}

IFACE="$(bkshading_sbc_wifi_iface)"
CTRL_DIR="$(bkshading_sbc_wpa_ctrl_dir)"
WPA_UNIT="$(bkshading_sbc_wpa_unit)"
STATE_DIR="${BKSHADING_WIFI_HEAL_STATE_DIR:-/run/bkshading-wifi-heal}"
SETTLE_S="$(int_or "${BKSHADING_WIFI_HEAL_SETTLE_S:-}" "$(bkshading_sbc_wifi_heal_settle_s)")"
TOOL_TIMEOUT_S="$(int_or "${BKSHADING_WIFI_HEAL_TOOL_TIMEOUT_S:-}" "$(bkshading_sbc_wifi_tool_timeout_s)")"
PING_COUNT="$(bkshading_sbc_wifi_heal_ping_count)"
PING_TIMEOUT_S="$(bkshading_sbc_wifi_heal_ping_timeout_s)"
MISS_LIMIT="$(bkshading_sbc_wifi_heal_miss_limit)"
MISSES_FILE="$STATE_DIR/misses"
STUCK_FILE="$STATE_DIR/stuck"
STUCK_LIMIT="$(bkshading_sbc_wifi_heal_stuck_limit)"
SYSFS_NET="${BKSHADING_WIFI_HEAL_SYSFS_NET:-/sys/class/net}"
SYSFS_MODULE="${BKSHADING_WIFI_HEAL_SYSFS_MODULE:-/sys/module}"
LAST_ACTION_FILE="$STATE_DIR/last-action"
JOURNAL_CURSOR_FILE="$STATE_DIR/journal-cursor"
DRIVER_MODULE_FILE="$STATE_DIR/driver-module"
SYSTEMCTL_TIMEOUT_S="$(bkshading_sbc_wifi_heal_systemctl_timeout_s)"
IFACE_WAIT_S="$(int_or "${BKSHADING_WIFI_HEAL_IFACE_WAIT_S:-}" "$(bkshading_sbc_wifi_heal_iface_wait_s)")"

snapshot() { bkshading_sbc_wifi_snapshot wpa_cli "$CTRL_DIR" "$IFACE" "$TOOL_TIMEOUT_S"; }

# Point the journal cursor at the supplicant journal's last line: the next pass counts only lines a
# fresh supplicant writes (journalctl writes the cursor of the last entry it shows).
journal_cursor_to_end() {
  command -v journalctl >/dev/null 2>&1 || return 0
  rm -f "$JOURNAL_CURSOR_FILE"
  timeout "$TOOL_TIMEOUT_S" journalctl -u "$WPA_UNIT" "--cursor-file=$JOURNAL_CURSOR_FILE" -n 1 \
    -o cat --no-pager >/dev/null 2>&1 || true
}

# The driver module behind wlan0 (sprdwl_ng on the Orange Pi Zero 2W), read from its sysfs link
# with bash builtins (cd + pwd -P) and remembered in /run whenever the link resolves. Once a reload
# has unloaded the module (its load then failed, or the pass was killed in between), wlan0 and its
# link are gone, and only the remembered name can load the driver again. Prints the name, or
# nothing: no link while wlan0 exists = a built-in driver; no link, no wlan0 and nothing
# remembered = unknown.
driver_module() {
  local link="$SYSFS_NET/$IFACE/device/driver/module" m="" saved=""
  if [ -e "$link" ]; then
    if m="$(cd "$link" 2>/dev/null && pwd -P)"; then m="${m##*/}"; else m=""; fi
  fi
  if [ -r "$DRIVER_MODULE_FILE" ]; then saved="$(<"$DRIVER_MODULE_FILE")"; fi
  if bkshading_sbc_wifi_heal_module_name_ok "$m"; then
    if [ "$m" != "$saved" ]; then
      printf '%s\n' "$m" >"$DRIVER_MODULE_FILE.tmp.$$"
      mv -f "$DRIVER_MODULE_FILE.tmp.$$" "$DRIVER_MODULE_FILE"
    fi
    printf '%s\n' "$m"
  elif [ ! -e "$SYSFS_NET/$IFACE" ] && bkshading_sbc_wifi_heal_module_name_ok "$saved"; then
    printf '%s\n' "$saved"
  fi
}

# Wait up to IFACE_WAIT_S for wlan0 after a driver load (the driver creates it while it probes,
# a moment after modprobe returns). Returns 1 when it did not come.
wait_for_iface() {
  local i
  for ((i = 0; i < IFACE_WAIT_S; i++)); do
    [ -e "$SYSFS_NET/$IFACE" ] && break
    sleep 1
  done
  [ -e "$SYSFS_NET/$IFACE" ]
}

# Load the driver module $mod (load only: a no-op when it is loaded) and wait for wlan0. Returns 1
# when the load failed or wlan0 did not come back.
load_driver() {
  timeout "$SYSTEMCTL_TIMEOUT_S" modprobe "$mod" || return 1
  wait_for_iface
}

# The stuck rung's driver plan for the board as it is now (bkshading_sbc_wifi_heal_driver_plan):
# wlan0 present, and the module loaded (/sys/module/<name>).
driver_plan_now() {
  local iface=no loaded=no
  if [ -e "$SYSFS_NET/$IFACE" ]; then iface=yes; fi
  if [ -n "$mod" ] && [ -d "$SYSFS_MODULE/$mod" ]; then loaded=yes; fi
  bkshading_sbc_wifi_heal_driver_plan "$mod" "$has_modprobe" "$iface" "$loaded"
}

# Carry out $plan: reload = unload + load (+ the wait for wlan0), load = load only; any other plan
# has no driver step. Returns 1 when a step failed.
apply_driver_plan() {
  case "$plan" in
    reload)
      timeout "$SYSTEMCTL_TIMEOUT_S" modprobe -r "$mod" || return 1
      load_driver
      ;;
    load) load_driver ;;
    *) return 0 ;;
  esac
}

# Armed (arm_restore_trap) from the reload's stop, and on every start pass from its driver step
# (none while wlan0 is there), until the supplicant start. A pass ended there (systemd's SIGTERM at
# TimeoutStartSec, a `systemctl stop` of the heal, a Ctrl-C, a command that fails under errexit)
# would leave the board without its driver or its supplicant until a later pass noticed. The trap
# loads the module when the plan has one (a no-op when it is loaded) and QUEUES the supplicant start
# (--no-block): when the heal itself is being stopped, systemd runs its stop job before the
# supplicant's start job, so a blocking start would wait for this very pass to end. A killed start
# pass with no driver step only queues the start it was making. Further TERM/INT run a no-op handler
# meanwhile, so a second signal cannot cut the restore short; a handler (unlike SIG_IGN) is not
# inherited by the commands the trap runs.
# shellcheck disable=SC2317  # called only from the traps arm_restore_trap sets
reload_interrupted() {
  local doing="queueing the start of $WPA_UNIT"
  set +e
  trap ':' TERM INT
  trap - EXIT
  if [ "$plan" = reload ] || [ "$plan" = load ]; then doing="loading the driver $mod and $doing"; fi
  echo "bkshading-wifi-heal: ERROR: the pass ended in the middle of the driver reload or supplicant start ($1) -- $doing before it exits" >&2
  if [ "$plan" = reload ] || [ "$plan" = load ]; then
    load_driver || echo "bkshading-wifi-heal: ERROR: loading $mod failed or $IFACE did not come back" >&2
  fi
  timeout "$SYSTEMCTL_TIMEOUT_S" systemctl start --no-block "$WPA_UNIT" \
    || echo "bkshading-wifi-heal: ERROR: queueing the start of $WPA_UNIT failed" >&2
}

# Arm reload_interrupted for TERM (exit 143), INT (exit 130) and EXIT (the failing status).
arm_restore_trap() {
  trap 'reload_interrupted SIGTERM; exit 143' TERM
  trap 'reload_interrupted SIGINT; exit 130' INT
  trap 'reload_interrupted "exit $?"' EXIT
}

# After an action: wait for the NEW association, then print its snapshot. A COMPLETED read counts
# only once the state left COMPLETED or the BSSID moved off $1 (wpa_cli reassociate returns at once
# and the supplicant stays COMPLETED on the old BSSID for its whole scan); otherwise the last read
# within SETTLE_S is the "after".
settled_snapshot() {
  local before="$1" start="$SECONDS" left=0 snap bssid state
  while true; do
    snap="$(snapshot)"
    bssid="${snap%% *}"
    state="${snap##* }"
    [ "$state" = COMPLETED ] || left=1
    if [ "$state" = COMPLETED ] && { [ "$left" = 1 ] || [ "$bssid" != "$before" ]; }; then
      break
    fi
    [ $((SECONDS - start)) -lt "$SETTLE_S" ] || break
    sleep 1
  done
  printf '%s\n' "$snap"
}

missing="$(bkshading_sbc_wifi_missing_tools wpa_cli ip ping systemctl timeout)"
if [ -n "$missing" ]; then
  echo "bkshading-wifi-heal: ERROR: ${missing//$'\n'/ } not found -- the link cannot be judged, nothing done" >&2
  exit 1
fi

mkdir -p "$STATE_DIR"
mod="$(driver_module)"
plan=""
has_modprobe=no
if command -v modprobe >/dev/null 2>&1; then has_modprobe=yes; fi
prev=0
if [ -r "$MISSES_FILE" ]; then
  prev="$(<"$MISSES_FILE")"
fi
# A stale or damaged count reads as 0 (the decision does the same); 10# so 08 is never octal.
if [[ "$prev" =~ ^[0-9]{1,6}$ ]]; then prev=$((10#$prev)); else prev=0; fi

prev_stuck=0
if [ -r "$STUCK_FILE" ]; then
  prev_stuck="$(<"$STUCK_FILE")"
fi
if [[ "$prev_stuck" =~ ^[0-9]{1,6}$ ]]; then prev_stuck=$((10#$prev_stuck)); else prev_stuck=0; fi

read -r b_bssid b_rssi wpa_state <<<"$(snapshot)"
if [ "$wpa_state" = COMPLETED ]; then
  # A working link ends any stuck stretch -- also on a pass that cannot judge reachability below
  # (it exits 1 there): the stuck count starts over and the journal cursor is dropped, so the next
  # not-COMPLETED stretch starts at its own pass, never at lines from a working stretch.
  if [ "$prev_stuck" != 0 ]; then
    printf '%s\n' 0 >"$STUCK_FILE.tmp.$$"
    mv -f "$STUCK_FILE.tmp.$$" "$STUCK_FILE"
    prev_stuck=0
  fi
  [ ! -e "$JOURNAL_CURSOR_FILE" ] || rm -f "$JOURNAL_CURSOR_FILE"
fi
gw=""
reachable=no
if [ "$wpa_state" = COMPLETED ]; then
  if ! gw="$(bkshading_sbc_wifi_gateway ip "$IFACE")"; then
    echo "bkshading-wifi-heal: ERROR: 'ip -4 route show default dev $IFACE' failed -- the link cannot be judged, miss count kept at $prev" >&2
    exit 1
  fi
  if [ -n "$gw" ]; then
    if bkshading_sbc_wifi_ping ping "$IFACE" "$gw" "$PING_COUNT" "$PING_TIMEOUT_S"; then
      reachable=yes
    elif [ "$BKSHADING_SBC_WIFI_PING_RC" != 1 ]; then
      echo "bkshading-wifi-heal: ERROR: ping $gw on $IFACE failed with exit $BKSHADING_SBC_WIFI_PING_RC (not a lost reply) -- the link cannot be judged, miss count kept at $prev" >&2
      exit 1
    fi
  fi
fi

read -r action misses <<<"$(bkshading_sbc_wifi_heal_decide "$prev" "$wpa_state" "$reachable")"
printf '%s\n' "$misses" >"$MISSES_FILE.tmp.$$"
mv -f "$MISSES_FILE.tmp.$$" "$MISSES_FILE"

# The stuck rung (see the header, step 5). Driver-refused associations since the last pass, read
# from the supplicant journal only while the link is not COMPLETED (journalctl is optional). The
# read follows a cursor in /run, so every line counts once: --since one interval (+5 s) only when
# there is no cursor yet (a COMPLETED pass dropped it above). A failed read (a cursor journald no
# longer has, a hung journalctl) counts 0 and drops the cursor: the next pass reads --since again.
refused=0
if [ "$wpa_state" != COMPLETED ] && command -v journalctl >/dev/null 2>&1; then
  journal_args=(-u "$WPA_UNIT" "--cursor-file=$JOURNAL_CURSOR_FILE")
  if [ ! -s "$JOURNAL_CURSOR_FILE" ]; then
    journal_args+=(--since "-$(($(bkshading_sbc_wifi_heal_interval_s) + 5))s")
  fi
  if journal_text="$(timeout "$TOOL_TIMEOUT_S" journalctl "${journal_args[@]}" -o cat --no-pager 2>/dev/null)"; then
    refused="$(bkshading_sbc_wifi_heal_count_refused "$journal_text")"
  else
    rm -f "$JOURNAL_CURSOR_FILE"
  fi
fi
# A supplicant that does not answer is either stopped (start it) or hung (the reload path): its
# unit tells which. Read only then; an unreadable word ("") counts as running, never a start.
unit_word=""
if [ "$wpa_state" = "?" ]; then
  unit_word="$(timeout "$TOOL_TIMEOUT_S" systemctl is-active "$WPA_UNIT" 2>/dev/null || true)"
fi
read -r stuck_action stuck <<<"$(bkshading_sbc_wifi_heal_stuck_decide "$prev_stuck" "$wpa_state" "$refused" "$unit_word")"
printf '%s\n' "$stuck" >"$STUCK_FILE.tmp.$$"
mv -f "$STUCK_FILE.tmp.$$" "$STUCK_FILE"
if [ "$stuck" = 1 ]; then
  if [ "$wpa_state" = "?" ]; then
    echo "bkshading-wifi-heal: the supplicant on $IFACE does not answer while $WPA_UNIT is ${unit_word:-unreadable} -- stuck pass 1 of $STUCK_LIMIT before a driver reload"
  else
    echo "bkshading-wifi-heal: the WiFi driver refused $refused association(s) on $IFACE (wpa_state=$wpa_state) -- stuck pass 1 of $STUCK_LIMIT before a driver reload"
  fi
fi
if [ "$stuck_action" = reload-driver ] || [ "$stuck_action" = start ]; then
  action="$stuck_action"
fi

last_action=""
if [ -r "$LAST_ACTION_FILE" ]; then
  last_action="$(<"$LAST_ACTION_FILE")"
fi
if [ "$action" = none ] && [ "$misses" = 1 ]; then
  if [ -z "$gw" ]; then
    echo "bkshading-wifi-heal: no DHCP default route on $IFACE (bssid=$b_bssid signal=$b_rssi dBm) -- miss 1 of $MISS_LIMIT before a reassociate"
  else
    echo "bkshading-wifi-heal: gateway $gw on $IFACE did not answer $PING_COUNT pings (bssid=$b_bssid signal=$b_rssi dBm) -- miss 1 of $MISS_LIMIT before a reassociate"
  fi
elif [ "$reachable" = yes ] && { [ "$prev" -gt 0 ] || [ -n "$last_action" ]; }; then
  echo "bkshading-wifi-heal: gateway $gw on $IFACE answers again after $prev consecutive misses (bssid=$b_bssid signal=$b_rssi dBm)${last_action:+, the last action: $last_action}"
  rm -f "$LAST_ACTION_FILE"
elif [ "$wpa_state" = "?" ] && [ "$prev" -gt 0 ]; then
  echo "bkshading-wifi-heal: the supplicant on $IFACE does not answer ($WPA_UNIT is ${unit_word:-unreadable}; the stuck rung starts a stopped one and reloads the driver under a hung one) -- miss count reset from $prev"
elif [ "$wpa_state" != COMPLETED ] && [ "$prev" -gt 0 ]; then
  echo "bkshading-wifi-heal: wpa_state=$wpa_state on $IFACE (the supplicant is working) -- miss count reset from $prev"
fi

rc=0
case "$action" in
  reassociate)
    result="$(timeout "$TOOL_TIMEOUT_S" wpa_cli -p "$CTRL_DIR" -i "$IFACE" reassociate 2>/dev/null || true)"
    read -r a_bssid a_rssi a_state <<<"$(settled_snapshot "$b_bssid")"
    printf '%s\n' "wpa_cli reassociate" >"$LAST_ACTION_FILE"
    bkshading_sbc_wifi_heal_action_line "wpa_cli reassociate" "$gw" "$((prev + 1))" \
      "$b_bssid" "$b_rssi" "$a_bssid" "$a_rssi" "$a_state" "${result:-no reply}"
    ;;
  restart)
    if timeout "$SYSTEMCTL_TIMEOUT_S" systemctl restart "$WPA_UNIT"; then
      result=ok
    else
      result=FAILED
      rc=1
    fi
    read -r a_bssid a_rssi a_state <<<"$(settled_snapshot "$b_bssid")"
    printf '%s\n' "systemctl restart $WPA_UNIT" >"$LAST_ACTION_FILE"
    bkshading_sbc_wifi_heal_action_line "systemctl restart $WPA_UNIT" "$gw" "$((prev + 1))" \
      "$b_bssid" "$b_rssi" "$a_bssid" "$a_rssi" "$a_state" "$result"
    ;;
  start)
    # The supplicant unit is stopped (inactive / failed), not hung: start it, no driver reload while
    # wlan0 is there. When wlan0 is gone (a reload whose load failed, or a load that never brought
    # wlan0 back), the remembered driver module is loaded -- or reloaded, when it is loaded without
    # wlan0 -- first, under the same restore trap as a reload. The journal cursor then moves to the
    # end, so the next pass judges the new supplicant only.
    label="start $WPA_UNIT"
    detail="it was $unit_word; no driver reload"
    result=ok
    if [ ! -e "$SYSFS_NET/$IFACE" ]; then
      plan="$(driver_plan_now)"
      case "$plan" in
        reload | load)
          label="$plan the WiFi driver $mod and start $WPA_UNIT"
          detail="it was $unit_word; $IFACE was gone"
          ;;
        no-modprobe) detail="it was $unit_word; $IFACE is gone and modprobe is not found" ;;
        *) detail="it was $unit_word; $IFACE is gone and its driver module is unknown" ;;
      esac
    fi
    arm_restore_trap
    apply_driver_plan || result=FAILED
    journal_cursor_to_end
    timeout "$SYSTEMCTL_TIMEOUT_S" systemctl start "$WPA_UNIT" || result=FAILED
    trap - TERM INT EXIT
    [ "$result" = ok ] || rc=1
    read -r a_bssid a_rssi a_state <<<"$(settled_snapshot "$b_bssid")"
    printf '%s (%s)\n' "$label" "$detail" >"$LAST_ACTION_FILE"
    printf 'bkshading-wifi-heal: %s on %s (%s); after bssid=%s signal=%s dBm wpa_state=%s; result=%s\n' \
      "$label" "$IFACE" "$detail" "$a_bssid" "$a_rssi" "$a_state" "$result"
    ;;
  reload-driver)
    # The driver module behind wlan0 (driver_module above): reload it when it is loaded, load it
    # alone when it is not, else the supplicant restart alone.
    plan="$(driver_plan_now)"
    case "$plan" in
      reload)
        label="reload the WiFi driver $mod"
        if [ ! -e "$SYSFS_NET/$IFACE" ]; then label="$label ($IFACE was gone)"; fi
        ;;
      load) label="load the WiFi driver $mod ($IFACE was gone)" ;;
      no-modprobe) label="restart $WPA_UNIT (no driver reload: modprobe not found)" ;;
      builtin) label="restart $WPA_UNIT (no driver reload: a built-in driver)" ;;
      *) label="restart $WPA_UNIT (no driver reload: $IFACE is gone and its driver module is unknown)" ;;
    esac
    result=ok
    arm_restore_trap
    timeout "$SYSTEMCTL_TIMEOUT_S" systemctl stop "$WPA_UNIT" || result=FAILED
    # the old supplicant wrote its last refusals until the stop returned: the next pass judges the
    # reloaded driver on the lines written from here on
    journal_cursor_to_end
    apply_driver_plan || result=FAILED
    timeout "$SYSTEMCTL_TIMEOUT_S" systemctl start "$WPA_UNIT" || result=FAILED
    trap - TERM INT EXIT
    [ "$result" = ok ] || rc=1
    read -r a_bssid a_rssi a_state <<<"$(settled_snapshot "$b_bssid")"
    printf '%s\n' "$label" >"$LAST_ACTION_FILE"
    printf 'bkshading-wifi-heal: %s on %s after %s stuck passes (wpa_state=%s, driver-refused associations=%s); before bssid=%s signal=%s dBm; after bssid=%s signal=%s dBm wpa_state=%s; result=%s\n' \
      "$label" "$IFACE" "$STUCK_LIMIT" "$wpa_state" "$refused" "$b_bssid" "$b_rssi" "$a_bssid" "$a_rssi" "$a_state" "$result"
    ;;
esac
exit "$rc"
