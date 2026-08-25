#!/usr/bin/env python3
"""bkshading — repeatable DEPLOY path for the SERVICE onto the strih Windows PC (issue 808).

Everything merged so far (M1 service, M2 NDI preview, WS push, cloudflared remote) ships to the
strih PC, and CI already release-builds + uploads the deployable service binary as
`bkshading-windows-amd64` (`target/release/bkshading.exe`) — but NO tool consumed it: the service
reached strih only by a MANUAL stage (`C:\\stage-bkshading`, issue #1157). `bkshading-deploy-relay.sh`
deploys only the *relay* to a Linux *cambox* (a different binary/OS/lifecycle).

This sub-step adds the repeatable deploy path (CI artifact -> strih staging -> config -> persistent
Task Scheduler keep-alive task -> verify :8770):
  - scripts/lib/bkshading-deploy-service-runtime.sh — SOURCE-ONLY pure invariants (the ONE source of
    truth for the artifact/exe/install-dir/config/task-name/port/keepalive, so the .sh + .ps1 cannot
    drift);
  - scripts/bkshading-deploy-service.sh — dev1-side orchestrator: resolve/download the
    `bkshading-windows-amd64` artifact (or --binary), then scp -O the exe + installer + config seed to
    strih and run the installer (transport = strih-recordings-retention.sh style: powershell -File,
    NEVER a nested powershell -Command). DRY-RUN default; --execute for the mutating half;
  - scripts/bkshading-install-service.ps1 — on-box installer: place exe + config under C:\\bkshading
    (config seeded ONLY IF ABSENT — never clobber an operator-tuned config), register a keep-alive
    Task Scheduler task (AtLogOn + a repetition, whose action is the same ps1 -KeepAlive), verify
    :8770 Listening. DRY-RUN default; -Execute mutates; -KeepAlive = the per-tick relaunch pass;
    -Uninstall removes.

Tier-0 (#557 — zero cargo): stdlib + pyyaml, no toolchain / root / real ssh-scp-gh (the impure ops
are overridden to fakes). Runs in the `python-tests` CI job. Runnable directly
(`python3 tests/python/test_bkshading_deploy_service_808.py`) or under pytest.
"""
import os
import re
import stat
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
SCRIPT = os.path.join(REPO, "scripts", "bkshading-deploy-service.sh")
LIB = os.path.join(REPO, "scripts", "lib", "bkshading-deploy-service-runtime.sh")
PS1 = os.path.join(REPO, "scripts", "bkshading-install-service.ps1")
CONFIG_EXAMPLE = os.path.join(REPO, "bkshading", "service", "bkshading.example.toml")

# The canonical shared invariants (the lib is the SINGLE source of truth; the tests below assert the
# lib prints exactly these AND that both the .sh and the .ps1 carry the same literals).
WINDOWS_ARTIFACT = "bkshading-windows-amd64"
EXE_NAME = "bkshading.exe"
INSTALL_DIR = r"C:\bkshading"
CONFIG_NAME = "bkshading.toml"
CONFIG_EXAMPLE_NAME = "bkshading.example.toml"
TASK_NAME = "bkshading-service"
PORT = "8770"
INSTALLER_PS1_NAME = "bkshading-install-service.ps1"


def _bash(snippet):
    """Source the lib, run `snippet`, return stripped stdout (raises on nonzero)."""
    src = '. "%s"\n%s' % (LIB, snippet)
    out = subprocess.run(["bash", "-c", src], capture_output=True, text=True, check=True)
    return out.stdout.strip()


def _run_script(args, env=None):
    e = dict(os.environ)
    if env:
        e.update(env)
    return subprocess.run(["bash", SCRIPT] + args, capture_output=True, text=True, env=e)


def _ps1():
    return open(PS1, encoding="utf-8").read()


# ---------------------------------------------------------------------------------------------
# parse / existence
# ---------------------------------------------------------------------------------------------
def test_files_exist():
    for f in (SCRIPT, LIB, PS1):
        assert os.path.exists(f), "missing %s" % f


def test_script_and_lib_parse():
    for f in (SCRIPT, LIB):
        r = subprocess.run(["bash", "-n", f], capture_output=True, text=True)
        assert r.returncode == 0, "bash -n failed on %s: %s" % (f, r.stderr)


