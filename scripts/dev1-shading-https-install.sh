#!/usr/bin/env bash
# scripts/dev1-shading-https-install.sh — provision + verify the LAN-only HTTPS front for the
# bkshading PWA panel on dev1 (camera-box issue 808). Full extended header below `set -euo pipefail`.
set -euo pipefail

# ---------------------------------------------------------------------------------------------
# WHY: the bkshading web panel (bkshading/service, binds 0.0.0.0:8770) is reachable only over plain
# HTTP on strih.lan:8770. Chrome/Edge offer a full PWA install ONLY in a SECURE CONTEXT (HTTPS with
# a trusted cert, or localhost), so from any other LAN PC there is no "Install app". Owner ruling
# 17.9.2026: HTTPS, but over the LOCAL network — NEVER through the internet (the cloudflared tunnel
# path is the rejected alternative). This installer stands up a dev1 nginx TLS reverse proxy on a
# PUBLIC DNS NAME that resolves to dev1's PRIVATE LAN IP (shading.newlevel.media -> 10.77.9.200),
# with a Let's Encrypt cert obtained via DNS-01 (certbot --dns-cloudflare). Traffic stays on the LAN
# (browser -> dev1 -> strih); the internet is used only for the DNS lookup + ACME cert renewal.
#
# Idempotent (a re-run re-writes/re-verifies), fail-loud (a gap exits non-zero with remediation).
# The pure decisions (site rendering, certbot argv, deploy hook, cloudflare.ini, --check verdict)
# live in scripts/lib/shading-https.sh so they are Tier-0 testable; this script wires the impure
# steps (apt, Cloudflare DNS record, certbot, nginx site + reload) around them.
#
# The Cloudflare DNS token is copied from ~/.secrets/cloudflare-newlevel into
# /etc/letsencrypt/cloudflare.ini (root:600) via `install -m 600` — it is NEVER printed and NEVER
# committed. The DNS A record is created via the airuleset `cli_cloudflare_dns` API client.
#
# Usage:
#   scripts/dev1-shading-https-install.sh [--site shading|interkom] --check
#   scripts/dev1-shading-https-install.sh [--site shading|interkom] --install
#                                         [--hostname H] [--upstream URL] [--lan-ip IP]
#                                         [--alias NAME]... [--email ADDR] [--dry-run]
#     --site       which front to provision (default shading = the bkshading panel; interkom = the
#                  strih-lx intercom hub + phone PWA, issue 1345 M3b — adds the /janus WS proxy).
#                  The per-site host/upstream/alias defaults apply unless overridden.
#     --hostname   public DNS name (PRIMARY / cert-keyed) the panel is served at (default per --site)
#     --upstream   the panel origin the proxy forwards to (default per --site)
#     --alias NAME extra server_name SAN on the SAME cert (repeatable; issue 1345 M4 — the crew
#                  production names). Default per --site (interkom = the two crew names; shading =
#                  none). Any explicit --alias (or SHADING_HTTPS_ALIASES env) REPLACES the default.
#     --lan-ip     dev1 LAN IP the A record points at (default 10.77.9.200)
#     --email      Let's Encrypt registration contact (default claude-02@newlevel.media)
#     --dry-run    rehearse the Cloudflare DNS record step (ensure_record dry_run) — no write
#
# GOTCHA — negative DNS cache: never query the public name BEFORE the A record exists. A failed
# lookup poisons resolver negative caches for the zone SOA minimum (1800 s). --install creates the
# record FIRST; run --check (which queries) only after.
#
# Exit codes: 0 = OK; 1 = not fully provisioned + remediation printed; 2 = bad argument.
#
# Overridable targets (for Tier-0 tests to a temp root — no root/apt/systemd/nginx/certbot needed):
#   SHADING_HTTPS_APT, SHADING_HTTPS_CERTBOT, SHADING_HTTPS_NGINX, SHADING_HTTPS_SYSTEMCTL,
#   SHADING_HTTPS_CURL, SHADING_HTTPS_GETENT, SHADING_HTTPS_PYTHON, SHADING_HTTPS_AIRULESET_DIR,
#   SHADING_HTTPS_SITE_AVAILABLE, SHADING_HTTPS_SITE_ENABLED, SHADING_HTTPS_DEFAULT_ENABLED,
#   SHADING_HTTPS_CF_INI, SHADING_HTTPS_CF_TOKEN_FILE, SHADING_HTTPS_DEPLOY_HOOK,
#   SHADING_HTTPS_CERT_DIR
# ---------------------------------------------------------------------------------------------

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/shading-https.sh
. "$HERE/lib/shading-https.sh"

