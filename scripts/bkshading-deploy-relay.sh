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
# RELAY STATE (issue 808, 25.9.2026): the deploy reads the relay's state first. An ACTIVE relay is
# STOPPED before the swap and started again after the ro remount (its PREVIOUS state is restored);
# a stopped relay (the TEST-mode default, issue 1311) stays stopped — the deploy never STARTS a relay
# that was not running and never `enable`s it (enable-state is setup-device.sh / rig-mode.sh's job).
# Swapping under a running relay left the replaced binary deleted-but-open, the final `remount,ro`
# failed "busy", and the old script swallowed it (cam6/cam7 root stayed read-WRITE); the ro remount
# now FAILS LOUD (non-zero, naming the holder via `lsof +L1` / `fuser -vm /`). USB / USB-Ethernet transports only —
# no wireless-pairing transport (owner hard rule). Per approval-scope.md the binary deploy + the ro-root remount it
# performs are the standing-approved WORK — this script does NOT ask permission and does NOT gate on
# "is it off-air"; the operator who runs it guards live timing. It does NOT reboot the host.
#
# Usage:  scripts/bkshading-deploy-relay.sh --host <ip> [--arch amd64|arm64] [--no-remount]
#                                           [--run <id> | --binary <path>] [--dry-run] [--force-live]
#   --host <ip>       (required) the cambox/SBC to deploy the relay to (e.g. 10.77.9.201).
#   --arch <a>        target arch of the CI artifact: `amd64` (default; cambox — the relay+service
#                     bkshading-linux-amd64 artifact) or `arm64` (SBC/handheld zero-class arm64 SBC —
#                     relay-only bkshading-relay-linux-arm64 artifact; issue 808 SBC milestone).
#   --no-remount      skip the read-only-root remount,rw/remount,ro swap. A camera-box appliance has
#                     a read-only root (default: remount); a stock arm64 SBC image (Raspberry Pi OS
#                     / Debian / Armbian) root is read-WRITE, so an SBC deploy passes --no-remount
#                     (remounting it ro is wrong).
#   --run <id>        pin a specific GitHub Actions ci.yml run id to download the artifact from.
#   --binary <path>   deploy an already-downloaded CI relay binary (skips gh download).
#   --dry-run         print the plan and touch no box (no ssh/scp; without --binary it still
#                     downloads the CI artifact read-only, to show its sha256).
#   --force-live      BYPASS the rig-busy guard and deploy even while a broadcast is live. Supervisor
#                     override ONLY, logged loudly — a relay deploy/restart during live production can
#                     fork-wedge the cambox (gphoto2 PTP on the shared xHCI bus, 2026-09-13 #1229).
#   -h | --help       show this header.
# With neither --run nor --binary, the newest SUCCESSFUL ci.yml run on $BRANCH that carries the
# artifact is used — the ONE shared resolver scripts/lib/ci-run-resolve.sh (deploy-fleet.sh uses it
# too), which logs the chosen run id + date + sha (issue 808: the old query picked a stale run).
# SBC/handheld example: scripts/bkshading-deploy-relay.sh --host <sbc> --arch arm64 --no-remount
#
# Env: SSH_PASS (default newlevel), REPO (default zbynekdrlik/camera-box), BRANCH (default main),
#      ARTIFACT (default from the lib), STRIH_HOST/STREAM_HOST (rig-busy OBS-WS hosts, default
#      10.77.9.202/.204), OBS_PASSWORD (default ""). Overridable for Tier-0 tests (inject fakes):
#      BKSHADING_DEPLOY_GH, BKSHADING_DEPLOY_SSH, BKSHADING_DEPLOY_SCP, BKSHADING_DEPLOY_SSHPASS_PREFIX,
#      BKSHADING_DEPLOY_OBS_PHASE2_DIR (dir holding obs_phase2.py for the rig-busy guard).
#
# Exit codes: 0 = relay deployed + byte-verified AND the root back read-only AND the relay back in
# its previous state; 1 = a step failed / sha256 mismatch / the ro remount failed (holder named) /
# the restore failed; 2 = bad args. A box never provisioned for the relay: run
# scripts/bkshading-provision-relay.sh --install on it (setup-device.sh does it on a fresh install).
# ---------------------------------------------------------------------------------------------

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bkshading-deploy-runtime.sh
. "$HERE/lib/bkshading-deploy-runtime.sh"
# shellcheck source=scripts/lib/bkshading-relay-runtime.sh
. "$HERE/lib/bkshading-relay-runtime.sh" # bkshading_relay_bin_path() — the ONE relay install path
# shellcheck source=scripts/lib/ci-run-resolve.sh
. "$HERE/lib/ci-run-resolve.sh" # ci_run_latest_success() — the ONE newest-successful-run resolver (shared with deploy-fleet.sh)
# shellcheck source=scripts/lib/stray-session-check.sh
. "$HERE/lib/stray-session-check.sh" # stray_session_check_assert() — the ONE shared rig-busy guard

