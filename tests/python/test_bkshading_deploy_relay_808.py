#!/usr/bin/env python3
"""bkshading M3 — CI binary artifacts + relay deploy path (issue 808; unblocks the live rig verify).

Everything already merged (M1 relay+service skeleton, M2 NDI preview, WS live-state push, relay
provisioning) could NOT be verified on the live rig because:
  (1) CI produced NO deployable bkshading artifact — the `bkshading` job only `cargo build --bins`
      (debug, compile-proof) with no `upload-artifact`, and `bkshading-windows` only `cargo check`;
  (2) `scripts/bkshading-provision-relay.sh` explicitly expects "the CI-built bkshading-relay binary
      (separate supervisor step)" and its `--check` FAILS on a missing binary — but nothing produced
      it and there was NO deploy tool.

This milestone closes both halves: the CI jobs now release-build + upload the relay/service binaries
(`bkshading-linux-amd64`, `bkshading-windows-amd64`), and `scripts/bkshading-deploy-relay.sh`
(+ pure lib `scripts/lib/bkshading-deploy-runtime.sh`) deploys the CI-built relay to a cambox with
the deploy-fleet remount-rw -> scp -> sha256 byte-verify -> remount-ro cycle, ENABLE-ONLY (never
starts the service — reboot / the post-reboot verify brings it live, per provisioning-scripts.md).

These stdlib-only + pyyaml structural/behavioural tests run in the `python-tests` CI job (no Rust
toolchain, no root, no real ssh/scp/gh — the impure ops are overridden to fakes into a temp root):
 - the deploy script + lib parse (`bash -n`);
 - the pure lib decisions (artifact name, relay bin name, ENABLE-ONLY should-start=no, sha match);
 - the CI `bkshading` / `bkshading-windows` jobs release-build + upload the deployable binaries,
   with NO `continue-on-error` added;
 - a `--dry-run` prints the correct plan and touches nothing;
 - a fake `--binary` deploy runs remount-rw -> scp(/usr/local/bin/bkshading-relay) -> remount-ro
   and NEVER `systemctl start`/`restart` (the enable-only safety invariant);
 - a sha256 mismatch fails; a missing --host fails with usage;
 - the deploy tool AGREES with `bkshading-relay-runtime.sh` on the relay bin path;
 - Bluetooth appears NOWHERE (owner hard rule).
Runnable directly (`python3 tests/python/test_bkshading_deploy_relay_808.py`) or under pytest.
"""
import os
import re
import stat
import subprocess
import tempfile

import yaml

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
SCRIPT = os.path.join(REPO, "scripts", "bkshading-deploy-relay.sh")
LIB = os.path.join(REPO, "scripts", "lib", "bkshading-deploy-runtime.sh")
RELAY_LIB = os.path.join(REPO, "scripts", "lib", "bkshading-relay-runtime.sh")
CI_YML = os.path.join(REPO, ".github", "workflows", "ci.yml")

RELAY_BIN_PATH = "/usr/local/bin/bkshading-relay"
LINUX_ARTIFACT = "bkshading-linux-amd64"
WINDOWS_ARTIFACT = "bkshading-windows-amd64"


def _bash(snippet):
    """Source the lib, run `snippet`, return stripped stdout (raises on nonzero)."""
    src = '. "%s"\n%s' % (LIB, snippet)
    out = subprocess.run(["bash", "-c", src], capture_output=True, text=True, check=True)
    return out.stdout.strip()


def _run_script(args, env=None, check=False):
    """Run the deploy script with args; returns CompletedProcess."""
    e = dict(os.environ)
    if env:
        e.update(env)
    return subprocess.run(
        ["bash", SCRIPT] + args, capture_output=True, text=True, env=e, check=check
    )


# ---------------------------------------------------------------------------------------------
# parse
# ---------------------------------------------------------------------------------------------
def test_script_and_lib_parse():
    for f in (SCRIPT, LIB):
        assert os.path.exists(f), "missing %s" % f
        r = subprocess.run(["bash", "-n", f], capture_output=True, text=True)
        assert r.returncode == 0, "bash -n failed on %s: %s" % (f, r.stderr)


