#!/usr/bin/env bash
# scripts/bkshading-provision-relay.sh — provision + verify the bkshading cambox RELAY (issue 808).
# Extended header below `set -euo pipefail`.
set -euo pipefail

# ---------------------------------------------------------------------------------------------
# WHY: the bkshading RELAY binary (bkshading/relay) drives the local Blackmagic camera over
# USB-PTP via the `gphoto2` CLI and exposes its shading over a small HTTP API the aggregation
# service polls. M1 shipped the binary (it already reads CAMERA_BOX_CAPTURE_FPS from its env and
# reports it as RelayState.capture_fps, which the service's issue-809 grab derive consumes) — but
# nothing RAN it on a cambox: no systemd unit, no gphoto2 install, no env wiring. So on a real box
# the relay never started and the derive saw capture_fps=None (static-config fallback). This script
# provisions all three.
#
# It DERIVES the env value from the box's OWN appliance capture-fps config (the camera-box.service.d
# drop-ins), mirroring src/capture.rs requested_capture_denominator — ONE source of truth, so the
# reported rate matches what the box actually grabs at (no hard-coded 60 duplicate).
#
# Idempotent (re-run just re-verifies / re-writes), fail-loud (a gap exits non-zero with the exact
# remediation), ENABLE-ONLY (daemon-reload + enable, NEVER start/restart — defer to reboot, per
# .claude/rules/provisioning-scripts.md; the relay's live verify against the camera is the
# supervisor's post-reboot rig step).
#
# The relay BINARY deploy (the CI-built bkshading-relay -> /usr/local/bin) is a SEPARATE supervisor
# step; --check treats a missing binary as a failure (the unit needs it), --install only warns.
#
# Usage:  scripts/bkshading-provision-relay.sh [--check|--install]
#   --check    (default) verify gphoto2 + unit + env + enabled + binary; 0 if all OK, 1 + remediation
#   --install  install gphoto2 (if missing), derive+write the env, install+enable the unit
#
# Exit codes: 0 = OK; 1 = not fully provisioned + remediation printed; 2 = bad argument.
#
# Overridable targets (for Tier-0 tests to a temp root — no root/apt/systemd needed):
#   BKSHADING_RELAY_UNIT_DEST, BKSHADING_RELAY_ENV_FILE, BKSHADING_RELAY_BIN,
#   BKSHADING_RELAY_DROPIN_DIR, BKSHADING_RELAY_GPHOTO2, BKSHADING_RELAY_SYSTEMCTL
# ---------------------------------------------------------------------------------------------

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
# shellcheck source=scripts/lib/bkshading-relay-runtime.sh
. "$HERE/lib/bkshading-relay-runtime.sh"

UNIT_NAME="$(bkshading_relay_unit_name)"
UNIT_SRC="$REPO/systemd/$UNIT_NAME"
UNIT_DEST="${BKSHADING_RELAY_UNIT_DEST:-/etc/systemd/system/$UNIT_NAME}"
ENV_FILE="${BKSHADING_RELAY_ENV_FILE:-$(bkshading_relay_env_path)}"
RELAY_BIN="${BKSHADING_RELAY_BIN:-$(bkshading_relay_bin_path)}"
DROPIN_DIR="${BKSHADING_RELAY_DROPIN_DIR:-/etc/systemd/system/camera-box.service.d}"
GPHOTO2="${BKSHADING_RELAY_GPHOTO2:-gphoto2}"
SYSTEMCTL="${BKSHADING_RELAY_SYSTEMCTL:-systemctl}"
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

