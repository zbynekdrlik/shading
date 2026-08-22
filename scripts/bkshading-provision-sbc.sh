#!/usr/bin/env bash
# scripts/bkshading-provision-sbc.sh — provision + verify the bkshading RELAY on a mini SBC/handheld.
# Extended header below `set -euo pipefail` (kept early for pre-write-script-check.sh).
set -euo pipefail

# ---------------------------------------------------------------------------------------------
# WHY: the LAST bkshading milestone (issue 808) — the handheld branch of the owner architecture
# (comment 5356048130 path 2, "cieľový stav"): a camera plugs USB into a mini SBC (a Pi Zero 2 W on
# the cage, on WiFi) which runs the SAME `bkshading-relay` component the camboxes run — a
# "mini-cambox without video". The strih aggregation service already understands this transport
# (`Transport::SbcRelay`, the `handheld-1` record in bkshading.example.toml, a params-only block
# with no NDI preview), but nothing PROVISIONS the relay on a bare SBC. This script does.
#
# It is the SBC counterpart of scripts/bkshading-provision-relay.sh, with two deliberate
# differences (see the design comment on issue 808):
#   (1) it REUSES systemd/bkshading-relay.service UNCHANGED — the SBC runs the same relay; and
#   (2) it writes NO CAMERA_BOX_CAPTURE_FPS env: an SBC has no camera-box appliance (no
#       camera-box.service.d drop-ins to derive from) and a handheld has no grab-rate comparison
#       (its config carries no grab_fps). The unit's `EnvironmentFile=-` makes the absent file
#       graceful — the relay reports capture_fps=None and the service uses its static config,
#       never a wrong value.
#
# Idempotent (re-run just re-verifies), fail-loud (a gap exits non-zero with the exact remediation),
# ENABLE-ONLY (daemon-reload + enable, NEVER start/restart — defer to reboot, per
# .claude/rules/provisioning-scripts.md; the relay's live verify against the camera is the
# supervisor's post-reboot rig step).
#
# The relay BINARY must be the aarch64 build (the `bkshading-relay-linux-arm64` CI artifact,
# deployed via `scripts/bkshading-deploy-relay.sh --arch arm64 --no-remount`). --check verifies the
# deployed binary is actually AArch64 (an ELF e_machine read) so a mis-deployed amd64 binary is
# caught here, not at reboot with an opaque `Exec format error`.
#
# Usage:  scripts/bkshading-provision-sbc.sh [--check|--install]
#   --check    (default) verify gphoto2 + unit + enabled + binary present + binary is aarch64;
#              0 if all OK, 1 + remediation.
#   --install  install gphoto2 (if missing), install + enable the (reused) relay unit; enable-only.
#
# Exit codes: 0 = OK; 1 = not fully provisioned + remediation printed; 2 = bad argument.
#
# Overridable targets (for Tier-0 tests to a temp root — no root/apt/systemd needed):
#   BKSHADING_SBC_UNIT_DEST, BKSHADING_SBC_BIN, BKSHADING_SBC_GPHOTO2, BKSHADING_SBC_SYSTEMCTL
# ---------------------------------------------------------------------------------------------

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
# shellcheck source=scripts/lib/bkshading-relay-runtime.sh
. "$HERE/lib/bkshading-relay-runtime.sh" # unit name / bin path / gphoto2 pkg — REUSED (one source of truth)
# shellcheck source=scripts/lib/bkshading-sbc-runtime.sh
. "$HERE/lib/bkshading-sbc-runtime.sh" # SBC-specific: ELF-arch check, cross target, no-env decision

UNIT_NAME="$(bkshading_relay_unit_name)"
UNIT_SRC="$REPO/systemd/$UNIT_NAME"
UNIT_DEST="${BKSHADING_SBC_UNIT_DEST:-/etc/systemd/system/$UNIT_NAME}"
RELAY_BIN="${BKSHADING_SBC_BIN:-$(bkshading_relay_bin_path)}"
GPHOTO2="${BKSHADING_SBC_GPHOTO2:-gphoto2}"
SYSTEMCTL="${BKSHADING_SBC_SYSTEMCTL:-systemctl}"
APT_PKG="$(bkshading_relay_apt_package)"

MODE="${1:---check}"
case "$MODE" in
  --check | --install) ;;
  -h | --help)
    grep -E '^# ' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  *)
    echo "unknown argument: $MODE (use --check or --install)" >&2
    exit 2
    ;;
esac

install_gphoto2() {
  if command -v "$GPHOTO2" >/dev/null 2>&1; then
    echo "  gphoto2 already present: $(command -v "$GPHOTO2")"
    return 0
  fi
  echo "  installing $APT_PKG (the relay's USB-PTP runtime) via apt ..."
  apt-get update -qq
  apt-get install -y -qq "$APT_PKG"
}

