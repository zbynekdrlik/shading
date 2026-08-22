#!/usr/bin/env python3
"""bkshading CLOUDFLARE remote-access provisioning — cloudflared tunnel systemd unit + config +
Access-enforcement gate (issue 808 cloudflare-remote milestone).

Everything merged so far (M1 panel + relay, M2 NDI preview, WS push, #809 fps sync, relay
provisioning, M3 relay deploy) is LAN-only: the aggregation service binds the web panel on
`0.0.0.0:8770` and is reachable only on `strih.lan:8770`. The owner decided remote access goes
through a password-protected **cloudflare proxy** (NOT tailscale — issue 808 comment 5355836067).
This milestone provisions that: a cloudflared tunnel connector (config-file mode) fronting the
local panel, an enable-only systemd unit, and an Access-enforcement gate that refuses to consider a
tunnel "provisioned" unless the operator confirmed the Cloudflare Access password policy is live —
so a naked public tunnel can never be the provisioned state. The connector holds ONLY its
credentials JSON (referenced by path, 0600, NEVER committed); the password is enforced at the
Cloudflare Access layer, not in the service (owner put the protection at the proxy).

These stdlib-only structural + behavioural tests run in the `python-tests` CI job (no Rust
toolchain, no root, no apt, no real systemd, no cloudflared — the impure ops are fully overridable
to a temp root):
 - the provision script + helper parse (`bash -n`);
 - the pure helper's constants + config.yml composer + credentials-path parser are correct;
 - the systemd unit + script + helper AGREE on the bin path / unit name / config path / access
   marker so the three sources of truth cannot silently drift;
 - the service origin PORT AGREES with the appliance service `default_bind` (config.rs) — one
   source of truth for where the tunnel points;
 - NO secret (tunnel token / credentials blob) is committed anywhere — only references-by-path;
 - `--install` is ENABLE-ONLY (never `systemctl start`/`restart`) per provisioning-scripts.md;
 - `--install` REFUSES without `--access-confirmed`, and `--check` FAILS without the Access marker
   (the password requirement is enforced, not merely documented);
 - Bluetooth appears NOWHERE (owner hard rule).
Runnable directly (`python3 tests/python/test_bkshading_cloudflared_provision_808.py`) or under
pytest.
"""
import os
import re
import shutil
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
SCRIPT = os.path.join(REPO, "scripts", "bkshading-provision-cloudflared.sh")
LIB = os.path.join(REPO, "scripts", "lib", "bkshading-cloudflared-runtime.sh")
UNIT = os.path.join(REPO, "systemd", "bkshading-cloudflared.service")
CONFIG_RS = os.path.join(REPO, "bkshading", "service", "src", "config.rs")

BIN_PATH = "/usr/local/bin/cloudflared"
UNIT_NAME = "bkshading-cloudflared.service"
CONFIG_PATH = "/etc/bkshading/cloudflared-config.yml"
ACCESS_MARKER = "/etc/bkshading/cloudflared-access-confirmed"
APT_PKG = "cloudflared"
SERVICE_PORT = "8770"
DEFAULT_ORIGIN = "http://localhost:8770"


def _bash(snippet):
    """Source the helper, run `snippet`, return stdout (raises on nonzero)."""
    src = '. "%s"\n%s' % (LIB, snippet)
    out = subprocess.run(
        ["bash", "-c", src], capture_output=True, text=True, check=True
    )
    return out.stdout


def test_files_exist_and_parse():
    for p in (SCRIPT, LIB, UNIT):
        assert os.path.isfile(p), p
    for p in (SCRIPT, LIB):
        r = subprocess.run(["bash", "-n", p], capture_output=True, text=True)
        assert r.returncode == 0, "bash -n %s: %s" % (p, r.stderr)


def test_lib_constants():
    assert _bash("bkshading_cloudflared_bin_path").strip() == BIN_PATH
    assert _bash("bkshading_cloudflared_unit_name").strip() == UNIT_NAME
    assert _bash("bkshading_cloudflared_config_path").strip() == CONFIG_PATH
    assert _bash("bkshading_cloudflared_access_marker_path").strip() == ACCESS_MARKER
    assert _bash("bkshading_cloudflared_apt_package").strip() == APT_PKG
    assert _bash("bkshading_cloudflared_service_port").strip() == SERVICE_PORT
    assert _bash("bkshading_cloudflared_default_origin").strip() == DEFAULT_ORIGIN


def test_service_port_matches_appliance_default_bind():
    # ONE source of truth: the tunnel origin port must equal the service's own default_bind port,
    # so the tunnel points at exactly where the panel listens.
    with open(CONFIG_RS, encoding="utf-8") as f:
        cfg = f.read()
    assert '"0.0.0.0:%s"' % SERVICE_PORT in cfg, (
        "bkshading service default_bind changed — update the cloudflared origin port too"
    )


def test_service_origin_for_host_is_pure():
    out = _bash('bkshading_cloudflared_service_origin_for_host strih.lan').strip()
    assert out == "http://strih.lan:%s" % SERVICE_PORT, out


