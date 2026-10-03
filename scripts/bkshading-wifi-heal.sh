#!/usr/bin/env bash
# scripts/bkshading-wifi-heal.sh — one pass of the handheld SBC's WiFi heal (issue 808).
# Extended header below `set -euo pipefail` (kept early for pre-write-script-check.sh).
set -euo pipefail

# ---------------------------------------------------------------------------------------------
# WHY (design 5972548198): on handheld-1 (3.10.2026) the board sat on a dead AP with
# wpa_state=COMPLETED and a DHCP lease, and no traffic passed; nothing in wpa_supplicant or
# systemd-networkd checks that a COMPLETED link carries traffic. A manual `wpa_cli reassociate`
# brought it back at once. This pass, run by bkshading-wifi-heal.timer every 20 s:
#   1. reads wpa_state; while it is not COMPLETED it does nothing (the supplicant is working);
#   2. reads the DHCP default gateway of wlan0 from the route table (never a hard-coded address)
#      and pings it over wlan0;
#   3. keeps the consecutive-miss count in /run (tmpfs: the root is read-only) and lets the pure
#      bkshading_sbc_wifi_heal_decide choose none / reassociate (N misses) / restart (2N misses);
#   4. writes ONE journal line per action naming the BSSID + signal before and after.
# No reboot, no ifdown loop. A first miss, and the first answer after misses or an action, are
# logged once each. A pass that cannot judge the link (a tool missing, ip or ping failing for
# another reason than a lost reply) changes nothing, says which tool, and exits 1, so the unit
# shows failed -- never a self-made reassociate or restart on a link nobody measured.
#
# Installed by scripts/bkshading-provision-sbc.sh --install to /usr/local/lib/bkshading/ (both libs
# beside it in lib/), run by systemd/bkshading-wifi-heal.service. The tools come from PATH
# (wpa_cli, ip, ping, systemctl, timeout). For tests: BKSHADING_WIFI_HEAL_STATE_DIR (default
# /run/bkshading-wifi-heal), BKSHADING_WIFI_HEAL_SETTLE_S, BKSHADING_WIFI_HEAL_TOOL_TIMEOUT_S.
# Exit 0 after a judged pass (also one that acted); 1 when it could not judge, or a restart failed.
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
MIN_SETTLE_S="$(bkshading_sbc_wifi_heal_min_settle_s)"
[ "$MIN_SETTLE_S" -le "$SETTLE_S" ] || MIN_SETTLE_S="$SETTLE_S"
PING_COUNT="$(bkshading_sbc_wifi_heal_ping_count)"
PING_TIMEOUT_S="$(bkshading_sbc_wifi_heal_ping_timeout_s)"
MISS_LIMIT="$(bkshading_sbc_wifi_heal_miss_limit)"
MISSES_FILE="$STATE_DIR/misses"
LAST_ACTION_FILE="$STATE_DIR/last-action"
SYSTEMCTL_TIMEOUT_S=20

snapshot() { bkshading_sbc_wifi_snapshot wpa_cli "$CTRL_DIR" "$IFACE" "$TOOL_TIMEOUT_S"; }

# After an action: wait for the NEW association, then print its snapshot. A COMPLETED read counts
# only once the state left COMPLETED, or the BSSID moved off $1, or MIN_SETTLE_S passed (wpa_cli
# reassociate returns at once and the supplicant stays COMPLETED on the old BSSID while it scans);
# never longer than SETTLE_S.
settled_snapshot() {
  local before="$1" start="$SECONDS" left=0 snap bssid state elapsed
  while true; do
    snap="$(snapshot)"
    bssid="${snap%% *}"
    state="${snap##* }"
    [ "$state" = COMPLETED ] || left=1
    elapsed=$((SECONDS - start))
    if [ "$state" = COMPLETED ] && { [ "$left" = 1 ] || [ "$bssid" != "$before" ] || [ "$elapsed" -ge "$MIN_SETTLE_S" ]; }; then
      break
    fi
    [ "$elapsed" -lt "$SETTLE_S" ] || break
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
prev=0
if [ -r "$MISSES_FILE" ]; then
  prev="$(<"$MISSES_FILE")"
fi
# A stale or damaged count reads as 0 (the decision does the same); 10# so 08 is never octal.
if [[ "$prev" =~ ^[0-9]{1,6}$ ]]; then prev=$((10#$prev)); else prev=0; fi

read -r b_bssid b_rssi wpa_state <<<"$(snapshot)"
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
esac
exit "$rc"