def test_deploy_script_is_executable():
    assert os.access(SCRIPT, os.X_OK), "%s must be executable" % SCRIPT


# ---------------------------------------------------------------------------------------------
# pure lib invariants (the ONE source of truth)
# ---------------------------------------------------------------------------------------------
def test_lib_invariants():
    assert _bash("bkshading_service_artifact_name") == WINDOWS_ARTIFACT
    assert _bash("bkshading_service_exe_name") == EXE_NAME
    assert _bash("bkshading_service_install_dir") == INSTALL_DIR
    assert _bash("bkshading_service_config_name") == CONFIG_NAME
    assert _bash("bkshading_service_config_example_name") == CONFIG_EXAMPLE_NAME
    assert _bash("bkshading_service_task_name") == TASK_NAME
    assert _bash("bkshading_service_port") == PORT
    assert _bash("bkshading_service_installer_ps1_name") == INSTALLER_PS1_NAME
    # keepalive minutes is a positive integer
    km = _bash("bkshading_service_keepalive_minutes")
    assert km.isdigit() and int(km) >= 1, "keepalive minutes must be a positive int, got %r" % km


def test_lib_is_source_only_no_set_e_leak():
    # A source-only lib must NOT `set -euo pipefail` (it would leak into the sourcing shell —
    # ci-testing-gotchas.md). Mirrors bkshading-deploy-runtime.sh.
    text = open(LIB, encoding="utf-8").read()
    assert "set -euo pipefail" not in text, "source-only lib must not set -euo pipefail"


def test_lib_sha_match_decision():
    # byte-verify decision (mirrors the relay sibling): match ONLY on two equal non-empty shas.
    assert _bash("bkshading_service_sha_match abc123 abc123") == "match"
    assert _bash("bkshading_service_sha_match abc123 def456") == "mismatch"
    assert _bash('bkshading_service_sha_match "" ""') == "mismatch"
    assert _bash('bkshading_service_sha_match abc123 ""') == "mismatch"


def test_port_matches_service_config_default():
    # The port the deploy verifies MUST equal the service's own default_bind port (config.rs).
    cfg = open(CONFIG_EXAMPLE, encoding="utf-8").read()
    assert (":%s" % PORT) in cfg, "port %s must match the service config bind" % PORT


# ---------------------------------------------------------------------------------------------
# .sh <-> .ps1 <-> lib cross-checks: the shared invariants cannot drift
# ---------------------------------------------------------------------------------------------
def test_sh_and_ps1_agree_on_shared_invariants():
    sh = open(SCRIPT, encoding="utf-8").read()
    ps1 = _ps1()
    # install dir, exe, task name, port, config name, installer ps1 name — must appear in BOTH files.
    for literal in (INSTALL_DIR, EXE_NAME, TASK_NAME, PORT, CONFIG_NAME, INSTALLER_PS1_NAME):
        assert literal in sh, ".sh must reference %r (shared invariant)" % literal
    for literal in (INSTALL_DIR, EXE_NAME, TASK_NAME, PORT, CONFIG_NAME, CONFIG_EXAMPLE_NAME):
        assert literal in ps1, ".ps1 must reference %r (shared invariant)" % literal
    # keepalive cadence: the .sh (via the lib) and the .ps1 (as its param default) must agree.
    km = _bash("bkshading_service_keepalive_minutes")
    assert re.search(r"\$KeepAliveMinutes\s*=\s*%s\b" % re.escape(km), ps1), \
        ".ps1 default KeepAliveMinutes must equal the lib's %s" % km


# ---------------------------------------------------------------------------------------------
# credentials / hard rules
# ---------------------------------------------------------------------------------------------
def test_no_credential_embedded():
    # The service config is a pure camera list — NO OBS-WS password or any credential. The deploy
    # must embed no secret anywhere (config seed included).
    for f in (SCRIPT, LIB, PS1, CONFIG_EXAMPLE):
        text = open(f, encoding="utf-8").read().lower()
        for banned in ("obs_ws_password", "obs-ws-password", "4455", "password ="):
            assert banned not in text, "%s must not embed a credential (%r)" % (f, banned)


