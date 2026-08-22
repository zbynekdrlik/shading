#!/usr/bin/env python3
"""bkshading SBC / handheld provisioning — the LAST milestone (issue 808).

The owner architecture (comment 5356048130 path 2, "cieľový stav") puts a handheld camera on a mini
SBC (a Pi Zero 2 W on the cage, on WiFi): the camera plugs USB into the Pi, which runs the SAME
`bkshading-relay` component the camboxes run — a "mini-cambox without video". The strih aggregation
service ALREADY understands this (`Transport::SbcRelay`, the `handheld-1` record in
bkshading.example.toml, a params-only block with no NDI preview), but nothing provisioned the relay
on a bare SBC, CI produced NO ARM binary (a Pi cannot run the amd64 one), and the amd64 deploy
assumes a read-only root (a stock Pi OS root is read-write). This milestone closes all three:

  - `scripts/bkshading-provision-sbc.sh` (+ pure lib `scripts/lib/bkshading-sbc-runtime.sh`)
    provisions the relay on a bare SBC: gphoto2 + the REUSED `bkshading-relay.service` unit, enabled
    (enable-only, defer to reboot). It writes NO CAMERA_BOX_CAPTURE_FPS env (an SBC has no camera-box
    appliance and a handheld has no grab comparison), and its `--check` verifies the deployed binary
    is actually aarch64 (an ELF e_machine read) so a mis-deployed amd64 binary is caught here, not at
    reboot with an opaque `Exec format error`.
  - the CI `bkshading` job cross-builds the relay for aarch64 and uploads `bkshading-relay-linux-arm64`.
  - `scripts/bkshading-deploy-relay.sh` gains `--arch amd64|arm64` (selects the artifact) and
    `--no-remount` (a stock Pi OS root is rw), so the arm64 relay has a real deploy path.

These stdlib-only + pyyaml structural/behavioural tests run in the `python-tests` CI job (no Rust
toolchain, no root, no apt, no real systemd — the impure ops are overridden to fakes into a temp
root). Runnable directly (`python3 tests/python/test_bkshading_sbc_provision_808.py`) or under pytest.
"""
import os
import re
import shutil
import struct
import subprocess
import tempfile

import yaml

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
SCRIPT = os.path.join(REPO, "scripts", "bkshading-provision-sbc.sh")
LIB = os.path.join(REPO, "scripts", "lib", "bkshading-sbc-runtime.sh")
RELAY_LIB = os.path.join(REPO, "scripts", "lib", "bkshading-relay-runtime.sh")
DEPLOY_SCRIPT = os.path.join(REPO, "scripts", "bkshading-deploy-relay.sh")
DEPLOY_LIB = os.path.join(REPO, "scripts", "lib", "bkshading-deploy-runtime.sh")
UNIT = os.path.join(REPO, "systemd", "bkshading-relay.service")
CI_YML = os.path.join(REPO, ".github", "workflows", "ci.yml")
README = os.path.join(REPO, "bkshading", "README.md")
EXAMPLE_TOML = os.path.join(REPO, "bkshading", "service", "bkshading.example.toml")

UNIT_NAME = "bkshading-relay.service"
BIN_PATH = "/usr/local/bin/bkshading-relay"
CROSS_TARGET = "aarch64-unknown-linux-gnu"
ARM64_ARTIFACT = "bkshading-relay-linux-arm64"
ARM64_BUILD_PATH = "target/aarch64-unknown-linux-gnu/release/bkshading-relay"


# ---------------------------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------------------------
def _bash(lib, snippet):
    src = '. "%s"\n%s' % (lib, snippet)
    out = subprocess.run(["bash", "-c", src], capture_output=True, text=True, check=True)
    return out.stdout.strip()


def _bash_arg(lib, func, arg=""):
    src = '. "%s"\n%s "$A1"' % (lib, func)
    env = dict(os.environ, A1=arg)
    r = subprocess.run(["bash", "-c", src], capture_output=True, text=True, env=env)
    return r.returncode, r.stdout.strip()


def _fake_elf(path, e_machine):
    """Write a minimal 20-byte ELF header with the given e_machine (little-endian)."""
    hdr = b"\x7fELF\x02\x01\x01" + b"\x00" * 9  # ident (16 bytes)
    hdr += struct.pack("<H", 2)  # e_type = ET_EXEC (offset 16-17)
    hdr += struct.pack("<H", e_machine)  # e_machine (offset 18-19)
    with open(path, "wb") as f:
        f.write(hdr)
    os.chmod(path, 0o755)


