#!/usr/bin/env bash
# scripts/lib/bkshading-deploy-service-runtime.sh — pure invariants for deploying the CI-built
# bkshading SERVICE (bkshading.exe) to the strih Windows PC + installing its persistent Task
# Scheduler keep-alive task (issue 808 service-deploy sub-step).
#
# The `bkshading-windows` CI job release-builds + uploads the deployable service binary as the
# `bkshading-windows-amd64` artifact (target/release/bkshading.exe). This lib is the SINGLE source
# of truth for that artifact NAME, the exe filename, the on-box install dir, the config filename +
# example-seed name, the Task Scheduler task name, the panel port (== config.rs default_bind), the
# keep-alive cadence, and the installer ps1's own filename — consumed by BOTH
# scripts/bkshading-deploy-service.sh and scripts/bkshading-install-service.ps1 (via the values the
# deploy script passes to it) AND the python cross-check test, so the CI upload, the dev1-side
# orchestrator, and the on-box installer cannot silently drift.
#
# Source-only: defines pure functions, performs NO side effects, and deliberately does NOT enable
# strict mode (no `set -e`/`-u`/`-o pipefail`) — enabling it here would leak into the sourcing shell
# (the sourced-harness leak in .claude/rules/ci-testing-gotchas.md). Mirrors the pure-decision-in-lib
# split of scripts/lib/bkshading-deploy-runtime.sh (the relay's sibling).
# airuleset:script-ok source-only lib — strict mode would leak into the sourcing shell (ci-testing-gotchas)

# The CI artifact the `bkshading-windows` job uploads the deployable service binary into (KEEP IN
# SYNC with .github/workflows/ci.yml `Upload bkshading Windows service binary`).
bkshading_service_artifact_name() { printf '%s\n' bkshading-windows-amd64; }

# The service binary file name inside that artifact (bkshading's [[bin]] name, .exe on Windows).
bkshading_service_exe_name() { printf '%s\n' bkshading.exe; }

# The stable on-box install directory the service + its config + the installer live in.
bkshading_service_install_dir() { printf '%s\n' 'C:\bkshading'; }

# The operator config filename (never clobbered on redeploy — seeded from the example only if absent).
bkshading_service_config_name() { printf '%s\n' bkshading.toml; }

# The example config shipped as the seed for a first install (bkshading/service/bkshading.example.toml).
bkshading_service_config_example_name() { printf '%s\n' bkshading.example.toml; }

# The installer ps1's own filename (shipped alongside the exe + config seed to the box).
bkshading_service_installer_ps1_name() { printf '%s\n' bkshading-install-service.ps1; }

# The persistent Task Scheduler task name that keeps the service alive.
bkshading_service_task_name() { printf '%s\n' bkshading-service; }

# The operator web-panel port — MUST equal the service's own default bind port
# (bkshading/service/src/config.rs -> "0.0.0.0:8770"); the install verifies THIS port is Listening.
bkshading_service_port() { printf '%s\n' 8770; }

# The keep-alive scheduled-task repetition cadence (minutes). Task Scheduler has no
# Restart=on-failure, so a repetition-triggered idempotent check-and-relaunch is the repo idiom
# (.claude/rules/avsync-monitoring.md); shading is colour/exposure not motion, so a few minutes is
# plenty for a service the operator opens on demand.
bkshading_service_keepalive_minutes() { printf '%s\n' 5; }

# Byte-verify decision (deploy-from-clean-tree.md Layer 3, mirroring bkshading-deploy-runtime.sh's
# relay sibling): compare the local sha256 of the exe we scp'd against the sha256 the box reports
# back (via certutil). `match` ONLY when both are non-empty AND equal -- an empty side (a failed
# remote read / a truncated scp) is `mismatch`, never a false `match`.
bkshading_service_sha_match() {  # $1 = local sha, $2 = remote sha
  local l="${1:-}" r="${2:-}"
  if [ -n "$l" ] && [ -n "$r" ] && [ "$l" = "$r" ]; then
    printf '%s\n' match
  else
    printf '%s\n' mismatch
  fi
}
