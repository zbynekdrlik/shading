#!/usr/bin/env bash
# scripts/lib/bkshading-sbc-runtime.sh — shared constants + pure helpers for provisioning the
# bkshading RELAY on a separately powered zero-class arm64 SBC with WiFi (running ONLY the relay, no
# camera-box appliance). This is the LAST bkshading milestone (issue 808) — the handheld branch of
# the owner architecture (comment 5356048130 path 2, "cieľový stav"; Design v3 comment 5664682477,
# 14.9.2026): the camera plugs USB into the SBC's host port -> the SAME bkshading-relay component the
# camboxes run -> the strih service sees it uniformly as a params-only camera (no NDI preview).
# The board is DEVICE-AGNOSTIC (owner has not finally chosen): a Raspberry Pi Zero 2 W, a Radxa
# ZERO 3W, or an Orange Pi Zero 2W (the ordered prototype) — all zero-class arm64 SBCs with WiFi,
# powered from the camera cage's V-mount 5 V USB splitter (never a power bank / PiSugar / raw 15 V
# D-tap). See .claude/rules/bkshading.md.
#
# This lib is the single source of truth for the SBC-SPECIFIC decisions: the cross-compile target,
# the ELF-arch classifier used by the provision --check (so a mis-deployed amd64 binary is caught),
# and the "an SBC writes NO capture-fps env" decision. It deliberately REUSES the relay's own
# constants (unit name / bin path / gphoto2 pkg / port) from bkshading-relay-runtime.sh — the SBC
# runs the SAME unit — so there is ONE source of truth for those; the python test cross-checks both
# libs + the systemd unit + ci.yml so nothing can silently drift.
#
# Source-only: defines pure functions, performs NO side effects, and deliberately does NOT
# `set -euo pipefail` (that would leak into the sourcing shell — the sourced-harness set-e leak in
# .claude/rules/ci-testing-gotchas.md). Mirrors the pure-decision-in-lib split of the sibling
# bkshading-*-runtime.sh helpers.
# airuleset:script-ok source-only lib — set -euo pipefail would leak into the sourcing shell (ci-testing-gotchas)

# --- Cross-build target (issue 808 target justification; see .claude/rules/bkshading.md) ---

# The Rust cross-compile target for the SBC relay binary. aarch64-unknown-linux-gnu, NOT armhf:
# every candidate board (Raspberry Pi Zero 2 W, Radxa ZERO 3W, Orange Pi Zero 2W) is a Cortex-A53
# (ARMv8-A, 64-bit) and ships a 64-bit stock arm64 image (Raspberry Pi OS / Debian / Armbian); the
# relay is a tiny headless axum/tokio service (well under the 512 MB / 2 GB budget on 64-bit), and
# aarch64-gnu is the best-supported Rust cross (pure-Rust relay -> a trivial cross-link with the
# gcc-aarch64-linux-gnu linker; glibc matches every candidate image). A 32-bit
# armv7-unknown-linux-gnueabihf build is one extra CI matrix entry to add IF a legacy 32-bit
# handheld ever needs it — not the default.
bkshading_sbc_cross_target() { printf '%s\n' aarch64-unknown-linux-gnu; }

# The apt package (in a Debian/Raspberry Pi OS sources.list) that provides the aarch64 cross
# compiler + linker on the CI runner. The relay has NO C link deps (axum/tokio/serde/clap; no
# reqwest/rustls/ring on the relay side, tokio's mio is pure-Rust epoll), so this linker is all the
# cross-build needs.
bkshading_sbc_cross_linker_apt() { printf '%s\n' gcc-aarch64-linux-gnu; }

# The env var cargo reads for the aarch64 target's linker (CARGO_TARGET_<TRIPLE>_LINKER, upper-cased
# with '-' -> '_'). Kept here so the CI step and any local doc reference ONE spelling.
bkshading_sbc_cross_linker_env() { printf '%s\n' CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER; }
bkshading_sbc_cross_linker_bin() { printf '%s\n' aarch64-linux-gnu-gcc; }

# --- The SBC writes NO CAMERA_BOX_CAPTURE_FPS env (one-source-of-truth decision) ---

# A cambox derives CAMERA_BOX_CAPTURE_FPS from its own camera-box.service.d drop-ins (mirroring
# src/capture.rs requested_capture_denominator). An SBC has NO camera-box appliance and no drop-ins,
# and a handheld has no grab-rate comparison (its bkshading.example.toml record carries no
# `grab_fps`) — so the SBC provision writes NO env file. The relay unit's `EnvironmentFile=-` makes
# that graceful: absent file -> relay reports capture_fps=None -> the service falls back to the
# static config (no grab comparison for the handheld), never a wrong value. This pure predicate is
# the single source of truth; the python test pins it to `no`.
bkshading_sbc_writes_capture_fps_env() { printf '%s\n' no; }

# --- ELF architecture classification (provision --check catches a mis-deployed amd64 binary) ---