RELAY_DEST="$(bkshading_relay_bin_path)"     # /usr/local/bin/bkshading-relay (one source of truth)
RELAY_UNIT="$(bkshading_relay_unit_name)"    # bkshading-relay.service (one source of truth)
RESTORE_ACTION=none                          # issue 808: `start` only when the relay was running before the swap
BOX_DIRTY=0                                  # issue 808: 1 from the relay stop / rw remount until finish_once ran
DIST=""                                      # the downloaded artifact dir (cleaned by the EXIT trap)
# Staging path for the ETXTBSY-safe swap: scp lands here (SAME dir → atomic rename), then `mv -f`
# replaces the (possibly RUNNING) relay inode. scp'ing directly onto a running exe fails ETXTBSY
# ("dest open: Failure", 2026-09-13 escalation). $$ is the local PID = a unique per-run stage name.
RELAY_STAGE="${RELAY_DEST}.deploy.$$"
# Rig-busy guard inputs (mirror scripts/rig-busy-gate.sh's own defaults). The guard reuses the
# shared obs_phase2.py rig-busy-check; RIG_BUSY_HERE names the dir holding obs_phase2.py (this
# script's own scripts/ dir), overridable so a Tier-0 test can point it at a fake.
STRIH_HOST="${STRIH_HOST:-10.77.9.202}"
STREAM_HOST="${STREAM_HOST:-10.77.9.204}"
OBS_PASSWORD="${OBS_PASSWORD:-}"
RIG_BUSY_HERE="${BKSHADING_DEPLOY_OBS_PHASE2_DIR:-$HERE}"
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
FORCE_LIVE=0   # --force-live: bypass the rig-busy guard (supervisor-only, logged loudly)

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
    --force-live) FORCE_LIVE=1; shift ;;
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

# arm64 targets an SBC, and a stock arm64 SBC image (Raspberry Pi OS / Debian / Armbian) root is
# read-WRITE — a ro-root remount on it is almost always wrong. --arch and --no-remount stay
# ORTHOGONAL (a deliberately read-only SBC image legitimately wants arm64 WITH the remount), so WARN
# rather than force — the operator keeps the choice. This removes the "forgot --no-remount" footgun
# without breaking the read-only-image case.
if [ "$ARCH" = "arm64" ] && [ "$RO_ROOT" = 1 ]; then
  echo "WARNING: --arch arm64 without --no-remount will remount the target root read-only after the" >&2
  echo "         deploy; a stock arm64 SBC image has a read-WRITE root, so pass --no-remount" >&2
  echo "         unless this is a deliberately read-only SBC image." >&2
fi

# Conditional read-only-root swap (a cambox has a read-only root; a stock arm64 SBC root is rw). No
# ssh remount call at all when --no-remount is set, so an SBC deploy never tries to remount its
# root ro (which would be wrong / fail-busy).
maybe_remount_rw() { [ "$RO_ROOT" = 1 ] || return 0; ssh_box "$1" "mount -o remount,rw /"; }

