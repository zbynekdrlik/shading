#!/usr/bin/env bash
# scripts/bkshading-provision-cloudflared.sh — provision + verify the bkshading CLOUDFLARE remote
# access (issue 808). Full extended header below `set -euo pipefail`.
set -euo pipefail

# ---------------------------------------------------------------------------------------------
# WHY: everything merged so far (M1 panel + relay, M2 NDI preview, WS push, #809 fps sync, relay
# provisioning, M3 relay deploy) is LAN-only — the aggregation service binds the web panel on
# 0.0.0.0:8770 and is reachable only on strih.lan. The owner decided remote access goes through a
# password-protected cloudflare proxy (NOT tailscale — issue 808 comment 5355836067). This script
# provisions a `cloudflared` tunnel (config-file mode) that fronts the local panel, an enable-only
# systemd unit, and an ACCESS-ENFORCEMENT gate.
#
# The password is enforced at the Cloudflare Access layer on the hostname (One-Time-PIN policy for
# the allowed operator emails) — NOT in the service. The tunnel connector holds ONLY its
# credentials JSON (referenced by PATH in the config, 0600, placed by the owner from
# `cloudflared tunnel create`; NEVER committed). Because a cloudflared tunnel exposes a PUBLIC
# hostname, --install REFUSES without --access-confirmed and --check FAILS without the Access
# marker — a naked, unprotected public tunnel can never be the "provisioned" state.
#
# Idempotent (a re-run just re-writes/re-verifies), fail-loud (a gap exits non-zero with the exact
# remediation), ENABLE-ONLY (daemon-reload + enable, NEVER start/restart — defer to reboot, per
# .claude/rules/provisioning-scripts.md; the live remote-access verify is the supervisor's step).
#
# Cross-platform (the service is Windows-first on the strih PC): the Linux path installs+enables the
# systemd unit; the Windows path DOCUMENTS `cloudflared service install` + the dashboard steps (a
# bash shell cannot drive a Windows service install — mirrors bkshading-provision-ndi.sh).
#
# Usage:
#   scripts/bkshading-provision-cloudflared.sh --check
#   scripts/bkshading-provision-cloudflared.sh --install --hostname <h> --tunnel <name> \
#       --credentials-file <path> [--origin <url>] --access-confirmed
#     --hostname          the public Cloudflare hostname the panel is served at (e.g. shading.example.org)
#     --tunnel            the tunnel name or UUID (from `cloudflared tunnel create`)
#     --credentials-file  PATH to the tunnel credentials JSON (owner-placed, 0600; NEVER committed)
#     --origin            local panel origin (default http://localhost:8770; use http://strih.lan:8770
#                         when the connector runs on a separate LAN box)
#     --access-confirmed  REQUIRED — asserts the Cloudflare Access password policy is live on the hostname
#
# Exit codes: 0 = OK; 1 = not fully provisioned + remediation printed; 2 = bad argument / missing gate;
#             3 = UNVERIFIABLE from this shell (Windows — verify live).
#
# Overridable targets (for Tier-0 tests to a temp root — no root/apt/systemd/cloudflared needed):
#   BKSHADING_CF_UNIT_DEST, BKSHADING_CF_CONFIG_FILE, BKSHADING_CF_ACCESS_MARKER,
#   BKSHADING_CF_CLOUDFLARED, BKSHADING_CF_SYSTEMCTL
# ---------------------------------------------------------------------------------------------

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
# shellcheck source=scripts/lib/bkshading-cloudflared-runtime.sh
. "$HERE/lib/bkshading-cloudflared-runtime.sh"

UNIT_NAME="$(bkshading_cloudflared_unit_name)"
UNIT_SRC="$REPO/systemd/$UNIT_NAME"
UNIT_DEST="${BKSHADING_CF_UNIT_DEST:-/etc/systemd/system/$UNIT_NAME}"
CONFIG_FILE="${BKSHADING_CF_CONFIG_FILE:-$(bkshading_cloudflared_config_path)}"
ACCESS_MARKER="${BKSHADING_CF_ACCESS_MARKER:-$(bkshading_cloudflared_access_marker_path)}"
CLOUDFLARED="${BKSHADING_CF_CLOUDFLARED:-$(bkshading_cloudflared_bin_path)}"
SYSTEMCTL="${BKSHADING_CF_SYSTEMCTL:-systemctl}"
APT_PKG="$(bkshading_cloudflared_apt_package)"