# Map an ELF e_machine value (decimal, little-endian) to a short arch name. 183 = AArch64,
# 62 = x86-64, 40 = ARM (32-bit / armhf). Anything else -> unknown.
bkshading_sbc_arch_from_machine() {
  case "${1:-}" in
    183) printf '%s\n' aarch64 ;;
    62) printf '%s\n' x86-64 ;;
    40) printf '%s\n' arm ;;
    *) printf '%s\n' unknown ;;
  esac
}

# Is a deployed relay binary's arch acceptable on the SBC? We build ONLY aarch64, so aarch64 is the
# one acceptable arch; a 32-bit `arm` or an `x86-64` binary is the wrong artifact and must fail the
# check loudly (the unit's ExecStart would otherwise die with `Exec format error` at reboot). If a
# future armhf target is added, widen this then.
bkshading_sbc_arch_ok() {
  case "${1:-}" in
    aarch64) printf '%s\n' yes ;;
    *) printf '%s\n' no ;;
  esac
}

# Read a binary's ELF e_machine (decimal, little-endian) from its header, or echo NOTHING when the
# file is missing / not an ELF / too short. Uses `od` (coreutils, always present) — no `file`/
# `readelf` dependency. Pure w.r.t. side effects (reads the file only). The trailing `|| true` on
# the od pipes keeps a short/odd file from aborting a caller under `set -euo pipefail`.
bkshading_sbc_elf_machine_from_file() {
  local f="${1:-}" magic bytes b18 b19
  [ -f "$f" ] || return 0
  magic="$(od -An -tx1 -N4 "$f" 2>/dev/null | tr -d ' \n' || true)"
  [ "$magic" = "7f454c46" ] || return 0
  bytes="$(od -An -tu1 -j18 -N2 "$f" 2>/dev/null || true)"
  # shellcheck disable=SC2086  # deliberate word-split of the two decimal bytes into positionals.
  set -- $bytes
  b18="${1:-}"
  b19="${2:-}"
  [ -n "$b18" ] && [ -n "$b19" ] || return 0
  printf '%s\n' "$(( b18 + b19 * 256 ))"
}

# Convenience: classify a binary file straight to an arch name (missing/non-ELF -> unknown).
bkshading_sbc_elf_arch_of_file() {
  local m
  m="$(bkshading_sbc_elf_machine_from_file "${1:-}")"
  [ -n "$m" ] || { printf '%s\n' unknown; return 0; }
  bkshading_sbc_arch_from_machine "$m"
}

# --- WiFi link classification (the handheld is wireless; provision --check verifies the link) ---

# Classify the wireless link state by reading <sysfs-root>/<iface-glob>/operstate. Prints:
#   up    - at least one matching interface has operstate "up" (carrier + associated)
#   down  - a matching wireless interface exists but none is "up" (not joined / no carrier)
#   none  - NO matching wireless interface at all -> a WIRED box (the cambox class): --check SKIPs it
# <sysfs-root> is the first positional so a Tier-0 test injects a fake /sys/class/net tree via
# BKSHADING_SBC_NET_SYSFS (the provision script passes ${BKSHADING_SBC_NET_SYSFS:-/sys/class/net}).
# BAND-AGNOSTIC on purpose: a 2.4 GHz-only board (e.g. a Pi Zero 2 W) is as valid as a dual-band one
# (Radxa / Orange Pi Zero 2W); the check proves only that a link is up, never which SSID or band.
# Reads only; a missing/empty tree -> none, never an error (safe under the caller's set -euo pipefail).
bkshading_sbc_wifi_link_state() {
  local root="${1:-/sys/class/net}" glob="${2:-wl*}"
  local iface found=0 up=0 st car restore_nullglob
  shopt -q nullglob && restore_nullglob=0 || restore_nullglob=1
  shopt -s nullglob
  # shellcheck disable=SC2231  # deliberate glob expansion of the iface pattern (no spaces in wl*).
  for iface in "$root"/$glob; do
    [ -r "$iface/operstate" ] || continue
    found=1
    st="$(cat "$iface/operstate" 2>/dev/null || true)"
    # operstate "up" is the primary signal, BUT some drivers (notably the out-of-tree uwe5622 on the
    # Orange Pi Zero 2W) leave operstate at "unknown"/"dormant" while genuinely associated, so
    # carrier==1 (an L1 link is present) also counts as up. A genuinely-down link has operstate
    # "down" AND no carrier. Reading `carrier` on a down iface can error ("Invalid argument") — the
    # 2>/dev/null + `|| true` degrades that to empty, never aborting the caller's set -euo pipefail.
    car="$(cat "$iface/carrier" 2>/dev/null || true)"
    if [ "$st" = "up" ] || [ "$car" = "1" ]; then up=1; fi
  done
  [ "$restore_nullglob" = 1 ] && shopt -u nullglob
  if [ "$found" -eq 0 ]; then
    printf '%s\n' none
  elif [ "$up" -eq 1 ]; then
    printf '%s\n' up
  else
    printf '%s\n' down
  fi
}