# RESTORE_SESSION is empty on the normal path and `setsid -w` inside the EXIT trap: the trap's restore
# then runs in its own session, so a second Ctrl-C from the terminal cannot reach it (sshpass forwards
# SIGINT to its ssh child even when this shell ignores it).
RESTORE_SESSION=()
ssh_box() { "${RESTORE_SESSION[@]}" "${SSHPASS_PREFIX[@]}" "$SSH_BIN" -o StrictHostKeyChecking=no -o ConnectTimeout=10 -o ServerAliveInterval=10 -o ServerAliveCountMax=3 "root@$1" "$2"; }
scp_box() { "${SSHPASS_PREFIX[@]}" "$SCP_BIN" -o StrictHostKeyChecking=no -o ConnectTimeout=10 -o ServerAliveInterval=10 -o ServerAliveCountMax=3 "$2" "root@$1:$3"; }

# issue 808 (cam6/cam7, 25.9.2026): the ro remount used to be `mount -o remount,ro / 2>/dev/null;
# true || true` -- a "busy" failure was SWALLOWED, the script printed OK and the box root stayed
# read-WRITE. Now it is retried (a just-stopped relay can take a moment to release its files) and a
# final failure FAILS LOUD: it names the holder (`lsof +L1` = deleted-but-still-open files, the real
# blocker; `fuser -vm /` for the full picture) and returns non-zero, so the caller exits non-zero.
remount_ro_checked() {
  [ "$RO_ROOT" = 1 ] || return 0
  local rc=0 fu lo holders
  ssh_box "$1" "for _i in 1 2 3; do mount -o remount,ro / && exit 0; sleep 2; done; exit 1" || rc=$?
  [ "$rc" -eq 0 ] && return 0
  if [ "$rc" -ne 1 ]; then
    # The remote loop exits only 0 or 1, so any other rc is the transport (ssh 255, sshpass 5/6).
    echo "ERROR: ssh to $1 FAILED during the ro remount (rc $rc) -- the root may still be read-WRITE;" >&2
    echo "       reach the box and check 'findmnt -no OPTIONS /' (it must say ro) by hand." >&2
    return 1
  fi
  fu="$(ssh_box "$1" "fuser -vm / 2>&1 | head -n 40" 2>/dev/null || true)"
  lo="$(ssh_box "$1" "$(bkshading_deploy_ro_holder_probe_cmd)" 2>/dev/null || true)"
  holders="$(bkshading_deploy_ro_holders "$lo")"
  echo "ERROR: mount -o remount,ro / FAILED on $1 -- the box root stays read-WRITE." >&2
  echo "       holder(s) of deleted-but-open files (lsof +L1 / /proc fd scan): ${holders:-<none reported>}" >&2
  echo "       fuser -vm / on $1:" >&2
  printf '%s\n' "${fu:-<no output>}" | sed 's/^/         /' >&2
  echo "       Fix: stop that holder, then run 'mount -o remount,ro /' on $1 (never leave a cambox root rw)." >&2
  return 1
}

# Restore the relay's PREVIOUS state after the swap: start it again ONLY when it was active before
# (bkshading_deploy_restore_action). A relay that was stopped (TEST mode, issue 1311) stays stopped.
restore_relay() {
  [ "$RESTORE_ACTION" = start ] || return 0
  if ssh_box "$1" "systemctl start $RELAY_UNIT"; then
    echo "restored: $RELAY_UNIT started again on $1 (it was active before the swap)"
  else
    echo "ERROR: could not start $RELAY_UNIT again on $1 (it was active before the swap) -- start it by hand" >&2
    return 1
  fi
}

# finish_box HOST -> the ro remount (checked) THEN the relay restore; non-zero when either failed.
finish_box() {
  local rc=0
  remount_ro_checked "$1" || rc=1
  restore_relay "$1" || rc=1
  return "$rc"
}