def test_config_composer_wires_ingress_and_never_emits_a_secret():
    snippet = (
        'bkshading_cloudflared_config_content '
        '"church-shading" "shading.example.org" '
        '"/etc/bkshading/church-shading.json" "http://localhost:8770"'
    )
    body = _bash(snippet)
    assert "tunnel: church-shading" in body, body
    assert "credentials-file: /etc/bkshading/church-shading.json" in body, body
    assert "hostname: shading.example.org" in body, body
    assert "service: http://localhost:8770" in body, body
    # a catch-all 404 must terminate the ingress list (cloudflared requires it)
    assert "service: http_status:404" in body, body
    # the composer references the credentials FILE by path — it must NEVER embed a token/secret.
    assert "token" not in body.lower(), body
    assert "TunnelSecret" not in body, body


def test_creds_file_parser_reads_the_referenced_path():
    body = _bash(
        'bkshading_cloudflared_config_content "t" "h.example.org" '
        '"/etc/bkshading/t.json" "http://localhost:8770"'
    )
    # feed the composed config back through the parser via a temp file-less pipe helper
    src = '. "%s"\nprintf "%%s" "$CFG" | bkshading_cloudflared_creds_file_from_config' % LIB
    r = subprocess.run(
        ["bash", "-c", src],
        capture_output=True,
        text=True,
        env=dict(os.environ, CFG=body),
    )
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "/etc/bkshading/t.json", r.stdout


def test_unit_wires_the_lib_constants_no_drift_and_carries_no_secret():
    with open(UNIT, encoding="utf-8") as f:
        u = f.read()
    assert (
        "ExecStart=%s tunnel --no-autoupdate --config %s run" % (BIN_PATH, CONFIG_PATH)
    ) in u, u
    assert "Restart=always" in u
    assert "WantedBy=multi-user.target" in u
    assert "SyslogIdentifier=bkshading-cloudflared" in u
    # the unit must NOT carry a token/credential inline — credentials live in the referenced
    # credentials-file (0600), never in a committed unit.
    assert "--token" not in u, u
    assert not re.search(r"(?i)tunnel[_-]?token", u), u
    assert "TunnelSecret" not in u, u


def test_no_secret_committed_anywhere():
    # No token/credential-shaped literal in any committed file of this milestone. cloudflared
    # tokens are long base64url blobs; the credentials JSON has a TunnelSecret. We assert only
    # references-by-path exist.
    for p in (SCRIPT, LIB, UNIT):
        with open(p, encoding="utf-8") as f:
            txt = f.read()
        assert "TunnelSecret" not in txt, p
        assert not re.search(r"--token\s+[A-Za-z0-9_\-]{20,}", txt), p
        # an "eyJ..."-style JWT/token blob must not be present
        assert not re.search(r"eyJ[A-Za-z0-9_\-]{20,}", txt), p


def test_provision_script_sources_lib_and_is_enable_only():
    with open(SCRIPT, encoding="utf-8") as f:
        s = f.read()
    assert "bkshading-cloudflared-runtime.sh" in s, "script must source the shared helper"
    assert "--check" in s and "--install" in s
    assert "--access-confirmed" in s, "install must gate on the Access confirmation"
    assert APT_PKG in s, "install must reference the cloudflared package"
    # ENABLE-ONLY (provisioning-scripts.md): never live-start/restart the tunnel — defer to reboot.
    assert not re.search(r"\bstart\s+bkshading-cloudflared", s), "must not systemctl-start"
    assert not re.search(r"\brestart\s+bkshading-cloudflared", s), "must not systemctl-restart"


def test_no_bluetooth_anywhere():
    for p in (SCRIPT, LIB, UNIT):
        with open(p, encoding="utf-8") as f:
            txt = f.read().lower()
        assert "bluetooth" not in txt and "ble" not in txt.split(), p


def _fake_bin(record_path, name, extra=""):
    """A stand-in executable that records its argv (one call per line) and succeeds."""
    d = tempfile.mkdtemp()
    p = os.path.join(d, name)
    with open(p, "w", encoding="utf-8") as f:
        f.write(
            "#!/usr/bin/env bash\n"
            'printf "%%s\\n" "$*" >> "%s"\n%s' % (record_path, extra)
        )
    os.chmod(p, 0o755)
    return p


def _fake_systemctl(record_path):
    return _fake_bin(
        record_path,
        "systemctl",
        'if [ "$1" = "is-enabled" ]; then echo enabled; fi\n',
    )


def _run_provision(args, root, make_creds=True):
    sysd = os.path.join(root, "systemd-system")
    config_file = os.path.join(root, "bkshading", "cloudflared-config.yml")
    marker = os.path.join(root, "bkshading", "cloudflared-access-confirmed")
    creds = os.path.join(root, "bkshading", "church-shading.json")
    calls = os.path.join(root, "systemctl-calls.log")
    sc = _fake_systemctl(calls)
    if make_creds:
        os.makedirs(os.path.dirname(creds), exist_ok=True)
        with open(creds, "w", encoding="utf-8") as f:
            f.write("{}\n")  # a stand-in credentials JSON (content irrelevant to the test)
        os.chmod(creds, 0o600)
    env = dict(
        os.environ,
        BKSHADING_CF_UNIT_DEST=os.path.join(sysd, UNIT_NAME),
        BKSHADING_CF_CONFIG_FILE=config_file,
        BKSHADING_CF_ACCESS_MARKER=marker,
        BKSHADING_CF_CLOUDFLARED="true",  # command -v succeeds -> apt install skipped
        BKSHADING_CF_SYSTEMCTL=sc,
    )
    r = subprocess.run(
        ["bash", SCRIPT] + args, capture_output=True, text=True, env=env
    )
    return r, calls, config_file, marker, creds