# Print the name of the FIRST wireless interface under <sysfs-root> matching <iface-glob> (e.g.
# `wlan0` / `wlp2s0`), or nothing if none. Used only to make the WiFi-down remediation name the real
# interface instead of a hard-coded `wlan0` (a board may enumerate as `wlan1`/`wlp2s0`). Pure, reads
# only; safe under set -euo pipefail (nullglob + a break, no head).
bkshading_sbc_first_wifi_iface() {
  local root="${1:-/sys/class/net}" glob="${2:-wl*}"
  local iface restore_nullglob
  shopt -q nullglob && restore_nullglob=0 || restore_nullglob=1
  shopt -s nullglob
  # shellcheck disable=SC2231  # deliberate glob expansion of the iface pattern (no spaces in wl*).
  for iface in "$root"/$glob; do
    if [ -e "$iface/operstate" ]; then
      [ "$restore_nullglob" = 1 ] && shopt -u nullglob
      basename "$iface"
      return 0
    fi
  done
  [ "$restore_nullglob" = 1 ] && shopt -u nullglob
}

# Parse the SSID from `iw dev <iface> link` output (the "SSID: <name>" line). Prints the SSID or
# nothing. Pure text parser (the caller runs iw/nmcli best-effort and feeds its output here) so it
# is Tier-0 testable without a real radio; informational only — never gates --check. No `head`
# (the SIGPIPE-under-pipefail footgun): `iw dev link` emits exactly one SSID line.
bkshading_sbc_wifi_ssid_from_iw() {
  printf '%s\n' "${1:-}" | sed -n 's/^[[:space:]]*SSID:[[:space:]]*//p'
}

# --- Read-only root (issue 808 slice B, owner ruling 5948648089: "the same as the camboxes") ---
# The fstab itself comes from the ONE shared canon scripts/lib/ro-root.sh. These are the SBC-only
# pieces around it: a single-partition SBC has no journal partition, so the journal lives in RAM.

# The journald drop-in the provision writes. `99-` sorts after every other drop-in, so a stray one
# can never turn the journal persistent on the read-only root again.
bkshading_sbc_journald_dropin_name() { printf '%s\n' 99-bkshading-volatile.conf; }

# Its content: Storage=volatile = the journal in /run/log/journal (RAM), never on the microSD.
bkshading_sbc_journald_dropin_content() {
  printf '%s\n' \
    "# Written by scripts/bkshading-provision-sbc.sh --install (issue 808): the SBC root is" \
    "# read-only, so the journal lives in RAM (/run/log/journal) and never writes the microSD." \
    "[Journal]" \
    "Storage=volatile"
}

# journald drop-ins left from bench debugging that make the journal persistent; --install removes
# them (handheld-1 carried 10-persistent.conf = Storage=persistent, 3.10.2026).
bkshading_sbc_stale_journald_dropins() { printf '%s\n' 10-persistent.conf; }

# Units that would write to the root on an Armbian image, so --install masks them and --check
# requires `systemctl is-enabled` = masked for each. Masking a unit an image does not ship only
# links it to /dev/null, and is-enabled then reads `masked` too (checked on systemd 255):
# - armbian-ramlog keeps /var/log in a zram and syncs it back to /var/log.hdd on the root. On a
#   read-only root /var/log is a tmpfs instead.
# - systemd-networkd-persistent-storage (Debian trixie) runs `networkctl persistent-storage yes`,
#   which fails with io.systemd.Network.StorageReadOnly on a read-only root and leaves the board
#   `degraded` (live on handheld-1, 3.10.2026). networkd keeps its state in /run without it.
# - fake-hwclock-save (service + its hourly timer) runs `fake-hwclock save`, which cannot write
#   /etc/fake-hwclock.data on a read-only root (the second read-only boot of handheld-1 went
#   `degraded` on it). The boot-time fake-hwclock-load only READS the file and stays; NTP sets the
#   real time right after boot.
bkshading_sbc_masked_units() {
  printf '%s\n' armbian-ramlog.service systemd-networkd-persistent-storage.service \
    fake-hwclock-save.service fake-hwclock-save.timer
}

# --- The handheld's WiFi belongs to wpa_supplicant, with a background scan (issue 808) ---
# Design 5972548198. Live on handheld-1 (3.10.2026, the second read-only boot): the two strong
# `newlevel.media` APs rejected the board right after the reboot (they still held its PMF
# association), wpa_supplicant joined the far AP at -73 dBm, DHCP succeeded, and no traffic passed.
# The supplicant stayed there: netplan 1.1 has no `bgscan` key, so the supplicant roams only when
# the link is LOST. --install therefore takes the WiFi over from netplan: its own
# wpa_supplicant@wlan0 conf (with bgscan) + a systemd-networkd DHCP file. Ethernet and usb0 stay
# on netplan.

# The ONE WiFi interface the takeover owns. Every candidate board (Pi Zero 2 W, Radxa ZERO 3W,
# Orange Pi Zero 2W) names its radio wlan0; a board with a wl* radio under another name is refused
# by --install, never guessed at.
bkshading_sbc_wifi_iface() { printf '%s\n' wlan0; }