# finish_once -> finish_box for $HOST exactly once, and only after the box was touched (BOX_DIRTY=1
# from the relay stop / rw remount on). Every explicit exit path calls it, and the EXIT trap below
# calls it too, so a Ctrl-C / SIGTERM mid-scp still restores the ro root and the relay's state.
# BOX_DIRTY clears only AFTER finish_box returned; FINISHING marks a restore in progress, so a signal
# landing INSIDE the restore is reported loudly by the EXIT trap instead of being mistaken for "done".
FINISHING=0
finish_once() {
  local rc=0
  [ "$BOX_DIRTY" = 1 ] || return 0
  [ "$FINISHING" = 0 ] || return 1
  FINISHING=1
  finish_box "$HOST" || rc=1
  BOX_DIRTY=0
  FINISHING=0
  return "$rc"
}

# The EXIT trap must survive a dead terminal (a HUP'd ssh session: every write to stderr fails) and a
# second signal: errexit off, signals ignored, every message write guarded.
deploy_on_exit() {
  local rc=$?
  set +e
  trap '' INT TERM HUP PIPE
  command -v setsid >/dev/null 2>&1 && RESTORE_SESSION=(setsid -w)
  if [ "$FINISHING" = 1 ]; then
    # The restore itself was interrupted. finish_box is safe to repeat (remount,ro on an ro root and
    # start on an active unit are no-ops), so run it again -- now signal-proof -- and only ask for a
    # hand check if that fails too.
    echo "WARNING: the ro-root/relay restore on $HOST was interrupted -- running it again" >&2 2>/dev/null
    FINISHING=0
    if ! finish_once; then
      echo "ERROR: the ro-root/relay restore on $HOST FAILED after an interruption -- check by hand: 'findmnt -no OPTIONS /' must say ro, 'systemctl is-active $RELAY_UNIT' must match its state before the deploy (${WAS_ACTIVE:-unknown})" >&2 2>/dev/null
    fi
  elif [ "$BOX_DIRTY" = 1 ]; then
    echo "ERROR: deploy interrupted/aborted after the box was touched -- restoring the ro root + the relay state" >&2 2>/dev/null
    finish_once
  fi
  [ -n "$DIST" ] && rm -rf "$DIST"
  return "$rc"
}
trap deploy_on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

# --- resolve the relay binary (a pre-downloaded --binary, or the CI artifact) ---
if [ -z "$BINARY" ]; then
  if [ -z "$RUN_ID" ]; then
    # issue 808: the ONE shared resolver (deploy-fleet.sh uses it too) -- the newest SUCCESSFUL
    # ci.yml run by createdAt that carries $ARTIFACT, decided client-side and logged. The old
    # server-side-filtered one-shot query picked a 3-week-old run on 25.9.2026.
    RUN_ID="$(CI_RUN_RESOLVE_GH="$GH" ci_run_latest_success "$REPO" "$BRANCH" ci.yml "$ARTIFACT")" || RUN_ID=""
    [ -n "$RUN_ID" ] || { echo "ERROR: no successful ci.yml run on $BRANCH carries $ARTIFACT" >&2; exit 1; }
  fi
  DIST="$(mktemp -d)"   # removed by the deploy_on_exit trap (mirrors deploy-fleet.sh's DIST cleanup)
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
    STEPS="rig-busy guard  ->  stop the relay if active  ->  mount -o remount,rw /  ->  scp (staged $RELAY_STAGE)  ->  chmod +x + atomic mv  ->  sha256 byte-verify  ->  mount -o remount,ro / (FAIL LOUD naming the holder if busy)  ->  start the relay again if it was active"
    NEXT="on the box run scripts/bkshading-provision-relay.sh --install (if not yet) + reboot"
  else
    STEPS="rig-busy guard  ->  stop the relay if active  ->  scp (staged $RELAY_STAGE)  ->  chmod +x + atomic mv  ->  sha256 byte-verify  ->  start the relay again if it was active   (no remount -- stock rw-root SBC, --no-remount)"
    NEXT="on the SBC run scripts/bkshading-provision-sbc.sh --install (if not yet) + reboot"
  fi
  cat <<PLAN
