#!/usr/bin/env bash
# scripts/lib/shading-https.sh — shared constants + pure helpers for the LAN-only HTTPS front that
# serves the bkshading PWA panel at a trusted-cert hostname (camera-box issue 808, owner ruling
# 17.9.2026: "HTTPS áno, ale cez lokálnu sieť, nie cez internet").
#
# WHY: the bkshading web panel (bkshading/service, binds 0.0.0.0:8770) is reachable only over plain
# HTTP on strih.lan:8770. Chrome/Edge offer a full PWA install ONLY in a SECURE CONTEXT (HTTPS with
# a trusted cert, or localhost), so from any other LAN PC there is no "Install app". The chosen
# design (owner-rejected the cloudflared/internet path) fronts the panel with a dev1 nginx TLS
# reverse proxy on a PUBLIC DNS NAME that resolves to a PRIVATE LAN IP (shading.newlevel.media ->
# 10.77.9.200), with a Let's Encrypt cert obtained via DNS-01 (certbot --dns-cloudflare). Traffic
# stays on the LAN (browser -> dev1 -> strih); the internet is used only for the DNS lookup and the
# ACME cert renewal.
#
# This lib is the SINGLE SOURCE OF TRUTH for the hostname / LAN IP / upstream / paths + the nginx
# SITE renderer + the certbot argv + the deploy-hook + the cloudflare.ini renderer + the --check
# verdict classifier. It is consumed by scripts/dev1-shading-https-install.sh AND the python
# cross-check test (tests/python/test_shading_https_install_808.py), so the committed nginx site
# file, the installer, and this helper cannot silently drift.
#
# Source-only: defines pure functions, performs NO side effects, and deliberately does NOT
# `set -euo pipefail` (that would leak into the sourcing shell — the sourced-harness set-e leak in
# .claude/rules/ci-testing-gotchas.md).
# airuleset:script-ok source-only lib — set -euo pipefail would leak into the sourcing shell (ci-testing-gotchas)

# --- Constants (KEEP IN SYNC with scripts/nginx/shading.newlevel.media.conf; the python test cross-checks) ---

# The public DNS name the panel is served at. A record -> the dev1 LAN IP below (DNS-only, NOT
# proxied), so the browser talks to dev1 on the LAN over a trusted-cert HTTPS origin.
shading_https_hostname() { printf '%s\n' shading.newlevel.media; }

# The DNS zone the hostname lives in (for the Cloudflare API).
shading_https_zone() { printf '%s\n' newlevel.media; }

# dev1's LAN IP the A record points at. Traffic never leaves the LAN.
shading_https_lan_ip() { printf '%s\n' 10.77.9.200; }