# --- defaults (from the lib; flags override) ---
SHADING_HOST="$(shading_https_hostname)"
UPSTREAM="$(shading_https_upstream)"
LAN_IP="$(shading_https_lan_ip)"
ZONE="$(shading_https_zone)"
EMAIL="$(shading_https_email)"
PROP="$(shading_https_propagation_seconds)"

# --- overridable tool + path targets (real defaults; tests redirect to a temp root) ---
APT="${SHADING_HTTPS_APT:-apt-get}"
CERTBOT="${SHADING_HTTPS_CERTBOT:-certbot}"
NGINX="${SHADING_HTTPS_NGINX:-nginx}"
SYSTEMCTL="${SHADING_HTTPS_SYSTEMCTL:-systemctl}"
CURL="${SHADING_HTTPS_CURL:-curl}"
GETENT="${SHADING_HTTPS_GETENT:-getent}"
PYTHON="${SHADING_HTTPS_PYTHON:-python3}"
AIRULESET_DIR="${SHADING_HTTPS_AIRULESET_DIR:-$HOME/devel/airuleset}"
SITE_AVAILABLE="${SHADING_HTTPS_SITE_AVAILABLE:-/etc/nginx/sites-available/$(shading_https_site_name)}"
SITE_ENABLED="${SHADING_HTTPS_SITE_ENABLED:-/etc/nginx/sites-enabled/$(shading_https_site_name)}"
DEFAULT_ENABLED="${SHADING_HTTPS_DEFAULT_ENABLED:-/etc/nginx/sites-enabled/default}"
CF_INI="${SHADING_HTTPS_CF_INI:-$(shading_https_cf_ini_path)}"
CF_TOKEN_FILE="${SHADING_HTTPS_CF_TOKEN_FILE:-$(shading_https_cf_token_file)}"
DEPLOY_HOOK="${SHADING_HTTPS_DEPLOY_HOOK:-$(shading_https_deploy_hook_path)}"

MODE="--check"
DRY_RUN=0
# issue 1345 M3b: which site to provision. `shading` (default) = the bkshading panel front, existing
# behaviour unchanged; `interkom` = the strih-lx intercom hub + phone PWA front (adds the /janus WS
# proxy). Explicit --hostname/--upstream still win over the per-site defaults.
SITE="shading"
HOST_OVERRIDDEN=0
UPSTREAM_OVERRIDDEN=0
# issue 1345 M4: extra server_name SANs on the same cert. Seeded from the SHADING_HTTPS_ALIASES env
# (space-separated); the FIRST --alias flag clears the seed and starts fresh from flags. When left at
# the empty default AND no flag/env given, the per-site block below fills in the interkom default.
ALIASES="${SHADING_HTTPS_ALIASES:-}"
ALIASES_FROM_FLAG=0

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
    --site)
      require_val "$1" "${2:-}"
      case "$2" in
        shading|interkom) SITE="$2" ;;
        *)
          echo "--site must be 'shading' or 'interkom' (got: $2)" >&2
          exit 2
          ;;
      esac
      shift 2
      ;;
    --hostname)
      require_val "$1" "${2:-}"
      SHADING_HOST="$2"
      HOST_OVERRIDDEN=1
      shift 2
      ;;
    --upstream)
      require_val "$1" "${2:-}"
      UPSTREAM="$2"
      UPSTREAM_OVERRIDDEN=1
      shift 2
      ;;
    --alias)
      require_val "$1" "${2:-}"
      # The first flag clears any env-seeded default; subsequent flags append. An explicit alias set
      # thus REPLACES the per-site default (same override semantics as --hostname/--upstream).
      if [ "$ALIASES_FROM_FLAG" = 0 ]; then
        ALIASES=""
        ALIASES_FROM_FLAG=1
      fi
      ALIASES="${ALIASES:+$ALIASES }$2"
      shift 2
      ;;
    --lan-ip)
      require_val "$1" "${2:-}"
      LAN_IP="$2"
      shift 2
      ;;
    --email)
      require_val "$1" "${2:-}"
      EMAIL="$2"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      sed -n '2,40p' "${BASH_SOURCE[0]}"
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

