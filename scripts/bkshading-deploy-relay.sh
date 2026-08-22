#!/usr/bin/env bash
# scripts/bkshading-deploy-relay.sh — deploy the CI-built bkshading RELAY binary to a cambox (808 M3).
# Full header below `set -euo pipefail` (kept early for pre-write-script-check.sh).
set -euo pipefail

# ---------------------------------------------------------------------------------------------
# WHY: everything merged so far (M1 relay/service, M2 NDI preview, WS live-state push, relay
# provisioning) could NOT be verified on the live rig because CI produced NO deployable bkshading
# binary and there was no deploy tool. scripts/bkshading-provision-relay.sh installs the systemd
# unit + gphoto2 + env (enable-only) but EXPLICITLY leaves "the CI-built bkshading-relay binary
# (separate supervisor step)" dangling — its --check FAILS on a missing binary. This script is that
# step: it fetches the CI-built relay (the `bkshading-linux-amd64` artifact the `bkshading` CI job
# now uploads) and places it on a cambox at /usr/local/bin/bkshading-relay.
#
# It mirrors the PROVEN deploy idiom in scripts/deploy-fleet.sh: download ONE CI artifact from a
# committed/pushed ref (deploy-from-clean-tree.md — never a locally built binary), then the
# read-only-root swap cycle `remount,rw -> scp -> chmod +x -> sha256 byte-verify -> remount,ro`.
# Pure decisions (artifact name, relay bin name, the ENABLE-ONLY invariant, the sha-match verdict)
# live in scripts/lib/bkshading-deploy-runtime.sh so they are Tier-0 unit-testable without a rig.
#
# ENABLE-ONLY (.claude/rules/provisioning-scripts.md + bkshading.md): this NEVER start/restart/
# `enable --now`s the relay service — reboot (or the supervisor's post-reboot verify) brings it
# live, so a deploy can never light the relay up mid-event. USB / USB-Ethernet transports only —
# no wireless-pairing transport (owner hard rule). Per approval-scope.md the binary deploy + the ro-root remount it
# performs are the standing-approved WORK — this script does NOT ask permission and does NOT gate on
# "is it off-air"; the operator who runs it guards live timing. It does NOT reboot the host.
#
# Usage:  scripts/bkshading-deploy-relay.sh --host <ip> [--arch amd64|arm64] [--no-remount]
#                                           [--run <id> | --binary <path>] [--dry-run]
#   --host <ip>       (required) the cambox/SBC to deploy the relay to (e.g. 10.77.9.201).
#   --arch <a>        target arch of the CI artifact: `amd64` (default; cambox — the relay+service
#                     bkshading-linux-amd64 artifact) or `arm64` (SBC/handheld Pi Zero 2 W — the
#                     relay-only bkshading-relay-linux-arm64 artifact; issue 808 SBC milestone).
#   --no-remount      skip the read-only-root remount,rw/remount,ro swap. A camera-box appliance has
#                     a read-only root (default: remount); a stock Raspberry Pi OS SBC root is
#                     read-WRITE, so an SBC deploy passes --no-remount (remounting it ro is wrong).
#   --run <id>        pin a specific GitHub Actions ci.yml run id to download the artifact from.
#   --binary <path>   deploy an already-downloaded CI relay binary (skips gh download).
#   --dry-run         print the plan and touch nothing (no gh/ssh/scp).
#   -h | --help       show this header.
# With neither --run nor --binary, the latest successful ci.yml run on $BRANCH is used.
# SBC/handheld example: scripts/bkshading-deploy-relay.sh --host <pi> --arch arm64 --no-remount
#
# Env: SSH_PASS (default newlevel), REPO (default zbynekdrlik/camera-box), BRANCH (default main),
#      ARTIFACT (default from the lib). Overridable for Tier-0 tests (inject fakes):
#      BKSHADING_DEPLOY_GH, BKSHADING_DEPLOY_SSH, BKSHADING_DEPLOY_SCP, BKSHADING_DEPLOY_SSHPASS_PREFIX.
#
# Exit codes: 0 = relay deployed + byte-verified; 1 = a step failed / sha256 mismatch; 2 = bad args.
# After a successful deploy: run scripts/bkshading-provision-relay.sh --install (if not yet) on the
# box + reboot to bring the relay live (enable-only).
# ---------------------------------------------------------------------------------------------

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bkshading-deploy-runtime.sh
. "$HERE/lib/bkshading-deploy-runtime.sh"
# shellcheck source=scripts/lib/bkshading-relay-runtime.sh
. "$HERE/lib/bkshading-relay-runtime.sh" # bkshading_relay_bin_path() — the ONE relay install path

RELAY_DEST="$(bkshading_relay_bin_path)"     # /usr/local/bin/bkshading-relay (one source of truth)
SSH_PASS="${SSH_PASS:-newlevel}"
REPO="${REPO:-zbynekdrlik/camera-box}"
BRANCH="${BRANCH:-main}"
# An explicit ARTIFACT env override wins; otherwise it is derived from --arch AFTER arg parsing (the
# arch flag decides which CI artifact to fetch), so capture the override here and resolve below.
ARTIFACT_ENV="${ARTIFACT:-}"
ARCH="amd64"   # default: cambox (relay+service amd64 artifact); --arch arm64 = SBC/handheld relay
RO_ROOT=1      # default: read-only-root remount cycle (cambox); --no-remount = stock rw-root SBC

