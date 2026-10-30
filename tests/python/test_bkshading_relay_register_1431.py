"""Issue 1431 (joint event: `strih.lan` is the ONE switch for the whole cambox).

Every cambox bkshading relay announces itself to the shading service behind `strih.lan` with its camera's relay
id (`camN`), the issue-808 registration the handheld SBC already uses. The relay unit is shared with the SBC and
stays unchanged: the cambox's own `/etc/bkshading/relay.env` carries `BKSHADING_REGISTER_URL` + `BKSHADING_RELAY_ID`
(both read by the relay through clap `env =`). setup-device / the provision CLI write them, verify-device `(ao)` and
the CLI's `--check` FAIL a relay.env without them.
"""
import hashlib
import os
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
RUNTIME = os.path.join(REPO, "scripts", "lib", "bkshading-relay-runtime.sh")
PROV_LIB = os.path.join(REPO, "scripts", "lib", "bkshading-relay-provision.sh")
PROVISION = os.path.join(REPO, "scripts", "bkshading-provision-relay.sh")
UNIT = os.path.join(REPO, "systemd", "bkshading-relay.service")
RELAY_MAIN = os.path.join(REPO, "bkshading", "relay", "src", "main.rs")
URL = "http://strih.lan:8770/api/relays/register"


def _bash(lib, snippet, *args, env=None):
    e = dict(os.environ)
    e.update(env or {})
    src = 'set -euo pipefail\n. "%s"\n%s' % (lib, snippet)
    return subprocess.run(["bash", "-c", src, "harness", *args], capture_output=True, text=True, env=e)