def test_no_shutdown_or_destructive_op():
    # A service deploy never reboots/powers-off the box.
    for f in (SCRIPT, PS1):
        text = open(f, encoding="utf-8").read().lower()
        assert not re.search(r"shutdown\s+/[rs]\b", text), "%s must not reboot/power-off" % f


def test_no_bluetooth_anywhere():
    # `ble` matched as a standalone word — a bare "ble " would false-match "enable"/"disable"/"table".
    for f in (SCRIPT, LIB, PS1):
        text = open(f, encoding="utf-8").read().lower()
        for banned in ("bluetooth", "bluez", "gatt"):
            assert banned not in text, "%s must not mention %r (owner hard rule)" % (f, banned)
        assert not re.search(r"\bble\b", text), "%s must not mention BLE (owner hard rule)" % f


# ---------------------------------------------------------------------------------------------
# .sh: DRY-RUN default touches nothing remote
# ---------------------------------------------------------------------------------------------
def test_dry_run_is_default_and_touches_nothing():
    # No --execute -> DRY-RUN: print the plan, make NO ssh/scp/gh call. Fakes are wired but must go
    # unused; if the script called them the fake would append to the log.
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, EXE_NAME)
        with open(fake_bin, "w") as f:
            f.write("MZ")  # a stand-in exe
        env, log = _fake_env(tmp, "deadbeef")
        r = _run_script(["--host", "10.77.9.202", "--binary", fake_bin], env=env)
        assert r.returncode == 0, "dry-run should succeed: %s%s" % (r.stdout, r.stderr)
        out = r.stdout + r.stderr
        assert INSTALL_DIR in out, "dry-run must name the install dir"
        assert TASK_NAME in out, "dry-run must name the task"
        assert PORT in out, "dry-run must name the verify port"
        assert "10.77.9.202" in out, "dry-run must name the host"
        assert re.search(r"dry.?run", out, re.I), "dry-run must say so"
        assert not os.path.exists(log) or open(log).read() == "", \
            "dry-run must make NO ssh/scp call (log must be empty)"


def test_missing_host_uses_default_strih_or_is_accepted():
    # --host defaults to strih (10.77.9.202); a bare dry-run with a --binary must still succeed.
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, EXE_NAME)
        open(fake_bin, "w").close()
        env, _ = _fake_env(tmp, "x")
        r = _run_script(["--binary", fake_bin], env=env)
        assert r.returncode == 0, "default-host dry-run should succeed: %s%s" % (r.stdout, r.stderr)
        assert "10.77.9.202" in (r.stdout + r.stderr), "default host must be strih"


# ---------------------------------------------------------------------------------------------
# .sh: --execute scp sequence + runs the installer with -Execute (fakes)
# ---------------------------------------------------------------------------------------------
def _write_fake(path, body):
    with open(path, "w") as f:
        f.write(body)
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)


def _fake_env(tmp, remote_sha):
    """Fake ssh/scp that log argv into one file; the ssh fake answers a `certutil -hashfile ... SHA256`
    byte-verify probe with `remote_sha` in certutil's real 3-line shape (line 2 = the hash), and is a
    no-op for the installer-run command. Pass the staged exe's real local sha as `remote_sha` for a
    matching (passing) byte-verify."""
    log = os.path.join(tmp, "calls.log")
    fake_ssh = os.path.join(tmp, "fake-ssh")
    ssh_body = (
        "#!/usr/bin/env bash\n"
        'printf "SSH %s\\n" "$*" >> "__LOG__"\n'
        'cmd="${!#}"\n'
        'case "$cmd" in\n'
        '  *certutil*) printf "SHA256 hash of x:\\n__SHA__\\nCertUtil: -hashfile command completed successfully.\\n" ;;\n'
        "  *) : ;;\n"
        "esac\n"
        "exit 0\n"
    ).replace("__LOG__", log).replace("__SHA__", remote_sha)
    _write_fake(fake_ssh, ssh_body)
    fake_scp = os.path.join(tmp, "fake-scp")
    scp_body = (
        "#!/usr/bin/env bash\n"
        'printf "SCP %s\\n" "$*" >> "__LOG__"\n'
        "exit 0\n"
    ).replace("__LOG__", log)
    _write_fake(fake_scp, scp_body)
    env = {
        "BKSHADING_SVC_SSH": fake_ssh,
        "BKSHADING_SVC_SCP": fake_scp,
        "BKSHADING_SVC_SSHPASS_PREFIX": "",  # bypass sshpass -> fakes run bare
    }
    return env, log


