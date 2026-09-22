#!/usr/bin/env python3
"""LAN-only HTTPS front for the bkshading PWA panel (camera-box issue 808, owner ruling 17.9.2026:
HTTPS over the LAN, never the internet).

The bkshading web panel (bkshading/service) binds 0.0.0.0:8770 and is reachable only over plain
HTTP on strih.lan:8770. Chrome/Edge offer a full PWA install ONLY in a SECURE CONTEXT (HTTPS with a
trusted cert, or localhost), so from any other LAN PC there is no "Install app". The owner rejected
the cloudflared/internet path and chose a dev1 nginx TLS reverse proxy on a PUBLIC DNS NAME that
resolves to dev1's PRIVATE LAN IP (shading.newlevel.media -> 10.77.9.200), with a Let's Encrypt cert
via DNS-01 (certbot --dns-cloudflare). Traffic stays on the LAN; the internet is used only for the
DNS lookup + cert renewal.

These stdlib-only structural + behavioural tests run in the `python-tests` CI job (no Rust
toolchain, no root, no apt, no real nginx/certbot/cloudflared, no Cloudflare API — every impure op
is overridden to a temp root / fake binary / fake airuleset module):
 - the installer + lib parse (`bash -n`);
 - the lib constants are correct and the committed nginx site file EQUALS the lib's default render
   (drift guard — one source of truth);
 - the site renderer parametrizes hostname + upstream and carries every required directive
   (80->301, `listen 443 ssl http2` with NO `http2 on;`, WS Upgrade/Connection passthrough,
   proxy_read_timeout 3600s, proxy_buffering off);
 - the proxy upstream PORT agrees with the appliance `default_bind` (config.rs) — one source of
   truth for where the panel listens;
 - the certbot argv / deploy hook / cloudflare.ini renderers are correct;
 - the --check verdict classifier is right;
 - NO secret (Cloudflare token) is committed anywhere — the token is read from a file at runtime;
 - `--install` end-to-end writes the site (== committed conf), enables the symlink + removes the
   default, writes the cloudflare.ini 0600 from the token file WITHOUT ever echoing the token,
   creates the A record via the real `cli_cloudflare_dns` snippet (against a fake module), calls
   certbot with the right argv, writes the deploy hook 755, and reloads nginx;
 - `--check` fails when unprovisioned and passes when the temp-root fixtures are present;
 - Bluetooth appears NOWHERE (owner hard rule).
Runnable directly (`python3 tests/python/test_shading_https_install_808.py`) or under pytest.
"""
import os
import re
import shutil
import stat
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
SCRIPT = os.path.join(REPO, "scripts", "dev1-shading-https-install.sh")
LIB = os.path.join(REPO, "scripts", "lib", "shading-https.sh")
CONF = os.path.join(REPO, "scripts", "nginx", "shading.newlevel.media.conf")
CONFIG_RS = os.path.join(REPO, "bkshading", "service", "src", "config.rs")

HOSTNAME = "shading.newlevel.media"
ZONE = "newlevel.media"
LAN_IP = "10.77.9.200"
UPSTREAM = "http://strih.lan:8770"
SERVICE_PORT = "8770"
EMAIL = "claude-02@newlevel.media"
CF_INI = "/etc/letsencrypt/cloudflare.ini"
DEPLOY_HOOK = "/etc/letsencrypt/renewal-hooks/deploy/nginx-reload.sh"
FAKE_CRED = "cf-fake-placeholder-value"


def _bash(snippet, env=None):
    """Source the helper, run `snippet`, return stdout (raises on nonzero)."""
    src = '. "%s"\n%s' % (LIB, snippet)
    out = subprocess.run(
        ["bash", "-c", src], capture_output=True, text=True, check=True,
        env=dict(os.environ, **(env or {})),
    )
    return out.stdout


def test_files_exist_and_parse():
    for p in (SCRIPT, LIB, CONF):
        assert os.path.isfile(p), p
    for p in (SCRIPT, LIB):
        r = subprocess.run(["bash", "-n", p], capture_output=True, text=True)
        assert r.returncode == 0, "bash -n %s: %s" % (p, r.stderr)


def test_lib_constants():
    assert _bash("shading_https_hostname").strip() == HOSTNAME
    assert _bash("shading_https_zone").strip() == ZONE
    assert _bash("shading_https_lan_ip").strip() == LAN_IP
    assert _bash("shading_https_upstream").strip() == UPSTREAM
    assert _bash("shading_https_service_port").strip() == SERVICE_PORT
    assert _bash("shading_https_email").strip() == EMAIL
    assert _bash("shading_https_apt_packages").strip() == (
        "nginx certbot python3-certbot-dns-cloudflare"
    )
    assert _bash("shading_https_site_name").strip() == "shading"
    assert _bash("shading_https_cf_ini_path").strip() == CF_INI
    assert _bash("shading_https_deploy_hook_path").strip() == DEPLOY_HOOK
    assert _bash("shading_https_propagation_seconds").strip() == "30"
    assert _bash("shading_https_cert_dir").strip() == "/etc/letsencrypt/live/%s" % HOSTNAME
    assert _bash('shading_https_cert_dir other.example.org').strip() == (
        "/etc/letsencrypt/live/other.example.org"
    )