# --- argument parsing (mode + install options); exit 2 on any bad/incomplete argument -----------
MODE="--check"
HOST_ARG=""
TUNNEL_ARG=""
CREDS_ARG=""
ORIGIN_ARG=""
ACCESS_CONFIRMED=0

require_val() { # $1 = flag name, $2 = candidate value (may be empty/missing)
  local flag="$1" val="${2:-}"
  if [ -z "$val" ]; then
    echo "option $flag needs a value" >&2
    exit 2
  fi
  case "$val" in
    --*)
      echo "option $flag needs a value (got: $val)" >&2
      exit 2
      ;;
  esac
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --check)
      MODE="--check"
      shift
      ;;
    --install)
      MODE="--install"
      shift
      ;;
    --hostname)
      require_val "$1" "${2:-}"
      HOST_ARG="$2"
      shift 2
      ;;
    --tunnel)
      require_val "$1" "${2:-}"
      TUNNEL_ARG="$2"
      shift 2
      ;;
    --credentials-file)
      require_val "$1" "${2:-}"
      CREDS_ARG="$2"
      shift 2
      ;;
    --origin)
      require_val "$1" "${2:-}"
      ORIGIN_ARG="$2"
      shift 2
      ;;
    --access-confirmed)
      ACCESS_CONFIRMED=1
      shift
      ;;
    -h | --help)
      grep -E '^# ' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "unknown argument: $1 (use --check or --install; see --help)" >&2
      exit 2
      ;;
  esac
done

verify_creds_mode() { # $1 = path; true iff mode is exactly 0600
  local mode
  mode="$(stat -c '%a' "$1" 2>/dev/null || echo '')"
  [ "$mode" = "600" ]
}

install_cloudflared() {
  if command -v "$CLOUDFLARED" >/dev/null 2>&1; then
    echo "  cloudflared already present: $(command -v "$CLOUDFLARED")"
    return 0
  fi
  echo "  installing $APT_PKG (the Cloudflare tunnel connector) via the official apt repo ..."
  # Official Cloudflare package repo (https://pkg.cloudflare.com). Idempotent — re-adding is safe.
  # /usr/share/keyrings is a standard 0755 dir on Debian/Ubuntu; -p is a no-op if it exists.
  mkdir -p /usr/share/keyrings
  curl -fsSL https://pkg.cloudflare.com/cloudflare-main.gpg |
    tee /usr/share/keyrings/cloudflare-main.gpg >/dev/null
  echo "deb [signed-by=/usr/share/keyrings/cloudflare-main.gpg] https://pkg.cloudflare.com/cloudflared any main" \
    >/etc/apt/sources.list.d/cloudflared.list
  apt-get update -qq
  apt-get install -y -qq cloudflared
}