# Debian's stock per-interface supplicant unit and the conf path it reads (-c of its ExecStart).
bkshading_sbc_wpa_unit() { printf 'wpa_supplicant@%s.service\n' "$(bkshading_sbc_wifi_iface)"; }
bkshading_sbc_wpa_conf_name() { printf 'wpa_supplicant-%s.conf\n' "$(bkshading_sbc_wifi_iface)"; }

# The control socket dir: in /run (the root is read-only), and what the heal + --check wpa_cli use.
bkshading_sbc_wpa_ctrl_dir() { printf '%s\n' /run/wpa_supplicant; }

# PMF (ieee80211w=1) as netplan generated it for this board (design: "as today"). key_mgmt
# deliberately DIFFERS from netplan's: no SAE (main ROZHODNUTÉ 5973115530 on the lane's
# Design-question 5972616531), only WPA-PSK + WPA-PSK-SHA256. SAE (WPA3) cannot
# authenticate from the 64-hex psk= the takeover writes -- it needs the passphrase. wpa_supplicant
# 2.10 keeps SAE among the candidate key_mgmt whenever the DRIVER supports SAE
# (wpa_supplicant.c wpa_supplicant_set_suites), prefers it over WPA-PSK-SHA256/WPA-PSK, and then
# fails the commit with "SAE: No password available" (sme.c). newlevel.media advertises
# WPA2-PSK+SAE on every BSSID (a WPA2/WPA3 transition network), so a listed SAE could keep the
# board off it; WPA2-PSK joins a transition AP fine.
bkshading_sbc_wpa_key_mgmt() { printf '%s\n' 'WPA-PSK WPA-PSK-SHA256'; }
bkshading_sbc_wpa_ieee80211w() { printf '%s\n' 1; }

# bgscan "simple:<short>:<threshold>:<long>": scan every <short> s while the signal is below
# <threshold> dBm, every <long> s otherwise, and roam to a stronger BSS of the same network.
# - threshold -65 dBm: in the handheld-1 incident the far AP read -73 dBm and the good one -63 dBm.
#   -65 sits between them, so a board stuck on a far AP scans fast and finds the near one, while a
#   board on a good AP (-63 or better) seldom scans.
# - short 30 s: a cameraman walks between the venue's three APs within minutes; 30 s finds the
#   nearer AP within one scan period without scanning a weak link continuously.
# - long 300 s: above the threshold the link is good; a scan every 5 min still finds a much
#   stronger AP and costs nothing the relay's small traffic would notice.
bkshading_sbc_bgscan_short_s() { printf '%s\n' 30; }
bkshading_sbc_bgscan_threshold_dbm() { printf '%s\n' -65; }
bkshading_sbc_bgscan_long_s() { printf '%s\n' 300; }
bkshading_sbc_bgscan_line() {
  printf 'bgscan="simple:%s:%s:%s"\n' "$(bkshading_sbc_bgscan_short_s)" \
    "$(bkshading_sbc_bgscan_threshold_dbm)" "$(bkshading_sbc_bgscan_long_s)"
}