# issue 1345 M3b: resolve per-site values. For `interkom`, adopt the interkom host/upstream/site
# name (unless a flag overrode them) and the /janus extra-location block + accurate header labels;
# for `shading` these stay empty so the renderer keeps the byte-identical default. `if` blocks (not
# `[ ] && …`) because a false test under `set -e` would abort the script.
EXTRA_LOCATIONS=""
FRONTED=""
SERVED=""
if [ "$SITE" = "interkom" ]; then
  if [ "$HOST_OVERRIDDEN" = 0 ]; then SHADING_HOST="$(interkom_https_hostname)"; fi
  if [ "$UPSTREAM_OVERRIDDEN" = 0 ]; then UPSTREAM="$(interkom_https_upstream)"; fi
  # issue 1345 M4: default the crew production aliases only when none were given (flag or env).
  if [ "$ALIASES_FROM_FLAG" = 0 ] && [ -z "$ALIASES" ]; then ALIASES="$(interkom_https_aliases)"; fi
  EXTRA_LOCATIONS="$(interkom_https_janus_location)"
  FRONTED="the strih-lx intercom hub + phone PWA (issue 1345)"
  SERVED="the strih-lx intercom hub"
  SITE_AVAILABLE="${SHADING_HTTPS_SITE_AVAILABLE:-/etc/nginx/sites-available/$(interkom_https_site_name)}"
  SITE_ENABLED="${SHADING_HTTPS_SITE_ENABLED:-/etc/nginx/sites-enabled/$(interkom_https_site_name)}"
fi

# CERT_DIR depends on the (possibly overridden / per-site) hostname.
CERT_DIR="${SHADING_HTTPS_CERT_DIR:-$(shading_https_cert_dir "$SHADING_HOST")}"

# ============================================================================================
# --check : probe each layer, classify via the pure helper, print remediation on any gap.
# ============================================================================================
do_check() {
  local packages_ok dns_ok cert_ok site_ok curl_ok answer code

  if command -v "$NGINX" >/dev/null 2>&1 && command -v "$CERTBOT" >/dev/null 2>&1; then
    packages_ok=ok
  else
    packages_ok=fail
  fi

  # DNS: resolve the hostname and compare to the expected LAN IP. No `head` (SIGPIPE under pipefail);
  # awk without an early exit reads all lines. The whole pipeline is `|| true` so a miss never aborts.
  answer="$("$GETENT" hosts "$SHADING_HOST" 2>/dev/null | awk 'NR==1{print $1}' || true)"
  if [ "$answer" = "$LAN_IP" ]; then
    dns_ok=ok
  else
    dns_ok=fail
  fi

  if [ -f "$CERT_DIR/fullchain.pem" ]; then
    cert_ok=ok
  else
    cert_ok=fail
  fi

  # site: the sites-enabled symlink must exist AND resolve, and `nginx -t` must pass.
  if [ -L "$SITE_ENABLED" ] && [ -e "$SITE_ENABLED" ] && "$NGINX" -t >/dev/null 2>&1; then
    site_ok=ok
  else
    site_ok=fail
  fi

  code="$("$CURL" -sk -o /dev/null -w '%{http_code}' "https://$SHADING_HOST/manifest.webmanifest" 2>/dev/null || true)"
  if [ "$code" = "200" ]; then
    curl_ok=ok
  else
    curl_ok=fail
  fi

  echo "shading HTTPS front — check ($SHADING_HOST -> $UPSTREAM):"
  echo "  packages (nginx+certbot): $packages_ok"
  echo "  dns A record            : $dns_ok (resolved '${answer:-<none>}', want $LAN_IP)"
  # issue 1345 M4: aliases are REPORT-ONLY — a fresh cut-over's alias A records may lag the primary's;
  # never fold them into the verdict, just report each so a lagging record is visible, not a failure.
  local alias aip
  # shellcheck disable=SC2086  # intentional word-split of the space-separated alias list
  for alias in $ALIASES; do
    aip="$("$GETENT" hosts "$alias" 2>/dev/null | awk 'NR==1{print $1}' || true)"
    if [ "$aip" = "$LAN_IP" ]; then
      echo "  alias A record          : ok ($alias -> $aip; report-only)"
    else
      echo "  alias A record          : lagging ($alias -> '${aip:-<none>}', want $LAN_IP; report-only, not a failure)"
    fi
  done
  echo "  letsencrypt cert        : $cert_ok ($CERT_DIR/fullchain.pem)"
  echo "  nginx site enabled+valid: $site_ok"
  echo "  curl manifest 200       : $curl_ok (HTTP ${code:-<none>})"

  if shading_https_check_classify "$packages_ok" "$dns_ok" "$cert_ok" "$site_ok" "$curl_ok" >/dev/null; then
    echo "OK — shading HTTPS front fully provisioned."
    return 0
  fi
  echo "NOT fully provisioned. Remediation: $HERE/dev1-shading-https-install.sh --install" >&2
  return 1
}