do_install() {
  echo "[bkshading-provision-cloudflared] --install (enable-only; takes effect on next reboot)"
  [ -n "$HOST_ARG" ] || {
    echo "--install needs --hostname <public hostname>" >&2
    exit 2
  }
  [ -n "$TUNNEL_ARG" ] || {
    echo "--install needs --tunnel <tunnel name/UUID>" >&2
    exit 2
  }
  [ -n "$CREDS_ARG" ] || {
    echo "--install needs --credentials-file <path to the tunnel credentials JSON>" >&2
    exit 2
  }
  # ACCESS-ENFORCEMENT GATE: a cloudflared tunnel exposes a PUBLIC hostname. The owner's remote is a
  # PASSWORD-PROTECTED cloudflare proxy, so refuse to provision without an explicit confirmation
  # that the Cloudflare Access policy is live -> no naked, unprotected public tunnel is ever
  # "provisioned".
  if [ "$ACCESS_CONFIRMED" -ne 1 ]; then
    cat >&2 <<MSG
REFUSING: --install needs --access-confirmed.
A cloudflared tunnel exposes the shading panel on a PUBLIC hostname. The owner's remote access is a
PASSWORD-PROTECTED cloudflare proxy, so first create a Cloudflare Access application with a password
(One-Time-PIN) policy on '$HOST_ARG' in the Zero Trust dashboard, THEN re-run with
--access-confirmed. This gate makes a naked, unprotected public tunnel impossible to provision.
MSG
    exit 2
  fi

  install_cloudflared

  local origin="${ORIGIN_ARG:-$(bkshading_cloudflared_default_origin)}"

  mkdir -p "$(dirname "$CONFIG_FILE")"
  bkshading_cloudflared_config_content "$TUNNEL_ARG" "$HOST_ARG" "$CREDS_ARG" "$origin" >"$CONFIG_FILE"
  chmod 0644 "$CONFIG_FILE"
  echo "  wrote $CONFIG_FILE (tunnel=$TUNNEL_ARG hostname=$HOST_ARG origin=$origin)"

  mkdir -p "$(dirname "$ACCESS_MARKER")"
  printf '%s\n' "Cloudflare Access password policy confirmed live on $HOST_ARG at install time." \
    >"$ACCESS_MARKER"
  chmod 0644 "$ACCESS_MARKER"
  echo "  wrote $ACCESS_MARKER (operator confirmed the Access password policy)"

  mkdir -p "$(dirname "$UNIT_DEST")"
  install -m 0644 "$UNIT_SRC" "$UNIT_DEST"
  echo "  installed $UNIT_DEST"

  # ENABLE-ONLY: never start/restart the tunnel here (provisioning-scripts.md) — reboot / the
  # post-reboot verify step brings it live.
  "$SYSTEMCTL" daemon-reload
  "$SYSTEMCTL" enable "$UNIT_NAME"
  echo "  enabled $UNIT_NAME (NOT started -- reboot to take effect)"

  if [ -f "$CREDS_ARG" ]; then
    if ! verify_creds_mode "$CREDS_ARG"; then
      echo "  WARNING: $CREDS_ARG is not mode 0600 -- a tunnel credential must be 0600 (chmod 0600)." >&2
    fi
  else
    echo "  WARNING: credentials JSON not present at $CREDS_ARG -- place the tunnel credentials" >&2
    echo "           JSON there (from 'cloudflared tunnel create', chmod 0600) before reboot." >&2
  fi
  echo "install done. Verify with: scripts/bkshading-provision-cloudflared.sh --check"
}