AARCH64 = 183
X86_64 = 62
ARM32 = 40


def _load_ci():
    with open(CI_YML) as f:
        return yaml.safe_load(f)


def _job_step_runs(job):
    return "\n".join(s.get("run", "") for s in job.get("steps", []))


def _job_uploads(job, artifact_name):
    for s in job.get("steps", []):
        if str(s.get("uses", "")).startswith("actions/upload-artifact") and \
                s.get("with", {}).get("name") == artifact_name:
            return s.get("with", {})
    return None


# ---------------------------------------------------------------------------------------------
# parse + executable
# ---------------------------------------------------------------------------------------------
def test_files_exist_and_parse():
    for p in (SCRIPT, LIB):
        assert os.path.isfile(p), p
        r = subprocess.run(["bash", "-n", p], capture_output=True, text=True)
        assert r.returncode == 0, "bash -n %s: %s" % (p, r.stderr)


def test_provision_script_is_executable():
    assert os.access(SCRIPT, os.X_OK), "%s must be executable" % SCRIPT


# ---------------------------------------------------------------------------------------------
# sbc lib pure decisions
# ---------------------------------------------------------------------------------------------
def test_cross_target_is_aarch64_gnu():
    assert _bash(LIB, "bkshading_sbc_cross_target") == CROSS_TARGET


def test_no_capture_fps_env_on_sbc():
    # An SBC has no camera-box appliance to derive from, and a handheld has no grab comparison, so
    # the provision writes NO env. Pinned so a future accidental env-write is a RED test.
    assert _bash(LIB, "bkshading_sbc_writes_capture_fps_env") == "no"


def test_arch_from_machine_classifier():
    assert _bash_arg(LIB, "bkshading_sbc_arch_from_machine", "183")[1] == "aarch64"
    assert _bash_arg(LIB, "bkshading_sbc_arch_from_machine", "62")[1] == "x86-64"
    assert _bash_arg(LIB, "bkshading_sbc_arch_from_machine", "40")[1] == "arm"
    assert _bash_arg(LIB, "bkshading_sbc_arch_from_machine", "999")[1] == "unknown"
    assert _bash_arg(LIB, "bkshading_sbc_arch_from_machine", "")[1] == "unknown"


def test_arch_ok_only_aarch64():
    assert _bash_arg(LIB, "bkshading_sbc_arch_ok", "aarch64")[1] == "yes"
    assert _bash_arg(LIB, "bkshading_sbc_arch_ok", "x86-64")[1] == "no"
    assert _bash_arg(LIB, "bkshading_sbc_arch_ok", "arm")[1] == "no"
    assert _bash_arg(LIB, "bkshading_sbc_arch_ok", "unknown")[1] == "no"


def test_elf_arch_of_real_files():
    with tempfile.TemporaryDirectory() as tmp:
        aa = os.path.join(tmp, "aa")
        x86 = os.path.join(tmp, "x86")
        notelf = os.path.join(tmp, "sh")
        _fake_elf(aa, AARCH64)
        _fake_elf(x86, X86_64)
        with open(notelf, "w") as f:
            f.write("#!/bin/sh\necho hi\n")
        assert _bash_arg(LIB, "bkshading_sbc_elf_machine_from_file", aa)[1] == str(AARCH64)
        assert _bash_arg(LIB, "bkshading_sbc_elf_arch_of_file", aa)[1] == "aarch64"
        assert _bash_arg(LIB, "bkshading_sbc_elf_arch_of_file", x86)[1] == "x86-64"
        # a non-ELF file -> unknown (empty machine)
        assert _bash_arg(LIB, "bkshading_sbc_elf_machine_from_file", notelf)[1] == ""
        assert _bash_arg(LIB, "bkshading_sbc_elf_arch_of_file", notelf)[1] == "unknown"
        # a missing file -> unknown, never an error
        assert _bash_arg(LIB, "bkshading_sbc_elf_arch_of_file", os.path.join(tmp, "nope"))[1] == "unknown"


# ---------------------------------------------------------------------------------------------
# provision script: sources both libs, reuses the relay unit, enable-only, no env
# ---------------------------------------------------------------------------------------------
def test_provision_sources_both_libs_and_reuses_relay_unit():
    with open(SCRIPT, encoding="utf-8") as f:
        s = f.read()
    assert "bkshading-relay-runtime.sh" in s, "must source the relay lib (reused unit/bin/pkg)"
    assert "bkshading-sbc-runtime.sh" in s, "must source the SBC lib (arch check / cross target)"
    assert "--check" in s and "--install" in s
    assert "gphoto2" in s, "install must reference the gphoto2 apt package"
    # the reused relay unit name comes from the relay lib (one source of truth)
    assert _bash(RELAY_LIB, "bkshading_relay_unit_name") == UNIT_NAME


