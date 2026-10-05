#!/usr/bin/env bash
# scripts/lib/bkshading-deploy-runtime.sh — pure decisions for deploying the CI-built bkshading
# RELAY binary to a cambox/SBC (issue 808 M3; unblocks the live rig verify of everything merged).
#
# The bkshading CI `bkshading` job release-builds + uploads the relay/service binaries as the
# `bkshading-linux-amd64` artifact; this lib is the single source of truth for that artifact NAME +
# the relay bin filename inside it + the ENABLE-ONLY invariant + the byte-verify decision, consumed
# by both scripts/bkshading-deploy-relay.sh and the python cross-check test so the CI upload, the
# deploy script, and this helper cannot silently drift.
#
# Source-only: defines pure functions, performs NO side effects, and deliberately does NOT
# `set -euo pipefail` (that would leak into the sourcing shell — the sourced-harness set-e leak in
# .claude/rules/ci-testing-gotchas.md). Mirrors the pure-decision-in-lib split of
# scripts/lib/frame-probe-deploy.sh + scripts/lib/bkshading-relay-runtime.sh.
# airuleset:script-ok source-only lib — set -euo pipefail would leak into the sourcing shell (ci-testing-gotchas)

# shellcheck source=scripts/lib/ro-root.sh
. "$(dirname "${BASH_SOURCE[0]}")/ro-root.sh"  # ro_root_mount_mode -- the ONE first-token root reading (issue 808)

# The CI artifact name the `bkshading` job uploads the relay/service into (KEEP IN SYNC with
# .github/workflows/ci.yml `Upload bkshading binaries`; the python test cross-checks both).
bkshading_deploy_artifact_name() { printf '%s\n' bkshading-linux-amd64; }

# The aarch64 relay-only artifact the `bkshading` job cross-builds + uploads for the SBC/handheld
# (issue 808 SBC milestone; a zero-class arm64 SBC is ARM and cannot run the amd64 binary). ONE source of
# truth for the arm64 artifact name — the CI upload, the deploy `--arch arm64` path, and the python
# cross-check all read this. Relay-ONLY (the service runs on the strih PC, never on a handheld SBC),
# hence the distinct `bkshading-relay-linux-arm64` name vs the amd64 `bkshading-linux-amd64` (relay
# + service).
bkshading_deploy_arm64_artifact_name() { printf '%s\n' bkshading-relay-linux-arm64; }

# Select the artifact name for a target arch. `amd64` (default) -> the relay+service amd64 artifact
# (cambox deploy, unchanged); `arm64` -> the relay-only aarch64 artifact (SBC/handheld deploy). An
# unknown arch echoes nothing (the caller validates + fails loud).
bkshading_deploy_artifact_name_for_arch() {
  case "${1:-amd64}" in
    amd64) bkshading_deploy_artifact_name ;;
    arm64) bkshading_deploy_arm64_artifact_name ;;
    *) : ;;
  esac
}

# The relay binary file name INSIDE that artifact (matches bkshading/relay's [[bin]] name).
bkshading_deploy_relay_artifact_bin() { printf '%s\n' bkshading-relay; }

# ENABLE-ONLY discipline (.claude/rules/provisioning-scripts.md + .claude/rules/bkshading.md): a
# relay binary deploy NEVER start/restart/`enable --now`s the service — reboot (or the supervisor's
# post-reboot verify) brings it live, so a deploy can never light up the relay mid-event. This pure
# predicate is the SINGLE source of truth for "should the deploy start it?" and the test pins it to
# `no`, so a future edit that tries to start the service is a RED test.
bkshading_deploy_should_start() { printf '%s\n' no; }

# Byte-verify decision (deploy-from-clean-tree.md Layer 3): compare the local sha256 of the binary
# we pushed against the sha256 the box reports back. `match` ONLY when both are non-empty AND equal
# — an empty side (a failed remote read / partial scp) is `mismatch`, never a false `match`.
bkshading_deploy_sha_match() {  # $1 = local sha, $2 = remote sha
  local l="${1:-}" r="${2:-}"
  if [ -n "$l" ] && [ -n "$r" ] && [ "$l" = "$r" ]; then
    printf '%s\n' match
  else
    printf '%s\n' mismatch
  fi
}

# The REMOTE command that reads the target's root mount options (issue 808, slice B). `findmnt`
# first; if it fails outright, /proc/mounts directly (the setup-device.sh ensure_root_writable
# fallback), so a missing findmnt never reads as an empty "unknown" root on a box that has one.
bkshading_deploy_root_opts_cmd() {
  # shellcheck disable=SC2016  # the $2/$4 are awk's fields on the REMOTE side, never expanded here
  printf '%s\n' 'findmnt -no OPTIONS / 2>/dev/null || awk '"'"'$2=="/"{print $4; exit}'"'"' /proc/mounts'
}

# Remount decision for the swap, from the TARGET's own root mount options (issue 808, slice B --
# the SBC root goes read-only "the same as the camboxes"): `remount` for a read-only root (the
# remount,rw -> swap -> remount,ro cycle), `none` for a read-write root (a board before its first
# read-only reboot: remounting it ro would be wrong), `unreadable` for an empty or unexpected read
# -- the caller refuses before touching the box. ONE first-token reading: ro_root_mount_mode, the
# same semantics as setup-device.sh / verify-device.sh root_mount_is_readonly.
bkshading_deploy_root_remount_action() {  # $1 = `findmnt -no OPTIONS /` output from the target
  case "$(ro_root_mount_mode "${1:-}")" in
    ro) printf '%s\n' remount ;;
    rw) printf '%s\n' none ;;
    *) printf '%s\n' unreadable ;;
  esac
}

# Restore decision after the binary swap (issue 808, 25.9.2026): the deploy STOPS an active relay
# before the swap so no process holds the replaced (deleted-but-open) binary -- that open file was
# what made the final `remount,ro` fail "busy" on cam6/cam7 and leave the root read-WRITE. After the
# swap it restores the relay's PREVIOUS state: `start` when it was running or about to run --
# `active`, `reloading`, or `activating` (the unit is Restart=on-failure, so `activating
# (auto-restart)` is a real state: left alone, systemd would relaunch the old inode mid-swap).
# Everything else (inactive / failed / deactivating / unknown) -> `none`, so a deploy never STARTS a
# relay that was not running (the TEST-mode disabled relay stays stopped -- issue 1311), and
# bkshading_deploy_should_start above stays `no`. An EMPTY read is `unreadable`: the caller must
# refuse before touching the box (an ssh failure must never be taken for "not running").
bkshading_deploy_restore_action() {  # $1 = `systemctl is-active` output read before the swap
  case "${1:-}" in
    active | activating | reloading) printf '%s\n' start ;;
    '') printf '%s\n' unreadable ;;
    *) printf '%s\n' none ;;
  esac
}

# The holders of deleted-but-open files that keep the root from going read-only again (issue 808:
# the `lsof +L1` / /proc probe and its one-line summary) moved to scripts/lib/ro-window.sh with the
# ONE verified ro close that names them (issue 1407): ro_window_holder_probe_cmd,
# ro_window_deleted_holders.
