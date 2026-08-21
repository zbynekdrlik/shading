#!/usr/bin/env python3
"""bkshading RELAY provisioning — systemd unit + gphoto2 runtime + CAMERA_BOX_CAPTURE_FPS env
(issue 808 relay-provisioning milestone; unblocks the issue-809 live grab derive).

M1 shipped the `bkshading-relay` binary (it already reads `CAMERA_BOX_CAPTURE_FPS` from its env
via `parse_capture_fps_env` and exposes the shading HTTP API over the `gphoto2` CLI), but there
was NO systemd unit to run it on a cambox, no gphoto2 runtime install, and no env wiring — so on a
real box the relay never ran and the service's issue-809 `resolve_grab` saw `capture_fps == None`
and fell back to the static config. This milestone provisions the relay: a static systemd unit, a
provision/verify script, and a sourceable pure helper that derives the env value from the box's own
appliance capture-fps config (ONE source of truth — mirrors `requested_capture_denominator`).

These stdlib-only structural + behavioural tests run in the `python-tests` CI job (no Rust
toolchain, no root, no apt, no real systemd — the impure ops are fully overridable to a temp root):
 - the provision script + helper parse (`bash -n`);
 - the pure helper's derive/parse/compose are correct;
 - the systemd unit + script + helper AGREE on the bin path / port / env path / unit name so the
   three sources of truth cannot silently drift;
 - the env default AGREES with the appliance's `requested_capture_denominator` default (src/capture.rs);
 - `--install` is ENABLE-ONLY (never `systemctl start`/`restart`) per provisioning-scripts.md;
 - Bluetooth appears NOWHERE (owner hard rule).
Runnable directly (`python3 tests/python/test_bkshading_relay_provision_808.py`) or under pytest.
"""
import os
import re
import shutil
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
SCRIPT = os.path.join(REPO, "scripts", "bkshading-provision-relay.sh")
LIB = os.path.join(REPO, "scripts", "lib", "bkshading-relay-runtime.sh")
UNIT = os.path.join(REPO, "systemd", "bkshading-relay.service")
CAPTURE_RS = os.path.join(REPO, "src", "capture.rs")

BIN_PATH = "/usr/local/bin/bkshading-relay"
ENV_PATH = "/etc/bkshading/relay.env"
UNIT_NAME = "bkshading-relay.service"
PORT = "8771"
APT_PKG = "gphoto2"
DEFAULT_FPS = "60"


def _bash(snippet):
    """Source the helper, run `snippet`, return stripped stdout (raises on nonzero)."""
    src = '. "%s"\n%s' % (LIB, snippet)
    out = subprocess.run(
        ["bash", "-c", src], capture_output=True, text=True, check=True
    )
    return out.stdout


def _bash_arg(func, *args):
    """Call a helper fn with args passed via env (no quoting pain); returns (rc, stdout)."""
    src = '. "%s"\n%s "$A1"' % (LIB, func)
    env = dict(os.environ, A1=args[0] if args else "")
    r = subprocess.run(["bash", "-c", src], capture_output=True, text=True, env=env)
    return r.returncode, r.stdout.strip()


def test_files_exist_and_parse():
    for p in (SCRIPT, LIB, UNIT):
        assert os.path.isfile(p), p
    for p in (SCRIPT, LIB):
        r = subprocess.run(["bash", "-n", p], capture_output=True, text=True)
        assert r.returncode == 0, "bash -n %s: %s" % (p, r.stderr)


def test_lib_constants():
    assert _bash("bkshading_relay_bin_path").strip() == BIN_PATH
    assert _bash("bkshading_relay_env_path").strip() == ENV_PATH
    assert _bash("bkshading_relay_unit_name").strip() == UNIT_NAME
    assert _bash("bkshading_relay_port").strip() == PORT
    assert _bash("bkshading_relay_apt_package").strip() == APT_PKG
    assert _bash("bkshading_relay_default_capture_fps").strip() == DEFAULT_FPS


def test_capture_fps_from_dropins_last_wins_and_empty_on_none():
    # A systemd Environment= drop-in line — the parser pulls the value after the key.
    rc, v = _bash_arg(
        "bkshading_relay_capture_fps_from_dropins",
        "Environment=CAMERA_BOX_GENLOCK_FPS=30\nEnvironment=CAMERA_BOX_CAPTURE_FPS=50\n",
    )
    assert rc == 0 and v == "50", (rc, v)
    # last wins if two set it
    rc, v = _bash_arg(
        "bkshading_relay_capture_fps_from_dropins",
        "CAMERA_BOX_CAPTURE_FPS=30\nCAMERA_BOX_CAPTURE_FPS=25\n",
    )
    assert rc == 0 and v == "25", (rc, v)
    # no match -> empty, and MUST stay exit 0 under the caller's pipefail (the || true footgun)
    rc, v = _bash_arg(
        "bkshading_relay_capture_fps_from_dropins",
        "Environment=CAMERA_BOX_GENLOCK_FPS=30\n",
    )
    assert rc == 0 and v == "", (rc, v)