def test_provision_is_enable_only():
    with open(SCRIPT, encoding="utf-8") as f:
        s = f.read()
    assert not re.search(r"\bstart\s+bkshading-relay", s), "must not systemctl-start the relay"
    assert not re.search(r"\brestart\s+bkshading-relay", s), "must not systemctl-restart the relay"
    assert "enable --now" not in s, "enable --now would live-start (not enable-only)"


def _fake_systemctl(record_path):
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


def _run_provision(mode, root, bin_machine=AARCH64, make_bin=True):
    sysd = os.path.join(root, "systemd-system")
    binp = os.path.join(root, "bin", "bkshading-relay")
    calls = os.path.join(root, "systemctl-calls.log")
    sc = _fake_systemctl(calls)
    if make_bin:
        os.makedirs(os.path.dirname(binp), exist_ok=True)
        _fake_elf(binp, bin_machine)
    env = dict(
        os.environ,
        BKSHADING_SBC_UNIT_DEST=os.path.join(sysd, UNIT_NAME),
        BKSHADING_SBC_BIN=binp,
        BKSHADING_SBC_GPHOTO2="true",  # exists -> command -v succeeds, apt skipped
        BKSHADING_SBC_SYSTEMCTL=sc,
    )
    r = subprocess.run(["bash", SCRIPT, mode], capture_output=True, text=True, env=env)
    return r, calls, binp


def test_check_fails_with_remediation_when_unprovisioned():
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--check", root, make_bin=False)
        assert r.returncode != 0, (r.returncode, r.stdout, r.stderr)
        assert "bkshading-provision-sbc.sh --install" in (r.stdout + r.stderr)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_then_check_end_to_end_enable_only_no_env():
    root = tempfile.mkdtemp()
    try:
        r, calls, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
        # unit installed (the reused relay unit) + byte-matches repo
        installed_unit = os.path.join(root, "systemd-system", UNIT_NAME)
        assert os.path.isfile(installed_unit)
        with open(installed_unit) as a, open(UNIT) as b:
            assert a.read() == b.read(), "installed unit must byte-match the repo relay unit"
        # NO env file written anywhere under the temp root (the SBC writes no capture-fps env)
        for dirpath, _dirs, files in os.walk(root):
            for fn in files:
                assert fn != "relay.env", "an SBC must NOT write a relay.env capture-fps file"
        # ENABLE-ONLY: daemon-reload + enable, NEVER start/restart.
        log = open(calls).read()
        assert "daemon-reload" in log, log
        assert re.search(r"\benable\b", log), log
        assert "start" not in log, log
        assert "restart" not in log, log
        # a subsequent --check on the freshly provisioned temp root passes.
        r2, _c2, _b2 = _run_provision("--check", root)
        assert r2.returncode == 0, (r2.returncode, r2.stdout, r2.stderr)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_check_fails_on_wrong_arch_binary():
    root = tempfile.mkdtemp()
    try:
        # provision fully, but with an amd64 binary deployed (the classic mistake).
        r, _c, _b = _run_provision("--install", root, bin_machine=X86_64)
        assert r.returncode == 0, (r.stdout, r.stderr)  # install warns, doesn't fail
        r2, _c2, _b2 = _run_provision("--check", root, bin_machine=X86_64)
        assert r2.returncode != 0, "an amd64 binary on the SBC must fail --check"
        assert re.search(r"aarch64|arch|arm64|x86", r2.stdout + r2.stderr, re.I), \
            "the failure must name the arch mismatch"
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_unknown_arg_exits_2():
    r = subprocess.run(["bash", SCRIPT, "--bogus"], capture_output=True, text=True)
    assert r.returncode == 2, r.returncode


# ---------------------------------------------------------------------------------------------
# CI: the bkshading job cross-builds + uploads the aarch64 relay; no continue-on-error
# ---------------------------------------------------------------------------------------------
def test_ci_bkshading_job_cross_builds_aarch64_relay():
    ci = _load_ci()
    job = ci["jobs"]["bkshading"]
    runs = _job_step_runs(job)
    assert CROSS_TARGET in runs, "bkshading job must reference the aarch64 target"
    assert "rustup target add %s" % CROSS_TARGET in runs, "must add the aarch64 rustup target"
    assert "gcc-aarch64-linux-gnu" in runs, "must install the aarch64 cross linker"
    assert re.search(
        r"cargo build --release[^\n]*--target %s[^\n]*-p bkshading-relay" % re.escape(CROSS_TARGET),
        runs,
    ), "must cross-build the relay for aarch64"


