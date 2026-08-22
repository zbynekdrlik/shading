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

# The CI artifact name the `bkshading` job uploads the relay/service into (KEEP IN SYNC with
# .github/workflows/ci.yml `Upload bkshading binaries`; the python test cross-checks both).
bkshading_deploy_artifact_name() { printf '%s\n' bkshading-linux-amd64; }

# The aarch64 relay-only artifact the `bkshading` job cross-builds + uploads for the SBC/handheld
# (issue 808 SBC milestone; a Pi Zero 2 W is ARM and cannot run the amd64 binary). ONE source of
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