# The value of a wpa_supplicant `ssid=` line: the quoted text for a printable-ASCII SSID without a
# double quote, else the unquoted hex of its bytes (wpa_supplicant reads both forms). Reads only
# its argument; `od -v` so a repeated run of bytes is never collapsed into a `*` line.
bkshading_sbc_wpa_ssid_value() {
  local s="${1:-}" hex i b printable=1
  hex="$(printf '%s' "$s" | od -An -tx1 -v | tr -d ' \n' || true)"
  [ -n "$hex" ] || printable=0
  for ((i = 0; i < ${#hex}; i += 2)); do
    b=$((16#${hex:i:2}))
    if [ "$b" -lt 32 ] || [ "$b" -gt 126 ] || [ "$b" -eq 34 ]; then
      printable=0
      break
    fi
  done
  if [ "$printable" = 1 ]; then
    printf '"%s"\n' "$s"
  else
    printf '%s\n' "$hex"
  fi
}

# The 64-hex PSK out of `wpa_passphrase` output: the `psk=` line, never its `#psk="<passphrase>"`
# comment line. Prints the lower-case hex or nothing. Walks the text with parameter expansion only
# (no here-string, no pipe), so the text it is handed is never written anywhere.
bkshading_sbc_psk_hex_from_wpa_passphrase() {
  local text="${1:-}" line
  while [ -n "$text" ]; do
    line="${text%%$'\n'*}"
    if [ "$line" = "$text" ]; then text=""; else text="${text#*$'\n'}"; fi
    line="${line#"${line%%[![:space:]]*}"}"
    if [[ "$line" =~ ^psk=([0-9a-fA-F]{64})$ ]]; then
      printf '%s\n' "${BASH_REMATCH[1],,}"
      return 0
    fi
  done
  return 0
}

# The wpa_supplicant conf --install writes. $1 = the country (ISO 3166 alpha-2; empty = no line),
# then pairs of <ssid> <64-hex psk>, one network block each. Pure: the PSK arrives as a FUNCTION
# argument, which never reaches any process's argv, and leaves only on this function's stdout.
bkshading_sbc_wpa_conf_text() {
  local country="${1:-}" ssid psk
  if [ "$#" -gt 0 ]; then shift; fi
  printf '%s\n' \
    "# Written by scripts/bkshading-provision-sbc.sh --install (issue 808): wpa_supplicant@$(bkshading_sbc_wifi_iface)" \
    "# owns the handheld's WiFi, migrated once from the board's netplan WiFi YAML (kept as .bak)." \
    "# psk= is the wpa_passphrase-derived 64-hex key, never the passphrase. A re-run keeps this file." \
    "ctrl_interface=$(bkshading_sbc_wpa_ctrl_dir)"
  if [ -n "$country" ]; then
    printf 'country=%s\n' "$country"
  fi
  while [ "$#" -ge 2 ]; do
    ssid="$1"
    psk="$2"
    shift 2
    printf '%s\n' "" "network={" \
      "	ssid=$(bkshading_sbc_wpa_ssid_value "$ssid")" \
      "	key_mgmt=$(bkshading_sbc_wpa_key_mgmt)" \
      "	ieee80211w=$(bkshading_sbc_wpa_ieee80211w)" \
      "	$(bkshading_sbc_bgscan_line)" \
      "	psk=$psk" \
      "}"
  done
}

# The WiFi IDENTITY of a wpa_supplicant conf: "country=<cc>" then one "network ssid=<v> psk=<v>"
# line per network block, everything else (comments, ctrl_interface, key_mgmt, bgscan) left out.
# --install compares identities, never whole texts, so a re-run is not refused because a comment or
# a constant of this lib changed since the conf was written. Holds the PSK: compare, never print.
bkshading_sbc_wpa_conf_identity() {
  local text="${1:-}" line in_net=0 country="" ssid="" psk="" nets=""
  while [ -n "$text" ]; do
    line="${text%%$'\n'*}"
    if [ "$line" = "$text" ]; then text=""; else text="${text#*$'\n'}"; fi
    line="${line%$'\r'}"
    line="${line#"${line%%[![:space:]]*}"}"
    line="${line%"${line##*[![:space:]]}"}"
    case "$line" in
      "network={")
        in_net=1
        ssid=""
        psk=""
        ;;
      "}")
        if [ "$in_net" = 1 ]; then nets+="network ssid=$ssid psk=$psk"$'\n'; fi
        in_net=0
        ;;
      country=*)
        if [ "$in_net" = 0 ]; then country="${line#country=}"; fi
        ;;
      ssid=*)
        if [ "$in_net" = 1 ]; then ssid="${line#ssid=}"; fi
        ;;
      psk=*)
        if [ "$in_net" = 1 ]; then psk="${line#psk=}"; fi
        ;;
    esac
  done
  printf 'country=%s\n%s' "$country" "$nets"
}

# The non-secret settings lines a conf must carry to match this lib (--check reports a drift, e.g.
# a conf kept from before a key_mgmt change); the bgscan line has its own --check row.
bkshading_sbc_wpa_conf_setting_lines() {
  printf '%s\n' "ctrl_interface=$(bkshading_sbc_wpa_ctrl_dir)" \
    "key_mgmt=$(bkshading_sbc_wpa_key_mgmt)" "ieee80211w=$(bkshading_sbc_wpa_ieee80211w)"
}

# The systemd-networkd file that runs DHCP on the WiFi once wpa_supplicant has associated: the same
# settings netplan generated before (/run/systemd/network/10-netplan-wlan0.network on handheld-1:
# DHCP=yes, LinkLocalAddressing=ipv6, RouteMetric=600, UseMTU=true), so nothing else changes. `05-`
# sorts before every netplan-generated `10-netplan-*` file, so a netplan catch-all can never win.
# $1 = the DHCP= value netplan would generate: `yes` for dhcp4 + dhcp6 (the Armbian preset, the
# default), `ipv4` for dhcp4 alone -- the takeover never adds DHCPv6 the YAML did not ask for.
bkshading_sbc_networkd_wifi_name() { printf '05-bkshading-%s.network\n' "$(bkshading_sbc_wifi_iface)"; }
bkshading_sbc_networkd_wifi_content() {
  local dhcp="${1:-yes}"
  printf '%s\n' \
    "# Written by scripts/bkshading-provision-sbc.sh --install (issue 808): wpa_supplicant@$(bkshading_sbc_wifi_iface)" \
    "# associates, systemd-networkd runs DHCP -- the same settings netplan generated before." \
    "[Match]" \
    "Name=$(bkshading_sbc_wifi_iface)" \
    "" \
    "[Network]" \
    "DHCP=$dhcp" \
    "LinkLocalAddressing=ipv6" \
    "" \
    "[DHCP]" \
    "RouteMetric=600" \
    "UseMTU=true"
}

# Debian's wpa_supplicant@.service has no Restart=. With this drop-in systemd restarts a CRASHED
# supplicant 5 s after the crash. The heal's stuck rung would also get there: a stopped/failed unit
# is started on the next pass (up to 20 s), and a hung one gets a driver reload after 3 passes
# (about a minute). A clean stop (systemctl stop, the heal's own restart) is untouched by
# Restart=on-failure. Path relative to the unit directory.
bkshading_sbc_wpa_restart_dropin_path() {
  printf '%s.d/bkshading-restart.conf\n' "$(bkshading_sbc_wpa_unit)"
}
bkshading_sbc_wpa_restart_dropin_content() {
  printf '%s\n' \
    "# Written by scripts/bkshading-provision-sbc.sh --install (issue 808): a crashed supplicant" \
    "# comes back by itself within 5 s; the WiFi heal starts a stopped one and reloads the driver" \
    "# under a hung one, which takes longer." \
    "[Service]" \
    "Restart=on-failure" \
    "RestartSec=5"
}

# --- The WiFi heal: re-join when the gateway stops answering (issue 808, design 5972548198) ---
# Nothing in wpa_supplicant or networkd checks that a COMPLETED link carries traffic: in the
# handheld-1 incident the board sat on a dead AP with wpa_state=COMPLETED and a DHCP lease, and a
# manual `wpa_cli reassociate` brought the traffic back at once. bkshading-wifi-heal.timer runs
# scripts/bkshading-wifi-heal.sh every interval: it pings the DHCP default gateway on wlan0 and,
# after N consecutive misses, reassociates; after another N it restarts wpa_supplicant@wlan0. No
# reboot, no ifdown loop. These reachability rungs do nothing while wpa_state is not COMPLETED (the
# supplicant itself is still working then); the stuck rung below is the one exception (a driver
# that refuses every association, a stopped or hung supplicant). Its state lives in /run (tmpfs,
# the root is read-only): the miss and stuck counts, the last action, the journal cursor and the
# remembered driver module.

# How often the timer fires one heal pass. The timer's OnUnitActiveSec carries the same value
# (a test pins the two equal).
bkshading_sbc_wifi_heal_interval_s() { printf '%s\n' 20; }

# Consecutive misses before an action: 3 x 20 s = about 60 s of a dead link before a reassociate,
# and another 60 s before a supplicant restart. Long enough for DHCP after a fresh association
# (about 20 s on handheld-1) and for one dropped ping burst, short enough that a cameraman notices
# no more than a minute without shading.
bkshading_sbc_wifi_heal_miss_limit() { printf '%s\n' 3; }

# One reachability probe = this many pings, each waiting this many seconds for its reply. A pass
# misses only when ALL of them are lost, so one dropped packet on a busy venue WiFi is not a miss.
bkshading_sbc_wifi_heal_ping_count() { printf '%s\n' 3; }
bkshading_sbc_wifi_heal_ping_timeout_s() { printf '%s\n' 2; }

# After an action, wait up to this long for the new association before reading the "after" BSSID
# and signal for the journal line. `wpa_cli reassociate` returns at once, and while the supplicant
# scans it keeps wpa_state=COMPLETED on the OLD BSSID (it leaves COMPLETED only once it moves), and
# a full 2.4 + 5 GHz scan with passive DFS channels takes about 3-8 s. So a COMPLETED read counts as
# the new association only after the state left COMPLETED or the BSSID changed; otherwise (a
# reassociation back to the same AP, too quick to see) the heal waits this whole bound.
bkshading_sbc_wifi_heal_settle_s() { printf '%s\n' 15; }

# One wpa_cli call may take this long: a wedged supplicant (each call otherwise waits ~10 s for its
# socket) must never push a pass past the service's TimeoutStartSec before its action line is out.
bkshading_sbc_wifi_tool_timeout_s() { printf '%s\n' 3; }

# One systemctl stop/start/restart or modprobe call may take this long. Generous for a supplicant
# stop (it deauthenticates first) and a module load (the driver loads its firmware), and still a
# bound: systemd/bkshading-wifi-heal.service's TimeoutStartSec is sized from it (a test computes the
# longest pass from these constants).
bkshading_sbc_wifi_heal_systemctl_timeout_s() { printf '%s\n' 20; }

# After a driver load, wait this long for wlan0 to come back: the driver creates the interface
# while it probes, a moment after modprobe returns (handheld-1: COMPLETED 6 s after the reload).
bkshading_sbc_wifi_heal_iface_wait_s() { printf '%s\n' 15; }

# Where the heal script + this lib are installed on the board (mirrors the repo layout, so the
# script's own `$HERE/lib/bkshading-sbc-runtime.sh` resolves unchanged) and the heal units.
bkshading_sbc_wifi_heal_install_dir() { printf '%s\n' /usr/local/lib/bkshading; }
bkshading_sbc_wifi_heal_script_name() { printf '%s\n' bkshading-wifi-heal.sh; }
bkshading_sbc_wifi_heal_timer() { printf '%s\n' bkshading-wifi-heal.timer; }
bkshading_sbc_wifi_heal_units() { printf '%s\n' bkshading-wifi-heal.service bkshading-wifi-heal.timer; }

# The heal decision. $1 = the previous consecutive-miss count (anything not a plain number reads
# as 0), $2 = wpa_state, $3 = whether the gateway answered: yes | no. Prints "<action> <misses>":
#   - wpa_state not COMPLETED -> none 0 (the supplicant is still working; the count starts over)
#   - reachable              -> none 0
#   - a miss                 -> the count + 1; at 2N -> restart 0 (a fresh start for the new
#                               supplicant), at N -> reassociate N, else none.
# A count already past 2N (a stale file) restarts at once, never loops forever without acting.
# The count starts over on a not-COMPLETED pass on purpose: each NEW association gets its full N
# passes (about 60 s) for DHCP before it is judged. The cost: a board that is caught mid-transition
# after every reassociate never reaches the restart tier. A reassociation to a dead AP reads
# COMPLETED again within one 20 s pass, so the restart still comes in practice. A supplicant that
# keeps failing to associate is the supplicant working, unless the DRIVER refuses the associations
# or the supplicant stopped or hung: those are the stuck rung's (bkshading_sbc_wifi_heal_stuck_decide).
bkshading_sbc_wifi_heal_decide() {
  local prev="${1:-0}" state="${2:-}" reachable="${3:-}" limit misses
  limit="$(bkshading_sbc_wifi_heal_miss_limit)"
  [[ "$prev" =~ ^[0-9]{1,6}$ ]] || prev=0
  if [ "$state" != COMPLETED ] || [ "$reachable" = yes ]; then
    printf '%s\n' "none 0"
    return 0
  fi
  misses=$((10#$prev + 1))
  if [ "$misses" -ge $((2 * limit)) ]; then
    printf '%s\n' "restart 0"
  elif [ "$misses" -eq "$limit" ]; then
    printf 'reassociate %s\n' "$misses"
  else
    printf 'none %s\n' "$misses"
  fi
}

# --- The stuck rung: a supplicant that never gets back to COMPLETED -----------------------------
# Live on handheld-1 (3.10.2026): after a run of forced reassociations the out-of-tree uwe5622
# driver answered every connect with "Association request to the driver failed", so wpa_state
# never reached COMPLETED and the reachability rungs above (which wait while the supplicant is
# "working") never acted. A supplicant restart and a link down/up did not help; reloading the
# driver module (sprdwl_ng) did, COMPLETED 6 s later. A supplicant that stops answering wpa_cli
# while its unit still runs (hung, not crashed) is the same dead end. So a pass is STUCK when
# wpa_state is not COMPLETED and either the supplicant journal shows a driver-refused association
# since the last pass, or the supplicant does not answer ("?") while `systemctl is-active` does not
# read it stopped. A STOPPED unit (inactive / failed: a manual `systemctl stop`, a start that failed)
# is simply started again on that pass, no driver reload. Plain scanning out of range is never
# stuck (no refusal lines). After this many consecutive stuck passes (3 x 20 s = about a minute)
# the heal stops the supplicant, reloads the WiFi driver module and starts the supplicant again.
bkshading_sbc_wifi_heal_stuck_limit() { printf '%s\n' 3; }
# The wpa_supplicant line a driver-refused association writes (wpa_supplicant 2.10, sme.c/events.c).
bkshading_sbc_wifi_heal_driver_failed_text() { printf '%s\n' 'Association request to the driver failed'; }

# How many lines of $1 (the supplicant journal text since the last pass) carry the driver-refused
# text; every other supplicant line is ignored. Counted with bash itself (no grep): the heal runs
# with the declared tools only, and an empty text is 0, never a false stuck pass.
bkshading_sbc_wifi_heal_count_refused() {
  local text="${1:-}" line needle n=0
  needle="$(bkshading_sbc_wifi_heal_driver_failed_text)"
  while [ -n "$text" ]; do
    line="${text%%$'\n'*}"
    if [ "$line" = "$text" ]; then text=""; else text="${text#*$'\n'}"; fi
    if [[ "$line" == *"$needle"* ]]; then n=$((n + 1)); fi
  done
  printf '%s\n' "$n"
}

# The stuck decision. $1 = the previous consecutive stuck-pass count (anything not a plain number
# reads as 0), $2 = wpa_state ("?" = the supplicant does not answer), $3 = driver-refused lines in
# the supplicant journal since the last pass (anything not a plain number reads as 0 = no
# evidence), $4 = the supplicant unit's `systemctl is-active` word, read only when $2 is "?"
# (empty = not read, or unreadable). Prints "<action> <stuck>":
#   COMPLETED                              -> none 0
#   "?" and the unit inactive | failed     -> start 0: the supplicant was stopped (a manual
#                                             `systemctl stop`, a failed start) -- start it, no
#                                             driver reload, and no stuck pass
#   "?" and any other word (active, activating, unreadable) -> a stuck pass: running but silent
#                                             = hung (an unreadable word never starts anything)
#   a driver-refused association           -> a stuck pass
#   a stuck pass                           -> the count + 1; at the limit (or a stale count past
#                                             it) -> reload-driver 0, else none <count>
#   any other pass (plain scanning)        -> none 0
bkshading_sbc_wifi_heal_stuck_decide() {
  local prev="${1:-0}" state="${2:-}" failed="${3:-0}" unit="${4:-}" limit stuck
  limit="$(bkshading_sbc_wifi_heal_stuck_limit)"
  [[ "$prev" =~ ^[0-9]{1,6}$ ]] || prev=0
  [[ "$failed" =~ ^[0-9]{1,6}$ ]] || failed=0
  if [ "$state" = COMPLETED ]; then
    printf '%s\n' "none 0"
    return 0
  fi
  if [ "$state" = "?" ] && { [ "$unit" = inactive ] || [ "$unit" = failed ]; }; then
    printf '%s\n' "start 0"
    return 0
  fi
  if [ "$state" = "?" ] || [ "$((10#$failed))" -gt 0 ]; then
    stuck=$((10#$prev + 1))
    if [ "$stuck" -ge "$limit" ]; then
      printf '%s\n' "reload-driver 0"
    else
      printf 'none %s\n' "$stuck"
    fi
    return 0
  fi
  printf '%s\n' "none 0"
}

# A kernel module name as modprobe takes it (letters, digits, _ and -). The driver module the heal
# reads from sysfs, or from its own file in /run, must look like one before it reaches modprobe.
bkshading_sbc_wifi_heal_module_name_ok() { [[ "${1:-}" =~ ^[A-Za-z0-9_-]{1,64}$ ]]; }

# What the stuck rung can do with the WiFi driver. $1 = the driver module behind wlan0 (empty, or
# anything that is not a module name, = none known), $2 = modprobe present: yes | no, $3 = wlan0
# present: yes | no. Prints one word:
#   reload       a module, and wlan0 is there (the module is loaded): unload it, load it
#   load         a module, and wlan0 is gone: the module is not loaded (a reload whose load failed,
#                a pass killed after the unload) -- load it only
#   no-modprobe  a module, but no modprobe on the board: the supplicant restart alone
#   builtin      wlan0 is there with no module behind it: a built-in driver, the restart alone
#   unknown      wlan0 is gone and no module name was ever read: nothing to load
bkshading_sbc_wifi_heal_driver_plan() {
  local mod="${1:-}" has_modprobe="${2:-no}" iface="${3:-no}"
  bkshading_sbc_wifi_heal_module_name_ok "$mod" || mod=""
  if [ -n "$mod" ] && [ "$has_modprobe" = yes ]; then
    if [ "$iface" = yes ]; then printf '%s\n' reload; else printf '%s\n' load; fi
  elif [ -n "$mod" ]; then
    printf '%s\n' no-modprobe
  elif [ "$iface" = yes ]; then
    printf '%s\n' builtin
  else
    printf '%s\n' unknown
  fi
}

# The DHCP default gateway out of `ip -4 route show default dev <iface>`: the address after the
# first `via`, or nothing. Never a hard-coded address, so the heal works on any venue's WiFi.
bkshading_sbc_default_gw_from_route() {
  local text="${1:-}" line
  while [ -n "$text" ]; do
    line="${text%%$'\n'*}"
    if [ "$line" = "$text" ]; then text=""; else text="${text#*$'\n'}"; fi
    if [[ "$line" =~ (^|[[:space:]])via[[:space:]]+([0-9a-fA-F.:]+) ]]; then
      printf '%s\n' "${BASH_REMATCH[2]}"
      return 0
    fi
  done
  return 0
}

# The value of one `key=value` line in `wpa_cli status` / `wpa_cli signal_poll` output (exact key
# match; the first line wins), or nothing.
bkshading_sbc_wpa_field() {
  local text="${1:-}" key="${2:-}" line
  [ -n "$key" ] || return 0
  while [ -n "$text" ]; do
    line="${text%%$'\n'*}"
    if [ "$line" = "$text" ]; then text=""; else text="${text#*$'\n'}"; fi
    line="${line%$'\r'}"
    if [ "${line%%=*}" = "$key" ] && [ "$line" != "$key" ]; then
      printf '%s\n' "${line#*=}"
      return 0
    fi
  done
  return 0
}

# The one journal line a heal action writes. $1 action, $2 gateway (empty = none), $3 misses,
# $4/$5 BSSID + signal before, $6/$7/$8 BSSID + signal + wpa_state after, $9 the action's result.
bkshading_sbc_wifi_heal_action_line() {
  local action="${1:-}" gw="${2:-}" misses="${3:-}"
  printf 'bkshading-wifi-heal: %s on %s after %s consecutive misses (gateway %s); before bssid=%s signal=%s dBm; after bssid=%s signal=%s dBm wpa_state=%s; result=%s\n' \
    "$action" "$(bkshading_sbc_wifi_iface)" "$misses" "${gw:-none (no DHCP default route)}" \
    "${4:-?}" "${5:-?}" "${6:-?}" "${7:-?}" "${8:-?}" "${9:-?}"
}