def _unit_sha():
    with open(UNIT, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def test_the_register_url_is_strih_lan_and_the_relay_reads_both_env_names():
    r = _bash(RUNTIME, "bkshading_relay_register_url")
    assert r.returncode == 0 and r.stdout.strip() == URL, r.stdout + r.stderr
    with open(RELAY_MAIN, encoding="utf-8") as f:
        main = f.read()
    # the relay reads exactly the two names the env file writes
    assert 'env = "BKSHADING_REGISTER_URL"' in main and 'env = "BKSHADING_RELAY_ID"' in main


def test_the_relay_id_is_the_lowercased_camera_name():
    for name, want in (("CAM3", "cam3"), ("cam7", "cam7"), ("Cam1", "cam1")):
        r = _bash(RUNTIME, 'bkshading_relay_id_for_device "$1"', name)
        assert r.returncode == 0 and r.stdout.strip() == want, (name, r.stdout, r.stderr)
    for bad in ("", "handheld-1", "cam", "cam0", "camX", "cam1 ", "cam1;x", "strih-snv"):
        r = _bash(RUNTIME, 'bkshading_relay_id_for_device "$1"', bad)
        assert r.returncode != 0 and r.stdout == "", (bad, r.stdout)


def test_the_env_file_registers_the_cambox_with_its_relay_id():
    r = _bash(RUNTIME, 'bkshading_relay_env_file_content 60 "$1"', "cam5")
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    assert "CAMERA_BOX_CAPTURE_FPS=60" in lines
    assert "BKSHADING_REGISTER_URL=%s" % URL in lines
    assert "BKSHADING_RELAY_ID=cam5" in lines


def test_the_unit_is_unchanged_so_the_sbc_keeps_its_own_env():
    # The register flag must come from the env file: the unit is shared with the handheld SBC, whose
    # relay.env names its own id (default its hostname).
    with open(UNIT, encoding="utf-8") as f:
        unit = f.read()
    assert "--register-url" not in unit and "--relay-id" not in unit
    assert "EnvironmentFile=-/etc/bkshading/relay.env" in unit


def _calls_systemctl(tmp):
    path = os.path.join(tmp, "systemctl")
    with open(path, "w") as f:
        f.write('#!/usr/bin/env bash\ncase "$1" in is-enabled) echo enabled ;; esac\nexit 0\n')
    os.chmod(path, 0o755)
    return path


def _prov_env(tmp):
    return {
        "BKSHADING_RELAY_UNIT_DEST": os.path.join(tmp, "sysd", "bkshading-relay.service"),
        "BKSHADING_RELAY_ENV_FILE": os.path.join(tmp, "etc", "relay.env"),
        "BKSHADING_RELAY_BIN": os.path.join(tmp, "bin", "bkshading-relay"),
        "BKSHADING_RELAY_DROPIN_DIR": os.path.join(tmp, "sysd", "camera-box.service.d"),
        "BKSHADING_RELAY_GPHOTO2": "true",
        "BKSHADING_RELAY_SYSTEMCTL": _calls_systemctl(tmp),
    }


def test_install_writes_the_registration_and_refuses_without_a_relay_id():
    with tempfile.TemporaryDirectory() as tmp:
        env = _prov_env(tmp)
        r = _bash(PROV_LIB, "bkshading_relay_provision_install enabled '' cam3", env=env)
        assert r.returncode == 0, r.stdout + r.stderr
        with open(env["BKSHADING_RELAY_ENV_FILE"], encoding="utf-8") as f:
            body = f.read().splitlines()
        assert "BKSHADING_REGISTER_URL=%s" % URL in body and "BKSHADING_RELAY_ID=cam3" in body, body
    with tempfile.TemporaryDirectory() as tmp:
        env = _prov_env(tmp)
        r = _bash(PROV_LIB, "bkshading_relay_provision_install enabled ''", env=env)
        assert r.returncode != 0 and "relay id" in r.stderr, r.stdout + r.stderr
        assert not os.path.exists(env["BKSHADING_RELAY_ENV_FILE"]), "no env file without a relay id"


def test_the_gather_reads_both_registration_lines_from_the_env_file():
    with tempfile.TemporaryDirectory() as tmp:
        envf = os.path.join(tmp, "relay.env")
        with open(envf, "w") as f:
            f.write("# c\nCAMERA_BOX_CAPTURE_FPS=60\nBKSHADING_REGISTER_URL=%s\nBKSHADING_RELAY_ID=cam2\n" % URL)
        snip = _bash(PROV_LIB, "bkshading_relay_provision_gather_remote_snippet").stdout
        snip = snip.replace("/etc/bkshading/relay.env", envf)
        out = subprocess.run(["sh", "-c", snip], capture_output=True, text=True).stdout
        assert "ENV_REGISTER_URL=%s" % URL in out.splitlines(), out
        assert "ENV_RELAY_ID=cam2" in out.splitlines(), out


def _block(url=URL, rid="cam3"):
    return ("BIN_X=yes\nUNIT_SHA=%s\nENV_FPS=60\nENV_REGISTER_URL=%s\nENV_RELAY_ID=%s\nGPHOTO2=yes\n"
            "ENABLED=enabled\n" % (_unit_sha(), url, rid))


def _verdict(block, rid):
    return _bash(PROV_LIB, 'bkshading_relay_provision_verdict "$1" enabled "$2"', block, rid).stdout


def test_the_verdict_fails_a_relay_that_does_not_register_as_its_camera():
    assert _verdict(_block(), "cam3").strip() == "ok"
    missing = _verdict(_block(url=""), "cam3")
    assert missing.startswith("FAIL") and "BKSHADING_REGISTER_URL" in missing, missing
    other = _verdict(_block(url="http://10.77.9.202:8770/api/relays/register"), "cam3")
    assert other.startswith("FAIL") and "strih.lan" in other, other
    wrong_id = _verdict(_block(rid="cam5"), "cam3")
    assert wrong_id.startswith("FAIL") and "cam5" in wrong_id and "cam3" in wrong_id, wrong_id
    no_id = _verdict(_block(rid=""), "cam3")
    assert no_id.startswith("FAIL") and "BKSHADING_RELAY_ID" in no_id, no_id
    unknown = _verdict(_block(), "")
    assert unknown.startswith("FAIL") and "relay id" in unknown, unknown


def test_the_cli_install_and_check_carry_the_registration():
    with tempfile.TemporaryDirectory() as tmp:
        env = _prov_env(tmp)
        env["BKSHADING_RELAY_DEVICE_NAME"] = "CAM5"
        e = dict(os.environ)
        e.pop("CAMERA_BOX_RIG_MODE", None)
        e.update(env)
        r = subprocess.run(["bash", PROVISION, "--install", "--rig-mode", "event"], capture_output=True, text=True,
                           env=e)
        assert r.returncode == 0, r.stdout + r.stderr
        with open(env["BKSHADING_RELAY_ENV_FILE"], encoding="utf-8") as f:
            assert "BKSHADING_RELAY_ID=cam5" in f.read().splitlines()
        os.makedirs(os.path.join(tmp, "bin"), exist_ok=True)
        with open(env["BKSHADING_RELAY_BIN"], "w") as f:
            f.write("R")
        os.chmod(env["BKSHADING_RELAY_BIN"], 0o755)
        ok = subprocess.run(["bash", PROVISION, "--check", "--rig-mode", "event"], capture_output=True, text=True,
                            env=e)
        assert ok.returncode == 0, ok.stdout + ok.stderr
        assert "registers as cam5" in ok.stdout, ok.stdout
        with open(env["BKSHADING_RELAY_ENV_FILE"], "w") as f:
            f.write("CAMERA_BOX_CAPTURE_FPS=60\n")
        bad = subprocess.run(["bash", PROVISION, "--check", "--rig-mode", "event"], capture_output=True, text=True,
                             env=e)
        assert bad.returncode != 0 and "BKSHADING_REGISTER_URL" in bad.stderr, bad.stdout + bad.stderr