do_install() {
  echo "[bkshading-provision-sbc] --install (enable-only; takes effect on next reboot)"
  install_gphoto2

  # An SBC writes NO CAMERA_BOX_CAPTURE_FPS env (no appliance to derive from; a handheld has no grab
  # comparison). The predicate is the single source of truth; if it ever flips, this branch is where
  # the env write would go — until then we deliberately do NOT create /etc/bkshading/relay.env.
  if [ "$(bkshading_sbc_writes_capture_fps_env)" = "yes" ]; then
    echo "  WARNING: SBC capture-fps env is enabled but this script has no derive path" >&2
  else
    echo "  no capture-fps env on an SBC (handheld has no grab comparison; unit degrades gracefully)"
  fi

  mkdir -p "$(dirname "$UNIT_DEST")"
  install -m 0644 "$UNIT_SRC" "$UNIT_DEST"
  echo "  installed $UNIT_DEST (reused relay unit)"

  # ENABLE-ONLY: never start/restart the relay here (provisioning-scripts.md) — reboot / the
  # post-reboot verify brings it live.
  "$SYSTEMCTL" daemon-reload
  "$SYSTEMCTL" enable "$UNIT_NAME"
  echo "  enabled $UNIT_NAME (NOT started -- reboot to take effect)"

  if [ ! -x "$RELAY_BIN" ]; then
    echo "  WARNING: relay binary not present/executable at $RELAY_BIN -- deploy the aarch64" >&2
    echo "           bkshading-relay there (scripts/bkshading-deploy-relay.sh --arch arm64" >&2
    echo "           --no-remount --host <pi>) before reboot." >&2
  elif [ "$(bkshading_sbc_arch_ok "$(bkshading_sbc_elf_arch_of_file "$RELAY_BIN")")" != "yes" ]; then
    echo "  WARNING: relay binary at $RELAY_BIN is not aarch64 (found: $(bkshading_sbc_elf_arch_of_file "$RELAY_BIN")) --" >&2
    echo "           deploy the arm64 build (bkshading-relay-linux-arm64), not the amd64 one." >&2
  fi
  echo "install done. Verify with: scripts/bkshading-provision-sbc.sh --check"
}

do_check() {
  local rc=0 en arch
  # (1) relay binary present -- FIRST, so an unprovisioned SBC fails deterministically before we
  #     touch gphoto2 / systemctl.
  if [ -x "$RELAY_BIN" ]; then
    echo "OK: relay binary $RELAY_BIN"
    # (1b) and it must be the aarch64 build -- a mis-deployed amd64 binary would die at reboot with
    #      an opaque "Exec format error"; catch it here.
    arch="$(bkshading_sbc_elf_arch_of_file "$RELAY_BIN")"
    if [ "$(bkshading_sbc_arch_ok "$arch")" = "yes" ]; then
      echo "OK: relay binary is aarch64"
    else
      echo "FAIL: relay binary $RELAY_BIN is not aarch64 (found: $arch) -- deploy the arm64 build" >&2
      rc=1
    fi
  else
    echo "FAIL: relay binary missing/non-executable at $RELAY_BIN" >&2
    rc=1
  fi
  # (2) systemd unit installed AND byte-matches the repo unit (the SAME reused relay unit).
  if [ -f "$UNIT_DEST" ]; then
    if cmp -s "$UNIT_SRC" "$UNIT_DEST"; then
      echo "OK: unit installed $UNIT_DEST"
    else
      echo "FAIL: installed unit $UNIT_DEST differs from repo $UNIT_SRC (re-run --install)" >&2
      rc=1
    fi
  else
    echo "FAIL: unit not installed at $UNIT_DEST" >&2
    rc=1
  fi
  # (3) gphoto2 runtime present.
  if command -v "$GPHOTO2" >/dev/null 2>&1; then
    echo "OK: gphoto2 ($(command -v "$GPHOTO2"))"
  else
    echo "FAIL: gphoto2 not installed (the relay's USB-PTP transport)" >&2
    rc=1
  fi
  # (4) unit enabled (reboot-survival; live runtime state is the post-reboot rig verify).
  en="$("$SYSTEMCTL" is-enabled "$UNIT_NAME" 2>/dev/null || true)"
  if [ "$en" = "enabled" ]; then
    echo "OK: unit enabled"
  else
    echo "FAIL: unit not enabled (is-enabled=${en:-<none>})" >&2
    rc=1
  fi

  if [ "$rc" -ne 0 ]; then
    cat >&2 <<MSG
bkshading relay NOT fully provisioned on this SBC. Fix:
  scripts/bkshading-provision-sbc.sh --install   # gphoto2 + reused relay unit; enable (defer to reboot)
Then deploy the aarch64 bkshading-relay binary to $RELAY_BIN and reboot:
  scripts/bkshading-deploy-relay.sh --arch arm64 --no-remount --host <pi-ip>
MSG
  else
    echo "OK: bkshading relay fully provisioned on this SBC (live after reboot / already running)."
  fi
  return "$rc"
}

case "$MODE" in
  --install) do_install ;;
  --check) do_check ;;
esac