do_check() {
  local rc=0 en creds
  # (1) cloudflared connector present.
  if command -v "$CLOUDFLARED" >/dev/null 2>&1; then
    echo "OK: cloudflared ($(command -v "$CLOUDFLARED"))"
  else
    echo "FAIL: cloudflared not installed (the tunnel connector)" >&2
    rc=1
  fi
  # (2) systemd unit installed AND byte-matches the repo unit.
  if [ -f "$UNIT_DEST" ]; then
    if cmp -s "$UNIT_SRC" "$UNIT_DEST"; then
      echo "OK: unit installed $UNIT_DEST"
    else
      echo "FAIL: installed unit $UNIT_DEST differs from repo $UNIT_SRC (re-run --install)" >&2
      rc=1
    fi
  else
    echo "FAIL: unit not installed at $UNIT_DEST" >&2
    rc=1
  fi
  # (3) tunnel config present + well-formed (hostname + ingress origin + credentials-file + 404).
  # The ingress entries are YAML list items, so `hostname:` / `service: http_status:404` sit on a
  # `  - ` line — allow an optional list dash before the key (credentials-file: is a top-level key).
  if [ -f "$CONFIG_FILE" ] &&
    grep -qE '^[[:space:]]*-?[[:space:]]*hostname:' "$CONFIG_FILE" &&
    grep -qE '^[[:space:]]*-?[[:space:]]*service: https?://' "$CONFIG_FILE" &&
    grep -qE '^[[:space:]]*credentials-file:' "$CONFIG_FILE" &&
    grep -qE '^[[:space:]]*-?[[:space:]]*service: http_status:404' "$CONFIG_FILE"; then
    echo "OK: config $CONFIG_FILE (hostname + ingress + credentials-file + catch-all)"
  else
    echo "FAIL: config $CONFIG_FILE missing or malformed (hostname/ingress/credentials-file/404)" >&2
    rc=1
  fi
  # (4) the referenced credentials JSON present + mode 0600 (a secret must stay 0600).
  creds="$(bkshading_cloudflared_creds_file_from_config <"$CONFIG_FILE" 2>/dev/null || true)"
  if [ -n "$creds" ] && [ -f "$creds" ]; then
    if verify_creds_mode "$creds"; then
      echo "OK: credentials $creds (mode 0600)"
    else
      echo "FAIL: credentials $creds not mode 0600 (a tunnel credential must be 0600)" >&2
      rc=1
    fi
  else
    echo "FAIL: credentials JSON not present (config references: ${creds:-<none>})" >&2
    rc=1
  fi
  # (5) Access-confirmed marker present (the password requirement — never an unprotected tunnel).
  if [ -f "$ACCESS_MARKER" ]; then
    echo "OK: Access-confirmed marker $ACCESS_MARKER"
  else
    echo "FAIL: no Access-confirmed marker at $ACCESS_MARKER (tunnel may be UNPROTECTED public)" >&2
    rc=1
  fi
  # (6) unit enabled (reboot-survival; live runtime state is the supervisor's post-reboot verify).
  en="$("$SYSTEMCTL" is-enabled "$UNIT_NAME" 2>/dev/null || true)"
  if [ "$en" = "enabled" ]; then
    echo "OK: unit enabled"
  else
    echo "FAIL: unit not enabled (is-enabled=${en:-<none>})" >&2
    rc=1
  fi

  if [ "$rc" -ne 0 ]; then
    cat >&2 <<MSG
bkshading cloudflare remote NOT fully provisioned on this box. Fix:
  scripts/bkshading-provision-cloudflared.sh --install --hostname <h> --tunnel <name> \\
      --credentials-file <path> --access-confirmed
Then place the tunnel credentials JSON (from 'cloudflared tunnel create', chmod 0600) at the
referenced path and reboot. Configure the Cloudflare Access password policy on the hostname first.
MSG
  else
    echo "OK: bkshading cloudflare remote fully provisioned (live after reboot / already running)."
  fi
  return "$rc"
}

windows_notice() {
  local cf_dir
  cf_dir='%USERPROFILE%\.cloudflared\'
  cat >&2 <<MSG
bkshading cloudflare remote (Windows / strih PC — the Windows-first service host):
  A bash shell cannot install a Windows service. On the strih PC, do it with cloudflared itself:
    1. Install cloudflared for Windows (https://pkg.cloudflare.com / the .msi).
    2. Create the tunnel + credentials:  cloudflared tunnel login ; cloudflared tunnel create <name>
       (credentials JSON lands in $cf_dir<UUID>.json — keep it private, never commit it).
    3. Write a config.yml there with the SAME shape this script composes on Linux:
         tunnel: <name>
         credentials-file: $cf_dir<UUID>.json
         ingress:
           - hostname: <public hostname>
             service: http://localhost:$(bkshading_cloudflared_service_port)
           - service: http_status:404
    4. Route DNS:  cloudflared tunnel route dns <name> <public hostname>
    5. In the Cloudflare Zero Trust dashboard, create an Access application with a PASSWORD
       (One-Time-PIN) policy on <public hostname> BEFORE going live (owner: password-protected proxy).
    6. Install the service:  cloudflared service install
The live end-to-end remote-access verify is the supervisor's step. (exit 3 = unverifiable here)
MSG
  exit 3
}

os="$(uname -s 2>/dev/null || echo unknown)"
case "$os" in
  Linux)
    case "$MODE" in
      --install) do_install ;;
      --check) do_check ;;
    esac
    ;;
  MINGW* | MSYS* | CYGWIN* | Windows*)
    windows_notice
    ;;
  *)
    echo "unsupported OS '$os' — the bkshading cloudflared provisioning targets Linux (systemd) and" >&2
    echo "Windows (cloudflared service install). See scripts/lib/bkshading-cloudflared-runtime.sh." >&2
    exit 1
    ;;
esac