def test_service_port_matches_appliance_default_bind():
    # ONE source of truth: the proxy upstream port must equal the service's own default_bind port.
    with open(CONFIG_RS, encoding="utf-8") as f:
        cfg = f.read()
    assert '"0.0.0.0:%s"' % SERVICE_PORT in cfg, (
        "bkshading service default_bind changed — update the proxy upstream port too"
    )
    assert (":%s" % SERVICE_PORT) in UPSTREAM


def test_committed_conf_equals_default_render():
    # DRIFT GUARD: the committed nginx site file must be byte-identical to the lib's default render,
    # so the two never diverge (the lib is the single source of truth).
    rendered = _bash(
        'shading_https_site_content "$(shading_https_hostname)" "$(shading_https_upstream)"'
    )
    with open(CONF, encoding="utf-8") as f:
        committed = f.read()
    assert committed == rendered, "scripts/nginx/shading.newlevel.media.conf drifted from the lib render"


def test_site_content_parametrizes_hostname_and_upstream():
    body = _bash(
        'shading_https_site_content "shading.example.org" "http://box.lan:9999"'
    )
    assert "server_name shading.example.org;" in body, body
    assert "ssl_certificate     /etc/letsencrypt/live/shading.example.org/fullchain.pem;" in body
    assert "ssl_certificate_key /etc/letsencrypt/live/shading.example.org/privkey.pem;" in body
    assert "proxy_pass http://box.lan:9999;" in body, body


def test_site_content_required_directives():
    body = _bash(
        'shading_https_site_content "$(shading_https_hostname)" "$(shading_https_upstream)"'
    )
    # 80 -> 301 https redirect (secure context required for PWA install)
    assert "return 301 https://$host$request_uri;" in body, body
    # nginx 1.24: http2 rides on `listen`, NEVER the `http2 on;` directive
    assert "listen 443 ssl http2;" in body, body
    # nginx 1.24 has no `http2 on;` DIRECTIVE (a line on its own) — the comment mentioning it is fine
    assert re.search(r"(?m)^\s*http2 on;", body) is None, "must not use the `http2 on;` directive"
    # WebSocket passthrough (HTTP/1.1 + Upgrade/Connection)
    assert "proxy_http_version 1.1;" in body, body
    assert "proxy_set_header Upgrade $http_upgrade;" in body, body
    assert "proxy_set_header Connection $http_connection;" in body, body
    # long read timeout + no buffering for WS/SSE
    assert "proxy_read_timeout 3600s;" in body, body
    assert "proxy_buffering off;" in body, body
    assert "proxy_set_header X-Forwarded-Proto https;" in body, body
    assert "proxy_set_header X-Forwarded-For $remote_addr;" in body, body


def test_certbot_argv():
    out = _bash(
        'shading_https_certbot_argv "%s" "%s" "%s" "30"' % (HOSTNAME, EMAIL, CF_INI)
    )
    lines = [ln for ln in out.splitlines()]
    assert lines[0] == "certonly", lines
    joined = "\n".join(lines)
    for tok in (
        "--dns-cloudflare",
        "--dns-cloudflare-credentials",
        CF_INI,
        "--dns-cloudflare-propagation-seconds",
        "30",
        "-d",
        HOSTNAME,
        "--non-interactive",
        "--agree-tos",
        "-m",
        EMAIL,
        "--no-eff-email",
    ):
        assert tok in lines, "certbot argv missing %r: %s" % (tok, joined)


def test_deploy_hook_content():
    body = _bash("shading_https_deploy_hook_content")
    assert body.startswith("#!/bin/sh"), body
    assert "set -e" in body, body
    assert "systemctl reload nginx" in body, body


def test_cf_ini_content_shape():
    body = _bash('shading_https_cf_ini_content "%s"' % FAKE_CRED)
    assert body.strip() == "dns_cloudflare_api_token = %s" % FAKE_CRED, body


def test_check_classify():
    # all ok -> OK, exit 0
    r = subprocess.run(
        ["bash", "-c", '. "%s"; shading_https_check_classify ok ok ok ok ok' % LIB],
        capture_output=True, text=True,
    )
    assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
    assert r.stdout.strip() == "OK", r.stdout
    # one fail -> FAIL naming it, exit 1
    r = subprocess.run(
        ["bash", "-c", '. "%s"; shading_https_check_classify ok fail ok ok ok' % LIB],
        capture_output=True, text=True,
    )
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    assert r.stdout.strip() == "FAIL: dns", r.stdout
    # several fail -> all named
    r = subprocess.run(
        ["bash", "-c", '. "%s"; shading_https_check_classify fail ok ok fail fail' % LIB],
        capture_output=True, text=True,
    )
    assert r.returncode == 1
    assert r.stdout.strip() == "FAIL: packages site curl", r.stdout


def test_no_secret_committed_anywhere():
    # No real Cloudflare token-shaped literal in any committed file of this milestone. The token is
    # read from a file at runtime and referenced by path only.
    for p in (SCRIPT, LIB, CONF):
        with open(p, encoding="utf-8") as f:
            txt = f.read()
        # a token value literally assigned into the ini must not be present (only the %s renderer)
        assert not re.search(r"dns_cloudflare_api_token\s*=\s*[A-Za-z0-9_\-]{20,}", txt), p
        # no long secret-shaped blob committed
        assert "eyJ" not in txt, p
    # the installer reads the token from the secret file, never inlines it
    with open(SCRIPT, encoding="utf-8") as f:
        s = f.read()
    assert "cloudflare-newlevel" in s or "CF_TOKEN_FILE" in s, s