DRY-RUN — bkshading relay deploy plan:
  host           : $HOST
  arch           : $ARCH (artifact $ARTIFACT)
  source binary  : $BINARY (sha256 $LOCAL_SHA)
  deploy target  : root@$HOST:$RELAY_DEST
  steps          : $STEPS
  relay state    : an ACTIVE relay is stopped for the swap and started again after; a stopped relay stays stopped (never started)
  next step      : $NEXT
PLAN
  exit 0
fi

# --- rig-busy PREFLIGHT (2026-09-13 escalation): NEVER deploy a cambox relay while a broadcast may
# be LIVE. A relay deploy DURING live production fork-wedged cam1 (gphoto2 PTP contention with the
# grabber on the single shared xHCI controller cascades into D-state gphoto2 + fork-exhaustion).
# Reuse the SAME shared rig-busy guard recording-e2e.sh uses (obs_phase2.py rig-busy-check via
# stray_session_check_assert) — refuse on a busy rig unless --force-live (supervisor-only, logged
# loudly); the guard fail-OPENs (WARN + proceed) only when NO box is readable, never on a transient.
if [ "$FORCE_LIVE" = 1 ]; then
  echo "WARNING: --force-live — BYPASSING the rig-busy guard for the bkshading relay deploy to $HOST." >&2
  echo "         A relay deploy/restart during a LIVE broadcast can fork-wedge the cambox (2026-09-13 escalation); supervisor override only." >&2
else
  stray_session_check_assert "$RIG_BUSY_HERE" "$STRIH_HOST" "$STREAM_HOST" "the bkshading relay deploy to $HOST"
fi

# --- real deploy: read-only-root swap cycle (mirrors deploy-fleet.sh) ---
if [ "${SSHPASS_PREFIX[0]:-}" = "sshpass" ]; then
  command -v sshpass >/dev/null 2>&1 || { echo "ERROR: sshpass required (apt-get install sshpass)" >&2; exit 1; }
fi

echo "[bkshading-deploy-relay] deploying $BINARY ($ARCH) -> root@$HOST:$RELAY_DEST (staged via $RELAY_STAGE, atomic mv)"

# issue 808 (cam6/cam7, 25.9.2026): swapping the binary under a RUNNING relay left that process
# holding the replaced, deleted-but-open inode, so the final `remount,ro` failed "busy" and the root
# stayed read-WRITE. So: read the relay's state FIRST, STOP it when it is active (the rig-busy guard
# above has already refused a live broadcast), swap, restore ro, and only then start it again --
# restoring the PREVIOUS state, never starting a relay that was stopped (bkshading_deploy_restore_action).
WAS_RC=0
WAS_ACTIVE="$(ssh_box "$HOST" "systemctl is-active $RELAY_UNIT 2>/dev/null || true" 2>/dev/null)" || WAS_RC=$?
WAS_ACTIVE="$(printf '%s' "$WAS_ACTIVE" | tr -d '[:space:]')"
RESTORE_ACTION="$(bkshading_deploy_restore_action "$WAS_ACTIVE")"
if [ "$WAS_RC" -ne 0 ] || [ "$RESTORE_ACTION" = unreadable ]; then
  # Never guess "not running" from a failed read: the relay could be live on the old inode.
  echo "ERROR: could not read $RELAY_UNIT's state on $HOST (ssh rc=$WAS_RC, is-active='${WAS_ACTIVE}') -- nothing changed on the box" >&2
  exit 1
fi
echo "relay state before the swap: $WAS_ACTIVE -> after the swap: $([ "$RESTORE_ACTION" = start ] && echo 'start it again' || echo 'leave it stopped')"
BOX_DIRTY=1
if [ "$RESTORE_ACTION" = start ]; then
  if ! ssh_box "$HOST" "systemctl stop $RELAY_UNIT"; then
    echo "ERROR: could not stop $RELAY_UNIT on $HOST before the swap -- restoring its state" >&2
    finish_once || true
    exit 1
  fi
fi

if ! maybe_remount_rw "$HOST"; then
  echo "ERROR: remount rw / failed on $HOST" >&2
  finish_once || true
  exit 1