# The upstream the proxy forwards to — the bkshading panel on strih. MUST match the service's own
# default_bind (bkshading/service/src/config.rs -> "0.0.0.0:8770"); the python test pins the port
# against config.rs so the proxy always points where the panel listens. ONE source of truth.
shading_https_upstream() { printf '%s\n' http://strih.lan:8770; }

# The bkshading service (web panel) port — cross-checked against config.rs.
shading_https_service_port() { printf '%s\n' 8770; }

# The ACME (Let's Encrypt) registration contact for expiry notices. A newlevel.media address the
# owner controls; overridable via --email.
shading_https_email() { printf '%s\n' claude-02@newlevel.media; }

# apt packages the installer needs (nginx + certbot + the Cloudflare DNS-01 plugin).
shading_https_apt_packages() { printf '%s\n' 'nginx certbot python3-certbot-dns-cloudflare'; }

# The nginx site name (basename under sites-available / sites-enabled).
shading_https_site_name() { printf '%s\n' shading; }

# The certbot credentials file that holds the Cloudflare DNS API token (root:600). Its token value
# is copied from ~/.secrets/cloudflare-newlevel at install time — NEVER printed, NEVER committed.
shading_https_cf_ini_path() { printf '%s\n' /etc/letsencrypt/cloudflare.ini; }

# The source secret file the DNS token is read from (owner-placed, never committed).
shading_https_cf_token_file() { printf '%s\n' "$HOME/.secrets/cloudflare-newlevel"; }

# The certbot renewal deploy hook that reloads nginx after a cert renewal (mode 755).
shading_https_deploy_hook_path() { printf '%s\n' /etc/letsencrypt/renewal-hooks/deploy/nginx-reload.sh; }

# DNS-01 propagation wait (seconds) passed to certbot --dns-cloudflare-propagation-seconds.
shading_https_propagation_seconds() { printf '%s\n' 30; }

# The Let's Encrypt live cert directory for a hostname.
shading_https_cert_dir() { printf '%s\n' "/etc/letsencrypt/live/${1:-$(shading_https_hostname)}"; }

# --- Pure renderers ---

# Render the nginx site file for <hostname> proxying to <upstream>.
#   $1 hostname (server_name + cert paths)   $2 upstream URL (proxy_pass)
# The committed scripts/nginx/shading.newlevel.media.conf is exactly this with the live defaults —
# the python test asserts equality so the two never drift. NO secret appears here.
# nginx runtime variables ($host, $http_upgrade, ...) are emitted literally (escaped \$); only the
# shell parameters $hostname/$upstream are interpolated.
shading_https_site_content() {
  local hostname="$1" upstream="$2"
  cat <<EOF
# scripts/nginx/shading.newlevel.media.conf — LAN-only HTTPS front for the bkshading PWA panel
# (camera-box issue 808; owner ruling 17.9.2026: HTTPS over the LAN, never the internet). Rendered
# by scripts/lib/shading-https.sh (shading_https_site_content); deployed to dev1 by
# scripts/dev1-shading-https-install.sh. NO secrets in this file.
#
# Browser (strih/stream/mobile on the LAN) -> DNS ${hostname} = $(shading_https_lan_ip) (dev1 LAN,
# DNS-only / not proxied) -> dev1 nginx :443 (Let's Encrypt cert, DNS-01) -> ${upstream} (bkshading
# panel). Traffic stays on the LAN; the internet is used only for the DNS lookup + cert renewal.

server {
    listen 80;
    listen [::]:80;
    server_name ${hostname};

    # Plain HTTP -> permanent HTTPS redirect (a trusted-cert secure context is required for
    # PWA install; plain LAN HTTP is an insecure context and offers no install).
    return 301 https://\$host\$request_uri;
}

server {
    # nginx 1.24 (Ubuntu 24.04) has NO \`http2 on;\` directive — the http2 flag rides on \`listen\`.
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
    server_name ${hostname};

    ssl_certificate     /etc/letsencrypt/live/${hostname}/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/${hostname}/privkey.pem;

    location / {
        proxy_pass ${upstream};
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
        proxy_set_header X-Forwarded-Proto https;
        proxy_set_header X-Forwarded-For \$remote_addr;

        # WebSocket / SSE upgrade — the panel pushes live camera state over WS. Browsers speak WS
        # over HTTP/1.1 (over h2 the Upgrade handshake is 400 — documented in bkshading.md).
        proxy_set_header Upgrade \$http_upgrade;
        proxy_set_header Connection \$http_connection;
        proxy_read_timeout 3600s;
        proxy_buffering off;
    }
}
EOF
}

# Render the certbot renewal deploy hook (reloads nginx after a cert renewal). Mode 755 at install.
shading_https_deploy_hook_content() {
  cat <<'EOF'
#!/bin/sh
# /etc/letsencrypt/renewal-hooks/deploy/nginx-reload.sh — reload nginx after a cert renewal so the
# fresh shading.newlevel.media cert is picked up. Installed by scripts/dev1-shading-https-install.sh
# (camera-box issue 808). certbot.timer runs the renewal; this hook fires on a successful deploy.
set -e
systemctl reload nginx
EOF
}

# Render the certbot cloudflare credentials ini for <token>. Fed to `install -m 600` by the
# installer; NEVER written to a committed file and NEVER echoed to a log. The token arg is a real
# secret at install time — the python test only ever passes a fake value.
shading_https_cf_ini_content() {
  printf 'dns_cloudflare_api_token = %s\n' "$1"
}

# Print the certbot argv (one token per line) for a DNS-01 issuance.
#   $1 hostname   $2 email   $3 credentials-ini path   $4 propagation seconds
# Single source of truth for the certbot invocation, shared by the installer + the test.
shading_https_certbot_argv() {
  local hostname="$1" email="$2" ini="$3" prop="$4"
  printf '%s\n' \
    certonly \
    --dns-cloudflare \
    --dns-cloudflare-credentials "$ini" \
    --dns-cloudflare-propagation-seconds "$prop" \
    -d "$hostname" \
    --non-interactive \
    --agree-tos \
    -m "$email" \
    --no-eff-email
}

# --- Pure decision helper: the --check verdict classifier ---

# Combine the five probe results into a verdict. Each arg is "ok" or anything-else (fail/skip).
#   $1 packages   $2 dns   $3 cert   $4 site   $5 curl
# Prints "OK" (returns 0) when every probe is ok, else "FAIL:<space-separated failed names>"
# (returns 1). Pure — no IO beyond stdout.
shading_https_check_classify() {
  local packages="${1:-}" dns="${2:-}" cert="${3:-}" site="${4:-}" curl_="${5:-}"
  local failed="" pair name val
  for pair in "packages:$packages" "dns:$dns" "cert:$cert" "site:$site" "curl:$curl_"; do
    name="${pair%%:*}"
    val="${pair#*:}"
    if [ "$val" != "ok" ]; then
      failed="$failed $name"
    fi
  done
  if [ -n "$failed" ]; then
    printf 'FAIL:%s\n' "$failed"
    return 1
  fi
  printf 'OK\n'
  return 0
}