def test_provision_script_sources_lib_and_has_modes():
    with open(SCRIPT, encoding="utf-8") as f:
        s = f.read()
    assert "shading-https.sh" in s, "script must source the shared helper"
    assert "--check" in s and "--install" in s
    assert "cli_cloudflare_dns" in s, "install must use the airuleset Cloudflare DNS client"
    assert "install -m 600" in s, "the credentials file must be written 0600"
    # never `sudo` inside the script — it writes /etc directly with the privilege it is run with
    assert not re.search(r"\bsudo\b", s), "script must not invoke sudo"


def test_no_bluetooth_anywhere():
    for p in (SCRIPT, LIB, CONF):
        with open(p, encoding="utf-8") as f:
            txt = f.read().lower()
        assert "bluetooth" not in txt and "ble" not in txt.split(), p


def _fake_bin(record_path, name, extra="", body_first=""):
    """A stand-in executable that records its argv (one call per line) and succeeds."""
    d = tempfile.mkdtemp()
    p = os.path.join(d, name)
    with open(p, "w", encoding="utf-8") as f:
        f.write(
            "#!/usr/bin/env bash\n%s"
            'printf "%%s\\n" "$*" >> "%s"\n%s' % (body_first, record_path, extra)
        )
    os.chmod(p, 0o755)
    return p


def _fake_airuleset(root):
    """A temp dir holding a fake `cli_cloudflare_dns` module that records ensure_record kwargs to a
    JSON line and returns ok — so the installer's REAL python snippet is exercised without the API."""
    d = os.path.join(root, "airuleset")
    os.makedirs(d, exist_ok=True)
    rec = os.path.join(root, "dns-call.json")
    with open(os.path.join(d, "cli_cloudflare_dns.py"), "w", encoding="utf-8") as f:
        f.write(
            "import json, os\n"
            "def _load_token(path):\n"
            "    with open(os.path.expanduser(path)) as fh:\n"
            "        return fh.read().strip()\n"
            "class DnsClient:\n"
            "    def __init__(self, token=None, transport=None):\n"
            "        self.token = token\n"
            "def ensure_record(client, zone_name, name, rtype, content, proxied,\n"
            "                  comment='', dry_run=True):\n"
            "    with open(%r, 'w') as fh:\n"
            "        json.dump({'zone_name': zone_name, 'name': name, 'rtype': rtype,\n"
            "                   'content': content, 'proxied': proxied, 'dry_run': dry_run,\n"
            "                   'token_len': len(client.token or '')}, fh)\n"
            "    return {'ok': True, 'action': ('would_create' if dry_run else 'created'),\n"
            "            'record_id': 'rec1', 'error': None}\n" % rec
        )
    return d, rec


def _install_env(root, calls, dryrun=False):
    apt = _fake_bin(calls, "apt-get")
    certbot = _fake_bin(calls, "certbot")
    nginx = _fake_bin(calls, "nginx")  # `nginx -t` -> exit 0
    systemctl = _fake_bin(calls, "systemctl")
    airu, rec = _fake_airuleset(root)
    token_file = os.path.join(root, "cloudflare-newlevel")
    with open(token_file, "w", encoding="utf-8") as f:
        f.write(FAKE_CRED + "\n")
    os.chmod(token_file, 0o600)
    env = dict(
        os.environ,
        SHADING_HTTPS_APT=apt,
        SHADING_HTTPS_CERTBOT=certbot,
        SHADING_HTTPS_NGINX=nginx,
        SHADING_HTTPS_SYSTEMCTL=systemctl,
        SHADING_HTTPS_PYTHON="python3",
        SHADING_HTTPS_AIRULESET_DIR=airu,
        SHADING_HTTPS_CF_TOKEN_FILE=token_file,
        SHADING_HTTPS_CF_INI=os.path.join(root, "etc", "cloudflare.ini"),
        SHADING_HTTPS_SITE_AVAILABLE=os.path.join(root, "nginx", "sites-available", "shading"),
        SHADING_HTTPS_SITE_ENABLED=os.path.join(root, "nginx", "sites-enabled", "shading"),
        SHADING_HTTPS_DEFAULT_ENABLED=os.path.join(root, "nginx", "sites-enabled", "default"),
        SHADING_HTTPS_DEPLOY_HOOK=os.path.join(root, "etc", "hooks", "nginx-reload.sh"),
        SHADING_HTTPS_CERT_DIR=os.path.join(root, "etc", "live", HOSTNAME),
    )
    return env, rec


