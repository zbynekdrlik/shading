#!/usr/bin/env bash
# scripts/lib/bkshading-relay-version.sh -- read a cambox's camera-box + bkshading relay versions and
# grade the relay against camera-box (issue 1452).
#
# Owner, 29.10.2026: the fleet upgrade before the 28.10 event left every cambox on an old relay
# (dev.755-810) because deploy-fleet.sh never touched it, and cam7's shading did not set values.
# Both binaries come from the ONE workspace version (root Cargo.toml [workspace.package]), so a relay
# from the same CI run as camera-box prints the same version. Two consumers share this lib:
#   - scripts/deploy-fleet.sh: the per-box relay step + the final summary line;
#   - scripts/rig-dev-handover-check.sh item 15 (shading): one `verdict=RELAY-*` line per box.
#
# Source-only: pure functions, no side effects, no `set -euo pipefail` (it would leak into the
# sourcing shell, ci-testing-gotchas.md).
# airuleset:script-ok source-only lib -- set -euo pipefail would leak into the sourcing shell

_BRV_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bkshading-relay-runtime.sh
. "$_BRV_HERE/bkshading-relay-runtime.sh" # bkshading_relay_bin_path / bkshading_relay_unit_name

# bkshading_relay_versions_cmd -> REMOTE bash that prints exactly two lines:
#   camera-box=<version | absent | unreadable>
#   relay=<version | absent | missing-binary | unreadable>
# `absent` relay = neither the relay binary nor its unit file exists (the box was never provisioned
# for shading: reported, never a failure). `missing-binary` = the unit is there but the binary is not
# (setup-device's recorded RELAY_PROBLEM): a deploy fixes it. Builtins + the two binaries only, so it
# runs on a stripped box; every statement ends with `;` (the $(...) trailing-newline gotcha).
bkshading_relay_versions_cmd() {
  local bin unit
  bin="$(bkshading_relay_bin_path)"
  unit="/etc/systemd/system/$(bkshading_relay_unit_name)"
  # shellcheck disable=SC2016  # every $ here expands on the REMOTE box
  printf '%s' 'cb=absent; if [ -e /usr/local/bin/camera-box ]; then cb=$(/usr/local/bin/camera-box --version 2>/dev/null) || cb=""; cb=${cb##* }; [ -n "$cb" ] || cb=unreadable; fi; echo "camera-box=$cb"; '
  # shellcheck disable=SC2016
  printf 'rl=absent; if [ -e %s ] || [ -e %s ]; then if [ -x %s ]; then rl=$(%s --version 2>/dev/null) || rl=""; rl=${rl##* }; [ -n "$rl" ] || rl=unreadable; else rl=missing-binary; fi; fi; echo "relay=$rl";\n' \
    "$bin" "$unit" "$bin" "$bin"
}

# bkshading_relay_versions_field TEXT KEY -> the value of the LAST `KEY=` line in TEXT, or nothing.
bkshading_relay_versions_field() {
  local text="${1:-}" key="${2:-}" line val=""
  while IFS= read -r line; do
    case "$line" in
      "$key="*) val="${line#"$key="}" ;;
    esac
  done <<<"$text"
  printf '%s\n' "$val"
}

# bkshading_relay_version_verdict CAMERA_BOX_VERSION RELAY_VERSION -> one token:
#   RELAY-NONE     no relay on the box (neutral)
#   RELAY-MISSING  the relay unit without its binary (the relay cannot run: forgot)
#   RELAY-UNKNOWN  either version could not be read (neutral, never a false STALE)
#   RELAY-CURRENT  the relay runs the box's camera-box version (the same build)
#   RELAY-STALE    the relay runs another version than camera-box (forgot: deploy-fleet.sh)
bkshading_relay_version_verdict() {
  local cb="${1:-}" rl="${2:-}"
  case "$rl" in
    absent) printf '%s\n' RELAY-NONE; return 0 ;;
    missing-binary) printf '%s\n' RELAY-MISSING; return 0 ;;
    '' | unreadable) printf '%s\n' RELAY-UNKNOWN; return 0 ;;
  esac
  case "$cb" in
    '' | absent | unreadable) printf '%s\n' RELAY-UNKNOWN; return 0 ;;
  esac
  if [ "$cb" = "$rl" ]; then
    printf '%s\n' RELAY-CURRENT
  else
    printf '%s\n' RELAY-STALE
  fi
}
