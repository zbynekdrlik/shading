#!/usr/bin/env bash
# scripts/bkshading-deploy-service.sh — deploy the CI-built bkshading SERVICE to strih (see header below).
set -euo pipefail
# ---------------------------------------------------------------------------------------------
# Deploy the CI-built bkshading SERVICE (bkshading.exe) to the strih Windows PC + install its
# persistent Task Scheduler keep-alive task (issue 808 service-deploy sub-step).
#
# WHY: everything merged (M1 service, M2 NDI preview, WS push, cloudflared remote) ships to the strih
# PC, and the `bkshading-windows` CI job already release-builds + uploads the deployable binary as the
# `bkshading-windows-amd64` artifact (target/release/bkshading.exe). But NO tool consumed it — the
# service reached strih only by a MANUAL stage (C:\stage-bkshading, issue #1157).
# `bkshading-deploy-relay.sh` deploys only the RELAY to a Linux CAMBOX (a different binary/OS/
# lifecycle). This is the missing repeatable path: CI artifact -> strih staging -> config -> a
# persistent Task Scheduler keep-alive task -> verify :8770.
#
# Ships to the box + registers (the values themselves come from the lib — this note documents them
# so the .sh header, the lib, and the installer ps1 stay legible together): the service exe
# (bkshading.exe), the config seed (bkshading.example.toml -> bkshading.toml on the box, seeded ONLY
# IF absent), and the installer (bkshading-install-service.ps1); the installer registers the
# Task Scheduler keep-alive task `bkshading-service` under C:\bkshading and verifies port 8770.
#
# Transport = the strih-recordings-retention.sh emission style: `scp -O` the payload, then run the
# installer via `powershell -NoProfile -ExecutionPolicy Bypass -File <remote.ps1>` — NEVER a nested
# `powershell -Command` over ssh (which fails silently on this rig, .claude/rules/rig-state-inspection.md).
# Pure invariants (artifact/exe/install-dir/config/task-name/port/keepalive) live in
# scripts/lib/bkshading-deploy-service-runtime.sh so the CI upload, this orchestrator, and the on-box
# installer cannot drift.
#
# DRY-RUN by default (prints the plan, touches NOTHING remote — no gh/ssh/scp). --execute performs the
# real deploy. Per approval-scope.md a deploy + its service start are the standing-approved WORK; this
# script does NOT gate on "is it off-air" (the operator who runs --execute guards live timing). It
# never reboots the host. The LIVE --execute run against strih is the supervisor's rig step.
#
# Usage:  scripts/bkshading-deploy-service.sh [--host <ip>] [--user <name>]
#             [--binary <bkshading.exe> | --run <ci.yml run id>] [--config-seed <path>]
#             [--install-dir <C:\dir>] [--keepalive-minutes <N>] [--execute]
#   --host <ip>            strih PC (default 10.77.9.202).
#   --user <name>          ssh user (default newlevel); also the C:\Users\<name> staging home.
#   --binary <path>        deploy an already-downloaded CI service exe (skips gh download).
#   --run <id>             pin a ci.yml run id to download the bkshading-windows-amd64 artifact from.
#   --config-seed <path>   the config seed shipped as bkshading.example.toml (default: the repo's
#                          bkshading/service/bkshading.example.toml). Seeded on the box ONLY IF the
#                          operator config is absent — a redeploy never clobbers a tuned config.
#   --install-dir <dir>    on-box stable install dir (default from the lib, C:\bkshading).
#   --keepalive-minutes N  keep-alive task repetition cadence (default from the lib).
#   --execute              perform the real deploy (scp + run installer). Default = DRY-RUN.
#   -h | --help            show this header.
# With neither --run nor --binary, the latest successful ci.yml run on $BRANCH is used (--execute only).
#
# Env: STRIH_SSH_PW (default newlevel), REPO (default zbynekdrlik/camera-box), BRANCH (default main).
#      Test overrides (inject fakes): BKSHADING_SVC_GH, BKSHADING_SVC_SSH, BKSHADING_SVC_SCP,
#      BKSHADING_SVC_SSHPASS_PREFIX.
#
# Exit codes: 0 = ok (plan printed, or deploy done); 1 = a step failed; 2 = bad args.
# After a successful --execute the installer has registered the keep-alive task, started the service,
# and verified :8770 Listening — open http://<host>:8770/ to confirm the panel.
# ---------------------------------------------------------------------------------------------

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bkshading-deploy-service-runtime.sh
. "$HERE/lib/bkshading-deploy-service-runtime.sh"