def test_install_end_to_end():
    root = tempfile.mkdtemp()
    try:
        calls = os.path.join(root, "calls.log")
        env, rec = _install_env(root, calls)
        # a pre-existing default site symlink must be removed
        os.makedirs(os.path.join(root, "nginx", "sites-enabled"), exist_ok=True)
        default_link = env["SHADING_HTTPS_DEFAULT_ENABLED"]
        real_default = os.path.join(root, "nginx", "default-real")
        with open(real_default, "w") as f:
            f.write("x")
        os.symlink(real_default, default_link)

        r = subprocess.run(["bash", SCRIPT, "--install"], capture_output=True, text=True, env=env)
        assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)

        # site file written == committed conf
        with open(env["SHADING_HTTPS_SITE_AVAILABLE"], encoding="utf-8") as f:
            site = f.read()
        with open(CONF, encoding="utf-8") as f:
            assert site == f.read(), "installed site file != committed conf"
        # sites-enabled symlink present + resolves
        assert os.path.islink(env["SHADING_HTTPS_SITE_ENABLED"])
        assert os.path.exists(env["SHADING_HTTPS_SITE_ENABLED"])
        # default site removed
        assert not os.path.lexists(default_link), "default site symlink must be removed"

        # cloudflare.ini written 0600 with the token, and the token NEVER echoed
        ini = env["SHADING_HTTPS_CF_INI"]
        assert os.path.isfile(ini)
        mode = stat.S_IMODE(os.stat(ini).st_mode)
        assert mode == 0o600, "cloudflare.ini mode %o != 600" % mode
        with open(ini, encoding="utf-8") as f:
            assert "dns_cloudflare_api_token = %s" % FAKE_CRED in f.read()
        assert FAKE_CRED not in (r.stdout + r.stderr), "token must NEVER be printed"

        # deploy hook written 755 with the reload
        hook = env["SHADING_HTTPS_DEPLOY_HOOK"]
        assert os.path.isfile(hook)
        assert stat.S_IMODE(os.stat(hook).st_mode) == 0o755
        with open(hook, encoding="utf-8") as f:
            assert "systemctl reload nginx" in f.read()

        # DNS record created via the real snippet against the fake module
        import json
        with open(rec, encoding="utf-8") as f:
            dns = json.load(f)
        assert dns["name"] == HOSTNAME, dns
        assert dns["content"] == LAN_IP, dns
        assert dns["rtype"] == "A", dns
        assert dns["proxied"] is False, dns
        assert dns["dry_run"] is False, dns
        assert dns["token_len"] == len(FAKE_CRED), dns

        # certbot + nginx -t + reload all invoked
        with open(calls, encoding="utf-8") as f:
            log = f.read()
        assert "certonly" in log, log
        assert "-d %s" % HOSTNAME in log or ("-d" in log and HOSTNAME in log), log
        assert "-t" in log, "nginx -t must run"
        assert "reload nginx" in log, log
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_dry_run_dns():
    root = tempfile.mkdtemp()
    try:
        calls = os.path.join(root, "calls.log")
        env, rec = _install_env(root, calls)
        r = subprocess.run(
            ["bash", SCRIPT, "--install", "--dry-run"], capture_output=True, text=True, env=env
        )
        assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
        import json
        with open(rec, encoding="utf-8") as f:
            dns = json.load(f)
        assert dns["dry_run"] is True, dns
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_fails_when_token_file_missing():
    root = tempfile.mkdtemp()
    try:
        calls = os.path.join(root, "calls.log")
        env, _rec = _install_env(root, calls)
        env["SHADING_HTTPS_CF_TOKEN_FILE"] = os.path.join(root, "does-not-exist")
        r = subprocess.run(["bash", SCRIPT, "--install"], capture_output=True, text=True, env=env)
        assert r.returncode != 0, (r.returncode, r.stdout, r.stderr)
        assert "token file" in (r.stdout + r.stderr).lower()
    finally:
        shutil.rmtree(root, ignore_errors=True)


def _check_env(root, packages=True, dns=True, cert=True, site=True, curl=True):
    calls = os.path.join(root, "calls.log")
    nginx = _fake_bin(calls, "nginx") if site else _fake_bin(calls, "nginx", extra="exit 1\n")
    certbot = _fake_bin(calls, "certbot")
    getent = _fake_bin(
        calls, "getent",
        body_first=('echo "%s %s"\n' % (LAN_IP if dns else "1.2.3.4", HOSTNAME)),
    )
    curl_bin = _fake_bin(
        calls, "curl", body_first=('printf "%s"\n' % ("200" if curl else "502")),
    )
    cert_dir = os.path.join(root, "etc", "live", HOSTNAME)
    if cert:
        os.makedirs(cert_dir, exist_ok=True)
        with open(os.path.join(cert_dir, "fullchain.pem"), "w") as f:
            f.write("cert")
    site_enabled = os.path.join(root, "nginx", "sites-enabled", "shading")
    site_available = os.path.join(root, "nginx", "sites-available", "shading")
    if site:
        os.makedirs(os.path.dirname(site_available), exist_ok=True)
        os.makedirs(os.path.dirname(site_enabled), exist_ok=True)
        with open(site_available, "w") as f:
            f.write("site")
        os.symlink(site_available, site_enabled)
    env = dict(
        os.environ,
        SHADING_HTTPS_NGINX=(nginx if packages else "/nonexistent/nginx"),
        SHADING_HTTPS_CERTBOT=(certbot if packages else "/nonexistent/certbot"),
        SHADING_HTTPS_GETENT=getent,
        SHADING_HTTPS_CURL=curl_bin,
        SHADING_HTTPS_CERT_DIR=cert_dir,
        SHADING_HTTPS_SITE_ENABLED=site_enabled,
    )
    return env