def test_effective_capture_fps_mirrors_requested_denominator():
    # requested_capture_denominator(override): override.filter(|f| f>0).unwrap_or(60)
    assert _bash_arg("bkshading_relay_effective_capture_fps", "30")[1] == "30"
    assert _bash_arg("bkshading_relay_effective_capture_fps", "60")[1] == "60"
    assert _bash_arg("bkshading_relay_effective_capture_fps", "0")[1] == DEFAULT_FPS
    assert _bash_arg("bkshading_relay_effective_capture_fps", "")[1] == DEFAULT_FPS
    assert _bash_arg("bkshading_relay_effective_capture_fps", "abc")[1] == DEFAULT_FPS
    assert _bash_arg("bkshading_relay_effective_capture_fps", "-5")[1] == DEFAULT_FPS


def test_decimal_dropin_matches_appliance_all_or_nothing():
    # A malformed decimal drop-in must derive the SAME rate the appliance would: the appliance's
    # `parse::<u32>("30.5")` fails -> None -> requested_capture_denominator -> 60. The parser hands
    # the WHOLE token ("30.5", not a prefix-matched "30") to effective, which rejects it -> 60.
    rc, tok = _bash_arg(
        "bkshading_relay_capture_fps_from_dropins",
        "Environment=CAMERA_BOX_CAPTURE_FPS=30.5\n",
    )
    assert rc == 0 and tok == "30.5", (rc, tok)  # full token, NOT a truncated "30"
    assert _bash_arg("bkshading_relay_effective_capture_fps", "30.5")[1] == DEFAULT_FPS


def test_env_file_content_carries_the_fps():
    body = _bash_arg("bkshading_relay_env_file_content", "50")[1]
    assert "CAMERA_BOX_CAPTURE_FPS=50" in body, body
    # a bare default when no arg
    body_def = _bash("bkshading_relay_env_file_content")
    assert "CAMERA_BOX_CAPTURE_FPS=%s" % DEFAULT_FPS in body_def, body_def


def test_unit_wires_the_lib_constants_no_drift():
    with open(UNIT, encoding="utf-8") as f:
        u = f.read()
    assert "ExecStart=%s --bind 0.0.0.0:%s" % (BIN_PATH, PORT) in u, u
    # EnvironmentFile is OPTIONAL (leading '-') so an unprovisioned box degrades to None, not a
    # wrong value.
    assert "EnvironmentFile=-%s" % ENV_PATH in u, u
    assert "Restart=always" in u
    assert "WantedBy=multi-user.target" in u
    assert "SyslogIdentifier=bkshading-relay" in u
    # unit must NOT hard-code a capture fps (Environment=CAMERA_BOX_CAPTURE_FPS=...) — that is the
    # rejected Approach 3 (duplicate truth); it comes from the derived EnvironmentFile.
    assert not re.search(r"^Environment=CAMERA_BOX_CAPTURE_FPS=", u, re.M), u


def test_default_capture_fps_matches_appliance_requested_denominator():
    # One source of truth: the env default must equal the appliance's own default so a box with no
    # capture-fps drop-in reports the SAME rate it actually grabs at.
    with open(CAPTURE_RS, encoding="utf-8") as f:
        cap = f.read()
    assert "unwrap_or(60)" in cap, "appliance default changed — update the relay env default too"
    assert DEFAULT_FPS == "60"


def test_provision_script_sources_lib_and_is_enable_only():
    with open(SCRIPT, encoding="utf-8") as f:
        s = f.read()
    assert "bkshading-relay-runtime.sh" in s, "script must source the shared helper"
    assert "--check" in s and "--install" in s
    assert APT_PKG in s, "install must reference the gphoto2 apt package"
    # ENABLE-ONLY (provisioning-scripts.md): never live-start/restart the relay — defer to reboot.
    assert not re.search(r"\bstart\s+bkshading-relay", s), "must not systemctl-start the relay"
    assert not re.search(r"\brestart\s+bkshading-relay", s), "must not systemctl-restart the relay"


def test_no_bluetooth_anywhere():
    for p in (SCRIPT, LIB, UNIT):
        with open(p, encoding="utf-8") as f:
            txt = f.read().lower()
        assert "bluetooth" not in txt and "ble" not in txt.split(), p


