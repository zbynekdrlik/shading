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
# Usage:  scripts/bkshading-provision-relay.sh [--check|--install] [--rig-mode test|event]
#   --check    (default) verify gphoto2 + unit + env + binary + the rig mode's enable-state;
#              0 if all OK, 1 + remediation
#   --install  install gphoto2 (if missing), derive+write the env, install the unit and enable OR
#              disable it per the rig mode (never start)
#   --rig-mode test|event  (default $CAMERA_BOX_RIG_MODE) -- issue 808: the SAME enable-state
#              table setup-device.sh uses (bkshading_relay_expected_enable_state): in TEST the relay
#              roster (the source box + cam2, issue 1311) is installed DISABLED. On a roster box the
#              mode MUST be given (the answer differs per mode, and guessing it wrong would re-arm
#              or kill the relay); any other box is enabled in both modes. The box is named by its
#              hostname (override: BKSHADING_RELAY_DEVICE_NAME).
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
# shellcheck source=scripts/lib/bkshading-relay-provision.sh
. "$HERE/lib/bkshading-relay-provision.sh" # the ONE install body, shared with setup-device.sh (issue 808)
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh" # camera_source_box -- the TEST relay roster's source box

UNIT_NAME="$(bkshading_relay_unit_name)"
UNIT_SRC="$REPO/systemd/$UNIT_NAME"
UNIT_DEST="${BKSHADING_RELAY_UNIT_DEST:-/etc/systemd/system/$UNIT_NAME}"
ENV_FILE="${BKSHADING_RELAY_ENV_FILE:-$(bkshading_relay_env_path)}"
RELAY_BIN="${BKSHADING_RELAY_BIN:-$(bkshading_relay_bin_path)}"
GPHOTO2="${BKSHADING_RELAY_GPHOTO2:-gphoto2}"
SYSTEMCTL="${BKSHADING_RELAY_SYSTEMCTL:-systemctl}"

MODE="--check"
RIG_MODE_ARG="${CAMERA_BOX_RIG_MODE:-}"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --check | --install) MODE="$1"; shift ;;
    --rig-mode)
      [ "$#" -ge 2 ] || { echo "--rig-mode needs test or event" >&2; exit 2; }
      RIG_MODE_ARG="$2"; shift 2 ;;
    -h | --help)
      grep -E '^# ' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "unknown argument: $1 (use --check or --install [--rig-mode test|event])" >&2
      exit 2
      ;;
  esac
done
# The enable-state this box must carry in this rig mode -- the ONE table (issue 808). An
# unresolvable roster (no source box) takes the passive direction: installed disabled.
DEVICE="${BKSHADING_RELAY_DEVICE_NAME:-$(hostname 2>/dev/null || true)}"
SOURCE_BOX="$(camera_source_box 2>/dev/null || true)"
PAINTER_BOX="$(bkshading_relay_roster_painter_box)"
if [ -z "$RIG_MODE_ARG" ]; then
  if [ "$(bkshading_relay_expected_enable_state "$DEVICE" test "$SOURCE_BOX" "$PAINTER_BOX")" = \
       "$(bkshading_relay_expected_enable_state "$DEVICE" event "$SOURCE_BOX" "$PAINTER_BOX")" ]; then
    RIG_MODE_ARG=event   # not a roster box: the same state in both modes
  elif [ -z "$SOURCE_BOX" ]; then
    echo "ERROR: the TEST relay roster could not be resolved (no source box from camera_source_box): pass --rig-mode test|event (or CAMERA_BOX_RIG_MODE)" >&2
    exit 2
  else
    echo "ERROR: ${DEVICE:-this box} is in the TEST relay roster (source box $SOURCE_BOX + $PAINTER_BOX): pass --rig-mode test|event (or CAMERA_BOX_RIG_MODE)" >&2
    exit 2
  fi
fi
case "$RIG_MODE_ARG" in
  test | event) ;;
  *) echo "--rig-mode must be test or event (got '$RIG_MODE_ARG')" >&2; exit 2 ;;
esac
WANT_STATE="$(bkshading_relay_expected_enable_state "$DEVICE" "$RIG_MODE_ARG" "$SOURCE_BOX" "$PAINTER_BOX")"
if [ "$WANT_STATE" = unknown ]; then
  echo "WARNING: could not resolve the rig source box -- the relay is kept DISABLED (the passive TEST direction)" >&2
  WANT_STATE=disabled
fi

# The install body lives in scripts/lib/bkshading-relay-provision.sh (issue 808): setup-device.sh
# runs the SAME functions on every fresh cambox, so the standalone CLI and the provisioner can never
# drift. It never starts/restarts the relay; the enable-state follows the rig mode (WANT_STATE).
do_install() {
  echo "[bkshading-provision-relay] --install (never started; unit ${WANT_STATE} for ${DEVICE:-this box} in rig mode ${RIG_MODE_ARG})"
  bkshading_relay_provision_install "$WANT_STATE" || {
    echo "bkshading relay install FAILED (see the lines above)" >&2
    return 1
  }
  echo "install done. Verify with: scripts/bkshading-provision-relay.sh --check --rig-mode $RIG_MODE_ARG"
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
  # (5) the unit's enable-state matches the rig mode (reboot-survival; the live runtime state is the
  #     post-reboot rig verify).
  en="$("$SYSTEMCTL" is-enabled "$UNIT_NAME" 2>/dev/null || true)"
  if [ "$en" = "$WANT_STATE" ]; then
    echo "OK: unit $en (rig mode $RIG_MODE_ARG)"
  else
    echo "FAIL: unit is-enabled=${en:-<none>} but rig mode $RIG_MODE_ARG wants it $WANT_STATE" >&2
    rc=1
  fi

  if [ "$rc" -ne 0 ]; then
    cat >&2 <<MSG
bkshading relay NOT fully provisioned on this box. Fix:
  scripts/bkshading-provision-relay.sh --install [--rig-mode test|event]   # gphoto2 + unit + env; never started
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