def test_ci_uploads_arm64_artifact():
    ci = _load_ci()
    job = ci["jobs"]["bkshading"]
    up = _job_uploads(job, ARM64_ARTIFACT)
    assert up is not None, "bkshading job must upload the %s artifact" % ARM64_ARTIFACT
    assert ARM64_BUILD_PATH in up.get("path", ""), "must upload the aarch64 relay build output"
    assert up.get("if-no-files-found") == "error", "must fail loud on a missing arm64 binary"


def test_ci_no_continue_on_error_in_bkshading_job():
    ci = _load_ci()
    job = ci["jobs"]["bkshading"]
    assert "continue-on-error" not in job
    for s in job.get("steps", []):
        assert "continue-on-error" not in s


def test_ci_arm64_upload_name_agrees_with_deploy_lib():
    # ONE source of truth for the arm64 artifact name: the deploy lib. CI must upload under it.
    lib_name = _bash(DEPLOY_LIB, "bkshading_deploy_arm64_artifact_name")
    assert lib_name == ARM64_ARTIFACT, "deploy lib arm64 artifact name drift: %s" % lib_name
    ci = _load_ci()
    assert _job_uploads(ci["jobs"]["bkshading"], lib_name) is not None, \
        "CI must upload under the deploy lib's arm64 artifact name (%s)" % lib_name


# ---------------------------------------------------------------------------------------------
# deploy extension: --arch arm64 selects the arm64 artifact; --no-remount skips the ro-root cycle
# ---------------------------------------------------------------------------------------------
def test_deploy_lib_arch_artifact_selection():
    # amd64 (default) unchanged; arm64 -> the relay-only arm64 artifact.
    assert _bash(DEPLOY_LIB, "bkshading_deploy_artifact_name") == "bkshading-linux-amd64"
    assert _bash_arg(DEPLOY_LIB, "bkshading_deploy_artifact_name_for_arch", "amd64")[1] == \
        "bkshading-linux-amd64"
    assert _bash_arg(DEPLOY_LIB, "bkshading_deploy_artifact_name_for_arch", "arm64")[1] == \
        ARM64_ARTIFACT


def _run_deploy(args, env=None):
    e = dict(os.environ)
    if env:
        e.update(env)
    return subprocess.run(["bash", DEPLOY_SCRIPT] + args, capture_output=True, text=True, env=e)


def test_deploy_dry_run_arm64_no_remount_skips_remount():
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, "bkshading-relay")
        _fake_elf(fake_bin, AARCH64)
        r = _run_deploy(["--host", "10.77.9.60", "--arch", "arm64", "--no-remount",
                         "--binary", fake_bin, "--dry-run"])
        assert r.returncode == 0, (r.stdout, r.stderr)
        out = r.stdout + r.stderr
        assert "10.77.9.60" in out
        assert BIN_PATH in out
        # NO ro-root remount cycle for a stock-rw-root Pi.
        assert "remount,rw" not in out and "remount,ro" not in out, \
            "--no-remount must skip the ro-root swap in the plan"
        # still enable-only.
        assert re.search(r"(NOT start|enable-only|reboot)", out, re.I)


def test_deploy_arm64_without_no_remount_warns():
    # arm64 targets an SBC (rw root); forgetting --no-remount is a footgun -> the script WARNS (but
    # does not force: --arch/--no-remount stay orthogonal for the read-only-Pi case).
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, "bkshading-relay")
        _fake_elf(fake_bin, AARCH64)
        # arm64 WITHOUT --no-remount -> warning present.
        r = _run_deploy(["--host", "10.77.9.60", "--arch", "arm64", "--binary", fake_bin, "--dry-run"])
        assert r.returncode == 0, (r.stdout, r.stderr)
        assert re.search(r"--no-remount", r.stdout + r.stderr) and \
            re.search(r"warning", r.stdout + r.stderr, re.I), \
            "arm64 without --no-remount must warn about the rw-root Pi footgun"
        # arm64 WITH --no-remount -> no such warning.
        r2 = _run_deploy(["--host", "10.77.9.60", "--arch", "arm64", "--no-remount",
                          "--binary", fake_bin, "--dry-run"])
        assert not re.search(r"WARNING: --arch arm64 without --no-remount", r2.stdout + r2.stderr), \
            "arm64 WITH --no-remount must not warn"