def test_check_passes_when_all_present():
    root = tempfile.mkdtemp()
    try:
        env = _check_env(root)
        r = subprocess.run(["bash", SCRIPT, "--check"], capture_output=True, text=True, env=env)
        assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
        assert "OK" in r.stdout
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_check_fails_when_cert_missing():
    root = tempfile.mkdtemp()
    try:
        env = _check_env(root, cert=False)
        r = subprocess.run(["bash", SCRIPT, "--check"], capture_output=True, text=True, env=env)
        assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
        assert "--install" in (r.stdout + r.stderr)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_check_fails_when_dns_wrong():
    root = tempfile.mkdtemp()
    try:
        env = _check_env(root, dns=False)
        r = subprocess.run(["bash", SCRIPT, "--check"], capture_output=True, text=True, env=env)
        assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_unknown_arg_exits_2():
    r = subprocess.run(["bash", SCRIPT, "--bogus"], capture_output=True, text=True)
    assert r.returncode == 2, (r.returncode, r.stdout, r.stderr)


def test_missing_option_value_exits_2():
    r = subprocess.run(
        ["bash", SCRIPT, "--install", "--hostname"], capture_output=True, text=True
    )
    assert r.returncode == 2, (r.returncode, r.stdout, r.stderr)


# ============================================================================================
# issue 1345 M3b — the SECOND site: interkom.newlevel.media (the phone PWA + the Janus /janus WS
# proxy). The renderer is GENERALISED to take an extra-location block; the shading site (empty
# extra) stays byte-identical, and the interkom site adds the `/janus` upgrade-passthrough block.
# ============================================================================================

INTERKOM_CONF = os.path.join(REPO, "scripts", "nginx", "interkom.newlevel.media.conf")
# issue 1345 M4: the PRIMARY (cert-keyed) name is interkom-lx.newlevel.media; the crew production
# names interkom.newlevel.media + interkom-snv.newlevel.media are ALIASES (extra server_name SANs on
# the SAME cert, expanded live). The upstreams follow the strih identity via the router-resolvable
# strih.lan (never the retired 10.77.9.203, never a literal .202). interkom-pp stays on VDO.Ninja
# until the Poprad rework (~4.10.2026) — it must NEVER be an alias.
INTERKOM_HOST = "interkom-lx.newlevel.media"
INTERKOM_ALIASES = ["interkom.newlevel.media", "interkom-snv.newlevel.media"]
INTERKOM_PP = "interkom-pp.newlevel.media"
INTERKOM_UPSTREAM = "http://strih.lan:8790"
INTERKOM_JANUS = "http://strih.lan:8188"
INTERKOM_SITE_NAME = "interkom"
RETIRED_IPS = ("10.77.9.203", "10.77.9.202")


def test_interkom_lib_constants():
    assert _bash("interkom_https_hostname").strip() == INTERKOM_HOST
    assert _bash("interkom_https_upstream").strip() == INTERKOM_UPSTREAM
    assert _bash("interkom_https_janus_upstream").strip() == INTERKOM_JANUS
    assert _bash("interkom_https_site_name").strip() == INTERKOM_SITE_NAME
    # issue 1345 M4: the default alias set = the two crew production names, interkom-pp EXCLUDED.
    assert _bash("interkom_https_aliases").split() == INTERKOM_ALIASES
    assert INTERKOM_PP not in _bash("interkom_https_aliases")


def test_generalised_renderer_empty_extra_is_byte_identical_shading():
    # DRIFT GUARD (shading side): the generalised renderer with NO extra locations must still
    # reproduce the committed shading site byte-for-byte — the shading site is untouched.
    rendered = _bash(
        'shading_https_site_content "$(shading_https_hostname)" "$(shading_https_upstream)" ""'
    )
    with open(CONF, encoding="utf-8") as f:
        assert f.read() == rendered, "shading site drifted when the renderer gained an extra arg"


def test_interkom_committed_conf_equals_lib_render():
    # DRIFT GUARD (interkom side): the committed interkom nginx site == the lib's interkom render.
    assert os.path.isfile(INTERKOM_CONF), INTERKOM_CONF
    rendered = _bash("interkom_https_site_content")
    with open(INTERKOM_CONF, encoding="utf-8") as f:
        assert f.read() == rendered, "scripts/nginx/interkom.newlevel.media.conf drifted from the lib"