ARTIFACT="${ARTIFACT:-$(bkshading_service_artifact_name)}"
EXE_NAME="$(bkshading_service_exe_name)"
INSTALL_DIR="$(bkshading_service_install_dir)"
INSTALLER_PS1="$(bkshading_service_installer_ps1_name)"
CONFIG_EXAMPLE="$(bkshading_service_config_example_name)"
TASK_NAME="$(bkshading_service_task_name)"
PORT="$(bkshading_service_port)"
KEEPALIVE_MIN="$(bkshading_service_keepalive_minutes)"

REPO="${REPO:-zbynekdrlik/camera-box}"
BRANCH="${BRANCH:-main}"
HOST="10.77.9.202"                 # strih
USER_NAME="newlevel"
SSH_PASS="${STRIH_SSH_PW:-newlevel}"
RUN_ID=""
BINARY=""
CONFIG_SEED="$HERE/../bkshading/service/bkshading.example.toml"
EXECUTE=0

# Overridable command surfaces (real defaults; the test injects fakes + an empty sshpass prefix).
GH="${BKSHADING_SVC_GH:-gh}"
SSH_BIN="${BKSHADING_SVC_SSH:-ssh}"
SCP_BIN="${BKSHADING_SVC_SCP:-scp}"
if [ -n "${BKSHADING_SVC_SSHPASS_PREFIX+set}" ]; then
  read -r -a SSHPASS_PREFIX <<<"$BKSHADING_SVC_SSHPASS_PREFIX"
else
  SSHPASS_PREFIX=(sshpass -p "$SSH_PASS")
fi

require_val() { [ "$1" -ge 2 ] || { echo "ERROR: $2 requires a value (see --help)" >&2; exit 2; }; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --host) require_val "$#" --host; HOST="$2"; shift 2 ;;
    --user) require_val "$#" --user; USER_NAME="$2"; shift 2 ;;
    --binary) require_val "$#" --binary; BINARY="$2"; shift 2 ;;
    --run) require_val "$#" --run; RUN_ID="$2"; shift 2 ;;
    --config-seed) require_val "$#" --config-seed; CONFIG_SEED="$2"; shift 2 ;;
    --install-dir) require_val "$#" --install-dir; INSTALL_DIR="$2"; shift 2 ;;
    --keepalive-minutes) require_val "$#" --keepalive-minutes; KEEPALIVE_MIN="$2"; shift 2 ;;
    --execute) EXECUTE=1; shift ;;
    -h | --help)
      grep -E '^# ' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown argument: $1 (see --help)" >&2; exit 2 ;;
  esac
done

if [ -n "$RUN_ID" ] && [ -n "$BINARY" ]; then
  echo "ERROR: --run and --binary are mutually exclusive" >&2
  exit 2
fi

# Remote (Windows) staging home + the three payload paths. The installer is scp'd next to the exe +
# config seed, so its $PSCommandPath parent (= the staging dir) is exactly where it reads them from.
STAGE_DIR='C:\Users\'"$USER_NAME"
REMOTE_EXE="$STAGE_DIR"'\'"$EXE_NAME"
REMOTE_SEED="$STAGE_DIR"'\'"$CONFIG_EXAMPLE"
REMOTE_PS1="$STAGE_DIR"'\'"$INSTALLER_PS1"
LOCAL_PS1="$HERE/$INSTALLER_PS1"

ssh_box() {
  "${SSHPASS_PREFIX[@]}" "$SSH_BIN" -o StrictHostKeyChecking=no -o ConnectTimeout=15 \
    "${USER_NAME}@${HOST}" "$1"
}
scp_box() {  # $1 = local src, $2 = remote dest path
  "${SSHPASS_PREFIX[@]}" "$SCP_BIN" -O -o StrictHostKeyChecking=no "$1" "${USER_NAME}@${HOST}:$2"
}

# The exact installer invocation run on the box (also printed in the DRY-RUN plan, so the plan and
# the real run cannot diverge). `powershell -File`, NEVER a nested `powershell -Command`.
installer_cmd() {  # $1 = "-Execute" or ""
  printf '%s' "powershell -NoProfile -ExecutionPolicy Bypass -File \"${REMOTE_PS1}\"" \
    " -InstallDir \"${INSTALL_DIR}\" -Port ${PORT} -TaskName ${TASK_NAME}" \
    " -KeepAliveMinutes ${KEEPALIVE_MIN} $1"
}

# --- DRY-RUN: print the plan, touch NOTHING remote ---
if [ "$EXECUTE" -eq 0 ]; then
  BIN_DESC="$BINARY"
  [ -n "$BIN_DESC" ] || BIN_DESC="(will download $ARTIFACT from ci.yml run ${RUN_ID:-<latest success on $BRANCH>})"
  cat <<PLAN