def test_deploy_script_is_executable():
    assert os.access(SCRIPT, os.X_OK), "%s must be executable" % SCRIPT


# ---------------------------------------------------------------------------------------------
# pure lib decisions
# ---------------------------------------------------------------------------------------------
def test_artifact_name():
    assert _bash("bkshading_deploy_artifact_name") == LINUX_ARTIFACT


def test_relay_artifact_bin_name():
    assert _bash("bkshading_deploy_relay_artifact_bin") == "bkshading-relay"


def test_should_start_is_never_yes_enable_only_invariant():
    # ENABLE-ONLY: a relay binary deploy NEVER starts the service. This pure predicate is the single
    # source of truth; pinning it to "no" makes a future accidental start a RED test.
    assert _bash("bkshading_deploy_should_start") == "no"


def test_sha_match_decision():
    assert _bash('bkshading_deploy_sha_match abc123 abc123') == "match"
    assert _bash('bkshading_deploy_sha_match abc123 def456') == "mismatch"
    # empty local or remote -> mismatch (never a false "match" on a failed read).
    assert _bash('bkshading_deploy_sha_match "" ""') == "mismatch"
    assert _bash('bkshading_deploy_sha_match abc123 ""') == "mismatch"


# ---------------------------------------------------------------------------------------------
# CI: release build + upload the deployable binaries; no continue-on-error
# ---------------------------------------------------------------------------------------------
def _load_ci():
    with open(CI_YML) as f:
        return yaml.safe_load(f)


def _job_step_runs(job):
    return "\n".join(s.get("run", "") for s in job.get("steps", []))


def _job_uploads(job, artifact_name):
    for s in job.get("steps", []):
        if str(s.get("uses", "")).startswith("actions/upload-artifact") and \
                s.get("with", {}).get("name") == artifact_name:
            return s.get("with", {}).get("path", "")
    return None


def test_bkshading_job_release_builds_and_uploads_relay_and_service():
    ci = _load_ci()
    job = ci["jobs"]["bkshading"]
    runs = _job_step_runs(job)
    # release build of BOTH the relay (default features) and the service (--features ndi, deploy shape)
    assert "cargo build --release" in runs, "bkshading job must release-build the deployable binaries"
    assert re.search(r"cargo build --release[^\n]*-p bkshading-relay", runs), \
        "must release-build the relay"
    assert re.search(r"cargo build --release[^\n]*--features ndi[^\n]*-p bkshading\b", runs) or \
        re.search(r"cargo build --release[^\n]*-p bkshading\b[^\n]*--features ndi", runs), \
        "must release-build the service with --features ndi"
    path = _job_uploads(job, LINUX_ARTIFACT)
    assert path is not None, "bkshading job must upload the %s artifact" % LINUX_ARTIFACT
    assert "target/release/bkshading-relay" in path
    assert re.search(r"target/release/bkshading\b", path), "must upload the service binary"


def test_bkshading_windows_job_builds_and_uploads_service():
    ci = _load_ci()
    job = ci["jobs"]["bkshading-windows"]
    runs = _job_step_runs(job)
    assert re.search(r"cargo build --release[^\n]*--features ndi", runs), \
        "windows job must release-build the service with --features ndi (the strih ship shape)"
    path = _job_uploads(job, WINDOWS_ARTIFACT)
    assert path is not None, "windows job must upload the %s artifact" % WINDOWS_ARTIFACT
    assert re.search(r"target/release/bkshading\.exe", path)


def test_no_continue_on_error_in_bkshading_jobs():
    ci = _load_ci()
    for name in ("bkshading", "bkshading-windows"):
        job = ci["jobs"][name]
        assert "continue-on-error" not in job, "%s job must not use continue-on-error" % name
        for s in job.get("steps", []):
            assert "continue-on-error" not in s, "%s step must not use continue-on-error" % name


