#!/usr/bin/env bash
# scripts/lib/bkshading-ndi-runtime.sh — shared NDI-runtime discovery constants + pure helpers
# for the bkshading service (issue 1157).
#
# The bkshading SERVICE loads libndi at RUNTIME (bkshading/service/src/preview/ndi_paths.rs) to
# receive the M2 live camera preview. This lib mirrors that module's LINUX search order (so a
# Linux service host can be provisioned + verified with the SAME dirs/names the binary will try)
# and records the documented Windows strih runtime DLL path.
#
# Source-only: defines pure functions and performs NO side effects, and deliberately does NOT
# `set -euo pipefail` (that would leak into the sourcing shell — the sourced-harness set-e leak
# in .claude/rules/ci-testing-gotchas.md).
# airuleset:script-ok source-only lib — set -euo pipefail would leak into the sourcing shell (ci-testing-gotchas)
#
# KEEP IN SYNC with bkshading/service/src/preview/ndi_paths.rs — the python test
# tests/python/test_bkshading_ndi_provision_1157.py cross-checks the two so they cannot drift.

# Env-var names (priority order) whose values are NDI runtime DIRECTORIES. Mirrors ndi_paths.rs
# NDI_ENV_DIRS. The NDI redistributable sets NDI_RUNTIME_DIR_V6; the appliance systemd unit too.
bkshading_ndi_env_vars() { printf '%s\n' NDI_RUNTIME_DIR_V6 NDI_RUNTIME_DIR_V5 NDI_RUNTIME_DIR; }

# Well-known Linux install dirs (mirrors ndi_paths.rs ndi_wellknown_dirs(Linux)).
bkshading_ndi_linux_dirs() { printf '%s\n' /usr/lib/ndi /usr/local/lib/ndi /opt/ndi/lib; }

# Linux library file names, most-preferred first (mirrors ndi_paths.rs ndi_lib_names(Linux)).
bkshading_ndi_linux_names() { printf '%s\n' libndi.so.6 libndi.so.5 libndi.so; }

# The documented strih (Windows) NDI runtime DLL — from scripts/bundle-state-server.py
# (DEFAULT_NDI_RUNTIME_DLL) / .claude/commands/drift-guard.md. The service's Windows discovery
# table (ndi_paths.rs) finds it here, or via NDI_RUNTIME_DIR_V6 / the PATH fallback.
bkshading_ndi_windows_dll() {
  printf '%s\n' 'C:\Program Files\NDI\NDI 6 Tools\Runtime\Processing.NDI.Lib.x64.dll'
}

# Pure: echo the FIRST candidate name present in LISTING, or nothing (return 1).
#   $1 NAMES   space-separated candidate names in preference order
#   $2 LISTING newline-separated filenames (an `ls -1`-style listing of one directory)
# Preference order is by NAMES, not by listing order (so libndi.so.6 wins over libndi.so).
bkshading_ndi_first_match() {
  local names_str="$1" listing="$2" name line
  local -a names
  read -r -a names <<<"$names_str"
  for name in "${names[@]}"; do
    while IFS= read -r line; do
      [ "$line" = "$name" ] && {
        printf '%s\n' "$name"
        return 0
      }
    done <<<"$listing"
  done
  return 1
}
