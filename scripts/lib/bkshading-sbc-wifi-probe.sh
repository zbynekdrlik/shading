#!/usr/bin/env bash
# scripts/lib/bkshading-sbc-wifi-probe.sh — the handheld SBC's WiFi link reads (issue 808), shared
# by the heal (scripts/bkshading-wifi-heal.sh) and `bkshading-provision-sbc.sh --check`.
#
# Read-only: each function runs one kind of read (wpa_cli, ip, ping) and prints what it saw; it
# writes nothing. The tool is passed in (a name on PATH, or a test's stub), so the heal and --check
# read the gateway, the ping and the BSSID/signal the SAME way and cannot drift. It needs
# scripts/lib/bkshading-sbc-runtime.sh sourced first (the pure text parsers live there).
#
# Source-only: defines functions, performs no side effects, and deliberately does NOT
# `set -euo pipefail` (that would leak into the sourcing shell — ci-testing-gotchas.md). Every
# function is safe under the caller's `set -euo pipefail`: a failing tool is caught, never an abort.
# airuleset:script-ok source-only lib — set -euo pipefail would leak into the sourcing shell (ci-testing-gotchas)

# The tools of $@ that cannot be run (a name not on PATH, or a path that is not executable), one
# per line; nothing when all are there.
bkshading_sbc_wifi_missing_tools() {
  local t
  for t in "$@"; do
    command -v "$t" >/dev/null 2>&1 || printf '%s\n' "$t"
  done
}

# "<bssid> <signal> <wpa_state>" of the link right now, "?" for anything unknown (no supplicant
# behind the socket, a wedged one, no signal reading). $1 wpa_cli, $2 control dir, $3 interface,
# $4 seconds one wpa_cli call may take: a wedged supplicant never stalls the caller longer.
bkshading_sbc_wifi_snapshot() {
  local wpa_cli="$1" ctrl="$2" iface="$3" limit_s="$4" status poll bssid rssi state
  status="$(timeout "$limit_s" "$wpa_cli" -p "$ctrl" -i "$iface" status 2>/dev/null || true)"
  poll="$(timeout "$limit_s" "$wpa_cli" -p "$ctrl" -i "$iface" signal_poll 2>/dev/null || true)"
  bssid="$(bkshading_sbc_wpa_field "$status" bssid)"
  state="$(bkshading_sbc_wpa_field "$status" wpa_state)"
  rssi="$(bkshading_sbc_wpa_field "$poll" RSSI)"
  printf '%s %s %s\n' "${bssid:-?}" "${rssi:-?}" "${state:-?}"
}

# The DHCP default gateway of interface $2, read with $1 (ip) from the route table: prints it, or
# nothing when there is no default route, and returns 0. Returns 2 when the read itself failed
# (ip missing or erroring) -- no answer, so a caller must never take it for "no gateway".
bkshading_sbc_wifi_gateway() {
  local ip="$1" iface="$2" out
  out="$("$ip" -4 route show default dev "$iface" 2>/dev/null)" || return 2
  bkshading_sbc_default_gw_from_route "$out"
}

# Ping gateway $3 over interface $2 with $1: $4 pings, each waiting $5 s for its reply. Returns
# 0 = a reply, 1 = no reply (iputils' own "nothing answered"), 2 = no verdict (the tool is missing
# or failed for another reason); its raw exit code is left in BKSHADING_SBC_WIFI_PING_RC.
BKSHADING_SBC_WIFI_PING_RC=0
bkshading_sbc_wifi_ping() {
  local ping="$1" iface="$2" gw="$3" count="$4" wait_s="$5"
  BKSHADING_SBC_WIFI_PING_RC=0
  "$ping" -n -q -c "$count" -W "$wait_s" -I "$iface" "$gw" >/dev/null 2>&1 || BKSHADING_SBC_WIFI_PING_RC=$?
  case "$BKSHADING_SBC_WIFI_PING_RC" in
    0) return 0 ;;
    1) return 1 ;;
    *) return 2 ;;
  esac
}