DRY-RUN — bkshading service deploy plan (touches nothing; re-run with --execute to deploy):
  host           : $HOST  (user $USER_NAME)
  service binary : $BIN_DESC
  config seed    : $CONFIG_SEED  (-> ${CONFIG_EXAMPLE} on the box; seeds ${INSTALL_DIR}\\$(bkshading_service_config_name) ONLY IF absent)
  installer ps1  : $LOCAL_PS1
  staging dir    : ${USER_NAME}@${HOST}:${STAGE_DIR}
  install dir    : $INSTALL_DIR
  task           : $TASK_NAME  (Task Scheduler keep-alive, AtLogOn + every ${KEEPALIVE_MIN} min)
  verify         : port $PORT Listening after start
  installer run  : $(installer_cmd -Execute)
  steps          : scp exe + config seed + installer ps1 -> $STAGE_DIR  ->  run installer (-Execute)
PLAN
  exit 0
fi

# --- real deploy (--execute) ---
if [ "${SSHPASS_PREFIX[0]:-}" = "sshpass" ]; then
  command -v sshpass >/dev/null 2>&1 || { echo "ERROR: sshpass required (apt-get install sshpass)" >&2; exit 1; }
fi

# Resolve the service exe (a pre-downloaded --binary, or the CI artifact).
if [ -z "$BINARY" ]; then
  if [ -z "$RUN_ID" ]; then
    RUN_ID="$("$GH" run list --repo "$REPO" --branch "$BRANCH" --workflow ci.yml \
      --status success --limit 1 --json databaseId --jq '.[0].databaseId' 2>/dev/null || true)"
    [ -n "$RUN_ID" ] || { echo "ERROR: no successful ci.yml run found on $BRANCH" >&2; exit 1; }
  fi
  DIST="$(mktemp -d)"
  # shellcheck disable=SC2064  # expand DIST now so the trap has the concrete path.
  trap "rm -rf '$DIST'" EXIT
  echo "Downloading $ARTIFACT from ci.yml run $RUN_ID ($REPO) ..."
  "$GH" run download "$RUN_ID" --repo "$REPO" -n "$ARTIFACT" --dir "$DIST"
  BINARY="$DIST/$EXE_NAME"
fi
[ -f "$BINARY" ] || { echo "ERROR: service binary not found: $BINARY" >&2; exit 1; }
[ -f "$CONFIG_SEED" ] || { echo "ERROR: config seed not found: $CONFIG_SEED" >&2; exit 1; }
[ -f "$LOCAL_PS1" ] || { echo "ERROR: installer ps1 not found: $LOCAL_PS1" >&2; exit 1; }

echo "[bkshading-deploy-service] deploying $BINARY -> ${USER_NAME}@${HOST}:${REMOTE_EXE}"
if ! scp_box "$BINARY" "$REMOTE_EXE"; then
  echo "ERROR: scp of the service exe to $HOST failed" >&2; exit 1
fi
if ! scp_box "$CONFIG_SEED" "$REMOTE_SEED"; then
  echo "ERROR: scp of the config seed to $HOST failed" >&2; exit 1
fi
if ! scp_box "$LOCAL_PS1" "$REMOTE_PS1"; then
  echo "ERROR: scp of the installer ps1 to $HOST failed" >&2; exit 1
fi

# Byte-verify the staged exe (deploy-from-clean-tree.md Layer 3, mirroring the relay sibling): a
# truncated / interrupted scp would otherwise pass unnoticed until the service fails to launch. Read
# the box's sha256 via certutil (cmd.exe builtin; line 2 is the hash) and compare to the local sha.
LOCAL_SHA="$(sha256sum "$BINARY" | awk '{print $1}')"
REMOTE_SHA="$(ssh_box "certutil -hashfile \"$REMOTE_EXE\" SHA256" 2>/dev/null | sed -n 2p | tr -d '[:space:]\r' | tr 'A-F' 'a-f' || echo "")"
if [ "$(bkshading_service_sha_match "$LOCAL_SHA" "$REMOTE_SHA")" != "match" ]; then
  echo "ERROR: sha256 mismatch after scp (local=$LOCAL_SHA remote=${REMOTE_SHA:-<none>}) -- deploy NOT verified" >&2
  exit 1
fi
echo "[bkshading-deploy-service] byte-verified staged exe on $HOST (sha256 $LOCAL_SHA)"

echo "[bkshading-deploy-service] running installer on $HOST (-Execute) ..."
if ! ssh_box "$(installer_cmd -Execute)"; then
  echo "ERROR: the on-box installer failed on $HOST — see its output above" >&2; exit 1
fi
echo "OK: bkshading service deployed + installed on $HOST (task $TASK_NAME, panel http://$HOST:$PORT/)."