# Overridable command surfaces (real defaults; the test injects fakes + an empty sshpass prefix).
GH="${BKSHADING_DEPLOY_GH:-gh}"
SSH_BIN="${BKSHADING_DEPLOY_SSH:-ssh}"
SCP_BIN="${BKSHADING_DEPLOY_SCP:-scp}"
# sshpass prefix as an ARRAY (not a word-split string), so a password with whitespace is safe
# (deploy-fleet.sh quotes `-p "$SSH_PASS"`; a word-split string here would not). The env var, when
# SET (even empty — a test), replaces the prefix verbatim: empty -> the fakes run bare (no sshpass);
# unset -> wrap ssh/scp with the fleet password.
if [ -n "${BKSHADING_DEPLOY_SSHPASS_PREFIX+set}" ]; then
  read -r -a SSHPASS_PREFIX <<<"$BKSHADING_DEPLOY_SSHPASS_PREFIX"
else
  SSHPASS_PREFIX=(sshpass -p "$SSH_PASS")
fi

HOST=""
RUN_ID=""
BINARY=""
DRY_RUN=0

# require_val: a flag needs a following value; without one, fail with the bad-args exit 2 + a
# message (NOT a bare `shift 2` that aborts under set -e with exit 1 and no diagnostic).
require_val() { [ "$1" -ge 2 ] || { echo "ERROR: $2 requires a value (see --help)" >&2; exit 2; }; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --host) require_val "$#" --host; HOST="$2"; shift 2 ;;
    --arch) require_val "$#" --arch; ARCH="$2"; shift 2 ;;
    --no-remount) RO_ROOT=0; shift ;;
    --run) require_val "$#" --run; RUN_ID="$2"; shift 2 ;;
    --binary) require_val "$#" --binary; BINARY="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h | --help)
      grep -E '^# ' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown argument: $1 (see --help)" >&2; exit 2 ;;
  esac
done

if [ -z "$HOST" ]; then
  echo "ERROR: --host <ip> is required. Usage: bkshading-deploy-relay.sh --host <ip> [--arch amd64|arm64] [--no-remount] [--run <id> | --binary <path>] [--dry-run]" >&2
  exit 2
fi
if [ -n "$RUN_ID" ] && [ -n "$BINARY" ]; then
  echo "ERROR: --run and --binary are mutually exclusive" >&2
  exit 2
fi

# Validate the arch and derive the CI artifact from it (an explicit ARTIFACT env override wins).
case "$ARCH" in
  amd64 | arm64) ;;
  *) echo "ERROR: --arch must be amd64 or arm64 (got: $ARCH)" >&2; exit 2 ;;
esac
ARTIFACT="${ARTIFACT_ENV:-$(bkshading_deploy_artifact_name_for_arch "$ARCH")}"

# arm64 targets an SBC, and a stock Raspberry Pi OS root is read-WRITE — a ro-root remount on it is
# almost always wrong. --arch and --no-remount stay ORTHOGONAL (a deliberately read-only Pi image
# legitimately wants arm64 WITH the remount), so WARN rather than force — the operator keeps the
# choice. This removes the "forgot --no-remount" footgun without breaking the read-only-Pi case.
if [ "$ARCH" = "arm64" ] && [ "$RO_ROOT" = 1 ]; then
  echo "WARNING: --arch arm64 without --no-remount will remount the target root read-only after the" >&2
  echo "         deploy; a stock Raspberry Pi OS SBC has a read-WRITE root, so pass --no-remount" >&2
  echo "         unless this is a deliberately read-only Pi image." >&2
fi

# Conditional read-only-root swap (a cambox has a read-only root; a stock Pi OS SBC root is rw). No
# ssh remount call at all when --no-remount is set, so an SBC deploy never tries to remount its
# root ro (which would be wrong / fail-busy).
maybe_remount_rw() { [ "$RO_ROOT" = 1 ] || return 0; ssh_box "$1" "mount -o remount,rw /"; }
maybe_remount_ro() { [ "$RO_ROOT" = 1 ] || return 0; ssh_box "$1" "mount -o remount,ro / 2>/dev/null; true" || true; }

ssh_box() { "${SSHPASS_PREFIX[@]}" "$SSH_BIN" -o StrictHostKeyChecking=no -o ConnectTimeout=10 "root@$1" "$2"; }
scp_box() { "${SSHPASS_PREFIX[@]}" "$SCP_BIN" -o StrictHostKeyChecking=no "$2" "root@$1:$3"; }

