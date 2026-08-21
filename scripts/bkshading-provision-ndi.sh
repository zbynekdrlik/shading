#!/usr/bin/env bash
# scripts/bkshading-provision-ndi.sh — provision + verify the NDI runtime the bkshading service
# needs (issue 1157). Extended header below `set -euo pipefail`.
set -euo pipefail

# ---------------------------------------------------------------------------------------------
# WHY: the bkshading service (bkshading/service) loads libndi at RUNTIME to receive the M2 live
# camera preview (feature `ndi`). It is a SEPARATE workspace member and does NOT inherit the
# appliance's own libndi provisioning. This script makes the runtime DISCOVERABLE by the EXACT
# search order the service uses (bkshading/service/src/preview/ndi_paths.rs) and verifies it.
#
# Idempotent (a re-run on a provisioned box just re-verifies), fail-loud (a missing/undiscoverable
# runtime exits non-zero with the exact remediation), enable-only (provisions a LIBRARY; never
# starts/stops/enables a service).
#
# Targets:
#   - Linux service host ("Linux later"): --check verifies discovery; --install delegates to the
#     repo's existing PUBLIC NDI Linux runtime fetch (vendor/distroav/CI/libndi-get.sh) when the
#     runtime is absent, then re-verifies.
#   - Windows strih (the primary M2 target): the NDI runtime ships with NDI Tools / the DistroAV
#     redist that OBS already uses, at the documented path
#     (C:\Program Files\NDI\NDI 6 Tools\Runtime\Processing.NDI.Lib.x64.dll); the service's #1157
#     load fix finds it there (or via NDI_RUNTIME_DIR_V6). A bash shell cannot drive the Windows
#     desktop, so run from git-bash it stats that DLL when it can (cygpath) and otherwise prints
#     the expected location + how to verify (run the service, open the panel). The live strih
#     end-to-end verify is the supervisor's rig step.
#
# Usage:  scripts/bkshading-provision-ndi.sh [--check|--install]
#   --check    (default) verify the runtime is discoverable; 0 if so, non-zero + remediation if not
#   --install  Linux only: fetch+install the public NDI Linux runtime if missing, then re-verify
#
# Exit codes: 0 = discoverable / OK; 1 = missing + remediation printed; 2 = bad argument;
#             3 = UNVERIFIABLE from this shell (a Windows bash with no cygpath — verify live).
# ---------------------------------------------------------------------------------------------

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
# shellcheck source=scripts/lib/bkshading-ndi-runtime.sh
. "$HERE/lib/bkshading-ndi-runtime.sh"

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

# Print the candidate NDI runtime DIRECTORIES for a Linux host, in the service's search order:
# each set env var's value first, then the well-known dirs. One dir per line.
ndi_candidate_dirs() {
  local var val
  while IFS= read -r var; do
    val="${!var:-}"
    [ -n "$val" ] && printf '%s\n' "$val"
  done < <(bkshading_ndi_env_vars)
  bkshading_ndi_linux_dirs
}

# Best-effort NDI runtime version banner from a .so (never fatal). Mirrors upgrade-fleet-ndi.sh:
# prefer `strings`, fall back to `grep -a` on boxes without binutils.
ndi_version_banner() {
  local so="$1" line="" ver=""
  if command -v strings >/dev/null 2>&1; then
    line="$(strings "$so" 2>/dev/null | grep -a -m1 'NDI SDK LINUX' || true)"
  fi
  [ -z "$line" ] && line="$(grep -a -m1 'NDI SDK LINUX' "$so" 2>/dev/null || true)"
  if [ -n "$line" ]; then
    # The banner is e.g. "NDI SDK LINUX 12:51:52 Apr 13 2026 6.3.2.0" — pull the X.Y.Z.W token.
    ver="$(printf '%s\n' "$line" | grep -aoE '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
    printf '%s\n' "${ver:-$line}"
  else
    printf '%s\n' "<version banner unavailable>"
  fi
}