def test_interkom_site_has_janus_upgrade_block():
    body = _bash("interkom_https_site_content")
    # issue 1345 M4: server_name (BOTH the :80 and :443 blocks) lists the primary + both crew aliases.
    expected_sn = "server_name %s;" % " ".join([INTERKOM_HOST] + INTERKOM_ALIASES)
    assert body.count(expected_sn) == 2, body
    # the cert paths stay keyed on the PRIMARY name (exactly the live --expand --cert-name).
    assert "ssl_certificate     /etc/letsencrypt/live/%s/fullchain.pem;" % INTERKOM_HOST in body, body
    # the hub upstream on `location /` — the strih.lan identity, never the retired .203 / a literal .202.
    assert "proxy_pass %s;" % INTERKOM_UPSTREAM in body, body
    # the dedicated /janus location proxying to the Janus WS API with HTTP/1.1 Upgrade passthrough.
    # It MUST be an EXACT match (`location = /janus`): a prefix `location /janus` also captures the
    # PWA's vendored `/janus.js` and proxies it to the Janus WS server, which answers 403 to a plain
    # GET -- the phone page then never loads janus.js (live 19.9.2026 on interkom-lx.newlevel.media).
    assert re.search(r"(?m)^\s*location\s+=\s+/janus\s*\{", body), "an EXACT-match /janus location is present"
    assert not re.search(r"(?m)^\s*location\s+/janus\b", body), "no prefix-match /janus location (it would swallow /janus.js)"
    assert "proxy_pass %s;" % INTERKOM_JANUS in body, body
    # WS upgrade passthrough must appear for BOTH the hub (/ + /ws) and Janus (/janus)
    assert body.count("proxy_set_header Upgrade $http_upgrade;") >= 2, body
    assert body.count("proxy_set_header Connection $http_connection;") >= 2, body
    assert body.count("proxy_read_timeout 3600s;") >= 2, body
    # h2 flag rides on listen; never the `http2 on;` directive
    assert "listen 443 ssl http2;" in body, body
    assert re.search(r"(?m)^\s*http2 on;", body) is None, "must not use the `http2 on;` directive"
    assert "return 301 https://$host$request_uri;" in body, body


def _interkom_install_env(root, calls):
    """An install env for the interkom site (dev1 nginx front; the A record still points at dev1)."""
    apt = _fake_bin(calls, "apt-get")
    certbot = _fake_bin(calls, "certbot")
    nginx = _fake_bin(calls, "nginx")
    systemctl = _fake_bin(calls, "systemctl")
    airu, rec = _fake_airuleset(root)
    token_file = os.path.join(root, "cloudflare-newlevel")
    with open(token_file, "w", encoding="utf-8") as f:
        f.write(FAKE_CRED + "\n")
    os.chmod(token_file, 0o600)
    env = dict(
        os.environ,
        SHADING_HTTPS_APT=apt,
        SHADING_HTTPS_CERTBOT=certbot,
        SHADING_HTTPS_NGINX=nginx,
        SHADING_HTTPS_SYSTEMCTL=systemctl,
        SHADING_HTTPS_PYTHON="python3",
        SHADING_HTTPS_AIRULESET_DIR=airu,
        SHADING_HTTPS_CF_TOKEN_FILE=token_file,
        SHADING_HTTPS_CF_INI=os.path.join(root, "etc", "cloudflare.ini"),
        SHADING_HTTPS_SITE_AVAILABLE=os.path.join(root, "nginx", "sites-available", INTERKOM_SITE_NAME),
        SHADING_HTTPS_SITE_ENABLED=os.path.join(root, "nginx", "sites-enabled", INTERKOM_SITE_NAME),
        SHADING_HTTPS_DEFAULT_ENABLED=os.path.join(root, "nginx", "sites-enabled", "default"),
        SHADING_HTTPS_DEPLOY_HOOK=os.path.join(root, "etc", "hooks", "nginx-reload.sh"),
        SHADING_HTTPS_CERT_DIR=os.path.join(root, "etc", "live", INTERKOM_HOST),
    )
    return env, rec


def test_install_site_interkom_writes_the_interkom_conf():
    root = tempfile.mkdtemp()
    try:
        calls = os.path.join(root, "calls.log")
        env, rec = _interkom_install_env(root, calls)
        r = subprocess.run(
            ["bash", SCRIPT, "--install", "--site", "interkom"],
            capture_output=True, text=True, env=env,
        )
        assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
        # the written site file == committed interkom conf
        with open(env["SHADING_HTTPS_SITE_AVAILABLE"], encoding="utf-8") as f:
            site = f.read()
        with open(INTERKOM_CONF, encoding="utf-8") as f:
            assert site == f.read(), "installed interkom site != committed interkom conf"
        assert "location = /janus" in site, "the exact-match /janus block must be in the installed interkom site"
        # the DNS A record is for the interkom host (still dev1's LAN IP)
        import json
        with open(rec, encoding="utf-8") as f:
            dns = json.load(f)
        assert dns["name"] == INTERKOM_HOST, dns
        assert dns["content"] == LAN_IP, dns
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_default_site_is_shading_backward_compatible():
    # No --site flag ⇒ the existing shading behaviour is UNCHANGED (writes the shading conf).
    root = tempfile.mkdtemp()
    try:
        calls = os.path.join(root, "calls.log")
        env, rec = _install_env(root, calls)
        r = subprocess.run(["bash", SCRIPT, "--install"], capture_output=True, text=True, env=env)
        assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
        with open(env["SHADING_HTTPS_SITE_AVAILABLE"], encoding="utf-8") as f:
            site = f.read()
        with open(CONF, encoding="utf-8") as f:
            assert site == f.read(), "default (no --site) must still write the shading conf"
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_interkom_conf_no_secret_no_bluetooth():
    with open(INTERKOM_CONF, encoding="utf-8") as f:
        txt = f.read()
    assert not re.search(r"dns_cloudflare_api_token\s*=\s*[A-Za-z0-9_\-]{20,}", txt)
    assert "bluetooth" not in txt.lower()