def test_check_fails_with_remediation_when_unprovisioned():
    root = tempfile.mkdtemp()
    try:
        r, _c, _cfg, _m, _cr = _run_provision(["--check"], root, make_creds=False)
        assert r.returncode != 0, (r.returncode, r.stdout, r.stderr)
        assert "bkshading-provision-cloudflared.sh --install" in (r.stdout + r.stderr)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_requires_access_confirmed():
    root = tempfile.mkdtemp()
    try:
        # install WITHOUT --access-confirmed must refuse (the password requirement) and NOT enable.
        r, calls, cfg, marker, creds = _run_provision(
            [
                "--install",
                "--hostname", "shading.example.org",
                "--tunnel", "church-shading",
                "--credentials-file", os.path.join(root, "bkshading", "church-shading.json"),
            ],
            root,
        )
        assert r.returncode != 0, (r.returncode, r.stdout, r.stderr)
        assert not os.path.isfile(marker), "access marker must NOT be written without confirmation"
        # nothing enabled either
        log = ""
        if os.path.isfile(calls):
            with open(calls, encoding="utf-8") as f:
                log = f.read()
        assert "enable" not in log, log
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_then_check_end_to_end_enable_only():
    root = tempfile.mkdtemp()
    try:
        creds_path = os.path.join(root, "bkshading", "church-shading.json")
        r, calls, cfg, marker, creds = _run_provision(
            [
                "--install",
                "--hostname", "shading.example.org",
                "--tunnel", "church-shading",
                "--credentials-file", creds_path,
                "--origin", "http://localhost:8770",
                "--access-confirmed",
            ],
            root,
        )
        assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
        # unit installed
        assert os.path.isfile(os.path.join(root, "systemd-system", UNIT_NAME))
        # config.yml composed with the ingress + creds reference
        with open(cfg, encoding="utf-8") as f:
            cfgtxt = f.read()
        assert "hostname: shading.example.org" in cfgtxt, cfgtxt
        assert "credentials-file: %s" % creds_path in cfgtxt, cfgtxt
        assert "service: http://localhost:8770" in cfgtxt, cfgtxt
        assert "service: http_status:404" in cfgtxt, cfgtxt
        # access marker written (operator confirmed the Access policy is live)
        assert os.path.isfile(marker), "access marker must be written on confirmed install"
        # ENABLE-ONLY: daemon-reload + enable, NEVER start/restart
        with open(calls, encoding="utf-8") as f:
            log = f.read()
        assert "daemon-reload" in log, log
        assert re.search(r"\benable\b", log), log
        assert "start" not in log, log
        assert "restart" not in log, log
        # a subsequent --check on the freshly provisioned temp root passes.
        r2, _c2, _cfg2, _m2, _cr2 = _run_provision(["--check"], root)
        assert r2.returncode == 0, (r2.returncode, r2.stdout, r2.stderr)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_check_fails_when_creds_file_wrong_mode():
    root = tempfile.mkdtemp()
    try:
        creds_path = os.path.join(root, "bkshading", "church-shading.json")
        _run_provision(
            [
                "--install",
                "--hostname", "shading.example.org",
                "--tunnel", "church-shading",
                "--credentials-file", creds_path,
                "--access-confirmed",
            ],
            root,
        )
        # loosen the credentials file mode -> --check must FAIL (a secret must stay 0600).
        # make_creds=False so the check run does NOT re-create the file back at 0600.
        os.chmod(creds_path, 0o644)
        r2, _c2, _cfg2, _m2, _cr2 = _run_provision(["--check"], root, make_creds=False)
        assert r2.returncode != 0, (r2.returncode, r2.stdout, r2.stderr)
        assert "0600" in (r2.stdout + r2.stderr) or "mode" in (r2.stdout + r2.stderr).lower()
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_unknown_arg_exits_2():
    r = subprocess.run(["bash", SCRIPT, "--bogus"], capture_output=True, text=True)
    assert r.returncode == 2, r.returncode


def test_missing_option_value_exits_2():
    # a trailing option with no value must exit 2 (bad args), not abort silently under set -e.
    r = subprocess.run(
        ["bash", SCRIPT, "--install", "--hostname"], capture_output=True, text=True
    )
    assert r.returncode == 2, (r.returncode, r.stdout, r.stderr)


if __name__ == "__main__":
    for _name, _fn in sorted(globals().items()):
        if _name.startswith("test_") and callable(_fn):
            _fn()
            print("ok %s" % _name)
    print("all passed")
