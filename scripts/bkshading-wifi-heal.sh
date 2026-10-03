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
# No reboot, no ifdown loop. A miss and the recovery after misses are logged once each.
#
# Installed by scripts/bkshading-provision-sbc.sh --install to /usr/local/lib/bkshading/ (with the
# lib beside it in lib/), run by systemd/bkshading-wifi-heal.service. The tools come from PATH
# (wpa_cli, ip, ping, systemctl); BKSHADING_WIFI_HEAL_STATE_DIR (default /run/bkshading-wifi-heal)
# and BKSHADING_WIFI_HEAL_SETTLE_S override the state dir and the post-action wait for tests.
# Exit 0 after every pass, also one that acted; exit 1 only when a supplicant restart failed.
# ---------------------------------------------------------------------------------------------

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bkshading-sbc-runtime.sh
. "$HERE/lib/bkshading-sbc-runtime.sh"

IFACE="$(bkshading_sbc_wifi_iface)"
CTRL_DIR="$(bkshading_sbc_wpa_ctrl_dir)"
WPA_UNIT="$(bkshading_sbc_wpa_unit)"
STATE_DIR="${BKSHADING_WIFI_HEAL_STATE_DIR:-/run/bkshading-wifi-heal}"
SETTLE_S="${BKSHADING_WIFI_HEAL_SETTLE_S:-}"
[[ "$SETTLE_S" =~ ^[0-9]{1,3}$ ]] || SETTLE_S="$(bkshading_sbc_wifi_heal_settle_s)"
SETTLE_S=$((10#$SETTLE_S))
PING_COUNT="$(bkshading_sbc_wifi_heal_ping_count)"
PING_TIMEOUT_S="$(bkshading_sbc_wifi_heal_ping_timeout_s)"
MISSES_FILE="$STATE_DIR/misses"

# wpa_cli on this interface's control socket; an unreachable supplicant answers nothing.
wpa() { wpa_cli -p "$CTRL_DIR" -i "$IFACE" "$@" 2>/dev/null || true; }

# "<bssid> <signal> <wpa_state>" of the link right now ("?" for what is unknown).
link_snapshot() {
  local status poll bssid rssi state
  status="$(wpa status)"
  poll="$(wpa signal_poll)"
  bssid="$(bkshading_sbc_wpa_field "$status" bssid)"
  state="$(bkshading_sbc_wpa_field "$status" wpa_state)"
  rssi="$(bkshading_sbc_wpa_field "$poll" RSSI)"
  printf '%s %s %s\n' "${bssid:-?}" "${rssi:-?}" "${state:-?}"
}

# Wait up to SETTLE_S for wpa_state=COMPLETED, then print the snapshot (whatever it is then).
settled_snapshot() {
  local i snap
  for ((i = 0; i <= SETTLE_S; i++)); do
    snap="$(link_snapshot)"
    if [ "${snap##* }" = COMPLETED ] || [ "$i" -ge "$SETTLE_S" ]; then
      break
    fi
    sleep 1
  done
  printf '%s\n' "$snap"
}

mkdir -p "$STATE_DIR"
prev=0
if [ -r "$MISSES_FILE" ]; then
  prev="$(<"$MISSES_FILE")"
fi
# A stale or damaged count reads as 0 (the decision does the same); 10# so 08 is never octal.
if [[ "$prev" =~ ^[0-9]{1,6}$ ]]; then prev=$((10#$prev)); else prev=0; fi

status="$(wpa status)"
wpa_state="$(bkshading_sbc_wpa_field "$status" wpa_state)"
gw=""
reachable=no
if [ "$wpa_state" = COMPLETED ]; then
  gw="$(bkshading_sbc_default_gw_from_route "$(ip -4 route show default dev "$IFACE" 2>/dev/null || true)")"
  if [ -n "$gw" ] && ping -n -q -c "$PING_COUNT" -W "$PING_TIMEOUT_S" -I "$IFACE" "$gw" >/dev/null 2>&1; then
    reachable=yes
  fi
fi

read -r action misses <<<"$(bkshading_sbc_wifi_heal_decide "$prev" "$wpa_state" "$reachable")"
printf '%s\n' "$misses" >"$MISSES_FILE.tmp.$$"
mv -f "$MISSES_FILE.tmp.$$" "$MISSES_FILE"

if [ "$action" = none ] && [ "$misses" = 1 ]; then
  echo "bkshading-wifi-heal: gateway ${gw:-none (no DHCP default route)} on $IFACE did not answer $PING_COUNT pings (miss 1 of $(bkshading_sbc_wifi_heal_miss_limit) before a reassociate)"
elif [ "$reachable" = yes ] && [ "$prev" -gt 0 ]; then
  echo "bkshading-wifi-heal: gateway $gw on $IFACE answers again after $prev consecutive misses"
elif [ "$wpa_state" != COMPLETED ] && [ "$prev" -gt 0 ]; then
  echo "bkshading-wifi-heal: wpa_state=${wpa_state:-unknown} on $IFACE (the supplicant is working) -- miss count reset from $prev"
fi

rc=0
case "$action" in
  reassociate)
    read -r b_bssid b_rssi _ <<<"$(link_snapshot)"
    result="$(wpa reassociate)"
    read -r a_bssid a_rssi a_state <<<"$(settled_snapshot)"
    bkshading_sbc_wifi_heal_action_line "wpa_cli reassociate" "$gw" "$((prev + 1))" \
      "$b_bssid" "$b_rssi" "$a_bssid" "$a_rssi" "$a_state" "${result:-no reply}"
    ;;
  restart)
    read -r b_bssid b_rssi _ <<<"$(link_snapshot)"
    if systemctl restart "$WPA_UNIT"; then
      result=ok
    else
      result=FAILED
      rc=1
    fi
    read -r a_bssid a_rssi a_state <<<"$(settled_snapshot)"
    bkshading_sbc_wifi_heal_action_line "systemctl restart $WPA_UNIT" "$gw" "$((prev + 1))" \
      "$b_bssid" "$b_rssi" "$a_bssid" "$a_rssi" "$a_state" "$result"
    ;;
esac
exit "$rc"