def _fake_systemctl(record_path):
    """A stand-in `systemctl` that records its argv, one call per line, and succeeds."""
    d = tempfile.mkdtemp()
    p = os.path.join(d, "systemctl")
    with open(p, "w", encoding="utf-8") as f:
        f.write(
            "#!/usr/bin/env bash\n"
            'printf "%%s\\n" "$*" >> "%s"\n'
            'if [ "$1" = "is-enabled" ]; then echo enabled; fi\n' % record_path
        )
    os.chmod(p, 0o755)
    return p


def _run_provision(mode, root, extra_env=None):
    sysd = os.path.join(root, "systemd-system")
    dropin = os.path.join(sysd, "camera-box.service.d")
    envfile = os.path.join(root, "bkshading", "relay.env")
    binp = os.path.join(root, "bin", "bkshading-relay")
    calls = os.path.join(root, "systemctl-calls.log")
    sc = _fake_systemctl(calls)
    env = dict(
        os.environ,
        BKSHADING_RELAY_UNIT_DEST=os.path.join(sysd, UNIT_NAME),
        BKSHADING_RELAY_ENV_FILE=envfile,
        BKSHADING_RELAY_BIN=binp,
        BKSHADING_RELAY_DROPIN_DIR=dropin,
        BKSHADING_RELAY_GPHOTO2="true",  # a binary that exists -> command -v succeeds, apt skipped
        BKSHADING_RELAY_SYSTEMCTL=sc,
    )
    if extra_env:
        env.update(extra_env)
    r = subprocess.run(
        ["bash", SCRIPT, mode], capture_output=True, text=True, env=env
    )
    return r, calls, envfile


def test_check_fails_with_remediation_when_unprovisioned():
    root = tempfile.mkdtemp()
    try:
        # nothing installed under the temp root -> --check must fail loud with a remediation hint.
        r, _calls, _env = _run_provision("--check", root)
        assert r.returncode != 0, (r.returncode, r.stdout, r.stderr)
        assert "bkshading-provision-relay.sh --install" in (r.stdout + r.stderr)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_then_check_end_to_end_enable_only_and_derives_fps():
    root = tempfile.mkdtemp()
    try:
        # the bin the unit points at must exist for --check to pass; create it under the temp root.
        os.makedirs(os.path.join(root, "bin"), exist_ok=True)
        open(os.path.join(root, "bin", "bkshading-relay"), "w").close()
        os.chmod(os.path.join(root, "bin", "bkshading-relay"), 0o755)
        # a capture-fps drop-in on the box -> the derived env must match it (not the 60 default).
        dropin = os.path.join(root, "systemd-system", "camera-box.service.d")
        os.makedirs(dropin, exist_ok=True)
        with open(os.path.join(dropin, "capture.conf"), "w", encoding="utf-8") as f:
            f.write("[Service]\nEnvironment=CAMERA_BOX_CAPTURE_FPS=30\n")

        r, calls, envfile = _run_provision("--install", root)
        assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
        # unit installed
        assert os.path.isfile(os.path.join(root, "systemd-system", UNIT_NAME))
        # env file derived from the drop-in (30), NOT the 60 default
        with open(envfile, encoding="utf-8") as f:
            assert "CAMERA_BOX_CAPTURE_FPS=30" in f.read()
        # ENABLE-ONLY: systemctl was called with daemon-reload + enable, NEVER start/restart.
        with open(calls, encoding="utf-8") as f:
            log = f.read()
        assert "daemon-reload" in log, log
        assert re.search(r"\benable\b", log), log
        assert "start" not in log, log
        assert "restart" not in log, log

        # a subsequent --check on the freshly provisioned temp root passes.
        r2, _c2, _e2 = _run_provision("--check", root)
        assert r2.returncode == 0, (r2.returncode, r2.stdout, r2.stderr)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_defaults_fps_to_60_when_no_dropin():
    root = tempfile.mkdtemp()
    try:
        os.makedirs(os.path.join(root, "bin"), exist_ok=True)
        open(os.path.join(root, "bin", "bkshading-relay"), "w").close()
        # no drop-in dir at all -> default 60
        r, _calls, envfile = _run_provision("--install", root)
        assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
        with open(envfile, encoding="utf-8") as f:
            assert "CAMERA_BOX_CAPTURE_FPS=60" in f.read()
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_unknown_arg_exits_2():
    r = subprocess.run(["bash", SCRIPT, "--bogus"], capture_output=True, text=True)
    assert r.returncode == 2, r.returncode


if __name__ == "__main__":
    for _name, _fn in sorted(globals().items()):
        if _name.startswith("test_") and callable(_fn):
            _fn()
            print("ok %s" % _name)
    print("all passed")