# ============================================================================================
# issue 1345 M4 — the interkom front carries the PRODUCTION hostnames + the strih.lan upstream as
# GENERATED config (today's live cut-over did it by hand). First-class aliases: the renderer takes
# an ALIASES arg (server_name HOST ALIASES… in BOTH blocks, empty = byte-identical single-name); the
# certbot argv builder emits -d per SAN + --expand + --cert-name HOST; the installer gains a
# repeatable --alias (+ SHADING_HTTPS_ALIASES env) with per-site interkom defaults; --check reports
# a non-resolving alias without failing; the committed conf is regenerated with the strih.lan
# upstream and NO retired IP.
# ============================================================================================


def test_interkom_aliases_default_excludes_pp():
    aliases = _bash("interkom_https_aliases").split()
    assert aliases == INTERKOM_ALIASES, aliases
    assert INTERKOM_PP not in aliases


def test_renderer_with_aliases_lists_every_name_in_both_server_name_blocks():
    body = _bash(
        'shading_https_site_content "primary.example.org" "http://box.lan:1234" "" "" "" '
        '"a1.example.org a2.example.org"'
    )
    sn = "server_name primary.example.org a1.example.org a2.example.org;"
    # BOTH the :80 redirect block and the :443 proxy block get the full name list.
    assert body.count(sn) == 2, body


def test_renderer_without_aliases_is_byte_identical_single_name():
    # an EMPTY aliases arg (6th) must be byte-identical to omitting it — the shading site is untouched.
    with_empty = _bash('shading_https_site_content "h.example.org" "http://u.lan:1" "" "" "" ""')
    without = _bash('shading_https_site_content "h.example.org" "http://u.lan:1"')
    assert with_empty == without, "empty aliases arg must be byte-identical to omitting it"
    assert with_empty.count("server_name h.example.org;") == 2, with_empty


def test_interkom_certbot_argv_expands_to_every_san():
    out = _bash(
        'shading_https_certbot_argv "%s" "%s" "%s" "30" "%s"'
        % (INTERKOM_HOST, EMAIL, CF_INI, " ".join(INTERKOM_ALIASES))
    )
    lines = out.splitlines()
    assert lines[0] == "certonly", lines
    # -d for the PRIMARY and each alias (one token per line, `-d` then the name)
    for name in [INTERKOM_HOST] + INTERKOM_ALIASES:
        adj = [
            i for i, ln in enumerate(lines)
            if ln == "-d" and i + 1 < len(lines) and lines[i + 1] == name
        ]
        assert adj, "certbot argv missing `-d %s`: %s" % (name, lines)
    # --expand + --cert-name keyed on the PRIMARY (exactly the live expand)
    assert "--expand" in lines, lines
    ci = lines.index("--cert-name")
    assert lines[ci + 1] == INTERKOM_HOST, lines


def test_shading_certbot_argv_unchanged_when_no_aliases():
    # the single-name (shading) certbot invocation stays byte-identical — no --expand / --cert-name.
    out = _bash('shading_https_certbot_argv "%s" "%s" "%s" "30"' % (HOSTNAME, EMAIL, CF_INI))
    lines = out.splitlines()
    assert "--expand" not in lines, lines
    assert "--cert-name" not in lines, lines
    assert lines.count("-d") == 1, lines


def test_no_retired_ip_in_generated_interkom_config():
    # the retired .203 (and a literal .202) must NEVER appear in the lib render OR the committed conf;
    # the upstreams use the router-resolvable strih.lan identity instead.
    rendered = _bash("interkom_https_site_content")
    with open(INTERKOM_CONF, encoding="utf-8") as f:
        committed = f.read()
    for ip in RETIRED_IPS:
        assert ip not in rendered, "retired IP %s in interkom render" % ip
        assert ip not in committed, "retired IP %s in committed interkom conf" % ip
    assert "proxy_pass http://strih.lan:8790;" in committed, committed
    assert "proxy_pass http://strih.lan:8188;" in committed, committed


def test_committed_interkom_conf_has_all_three_server_names():
    with open(INTERKOM_CONF, encoding="utf-8") as f:
        committed = f.read()
    sn = "server_name %s;" % " ".join([INTERKOM_HOST] + INTERKOM_ALIASES)
    assert committed.count(sn) == 2, committed
    assert INTERKOM_PP not in committed, "interkom-pp must not appear (Poprad stays on VDO.Ninja)"


