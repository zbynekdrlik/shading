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
  local iface found=0 up=0 st restore_nullglob
  shopt -q nullglob && restore_nullglob=0 || restore_nullglob=1
  shopt -s nullglob
  # shellcheck disable=SC2231  # deliberate glob expansion of the iface pattern (no spaces in wl*).
  for iface in "$root"/$glob; do
    [ -r "$iface/operstate" ] || continue
    found=1
    st="$(cat "$iface/operstate" 2>/dev/null || true)"
    if [ "$st" = "up" ]; then up=1; fi
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

# Parse the SSID from `iw dev <iface> link` output (the "SSID: <name>" line). Prints the SSID or
# nothing. Pure text parser (the caller runs iw/nmcli best-effort and feeds its output here) so it
# is Tier-0 testable without a real radio; informational only — never gates --check. No `head`
# (the SIGPIPE-under-pipefail footgun): `iw dev link` emits exactly one SSID line.
bkshading_sbc_wifi_ssid_from_iw() {
  printf '%s\n' "${1:-}" | sed -n 's/^[[:space:]]*SSID:[[:space:]]*//p'
}