def _sha256(path):
    return subprocess.run(["sha256sum", path], capture_output=True, text=True, check=True).stdout.split()[0]


def test_execute_scps_the_three_files_then_runs_installer():
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, EXE_NAME)
        with open(fake_bin, "w") as f:
            f.write("MZ-exe")
        seed = os.path.join(tmp, CONFIG_EXAMPLE_NAME)
        with open(seed, "w") as f:
            f.write("bind = \"0.0.0.0:%s\"\n" % PORT)
        env, log = _fake_env(tmp, _sha256(fake_bin))  # matching sha -> byte-verify passes
        r = _run_script(
            ["--host", "10.77.9.202", "--binary", fake_bin, "--config-seed", seed, "--execute"],
            env=env,
        )
        assert r.returncode == 0, "execute should succeed: %s%s" % (r.stdout, r.stderr)
        calls = open(log).read()
        # three scp uploads: the exe, the config seed, and the installer ps1.
        assert "SCP" in calls, "must scp the payload"
        assert EXE_NAME in calls, "must scp the service exe"
        assert CONFIG_EXAMPLE_NAME in calls, "must scp the config seed"
        assert INSTALLER_PS1_NAME in calls, "must scp the installer ps1"
        # then run the installer via powershell -File ... -Execute, NEVER a nested -Command.
        assert "SSH" in calls, "must run the installer over ssh"
        assert re.search(r"powershell[^\n]*-File[^\n]*%s" % re.escape(INSTALLER_PS1_NAME), calls), \
            "must invoke the installer via powershell -File"
        assert "-Execute" in calls, "the installer must be run with -Execute on a real deploy"
        assert "-Command" not in calls, "NEVER a nested powershell -Command over ssh"
        # byte-verify the staged exe before running the installer (mirrors the relay sibling).
        assert "certutil" in calls, "must byte-verify the staged exe (certutil sha256)"
        # ordering: every scp happens before the installer-run ssh call.
        i_ssh_run = calls.index(INSTALLER_PS1_NAME + '" -')  # the run-installer ssh command
        i_last_scp = calls.rfind("SCP ")
        assert i_last_scp < i_ssh_run, "all scp uploads must precede the installer run"
        assert calls.index("certutil") < i_ssh_run, "byte-verify must precede the installer run"


def test_execute_passes_port_and_task_to_installer():
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, EXE_NAME)
        open(fake_bin, "w").close()
        seed = os.path.join(tmp, CONFIG_EXAMPLE_NAME)
        open(seed, "w").close()
        env, log = _fake_env(tmp, _sha256(fake_bin))  # matching sha -> byte-verify passes
        r = _run_script(
            ["--host", "10.77.9.202", "--binary", fake_bin, "--config-seed", seed, "--execute"],
            env=env,
        )
        assert r.returncode == 0
        calls = open(log).read()
        assert "-Port %s" % PORT in calls, "installer run must pass -Port %s" % PORT
        assert "-TaskName %s" % TASK_NAME in calls, "installer run must pass -TaskName"


def test_execute_byte_verify_sha_mismatch_fails():
    # A truncated / wrong-sha staged exe must fail the deploy at the byte-verify (never a false OK).
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, EXE_NAME)
        with open(fake_bin, "w") as f:
            f.write("MZ-exe")
        seed = os.path.join(tmp, CONFIG_EXAMPLE_NAME)
        open(seed, "w").close()
        env, log = _fake_env(tmp, "deadbeef_wrong_sha")  # remote sha != local -> mismatch
        r = _run_script(
            ["--host", "10.77.9.202", "--binary", fake_bin, "--config-seed", seed, "--execute"],
            env=env,
        )
        assert r.returncode != 0, "a sha256 mismatch must fail the deploy"
        assert re.search(r"mismatch", r.stdout + r.stderr, re.I), "the failure must name the byte-verify mismatch"
        # the installer must NOT run after a failed byte-verify.
        calls = open(log).read() if os.path.exists(log) else ""
        assert (INSTALLER_PS1_NAME + '" -') not in calls, "installer must not run when byte-verify failed"