# ---------------------------------------------------------------------------------------------
# --dry-run plan
# ---------------------------------------------------------------------------------------------
def test_dry_run_prints_plan_and_touches_nothing():
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, "bkshading-relay")
        with open(fake_bin, "w") as f:
            f.write("#!/bin/true\n")
        r = _run_script(["--host", "10.77.9.201", "--binary", fake_bin, "--dry-run"])
        assert r.returncode == 0, "dry-run should succeed: %s%s" % (r.stdout, r.stderr)
        out = r.stdout + r.stderr
        assert RELAY_BIN_PATH in out, "dry-run must name the deploy target path"
        assert "10.77.9.201" in out, "dry-run must name the target host"
        # enable-only intent surfaced; never a start.
        assert re.search(r"(NOT start|enable-only|not.*start|reboot)", out, re.I)
        assert "remount,rw" in out and "remount,ro" in out, "dry-run must describe the ro-root swap"


def test_missing_host_fails_with_usage():
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, "bkshading-relay")
        open(fake_bin, "w").close()
        r = _run_script(["--binary", fake_bin, "--dry-run"])
        assert r.returncode != 0, "missing --host must fail"
        assert re.search(r"(host|usage)", r.stdout + r.stderr, re.I)


def test_option_as_last_token_fails_with_bad_args_not_silent_abort():
    # `--host` with no following value must exit 2 (bad args) with a message — NOT a bare `shift 2`
    # that aborts silently under `set -euo pipefail` (exit 1, no diagnostic).
    r = _run_script(["--host"])
    assert r.returncode == 2, "an option missing its value must exit 2 (bad args), got %d" % r.returncode
    assert re.search(r"(requires a value|host|usage)", r.stdout + r.stderr, re.I), \
        "must print a diagnostic naming the missing value"


# ---------------------------------------------------------------------------------------------
# fake --binary deploy: proves the remount->scp->remount sequence and ENABLE-ONLY (no systemctl start)
# ---------------------------------------------------------------------------------------------
def _write_fake(path, body):
    with open(path, "w") as f:
        f.write(body)
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)


def _fake_deploy_env(tmp, remote_sha):
    """Fake ssh/scp that log argv; ssh's sha256sum echoes remote_sha. Returns (env, logfile).

    Built with sentinel replacement (not %-formatting) so the literal bash ``%s`` printf specs never
    collide with Python string interpolation.
    """
    log = os.path.join(tmp, "calls.log")
    # Fake ssh: log the command; if it's a sha256sum, print the injected sha for the relay path.
    fake_ssh = os.path.join(tmp, "fake-ssh")
    # The real remote command is `sha256sum <path> | awk '{print $1}'`, so a faithful fake returns
    # ONLY the sha (the awk'd first field), not the raw `sha256sum` "<sha>  <path>" line. The
    # `test -x … && echo yes` exec-bit probe is faked as "yes" (a deployed binary is executable).
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
    _write_fake(fake_ssh, ssh_body)
    # Fake scp: log argv (the last non-flag arg is the remote target root@host:/path).
    fake_scp = os.path.join(tmp, "fake-scp")
    scp_body = (
        "#!/usr/bin/env bash\n"
        'printf "SCP %s\\n" "$*" >> "__LOG__"\n'
        "exit 0\n"
    ).replace("__LOG__", log)
    _write_fake(fake_scp, scp_body)
    env = {
        "BKSHADING_DEPLOY_SSH": fake_ssh,
        "BKSHADING_DEPLOY_SCP": fake_scp,
        "BKSHADING_DEPLOY_SSHPASS_PREFIX": "",  # bypass sshpass -> fakes run bare
    }
    return env, log


