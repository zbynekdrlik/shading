#!/usr/bin/env bash
# scripts/lib/bkshading-relay-runtime.sh — shared constants + pure helpers for provisioning the
# bkshading cambox/SBC RELAY (issue 808 relay-provisioning milestone; unblocks the issue-809 live
# grab derive).
#
# The relay BINARY (bkshading/relay/) already reads CAMERA_BOX_CAPTURE_FPS from its environment
# (parse_capture_fps_env in main.rs -> CameraSession::with_capture_fps -> RelayState.capture_fps)
# and drives the local Blackmagic camera over USB-PTP via the gphoto2 CLI. What was missing is the
# PROVISIONING: a systemd unit to run it, gphoto2 installed, and the env value. This lib is the
# single source of truth for the paths/port + the capture-fps DERIVE, consumed by both
# scripts/bkshading-provision-relay.sh and the python cross-check test so the systemd unit, the
# script, and this helper cannot silently drift.
#
# Source-only: defines pure functions and performs NO side effects, and deliberately does NOT
# `set -euo pipefail` (that would leak into the sourcing shell — the sourced-harness set-e leak in
# .claude/rules/ci-testing-gotchas.md).
# airuleset:script-ok source-only lib — set -euo pipefail would leak into the sourcing shell (ci-testing-gotchas)

# --- Constants (KEEP IN SYNC with systemd/bkshading-relay.service; the python test cross-checks) ---

# Where the relay binary is installed (mirrors setup-device.sh's /usr/local/bin/camera-box idiom).
bkshading_relay_bin_path() { printf '%s\n' /usr/local/bin/bkshading-relay; }

# The relay's HTTP bind port (matches bkshading-relay's --bind default and the service poller).
bkshading_relay_port() { printf '%s\n' 8771; }

# The systemd unit name + the provisioned EnvironmentFile that carries CAMERA_BOX_CAPTURE_FPS.
bkshading_relay_unit_name() { printf '%s\n' bkshading-relay.service; }
bkshading_relay_env_path() { printf '%s\n' /etc/bkshading/relay.env; }

# The gphoto2 RUNTIME dependency (a distro package — NOT a build/link dep; the relay shells out to
# the CLI behind the Gphoto2Runner trait, keeping a clean ARM cross-build; see .claude/rules/bkshading.md).
bkshading_relay_apt_package() { printf '%s\n' gphoto2; }

# The appliance's own default grab rate. MUST equal src/capture.rs requested_capture_denominator's
# `.unwrap_or(60)` so a box with no capture-fps drop-in reports the SAME rate it actually grabs at
# (ONE source of truth — the python test pins this against src/capture.rs).
bkshading_relay_default_capture_fps() { printf '%s\n' 60; }

# --- Pure helpers ---

# Extract the LAST CAMERA_BOX_CAPTURE_FPS value from concatenated camera-box.service.d drop-in text
# (a systemd `Environment=CAMERA_BOX_CAPTURE_FPS=N` line, or a bare KEY=N). Echoes the WHOLE value
# TOKEN (everything up to whitespace/EOL), or NOTHING when unset — deliberately NOT `[0-9]+`, which
# would PREFIX-match a decimal (`=30.5` -> `30`) and thus REPORT 30 while the appliance's own
# `std::env::var(...).parse::<u32>()` rejects "30.5" entirely and falls back to 60 — a one-source-of
# -truth divergence. Handing the full token to `..._effective_capture_fps` (which accepts pure digits
# only, else the 60 default) mirrors the appliance's all-or-nothing parse exactly. Mirrors
# setup-device.sh's genlock_dropin_fps shape — the trailing `|| true` is mandatory (the #458 footgun:
# a no-match grep|tail|cut fails under the caller's pipefail even though tail/cut succeed on empty
# input, and a bare X="$(...)" caller must never abort).
bkshading_relay_capture_fps_from_dropins() {
  printf '%s\n' "$1" | grep -oE 'CAMERA_BOX_CAPTURE_FPS=[^[:space:]]+' | tail -1 | cut -d= -f2- || true
}

# Derive the effective capture fps to provision, mirroring src/capture.rs
# requested_capture_denominator(override): a positive integer wins, anything else falls back to the
# appliance default (60). No grep/pipe (avoids the SIGPIPE-under-pipefail trap) — a pure `case`.
bkshading_relay_effective_capture_fps() {
  local raw="${1:-}"
  case "$raw" in
    '' | *[!0-9]*) bkshading_relay_default_capture_fps ;;
    *) if [ "$raw" -gt 0 ] 2>/dev/null; then printf '%s\n' "$raw"; else bkshading_relay_default_capture_fps; fi ;;
  esac
}

# Compose the EnvironmentFile body (/etc/bkshading/relay.env) for a given effective fps. Defaults to
# the appliance default when called with no argument.
bkshading_relay_env_file_content() {
  local fps="${1:-}"
  [ -n "$fps" ] || fps="$(bkshading_relay_default_capture_fps)"
  cat <<EOF
# /etc/bkshading/relay.env -- provisioned by scripts/bkshading-provision-relay.sh (issue 808/809).
# CAMERA_BOX_CAPTURE_FPS: this box's grab-mode fps, derived from the appliance's own capture-fps
# config (the SAME value src/capture.rs requested_capture_denominator uses). Read by the relay
# (bkshading/relay/src/main.rs) -> RelayState.capture_fps -> the service's issue-809 grab derive.
# The relay's EnvironmentFile= line makes this file OPTIONAL: if it is absent the relay reports no
# capture fps and the service falls back to the static grab_fps config instead of a wrong value.
CAMERA_BOX_CAPTURE_FPS=$fps
EOF
}