fi
# Stage to a temp path in the SAME directory, then atomic `mv -f` over the relay binary. scp'ing
# directly onto a running executable fails ETXTBSY ("dest open: Failure", 2026-09-13); the stage +
# rename(2) also keeps a half-copied file from ever sitting at the real path. On any failure, clean
# up the stage file, restore the ro root AND the relay's previous state (finish_once).
if ! scp_box "$HOST" "$BINARY" "$RELAY_STAGE"; then
  echo "ERROR: scp of relay binary to $HOST failed" >&2
  ssh_box "$HOST" "rm -f $RELAY_STAGE 2>/dev/null || true" || true
  finish_once || true
  exit 1
fi
if ! ssh_box "$HOST" "chmod +x $RELAY_STAGE && mv -f $RELAY_STAGE $RELAY_DEST"; then
  echo "ERROR: staging chmod + atomic mv of the relay binary failed on $HOST" >&2
  ssh_box "$HOST" "rm -f $RELAY_STAGE 2>/dev/null || true" || true
  finish_once || true
  exit 1
fi

# Byte-verify (deploy-from-clean-tree.md Layer 3): a partial scp / stale same-name binary would
# otherwise pass unnoticed. Read the remote sha AND the exec bit BEFORE restoring ro (fresh file):
# scp with no `-p` creates a 0644 file on a FIRST deploy, so the chmod above is the ONLY thing making
# it executable — verify that too, else the unit's ExecStart would fail at reboot.
REMOTE_SHA="$(ssh_box "$HOST" "sha256sum $RELAY_DEST 2>/dev/null | awk '{print \$1}'" || echo "")"
REMOTE_EXEC="$(ssh_box "$HOST" "test -x $RELAY_DEST && echo yes || echo no" 2>/dev/null || echo no)"
VERIFIED=1
if [ "$(bkshading_deploy_sha_match "$LOCAL_SHA" "$REMOTE_SHA")" != "match" ]; then
  echo "ERROR: sha256 mismatch after deploy (local=$LOCAL_SHA remote=${REMOTE_SHA:-<none>}) — deploy NOT verified" >&2
  VERIFIED=0
elif [ "$REMOTE_EXEC" != "yes" ]; then
  echo "ERROR: $RELAY_DEST is not executable on $HOST after deploy — deploy NOT verified" >&2
  VERIFIED=0
fi
# A binary that failed the byte-verify is never started: the relay is left STOPPED (loudly) rather
# than run on unverified bytes -- redeploy, then start it.
if [ "$VERIFIED" = 0 ] && [ "$RESTORE_ACTION" = start ]; then
  echo "ERROR: leaving $RELAY_UNIT STOPPED on $HOST -- the swapped binary is not verified; redeploy, then start it" >&2
  RESTORE_ACTION=none
fi

# The ro remount (checked, FAIL LOUD naming the holder) + the relay restore -- on every verdict.
FINISH_RC=0
finish_once || FINISH_RC=1
[ "$VERIFIED" = 1 ] || exit 1
if [ "$FINISH_RC" -ne 0 ]; then
  echo "ERROR: relay binary swapped + byte-verified on $HOST, but the box was NOT left clean (see above) — deploy FAILED" >&2
  exit 1
fi
echo "OK: relay deployed + byte-verified (executable) on $HOST ($RELAY_DEST, sha256 $LOCAL_SHA)$([ "$RO_ROOT" = 1 ] && echo ', root back to read-only')"

# The deploy never STARTS a relay that was not running: the predicate is the single source of truth
# (always `no`); if a future change flips it, that is a RED test, not a silent live start. An active
# relay was restored above (its previous state), which is not a start of a stopped service.
if [ "$(bkshading_deploy_should_start)" = "yes" ]; then
  echo "WARNING: should_start=yes — refusing to start a stopped relay anyway (enable-only invariant)" >&2
elif [ "$RESTORE_ACTION" != start ]; then
  echo "relay was not running: left stopped. Run scripts/bkshading-provision-relay.sh --install"
  echo "             (if not yet provisioned) on the box; it comes up at boot / rig-mode.sh event."
fi