def test_fake_deploy_sequence_and_enable_only():
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, "bkshading-relay")
        with open(fake_bin, "wb") as f:
            f.write(b"RELAYBINARYCONTENT")
        # local sha256 of the fake binary (what the script computes + the fake remote must echo).
        local_sha = subprocess.run(
            ["sha256sum", fake_bin], capture_output=True, text=True, check=True
        ).stdout.split()[0]
        env, log = _fake_deploy_env(tmp, local_sha)
        r = _run_script(["--host", "10.77.9.201", "--binary", fake_bin], env=env)
        assert r.returncode == 0, "deploy should succeed on matching sha: %s%s" % (r.stdout, r.stderr)
        calls = open(log).read()
        # Full ro-root swap cycle in order: remount,rw  <  scp  <  byte-verify(sha read)  <  remount,ro.
        scp_target = "root@10.77.9.201:" + RELAY_BIN_PATH
        assert "remount,rw /" in calls, "must remount rw before scp"
        assert "remount,ro /" in calls, "must remount ro after the swap"
        assert scp_target in calls, "must scp to the relay bin path"
        assert "sha256sum" in calls, "must byte-verify (remote sha read)"
        i_rw = calls.index("remount,rw /")
        i_scp = calls.index(scp_target)
        i_sha = calls.index("sha256sum")
        i_ro = calls.index("remount,ro /")
        assert i_rw < i_scp < i_ro, "swap order must be remount,rw -> scp -> remount,ro"
        assert i_sha < i_ro, "byte-verify must read the fresh file before remounting ro"
        # ENABLE-ONLY: NEVER start/restart the service.
        assert "systemctl start" not in calls, "deploy must never start the service"
        assert "systemctl restart" not in calls, "deploy must never restart the service"
        assert "enable --now" not in calls, "deploy must never enable --now the service"


def test_fake_deploy_sha_mismatch_fails():
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, "bkshading-relay")
        with open(fake_bin, "wb") as f:
            f.write(b"RELAYBINARYCONTENT")
        env, log = _fake_deploy_env(tmp, "deadbeef_wrong_sha")
        r = _run_script(["--host", "10.77.9.201", "--binary", fake_bin], env=env)
        assert r.returncode != 0, "a sha256 mismatch must fail the deploy"
        assert re.search(r"mismatch", r.stdout + r.stderr, re.I), \
            "the failure must name the byte-verify mismatch"


# ---------------------------------------------------------------------------------------------
# cross-checks: no drift with the relay runtime lib; no Bluetooth
# ---------------------------------------------------------------------------------------------
def test_relay_bin_path_agrees_with_relay_runtime_lib():
    got = subprocess.run(
        ["bash", "-c", '. "%s"\nbkshading_relay_bin_path' % RELAY_LIB],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    assert got == RELAY_BIN_PATH, "relay bin path drift: runtime lib says %s" % got
    # the deploy script must place the binary at exactly that path.
    assert RELAY_BIN_PATH in open(SCRIPT).read()


def test_no_bluetooth_anywhere():
    # `ble` is matched as a standalone word (\bble\b) — a bare "ble " substring would false-match the
    # ubiquitous "enable "/"disable "/"table ", so the acronym BLE is checked with word boundaries.
    # Scans the deploy script, its lib, AND the CI workflow (the epic hard rule spans all of them).
    for f in (SCRIPT, LIB, CI_YML):
        text = open(f).read().lower()
        for banned in ("bluetooth", "bluez", "gatt"):
            assert banned not in text, "%s must not mention %r (owner hard rule)" % (f, banned)
        assert not re.search(r"\bble\b", text), "%s must not mention BLE (owner hard rule)" % f


if __name__ == "__main__":
    import sys

    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    failed = 0
    for fn in fns:
        try:
            fn()
            print("ok   %s" % fn.__name__)
        except Exception as e:  # noqa: BLE001 - test runner surfaces the failure, never swallows it
            failed += 1
            print("FAIL %s: %s" % (fn.__name__, e))
    print("\n%d/%d passed" % (len(fns) - failed, len(fns)))
    sys.exit(1 if failed else 0)