# ============================================================================================
# --install : apt -> cloudflare.ini -> DNS A record -> certbot -> nginx site + reload + deploy hook.
# ============================================================================================
do_install() {
  local token

  echo "[1/6] apt packages: $(shading_https_apt_packages)"
  # shellcheck disable=SC2046
  DEBIAN_FRONTEND=noninteractive "$APT" install -y $(shading_https_apt_packages)

  echo "[2/6] cloudflare credentials -> $CF_INI (root:600, token NEVER printed)"
  if [ ! -r "$CF_TOKEN_FILE" ]; then
    echo "cloudflare token file not readable: $CF_TOKEN_FILE" >&2
    exit 1
  fi
  token="$(cat "$CF_TOKEN_FILE")"
  if [ -z "$token" ]; then
    echo "cloudflare token file is empty: $CF_TOKEN_FILE" >&2
    exit 1
  fi
  install -d -m 755 "$(dirname "$CF_INI")"
  shading_https_cf_ini_content "$token" | install -m 600 /dev/stdin "$CF_INI"
  unset token

  echo "[3/6] DNS A record $SHADING_HOST -> $LAN_IP (DNS-only, not proxied)$( [ "$DRY_RUN" = 1 ] && echo ' [dry-run]')"
  # The A record MUST be created before any client query (negative-cache gotcha, see header).
  SHADING_HTTPS_DNS_HOST="$SHADING_HOST" \
  SHADING_HTTPS_DNS_ZONE="$ZONE" \
  SHADING_HTTPS_DNS_IP="$LAN_IP" \
  SHADING_HTTPS_DNS_TOKEN_FILE="$CF_TOKEN_FILE" \
  SHADING_HTTPS_DNS_DRYRUN="$DRY_RUN" \
  SHADING_HTTPS_DNS_AIRULESET="$AIRULESET_DIR" \
  "$PYTHON" - <<'PY'
import os
import sys

sys.path.insert(0, os.path.expanduser(os.environ["SHADING_HTTPS_DNS_AIRULESET"]))
import cli_cloudflare_dns as cf  # noqa: E402

token = cf._load_token(os.environ["SHADING_HTTPS_DNS_TOKEN_FILE"])
client = cf.DnsClient(token=token)
res = cf.ensure_record(
    client,
    zone_name=os.environ["SHADING_HTTPS_DNS_ZONE"],
    name=os.environ["SHADING_HTTPS_DNS_HOST"],
    rtype="A",
    content=os.environ["SHADING_HTTPS_DNS_IP"],
    proxied=False,
    comment="camera-box #808 — LAN-only HTTPS front for the bkshading panel (DNS-only)",
    dry_run=os.environ["SHADING_HTTPS_DNS_DRYRUN"] == "1",
)
if not res["ok"]:
    sys.stderr.write("DNS ensure_record failed: %s\n" % res["error"])
    sys.exit(1)
sys.stderr.write(
    "DNS %s: %s A %s\n"
    % (res["action"], os.environ["SHADING_HTTPS_DNS_HOST"], os.environ["SHADING_HTTPS_DNS_IP"])
)
PY

  echo "[4/6] certbot certonly (DNS-01)"
  local certbot_args=()
  while IFS= read -r line; do
    certbot_args+=("$line")
  done < <(shading_https_certbot_argv "$SHADING_HOST" "$EMAIL" "$CF_INI" "$PROP" "$ALIASES")
  "$CERTBOT" "${certbot_args[@]}"

  echo "[5/6] nginx site -> $SITE_AVAILABLE, enable, remove default, reload"
  install -d -m 755 "$(dirname "$SITE_AVAILABLE")"
  install -d -m 755 "$(dirname "$SITE_ENABLED")"
  shading_https_site_content "$SHADING_HOST" "$UPSTREAM" "$EXTRA_LOCATIONS" "$FRONTED" "$SERVED" "$ALIASES" > "$SITE_AVAILABLE"
  ln -sfn "$SITE_AVAILABLE" "$SITE_ENABLED"
  # The default nginx site would shadow our server_name-less :80 default — remove its symlink.
  rm -f "$DEFAULT_ENABLED"
  "$NGINX" -t
  "$SYSTEMCTL" reload nginx

  echo "[6/6] certbot deploy hook -> $DEPLOY_HOOK (755, reloads nginx on renewal)"
  install -d -m 755 "$(dirname "$DEPLOY_HOOK")"
  shading_https_deploy_hook_content | install -m 755 /dev/stdin "$DEPLOY_HOOK"

  echo "OK — shading HTTPS front installed. Verify: $HERE/dev1-shading-https-install.sh --check"
}

case "$MODE" in
  --check)
    do_check
    ;;
  --install)
    do_install
    ;;
esac