# ---------------------------------------------------------------------------------------------
# .ps1 static structure (no pwsh on dev1 CI)
# ---------------------------------------------------------------------------------------------
def test_ps1_has_execute_keepalive_uninstall_switches():
    s = _ps1()
    assert re.search(r"\[switch\]\$Execute", s), "must have -Execute (mutating) switch"
    assert re.search(r"\[switch\]\$KeepAlive", s), "must have -KeepAlive (per-tick relaunch) switch"
    assert re.search(r"\[switch\]\$Uninstall", s), "must have -Uninstall switch"


def test_ps1_dry_run_is_default():
    # Default (no -Execute) must be a PLAN that mutates nothing — an `if (-not $Execute)` guard.
    s = _ps1()
    assert re.search(r"if\s*\(\s*-not\s*\$Execute\s*\)", s), \
        "default must be dry-run (guarded by -not $Execute)"
    assert re.search(r"dry.?run", s, re.I), "the ps1 must announce DRY-RUN"


def test_ps1_registers_keepalive_scheduled_task():
    s = _ps1()
    assert "Register-ScheduledTask" in s
    assert "-Force" in s, "task registration must be idempotent (-Force)"
    assert "New-ScheduledTaskTrigger" in s
    assert re.search(r"-AtLogOn|-AtStartup", s), "task must start at logon/boot"
    assert "RepetitionInterval" in s, "keep-alive = a repetition interval (Task Scheduler has no Restart=on-failure)"
    assert "New-TimeSpan -Minutes" in s
    assert TASK_NAME in s


def test_ps1_keepalive_pass_relaunches_when_absent():
    s = _ps1()
    # the -KeepAlive pass must check the running process by its EXACT exe path (never a bare name —
    # avsync-monitoring.md gotcha #2) and relaunch via Start-Process if absent. Pin the LITERAL
    # exact-path match so a regression to a bare-name match is caught (issue 808 review).
    assert "Win32_Process" in s, "keep-alive must query Win32_Process"
    assert re.search(r"\$_\.ExecutablePath\s*-eq\s*\$ExePath", s), \
        "the process match must be the EXACT exe path ($_.ExecutablePath -eq $ExePath), never a bare name"
    assert "Start-Process" in s, "keep-alive must relaunch the service when it is not running"


def test_ps1_port_verify_confirms_owner_not_just_any_listener():
    # issue 808 review: a stale/foreign instance on :8770 must NOT false-green the deploy — the verify
    # must resolve the listening connection's owning process and require it be the deployed exe.
    s = _ps1()
    assert "OwningProcess" in s, "port verify must resolve the connection's OwningProcess"
    assert re.search(r"ProcessId=\$\(\$conn\.OwningProcess\)", s), \
        "must look up the owning process by the connection's OwningProcess id"
    assert re.search(r"\$owner\s*-eq\s*\$ExePath", s), \
        "must require the port owner to be exactly the deployed exe path"


def test_ps1_verifies_port_listening():
    s = _ps1()
    assert "Get-NetTCPConnection" in s, "install must verify the panel port is Listening"
    assert PORT in s
    assert re.search(r"Listen", s), "the port verify must check the Listen state"


def test_ps1_config_seeded_only_if_absent_never_clobbers():
    s = _ps1()
    # config-preserve: copy the example -> bkshading.toml ONLY IF the operator config is absent.
    assert CONFIG_NAME in s
    assert CONFIG_EXAMPLE_NAME in s
    assert re.search(r"if\s*\(\s*-not\s*\(\s*Test-Path", s), \
        "config must be seeded only if absent (never clobber an operator-tuned config)"


def test_ps1_launches_service_with_config():
    s = _ps1()
    # the service is launched with --config <InstallDir>\bkshading.toml (main.rs --config flag).
    assert "--config" in s, "the service must be launched with its --config flag"


def test_ps1_uninstall_unregisters_task():
    s = _ps1()
    assert "Unregister-ScheduledTask" in s


def test_ps1_error_action_stop():
    s = _ps1()
    assert re.search(r"\$ErrorActionPreference\s*=\s*['\"]Stop['\"]", s), \
        "a mutating ps1 must fail loud ($ErrorActionPreference = 'Stop')"


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