def test_check_interkom_tolerates_non_resolving_alias():
    # issue 1345 M4: a fresh cut-over's alias A records may lag the primary; --check must REPORT a
    # non-resolving alias, never FAIL on it (the verdict is gated on the primary probes only).
    root = tempfile.mkdtemp()
    try:
        calls = os.path.join(root, "calls.log")
        nginx = _fake_bin(calls, "nginx")
        certbot = _fake_bin(calls, "certbot")
        # getent: primary + the first alias resolve to LAN_IP; the second alias does NOT resolve.
        getent = _fake_bin(
            calls, "getent",
            body_first=(
                'if [ "$2" = "%s" ]; then :; else echo "%s $2"; fi\n'
                % (INTERKOM_ALIASES[1], LAN_IP)
            ),
        )
        curl_bin = _fake_bin(calls, "curl", body_first='printf "200"\n')
        cert_dir = os.path.join(root, "etc", "live", INTERKOM_HOST)
        os.makedirs(cert_dir, exist_ok=True)
        with open(os.path.join(cert_dir, "fullchain.pem"), "w") as f:
            f.write("cert")
        site_available = os.path.join(root, "nginx", "sites-available", INTERKOM_SITE_NAME)
        site_enabled = os.path.join(root, "nginx", "sites-enabled", INTERKOM_SITE_NAME)
        os.makedirs(os.path.dirname(site_available), exist_ok=True)
        os.makedirs(os.path.dirname(site_enabled), exist_ok=True)
        with open(site_available, "w") as f:
            f.write("site")
        os.symlink(site_available, site_enabled)
        env = dict(
            os.environ,
            SHADING_HTTPS_NGINX=nginx,
            SHADING_HTTPS_CERTBOT=certbot,
            SHADING_HTTPS_GETENT=getent,
            SHADING_HTTPS_CURL=curl_bin,
            SHADING_HTTPS_CERT_DIR=cert_dir,
            SHADING_HTTPS_SITE_ENABLED=site_enabled,
        )
        r = subprocess.run(
            ["bash", SCRIPT, "--site", "interkom", "--check"],
            capture_output=True, text=True, env=env,
        )
        assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
        assert "OK" in r.stdout, r.stdout
        # the non-resolving alias is REPORTED (report-only), not a failure
        assert INTERKOM_ALIASES[1] in r.stdout, r.stdout
        assert ("lagging" in r.stdout.lower()) or ("report-only" in r.stdout.lower()), r.stdout
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_interkom_certbot_gets_every_san():
    # the interkom install invokes certbot with -d for the primary AND both crew aliases.
    root = tempfile.mkdtemp()
    try:
        calls = os.path.join(root, "calls.log")
        env, _rec = _interkom_install_env(root, calls)
        r = subprocess.run(
            ["bash", SCRIPT, "--install", "--site", "interkom"],
            capture_output=True, text=True, env=env,
        )
        assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
        with open(calls, encoding="utf-8") as f:
            log = f.read()
        for name in [INTERKOM_HOST] + INTERKOM_ALIASES:
            assert ("-d %s" % name) in log, (name, log)
        assert "--expand" in log, log
        assert ("--cert-name %s" % INTERKOM_HOST) in log, log
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_alias_flags_replace_the_per_site_default():
    # an explicit --alias set REPLACES the interkom crew default (same override semantics as
    # --hostname/--upstream): certbot gets the flag aliases and NOT the default crew names.
    root = tempfile.mkdtemp()
    try:
        calls = os.path.join(root, "calls.log")
        env, _rec = _interkom_install_env(root, calls)
        r = subprocess.run(
            ["bash", SCRIPT, "--install", "--site", "interkom",
             "--alias", "one.example.org", "--alias", "two.example.org"],
            capture_output=True, text=True, env=env,
        )
        assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
        with open(calls, encoding="utf-8") as f:
            log = f.read()
        assert "-d one.example.org" in log, log
        assert "-d two.example.org" in log, log
        # the per-site crew default must NOT leak in when explicit flags were given
        for name in INTERKOM_ALIASES:
            assert ("-d %s" % name) not in log, (name, log)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_alias_env_replaces_default_and_first_flag_clears_env():
    # SHADING_HTTPS_ALIASES env seeds the alias set (replacing the crew default); the FIRST --alias
    # flag then CLEARS that env seed and starts fresh from flags.
    root = tempfile.mkdtemp()
    try:
        # env only, no flag: the env seed replaces the crew default
        calls = os.path.join(root, "calls-env.log")
        env, _rec = _interkom_install_env(root, calls)
        env["SHADING_HTTPS_ALIASES"] = "env1.example.org env2.example.org"
        r = subprocess.run(
            ["bash", SCRIPT, "--install", "--site", "interkom"],
            capture_output=True, text=True, env=env,
        )
        assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
        with open(calls, encoding="utf-8") as f:
            log = f.read()
        assert "-d env1.example.org" in log and "-d env2.example.org" in log, log
        for name in INTERKOM_ALIASES:
            assert ("-d %s" % name) not in log, (name, log)

        # env + a flag: the flag clears the env seed and wins
        calls2 = os.path.join(root, "calls-flag.log")
        env2, _rec2 = _interkom_install_env(root, calls2)
        env2["SHADING_HTTPS_ALIASES"] = "env1.example.org"
        r2 = subprocess.run(
            ["bash", SCRIPT, "--install", "--site", "interkom", "--alias", "flag1.example.org"],
            capture_output=True, text=True, env=env2,
        )
        assert r2.returncode == 0, (r2.returncode, r2.stdout, r2.stderr)
        with open(calls2, encoding="utf-8") as f:
            log2 = f.read()
        assert "-d flag1.example.org" in log2, log2
        assert "-d env1.example.org" not in log2, log2
    finally:
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    for _name, _fn in sorted(globals().items()):
        if _name.startswith("test_") and callable(_fn):
            _fn()
            print("ok %s" % _name)
    print("all passed")