# --- resolve the relay binary (a pre-downloaded --binary, or the CI artifact) ---
if [ -z "$BINARY" ]; then
  if [ -z "$RUN_ID" ]; then
    RUN_ID="$("$GH" run list --repo "$REPO" --branch "$BRANCH" --workflow ci.yml \
      --status success --limit 1 --json databaseId --jq '.[0].databaseId' 2>/dev/null || true)"
    [ -n "$RUN_ID" ] || { echo "ERROR: no successful ci.yml run found on $BRANCH" >&2; exit 1; }
  fi
  DIST="$(mktemp -d)"
  # Clean up the downloaded artifact dir on exit (mirrors deploy-fleet.sh's DIST trap).
  # shellcheck disable=SC2064  # expand DIST now so the trap has the concrete path.
  trap "rm -rf '$DIST'" EXIT
  echo "Downloading $ARTIFACT from ci.yml run $RUN_ID ($REPO) ..."
  "$GH" run download "$RUN_ID" --repo "$REPO" -n "$ARTIFACT" --dir "$DIST"
  BINARY="$DIST/$(bkshading_deploy_relay_artifact_bin)"
fi
[ -f "$BINARY" ] || { echo "ERROR: relay binary not found: $BINARY" >&2; exit 1; }
chmod +x "$BINARY" 2>/dev/null || true

LOCAL_SHA="$(sha256sum "$BINARY" | awk '{print $1}')"

# --- dry-run: print the plan, touch nothing ---
if [ "$DRY_RUN" -eq 1 ]; then
  if [ "$RO_ROOT" = 1 ]; then
    STEPS="mount -o remount,rw /  ->  scp  ->  chmod +x  ->  sha256 byte-verify  ->  mount -o remount,ro /"
    NEXT="on the box run scripts/bkshading-provision-relay.sh --install (if not yet) + reboot"
  else
    STEPS="scp  ->  chmod +x  ->  sha256 byte-verify   (no remount -- stock rw-root SBC, --no-remount)"
    NEXT="on the SBC run scripts/bkshading-provision-sbc.sh --install (if not yet) + reboot"
  fi
  cat <<PLAN
DRY-RUN — bkshading relay deploy plan:
  host           : $HOST
  arch           : $ARCH (artifact $ARTIFACT)
  source binary  : $BINARY (sha256 $LOCAL_SHA)
  deploy target  : root@$HOST:$RELAY_DEST
  steps          : $STEPS
  enable-only    : will NOT start/restart the service (reboot brings it live; provisioning-scripts.md)
  next step      : $NEXT
PLAN
  exit 0
fi

# --- real deploy: read-only-root swap cycle (mirrors deploy-fleet.sh) ---
if [ "${SSHPASS_PREFIX[0]:-}" = "sshpass" ]; then
  command -v sshpass >/dev/null 2>&1 || { echo "ERROR: sshpass required (apt-get install sshpass)" >&2; exit 1; }
fi

echo "[bkshading-deploy-relay] deploying $BINARY ($ARCH) -> root@$HOST:$RELAY_DEST"
if ! maybe_remount_rw "$HOST"; then
  echo "ERROR: remount rw / failed on $HOST" >&2; exit 1
fi
if ! scp_box "$HOST" "$BINARY" "$RELAY_DEST"; then
  echo "ERROR: scp of relay binary to $HOST failed" >&2
  maybe_remount_ro "$HOST"
  exit 1
fi
ssh_box "$HOST" "chmod +x $RELAY_DEST 2>/dev/null || true" || true

# Byte-verify (deploy-from-clean-tree.md Layer 3): a partial scp / stale same-name binary would
# otherwise pass unnoticed. Read the remote sha AND the exec bit BEFORE restoring ro (fresh file):
# scp with no `-p` creates a 0644 file on a FIRST deploy, so the chmod above is the ONLY thing making
# it executable — verify that too, else the unit's ExecStart would fail at reboot.
REMOTE_SHA="$(ssh_box "$HOST" "sha256sum $RELAY_DEST 2>/dev/null | awk '{print \$1}'" || echo "")"
REMOTE_EXEC="$(ssh_box "$HOST" "test -x $RELAY_DEST && echo yes || echo no" 2>/dev/null || echo no)"

# Always restore the ro root, whatever the verdict (a no-op under --no-remount for a rw-root SBC).
maybe_remount_ro "$HOST"

if [ "$(bkshading_deploy_sha_match "$LOCAL_SHA" "$REMOTE_SHA")" != "match" ]; then
  echo "ERROR: sha256 mismatch after deploy (local=$LOCAL_SHA remote=${REMOTE_SHA:-<none>}) — deploy NOT verified" >&2
  exit 1
fi
if [ "$REMOTE_EXEC" != "yes" ]; then
  echo "ERROR: $RELAY_DEST is not executable on $HOST after deploy — deploy NOT verified" >&2
  exit 1
fi
echo "OK: relay deployed + byte-verified (executable) on $HOST ($RELAY_DEST, sha256 $LOCAL_SHA)"

# ENABLE-ONLY: never start/restart the service here. The predicate is the single source of truth
# (always `no`); if a future change flips it, that is a RED test, not a silent live start.
if [ "$(bkshading_deploy_should_start)" = "yes" ]; then
  echo "WARNING: should_start=yes — refusing to start anyway (enable-only invariant)" >&2
else
  echo "enable-only: NOT starting the service. Run scripts/bkshading-provision-relay.sh --install"
  echo "             (if not yet provisioned) on the box + reboot to bring the relay live."
fi