# Derive the effective capture fps from the box's camera-box.service.d drop-ins (or the appliance
# default when none set it). Pure logic lives in the sourced helper.
derive_capture_fps() {
  local text="" f raw
  if [ -d "$DROPIN_DIR" ]; then
    for f in "$DROPIN_DIR"/*.conf; do
      [ -f "$f" ] || continue
      text="$text$(cat "$f")"$'\n'
    done
  fi
  raw="$(bkshading_relay_capture_fps_from_dropins "$text")"
  bkshading_relay_effective_capture_fps "$raw"
}

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
  echo "[bkshading-provision-relay] --install (enable-only; takes effect on next reboot)"
  install_gphoto2

  local fps
  fps="$(derive_capture_fps)"
  mkdir -p "$(dirname "$ENV_FILE")"
  bkshading_relay_env_file_content "$fps" >"$ENV_FILE"
  chmod 0644 "$ENV_FILE"
  echo "  wrote $ENV_FILE (CAMERA_BOX_CAPTURE_FPS=$fps, derived from $DROPIN_DIR)"

  mkdir -p "$(dirname "$UNIT_DEST")"
  install -m 0644 "$UNIT_SRC" "$UNIT_DEST"
  echo "  installed $UNIT_DEST"

  # ENABLE-ONLY: never start/restart the relay here (provisioning-scripts.md) — reboot / the
  # post-reboot verify step brings it live.
  "$SYSTEMCTL" daemon-reload
  "$SYSTEMCTL" enable "$UNIT_NAME"
  echo "  enabled $UNIT_NAME (NOT started -- reboot to take effect)"

  if [ ! -x "$RELAY_BIN" ]; then
    echo "  WARNING: relay binary not present/executable at $RELAY_BIN -- deploy the CI-built" >&2
    echo "           bkshading-relay there (separate supervisor step) before reboot." >&2
  fi
  echo "install done. Verify with: scripts/bkshading-provision-relay.sh --check"
}

do_check() {
  local rc=0 en fpsline
  # (1) relay binary present -- FIRST, so an unprovisioned box fails deterministically before we
  #     touch gphoto2 / systemctl.
  if [ -x "$RELAY_BIN" ]; then
    echo "OK: relay binary $RELAY_BIN"
  else
    echo "FAIL: relay binary missing/non-executable at $RELAY_BIN" >&2
    rc=1
  fi
  # (2) systemd unit installed AND byte-matches the repo unit.
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
  # (3) env file present + carries a valid integer CAMERA_BOX_CAPTURE_FPS.
  if [ -f "$ENV_FILE" ] && grep -qE '^CAMERA_BOX_CAPTURE_FPS=[0-9]+$' "$ENV_FILE"; then
    fpsline="$(grep -oE 'CAMERA_BOX_CAPTURE_FPS=[0-9]+' "$ENV_FILE" | tail -1)"
    echo "OK: env $ENV_FILE ($fpsline)"
  else
    echo "FAIL: env file $ENV_FILE missing or has no CAMERA_BOX_CAPTURE_FPS=<int>" >&2
    rc=1
  fi
  # (4) gphoto2 runtime present.
  if command -v "$GPHOTO2" >/dev/null 2>&1; then
    echo "OK: gphoto2 ($(command -v "$GPHOTO2"))"
  else
    echo "FAIL: gphoto2 not installed (the relay's USB-PTP transport)" >&2
    rc=1
  fi
  # (5) unit enabled (reboot-survival; live runtime state is the post-reboot rig verify).
  en="$("$SYSTEMCTL" is-enabled "$UNIT_NAME" 2>/dev/null || true)"
  if [ "$en" = "enabled" ]; then
    echo "OK: unit enabled"
  else
    echo "FAIL: unit not enabled (is-enabled=${en:-<none>})" >&2
    rc=1
  fi

  if [ "$rc" -ne 0 ]; then
    cat >&2 <<MSG
bkshading relay NOT fully provisioned on this box. Fix:
  scripts/bkshading-provision-relay.sh --install   # gphoto2 + unit + derived env; enable (defer to reboot)
Then deploy the CI-built bkshading-relay binary to $RELAY_BIN and reboot.
MSG
  else
    echo "OK: bkshading relay fully provisioned (live after reboot / already running)."
  fi
  return "$rc"
}

case "$MODE" in
  --install) do_install ;;
  --check) do_check ;;
esac