def _fake_deploy_env(tmp, remote_sha):
    log = os.path.join(tmp, "calls.log")
    fake_ssh = os.path.join(tmp, "fake-ssh")
    ssh_body = (
        "#!/usr/bin/env bash\n"
        'printf "SSH %s\\n" "$*" >> "__LOG__"\n'
        'cmd="${!#}"\n'
        "case \"$cmd\" in\n"
        '  *sha256sum*) printf "__SHA__\\n" ;;\n'
        '  *"test -x"*) printf "yes\\n" ;;\n'
        "  *) : ;;\n"
        "esac\n"
        "exit 0\n"
    ).replace("__LOG__", log).replace("__SHA__", remote_sha)
    with open(fake_ssh, "w") as f:
        f.write(ssh_body)
    os.chmod(fake_ssh, 0o755)
    fake_scp = os.path.join(tmp, "fake-scp")
    scp_body = (
        "#!/usr/bin/env bash\n"
        'printf "SCP %s\\n" "$*" >> "__LOG__"\n'
        "exit 0\n"
    ).replace("__LOG__", log)
    with open(fake_scp, "w") as f:
        f.write(scp_body)
    os.chmod(fake_scp, 0o755)
    env = {
        "BKSHADING_DEPLOY_SSH": fake_ssh,
        "BKSHADING_DEPLOY_SCP": fake_scp,
        "BKSHADING_DEPLOY_SSHPASS_PREFIX": "",
    }
    return env, log


def test_fake_deploy_arm64_no_remount_scps_without_remount():
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, "bkshading-relay")
        _fake_elf(fake_bin, AARCH64)
        local_sha = subprocess.run(
            ["sha256sum", fake_bin], capture_output=True, text=True, check=True
        ).stdout.split()[0]
        env, log = _fake_deploy_env(tmp, local_sha)
        r = _run_deploy(["--host", "10.77.9.60", "--arch", "arm64", "--no-remount",
                         "--binary", fake_bin], env=env)
        assert r.returncode == 0, (r.stdout, r.stderr)
        calls = open(log).read()
        assert ("root@10.77.9.60:" + BIN_PATH) in calls, "must scp to the relay bin path"
        assert "sha256sum" in calls, "must byte-verify"
        assert "remount,rw" not in calls and "remount,ro" not in calls, \
            "--no-remount must not remount a stock-rw-root Pi"
        assert "systemctl start" not in calls and "systemctl restart" not in calls, \
            "deploy is enable-only"


# ---------------------------------------------------------------------------------------------
# no Bluetooth; README + example config document the SBC handheld
# ---------------------------------------------------------------------------------------------
def test_no_bluetooth_anywhere():
    for f in (SCRIPT, LIB, DEPLOY_SCRIPT, DEPLOY_LIB, CI_YML):
        text = open(f).read().lower()
        for banned in ("bluetooth", "bluez", "gatt"):
            assert banned not in text, "%s must not mention %r (owner hard rule)" % (f, banned)
        assert not re.search(r"\bble\b", text), "%s must not mention BLE (owner hard rule)" % f


def test_readme_documents_sbc_handheld_image():
    txt = open(README, encoding="utf-8").read()
    assert "bkshading-provision-sbc.sh" in txt, "README must document the SBC provision script"
    assert "aarch64" in txt.lower() or "arm64" in txt.lower(), "README must name the ARM target"
    assert "Pi Zero 2 W" in txt, "README must name the SBC device"
    # the milestone is done -> it must NOT still be listed as deferred.
    assert not re.search(r"[Dd]eferred[^\n]*SBC handheld image", txt), \
        "README must not still list the SBC image as deferred"


def test_example_config_has_sbc_handheld_record():
    txt = open(EXAMPLE_TOML, encoding="utf-8").read()
    assert 'transport = "sbc-relay"' in txt, "the example config must carry the sbc-relay transport"
    assert "handheld-1" in txt


if __name__ == "__main__":
    import sys

    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    failed = 0
    for fn in fns:
        try:
            fn()
            print("ok   %s" % fn.__name__)
        except Exception as e:  # noqa: BLE001 - runner surfaces the failure, never swallows it
            failed += 1
            print("FAIL %s: %s" % (fn.__name__, e))
    print("\n%d/%d passed" % (len(fns) - failed, len(fns)))
    sys.exit(1 if failed else 0)
