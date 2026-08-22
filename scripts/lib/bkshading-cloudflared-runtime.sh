#!/usr/bin/env bash
# scripts/lib/bkshading-cloudflared-runtime.sh — shared constants + pure helpers for provisioning
# the bkshading CLOUDFLARE remote access (issue 808 cloudflare-remote milestone).
#
# Everything merged so far is LAN-only: the aggregation service (bkshading/service) binds the web
# panel on 0.0.0.0:8770 and is reachable only on strih.lan. The owner decided remote access goes
# through a password-protected cloudflare proxy (NOT tailscale — issue 808 comment 5355836067). A
# `cloudflared` tunnel (config-file mode) fronts the local panel; the password is enforced at the
# Cloudflare Access layer (not in the service). This lib is the single source of truth for the
# paths / unit name / config path / access-marker + the config.yml COMPOSER + the service origin,
# consumed by both scripts/bkshading-provision-cloudflared.sh and the python cross-check test so the
# systemd unit, the script, and this helper cannot silently drift.
#
# Source-only: defines pure functions and performs NO side effects, and deliberately does NOT
# `set -euo pipefail` (that would leak into the sourcing shell — the sourced-harness set-e leak in
# .claude/rules/ci-testing-gotchas.md).
# airuleset:script-ok source-only lib — set -euo pipefail would leak into the sourcing shell (ci-testing-gotchas)

# --- Constants (KEEP IN SYNC with systemd/bkshading-cloudflared.service; the python test cross-checks) ---

# Where the cloudflared connector binary is installed (the official Cloudflare tunnel client).
bkshading_cloudflared_bin_path() { printf '%s\n' /usr/local/bin/cloudflared; }

# The systemd unit name.
bkshading_cloudflared_unit_name() { printf '%s\n' bkshading-cloudflared.service; }

# The tunnel config file the connector reads (config-file mode). Names the tunnel, references the
# credentials JSON by PATH, and carries the ingress rules — auditable ON the box (unlike token mode,
# where ingress hides in the dashboard).
bkshading_cloudflared_config_path() { printf '%s\n' /etc/bkshading/cloudflared-config.yml; }

# The Access-enforcement acknowledgement marker. Written by --install ONLY when the operator passes
# --access-confirmed (i.e. confirmed the Cloudflare Access password policy is live on the hostname).
# --check FAILS without it, so a naked public tunnel can never be the "provisioned" state.
bkshading_cloudflared_access_marker_path() { printf '%s\n' /etc/bkshading/cloudflared-access-confirmed; }

# The cloudflared RUNTIME dependency (the official Cloudflare package — a distro/official .deb, NOT a
# build/link dep; the connector is a standalone binary, keeping a clean ARM cross-build for the epic).
bkshading_cloudflared_apt_package() { printf '%s\n' cloudflared; }

# The bkshading service (web panel) port. MUST equal the service's own default_bind port
# (bkshading/service/src/config.rs -> "0.0.0.0:8770") so the tunnel points at exactly where the
# panel listens — the python test pins this against config.rs. ONE source of truth.
bkshading_cloudflared_service_port() { printf '%s\n' 8770; }

# The default tunnel origin (the local panel, when the connector runs co-located on the service box).
bkshading_cloudflared_default_origin() { printf '%s\n' "http://localhost:$(bkshading_cloudflared_service_port)"; }

# --- Pure helpers ---

# The tunnel origin for a connector running on a SEPARATE LAN box, pointing at the service host
# (e.g. `strih.lan`) on the service port. Pure — no IO.
bkshading_cloudflared_service_origin_for_host() {
  printf '%s\n' "http://${1}:$(bkshading_cloudflared_service_port)"
}

# Compose the cloudflared config.yml (config-file mode) for a tunnel.
#   $1 tunnel name (or UUID)   $2 public hostname   $3 credentials-file PATH   $4 origin URL
# The credentials JSON is referenced by PATH ONLY — this composer NEVER embeds a token/secret. The
# ingress routes the hostname to the local panel and terminates with the mandatory catch-all 404.
bkshading_cloudflared_config_content() {
  local tunnel="$1" hostname="$2" creds="$3" origin="$4"
  cat <<EOF
# /etc/bkshading/cloudflared-config.yml -- provisioned by scripts/bkshading-provision-cloudflared.sh
# (issue 808 cloudflare-remote milestone). Config-file mode: this file names the tunnel, references
# the credentials JSON by PATH (0600, placed by the owner from \`cloudflared tunnel create\`; NEVER
# committed), and routes the public hostname to the local bkshading panel. The password is enforced
# at the Cloudflare Access layer on the hostname (see the README operator-auth story) -- NOT here.
tunnel: $tunnel
credentials-file: $creds
ingress:
  - hostname: $hostname
    service: $origin
  - service: http_status:404
EOF
}

# Extract the credentials-file PATH referenced by a config.yml (read from stdin). Echoes the path,
# or nothing. Pure text — no secret is ever read (the path, not the JSON content). The composer
# emits exactly ONE `credentials-file:` line, so `sed` alone yields one line — deliberately NO
# `| head -1`, which under a caller's `pipefail` can SIGPIPE `sed` on early close (the #458 footgun);
# a malformed multi-line config would yield a multi-line value that fails the caller's `[ -f ]` check
# (safe — never a false match).
bkshading_cloudflared_creds_file_from_config() {
  sed -n 's/^[[:space:]]*credentials-file:[[:space:]]*//p'
}