# Verify the runtime is discoverable on a Linux host. Echoes the resolved .so path on success.
verify_linux() {
  local names dir listing match
  names="$(bkshading_ndi_linux_names | tr '\n' ' ')"
  while IFS= read -r dir; do
    [ -d "$dir" ] || continue
    listing="$(ls -1 "$dir" 2>/dev/null || true)"
    if match="$(bkshading_ndi_first_match "$names" "$listing")"; then
      printf '%s/%s\n' "$dir" "$match"
      return 0
    fi
  done < <(ndi_candidate_dirs)
  return 1
}

remediation_linux() {
  cat >&2 <<'MSG'
NDI runtime not discoverable by the bkshading service on this Linux host.
The service searches (in order): $NDI_RUNTIME_DIR_V6/_V5/(none), then /usr/lib/ndi,
/usr/local/lib/ndi, /opt/ndi/lib — for libndi.so.6 / .5 / .so.

Fix (any one):
  - scripts/bkshading-provision-ndi.sh --install         # fetch the public NDI Linux runtime
  - sudo bash vendor/distroav/CI/libndi-get.sh install   # the same public fetch, directly
  - copy an existing libndi.so.* into /usr/lib/ndi/ and run: sudo ldconfig
  - or export NDI_RUNTIME_DIR_V6=/path/to/dir/with/libndi.so.6
MSG
}

install_linux() {
  local getter="$REPO/vendor/distroav/CI/libndi-get.sh"
  if resolved="$(verify_linux)"; then
    echo "bkshading NDI runtime already present: $resolved (nothing to install)"
    return 0
  fi
  [ -f "$getter" ] || {
    echo "cannot --install: $getter not found" >&2
    exit 1
  }
  echo "NDI runtime absent — fetching the public NDI Linux runtime via libndi-get.sh ..."
  bash "$getter" install
}

os="$(uname -s 2>/dev/null || echo unknown)"
case "$os" in
  Linux)
    if [ "$MODE" = "--install" ]; then
      install_linux
    fi
    if resolved="$(verify_linux)"; then
      echo "OK: bkshading NDI runtime discoverable at $resolved"
      echo "    version: $(ndi_version_banner "$resolved")"
      exit 0
    fi
    remediation_linux
    exit 1
    ;;
  MINGW* | MSYS* | CYGWIN* | Windows*)
    dll="$(bkshading_ndi_windows_dll)"
    echo "bkshading service NDI runtime (Windows / strih):"
    echo "    documented DLL: $dll"
    echo "    env override:   NDI_RUNTIME_DIR_V6=${NDI_RUNTIME_DIR_V6:-<unset>}"
    if command -v cygpath >/dev/null 2>&1; then
      unix_dll="$(cygpath -u "$dll" 2>/dev/null || echo "$dll")"
      if [ -f "$unix_dll" ]; then
        echo "OK: runtime DLL present at $dll"
        exit 0
      fi
      echo "NDI runtime DLL NOT found at $dll." >&2
      echo "Install NDI Tools (the same runtime OBS/DistroAV use) or set NDI_RUNTIME_DIR_V6." >&2
      exit 1
    fi
    echo "UNVERIFIED: cannot stat a Windows path from this shell (no cygpath)." >&2
    echo "The service's cross-platform load resolves the runtime at the path above or via" >&2
    echo "NDI_RUNTIME_DIR_V6 / PATH. Verify live by running the service (--features ndi) and" >&2
    echo "opening the operator panel; the preview blocks update. (exit 3 = unverifiable here)" >&2
    exit 3
    ;;
  *)
    echo "unsupported OS '$os' — the bkshading service targets Linux (/usr/lib/ndi) and Windows" >&2
    echo "(NDI Tools runtime DLL). See bkshading/service/src/preview/ndi_paths.rs." >&2
    exit 1
    ;;
esac
