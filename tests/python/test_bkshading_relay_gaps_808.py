#!/usr/bin/env python3
"""issue 808 -- the two bkshading RELAY code gaps found live on 25.9.2026.

Gap 1 (setup-device.sh never provisions the relay): the M.2 re-provisioning of cam1-4 left NO relay
on them -- no unit, no binary, no gphoto2 -- because the relay was installed only by the separate
bkshading-provision-relay.sh + bkshading-deploy-relay.sh, and verify-device.sh graded a missing relay
`na`. The fix: the relay install functions live in ONE sourced lib
(scripts/lib/bkshading-relay-provision.sh) that BOTH bkshading-provision-relay.sh and setup-device.sh
source; setup-device.sh provisions the relay ENABLE-only with the enable-state following the rig mode
(TEST: the relay roster -- the source box + cam2 -- is installed DISABLED, the issue-1311 passive
rule); verify-device.sh gains the (ao) relay check before (q).

Gap 2 (bkshading-deploy-relay.sh): it swapped the binary under a RUNNING relay, so the final
`remount,ro` failed "busy" on the deleted-but-open binary -- and the script swallowed that failure,
reported OK and left the root read-WRITE (cam6/cam7). It also resolved a stale main run. The fix:
STOP the relay before the swap and restore its previous active state after; FAIL LOUD (non-zero,
naming the holder via `fuser -vm /` / `lsof +L1`) when the ro remount fails; resolve the newest
SUCCESSFUL run that carries the artifact through the ONE shared resolver deploy-fleet.sh also uses
(scripts/lib/ci-run-resolve.sh).

Tier-0: stdlib-only, fakes for ssh/scp/gh/systemctl, a temp root -- no rig, no root, no network.
"""
import hashlib
import json
import os
import re
import stat
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
DEPLOY = os.path.join(REPO, "scripts", "bkshading-deploy-relay.sh")
DEPLOY_FLEET = os.path.join(REPO, "scripts", "deploy-fleet.sh")
PROVISION = os.path.join(REPO, "scripts", "bkshading-provision-relay.sh")
SETUP = os.path.join(REPO, "scripts", "setup-device.sh")
VERIFY = os.path.join(REPO, "scripts", "verify-device.sh")
PROV_LIB = os.path.join(REPO, "scripts", "lib", "bkshading-relay-provision.sh")
RESOLVE_LIB = os.path.join(REPO, "scripts", "lib", "ci-run-resolve.sh")
UNIT = os.path.join(REPO, "systemd", "bkshading-relay.service")
RELAY_BIN_PATH = "/usr/local/bin/bkshading-relay"
ARTIFACT = "bkshading-linux-amd64"


def _read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


def _write_exec(path, body):
    with open(path, "w", encoding="utf-8") as f:
        f.write(body)
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)


def _src(lib, snippet, env=None):
    e = dict(os.environ)
    if env:
        e.update(env)
    return subprocess.run(
        ["bash", "-c", '. "%s"\n%s' % (lib, snippet)], capture_output=True, text=True, env=e
    )


def _noncomment(text):
    return "\n".join(ln for ln in text.splitlines() if not ln.lstrip().startswith("#"))


# =============================================================================================
# Shared CI run resolver (scripts/lib/ci-run-resolve.sh) -- ONE resolver, deploy-fleet + relay
# =============================================================================================
RUNS = [
    # deliberately OUT of createdAt order, with a failure newer than every success
    {"databaseId": 100, "createdAt": "2026-09-04T09:17:04Z", "conclusion": "success", "headSha": "aaa"},
    {"databaseId": 300, "createdAt": "2026-09-25T17:34:20Z", "conclusion": "success", "headSha": "ccc"},
    {"databaseId": 400, "createdAt": "2026-09-25T19:00:00Z", "conclusion": "failure", "headSha": "ddd"},
    {"databaseId": 200, "createdAt": "2026-09-25T07:08:01Z", "conclusion": "success", "headSha": "bbb"},
]


def test_resolve_lib_parses():
    assert os.path.isfile(RESOLVE_LIB), "missing the shared resolver lib %s" % RESOLVE_LIB
    r = subprocess.run(["bash", "-n", RESOLVE_LIB], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr


def test_newest_success_ids_orders_by_created_at_and_drops_failures():
    r = _src(RESOLVE_LIB, 'printf "%s" "$J" | ci_run_newest_success_ids', env={"J": json.dumps(RUNS)})
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["300", "200", "100"], r.stdout


def _fake_gh(tmp, runs, artifacts_by_run, log):
    """A fake gh: `run list` prints RUNS; `api .../runs/<id>/artifacts` lists that run's artifacts;
    `run view <id>` prints a headSha; `run download <id> -n <a> --dir <d>` writes <d>/bkshading-relay."""
    runs_json = os.path.join(tmp, "runs.json")
    with open(runs_json, "w", encoding="utf-8") as f:
        json.dump(runs, f)
    arts = os.path.join(tmp, "arts")
    os.makedirs(arts, exist_ok=True)
    for rid, names in artifacts_by_run.items():
        with open(os.path.join(arts, str(rid)), "w", encoding="utf-8") as f:
            f.write("\n".join(names) + ("\n" if names else ""))
    gh = os.path.join(tmp, "fake-gh")
    body = (
        "#!/usr/bin/env bash\n"
        'printf "GH %s\\n" "$*" >> "__LOG__"\n'
        'if [ "$1 $2" = "run list" ]; then cat "__RUNS__"; exit 0; fi\n'
        'if [ "$1" = "api" ]; then\n'
        '  id="$(printf "%s" "$2" | sed -n "s#.*/runs/\\([0-9]*\\)/artifacts.*#\\1#p")"\n'
        '  [ -f "__ARTS__/$id" ] && cat "__ARTS__/$id"; exit 0\n'
        "fi\n"
        'if [ "$1 $2" = "run view" ]; then echo "sha-of-$3"; exit 0; fi\n'
        'if [ "$1 $2" = "run download" ]; then\n'
        '  d=""; while [ $# -gt 0 ]; do [ "$1" = "--dir" ] && d="$2"; shift; done\n'
        '  mkdir -p "$d"; printf "RELAYBIN" > "$d/bkshading-relay"; printf "CAMBOX" > "$d/camera-box"; exit 0\n'
        "fi\n"
        "exit 0\n"
    ).replace("__LOG__", log).replace("__RUNS__", runs_json).replace("__ARTS__", arts)
    _write_exec(gh, body)
    return gh


def test_latest_success_skips_a_run_missing_the_artifact():
    with tempfile.TemporaryDirectory() as tmp:
        log = os.path.join(tmp, "gh.log")
        # the newest SUCCESS (300) has no bkshading artifact -> the resolver falls to 200.
        gh = _fake_gh(tmp, RUNS, {300: ["camera-box-linux-amd64"], 200: [ARTIFACT], 100: [ARTIFACT]}, log)
        r = _src(RESOLVE_LIB, "ci_run_latest_success zbynekdrlik/camera-box main ci.yml %s" % ARTIFACT,
                 env={"CI_RUN_RESOLVE_GH": gh})
        assert r.returncode == 0, r.stderr
        assert r.stdout.strip() == "200", (r.stdout, r.stderr)
        # the run list is NOT filtered server-side by status (the stale-pick path) -- client-side only
        calls = _read(log)
        assert "--status" not in calls, calls


def test_latest_success_picks_newest_success_by_created_at():
    with tempfile.TemporaryDirectory() as tmp:
        log = os.path.join(tmp, "gh.log")
        gh = _fake_gh(tmp, RUNS, {300: [ARTIFACT], 200: [ARTIFACT], 100: [ARTIFACT]}, log)
        r = _src(RESOLVE_LIB, "ci_run_latest_success zbynekdrlik/camera-box main ci.yml %s" % ARTIFACT,
                 env={"CI_RUN_RESOLVE_GH": gh})
        assert r.returncode == 0, r.stderr
        assert r.stdout.strip() == "300", r.stdout


def test_latest_success_fails_when_no_run_carries_the_artifact():
    with tempfile.TemporaryDirectory() as tmp:
        log = os.path.join(tmp, "gh.log")
        gh = _fake_gh(tmp, RUNS, {}, log)
        r = _src(RESOLVE_LIB, "ci_run_latest_success zbynekdrlik/camera-box main ci.yml %s" % ARTIFACT,
                 env={"CI_RUN_RESOLVE_GH": gh})
        assert r.returncode != 0, "no run carrying the artifact must be a non-zero resolve"
        assert r.stdout.strip() == "", r.stdout


def test_deploy_fleet_and_relay_deploy_share_the_one_resolver():
    for p in (DEPLOY, DEPLOY_FLEET):
        s = _read(p)
        assert "lib/ci-run-resolve.sh" in s, "%s must source the shared resolver" % p
        assert "ci_run_latest_success" in _noncomment(s), "%s must call ci_run_latest_success" % p
    # the old server-side-filtered one-shot query is gone from both
    for p in (DEPLOY, DEPLOY_FLEET):
        assert not re.search(r"--status success --limit 1", _noncomment(_read(p))), p


# =============================================================================================
# bkshading-deploy-relay.sh: stop before swap, restore after, FAIL LOUD on a busy ro remount
# =============================================================================================
def _fake_obs_phase2_idle(dirpath):
    _write_exec(
        os.path.join(dirpath, "obs_phase2.py"),
        "#!/usr/bin/env python3\nimport sys, json\n"
        "if len(sys.argv) > 1 and sys.argv[1] == 'rig-busy-check':\n"
        "    print(json.dumps({'busy': False, 'diagnostics': [{'host': 'strih', 'streaming': False, "
        "'recording': False}, {'host': 'stream', 'streaming': False, 'recording': False}]}))\n",
    )


def _deploy_env(tmp, remote_sha, was_active="active", ro_fails=False):
    """Fake ssh answering is-active/sha256sum/test -x/remount/lsof; fake scp. Returns (env, log)."""
    log = os.path.join(tmp, "calls.log")
    ssh = os.path.join(tmp, "fake-ssh")
    body = (
        "#!/usr/bin/env bash\n"
        'printf "SSH %s\\n" "$*" >> "__LOG__"\n'
        'cmd="${!#}"\n'
        'case "$cmd" in\n'
        '  *"is-active"*) printf "__WAS__\\n" ;;\n'
        '  *sha256sum*) printf "__SHA__\\n" ;;\n'
        '  *"test -x"*) printf "yes\\n" ;;\n'
        '  *"remount,ro"*) [ "__ROFAIL__" = 1 ] && { echo "mount: /: mount point is busy." >&2; exit 32; } ;;\n'
        '  *"lsof +L1"*) printf "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NLINK NODE NAME\\n'
        'bkshading 4242 root txt REG 8,2 9000 0 1234 /usr/local/bin/bkshading-relay (deleted)\\n" ;;\n'
        "esac\n"
        "exit 0\n"
    ).replace("__LOG__", log).replace("__SHA__", remote_sha).replace("__WAS__", was_active)
    body = body.replace("__ROFAIL__", "1" if ro_fails else "0")
    _write_exec(ssh, body)
    scp = os.path.join(tmp, "fake-scp")
    _write_exec(scp, '#!/usr/bin/env bash\nprintf "SCP %s\\n" "$*" >> "' + log + '"\nexit 0\n')
    _fake_obs_phase2_idle(tmp)
    env = {
        "BKSHADING_DEPLOY_SSH": ssh,
        "BKSHADING_DEPLOY_SCP": scp,
        "BKSHADING_DEPLOY_SSHPASS_PREFIX": "",
        "BKSHADING_DEPLOY_OBS_PHASE2_DIR": tmp,
    }
    return env, log


def _bin(tmp):
    p = os.path.join(tmp, "bkshading-relay")
    with open(p, "wb") as f:
        f.write(b"RELAYBINARYCONTENT")
    with open(p, "rb") as f:
        return p, hashlib.sha256(f.read()).hexdigest()


def _run_deploy(args, env):
    e = dict(os.environ)
    e.update(env)
    return subprocess.run(["bash", DEPLOY] + args, capture_output=True, text=True, env=e)


def test_active_relay_is_stopped_before_the_swap_and_started_after_the_ro_remount():
    with tempfile.TemporaryDirectory() as tmp:
        b, sha = _bin(tmp)
        env, log = _deploy_env(tmp, sha, was_active="active")
        r = _run_deploy(["--host", "10.77.9.66", "--binary", b], env)
        assert r.returncode == 0, r.stdout + r.stderr
        calls = _read(log)
        i_stop = calls.find("systemctl stop bkshading-relay")
        i_rw = calls.find("remount,rw /")
        i_scp = calls.find("SCP ")
        i_ro = calls.find("remount,ro /")
        i_start = calls.find("systemctl start bkshading-relay")
        assert i_stop >= 0, "an ACTIVE relay must be stopped before the swap:\n" + calls
        assert i_start >= 0, "an ACTIVE relay must be restored (started) after the swap:\n" + calls
        assert i_stop < i_rw < i_scp < i_ro < i_start, "order must be stop -> rw -> scp -> ro -> start:\n" + calls


def test_inactive_relay_is_neither_stopped_nor_started():
    with tempfile.TemporaryDirectory() as tmp:
        b, sha = _bin(tmp)
        env, log = _deploy_env(tmp, sha, was_active="inactive")
        r = _run_deploy(["--host", "10.77.9.66", "--binary", b], env)
        assert r.returncode == 0, r.stdout + r.stderr
        calls = _read(log)
        assert "is-active" in calls, "the deploy must READ the relay's active state first:\n" + calls
        assert "systemctl stop bkshading-relay" not in calls
        assert "systemctl start bkshading-relay" not in calls, "an inactive relay must stay stopped"


def test_failed_ro_remount_fails_loud_and_names_the_holder():
    with tempfile.TemporaryDirectory() as tmp:
        b, sha = _bin(tmp)
        env, log = _deploy_env(tmp, sha, was_active="active", ro_fails=True)
        r = _run_deploy(["--host", "10.77.9.66", "--binary", b], env)
        out = r.stdout + r.stderr
        assert r.returncode != 0, "a failed remount,ro must FAIL the deploy (the cam6/cam7 rw root):\n" + out
        assert "OK: relay deployed" not in out, "must never report OK while the root stays read-write"
        assert re.search(r"read-?WRITE|read-write", out, re.I), out
        calls = _read(log)
        assert "fuser -vm /" in calls and "lsof +L1" in calls, "must gather the holder:\n" + calls
        assert "bkshading" in out and "4242" in out, "the error must NAME the holder (command + pid):\n" + out
        # the relay's previous active state is still restored on the failure path
        assert "systemctl start bkshading-relay" in calls, calls


def test_ro_holder_summary_is_pure():
    r = _src(
        os.path.join(REPO, "scripts", "lib", "bkshading-deploy-runtime.sh"),
        'bkshading_deploy_ro_holders "$T"',
        env={"T": "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NLINK NODE NAME\n"
                  "bkshading 4242 root txt REG 8,2 9000 0 1234 /usr/local/bin/bkshading-relay (deleted)\n"
                  "journald 99 root 5w REG 8,2 1 0 55 /var/x (deleted)\n"},
    )
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "bkshading[4242] /usr/local/bin/bkshading-relay; journald[99] /var/x", r.stdout
    empty = _src(os.path.join(REPO, "scripts", "lib", "bkshading-deploy-runtime.sh"),
                 'bkshading_deploy_ro_holders ""')
    assert empty.returncode == 0 and empty.stdout.strip() == ""


def test_restore_action_is_pure_and_never_starts_an_inactive_relay():
    lib = os.path.join(REPO, "scripts", "lib", "bkshading-deploy-runtime.sh")
    for was, want in (("active", "start"), ("inactive", "none"), ("failed", "none"), ("", "none"),
                      ("activating", "none")):
        r = _src(lib, 'bkshading_deploy_restore_action "$W"', env={"W": was})
        assert r.returncode == 0 and r.stdout.strip() == want, (was, r.stdout)
    # the enable-only invariant stays: a deploy never STARTS a relay that was not running
    assert _src(lib, "bkshading_deploy_should_start").stdout.strip() == "no"


def test_default_run_comes_from_the_shared_resolver_and_is_logged():
    with tempfile.TemporaryDirectory() as tmp:
        env, log = _deploy_env(tmp, hashlib.sha256(b"RELAYBIN").hexdigest(), was_active="inactive")
        glog = os.path.join(tmp, "gh.log")
        gh = _fake_gh(tmp, RUNS, {300: [ARTIFACT], 200: [ARTIFACT]}, glog)
        env["BKSHADING_DEPLOY_GH"] = gh
        r = _run_deploy(["--host", "10.77.9.66"], env)
        out = r.stdout + r.stderr
        assert r.returncode == 0, out
        assert re.search(r"run download 300\b", _read(glog)), _read(glog)
        assert "300" in out and "2026-09-25" in out, "the chosen run id + date must be printed:\n" + out


# =============================================================================================
# The ONE relay provisioning lib + its enable-state decision
# =============================================================================================
def test_provision_lib_parses_and_both_callers_source_it():
    assert os.path.isfile(PROV_LIB), "missing %s" % PROV_LIB
    r = subprocess.run(["bash", "-n", PROV_LIB], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    for p in (PROVISION, SETUP):
        assert "lib/bkshading-relay-provision.sh" in _read(p), "%s must source the ONE provisioning lib" % p
    # never a copy: the install body lives only in the lib
    assert "apt-get install -y -qq" not in _noncomment(_read(PROVISION)), "the install body moved to the lib"


def test_expected_enable_state_follows_the_rig_mode():
    cases = [
        ("cam1", "test", "disabled"),
        ("CAM1", "test", "disabled"),
        ("cam2", "test", "disabled"),
        ("cam3", "test", "enabled"),
        ("cam1", "event", "enabled"),
        ("cam2", "event", "enabled"),
        ("cam1", "unknown", "unknown"),
        ("cam3", "unknown", "enabled"),
    ]
    for dev, mode, want in cases:
        r = _src(PROV_LIB, 'bkshading_relay_expected_enable_state "$D" "$M" cam1 cam2',
                 env={"D": dev, "M": mode})
        assert r.returncode == 0, r.stderr
        assert r.stdout.strip() == want, (dev, mode, r.stdout)


def _fake_systemctl(tmp, calls):
    p = os.path.join(tmp, "systemctl")
    _write_exec(p, '#!/usr/bin/env bash\nprintf "%s\\n" "$*" >> "' + calls + '"\n'
                   'if [ "$1" = "is-enabled" ]; then cat "' + tmp + '/state" 2>/dev/null; fi\n'
                   'if [ "$1" = enable ]; then echo enabled > "' + tmp + '/state"; fi\n'
                   'if [ "$1" = disable ]; then echo disabled > "' + tmp + '/state"; fi\n')
    return p


def _prov_env(tmp):
    calls = os.path.join(tmp, "sc.log")
    return {
        "BKSHADING_RELAY_UNIT_DEST": os.path.join(tmp, "sysd", "bkshading-relay.service"),
        "BKSHADING_RELAY_ENV_FILE": os.path.join(tmp, "etc", "relay.env"),
        "BKSHADING_RELAY_BIN": os.path.join(tmp, "bin", "bkshading-relay"),
        "BKSHADING_RELAY_DROPIN_DIR": os.path.join(tmp, "sysd", "camera-box.service.d"),
        "BKSHADING_RELAY_GPHOTO2": "true",
        "BKSHADING_RELAY_SYSTEMCTL": _fake_systemctl(tmp, calls),
    }, calls


def test_install_disabled_writes_everything_and_never_enables_or_starts():
    with tempfile.TemporaryDirectory() as tmp:
        env, calls = _prov_env(tmp)
        src_bin = os.path.join(tmp, "dl-relay")
        with open(src_bin, "w") as f:
            f.write("RELAY")
        r = _src(PROV_LIB, 'bkshading_relay_provision_install disabled "$SRC"', env=dict(env, SRC=src_bin))
        assert r.returncode == 0, r.stdout + r.stderr
        assert os.path.isfile(env["BKSHADING_RELAY_UNIT_DEST"])
        assert os.path.isfile(env["BKSHADING_RELAY_ENV_FILE"])
        assert os.access(env["BKSHADING_RELAY_BIN"], os.X_OK), "the relay binary must be installed executable"
        log = _read(calls)
        assert "daemon-reload" in log and "disable" in log, log
        assert not re.search(r"^enable\b", log, re.M), "a TEST-roster box must not be enabled:\n" + log
        assert "start" not in log, "provisioning never starts the relay:\n" + log


def test_install_enabled_enables_and_reads_back():
    with tempfile.TemporaryDirectory() as tmp:
        env, calls = _prov_env(tmp)
        r = _src(PROV_LIB, "bkshading_relay_provision_install enabled", env=env)
        assert r.returncode == 0, r.stdout + r.stderr
        log = _read(calls)
        assert re.search(r"^enable bkshading-relay.service$", log, re.M), log
        assert "start" not in log, log


def test_binary_plan_is_pure():
    cases = [
        (("/tmp/x", "", "no"), "none"),  # a missing local path is not silently a URL
        (("https://h/relay", "", "no"), "url:https://h/relay"),
        (("", "36106119088", "yes"), "run:36106119088"),
        (("", "", "yes"), "latest"),
        (("", "", "no"), "none"),
    ]
    for (arg, run, gh), want in cases:
        r = _src(PROV_LIB, 'bkshading_relay_provision_binary_plan "$A" "$R" "$G"', env={"A": arg, "R": run, "G": gh})
        assert r.returncode == 0, r.stderr
        assert r.stdout.strip() == want, ((arg, run, gh), r.stdout)
    with tempfile.TemporaryDirectory() as tmp:
        p = os.path.join(tmp, "relay")
        open(p, "w").close()
        r = _src(PROV_LIB, 'bkshading_relay_provision_binary_plan "$A" "" no', env={"A": p})
        assert r.stdout.strip() == "local:" + p


def _unit_sha():
    with open(UNIT, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def test_provision_verdict_grades_every_facet():
    good = "BIN_X=yes\nUNIT_SHA=%s\nENV_FPS=60\nGPHOTO2=yes\nENABLED=disabled\n" % _unit_sha()
    ok = _src(PROV_LIB, 'bkshading_relay_provision_verdict "$B" disabled', env={"B": good})
    assert ok.stdout.strip() == "ok", ok.stdout
    wrong_mode = _src(PROV_LIB, 'bkshading_relay_provision_verdict "$B" enabled', env={"B": good})
    assert wrong_mode.stdout.startswith("FAIL"), wrong_mode.stdout
    unknown = _src(PROV_LIB, 'bkshading_relay_provision_verdict "$B" unknown', env={"B": good})
    assert unknown.stdout.startswith("FAIL") and "mode" in unknown.stdout, unknown.stdout
    bad = "BIN_X=no\nUNIT_SHA=deadbeef\nENV_FPS=\nGPHOTO2=no\nENABLED=\n"
    out = _src(PROV_LIB, 'bkshading_relay_provision_verdict "$B" enabled', env={"B": bad}).stdout
    for word in ("binary", "unit", "env", "gphoto2", "enabled"):
        assert word in out.lower(), (word, out)
    assert _src(PROV_LIB, 'bkshading_relay_provision_verdict "" enabled').stdout.startswith("FAIL")


def test_gather_snippet_emits_every_verdict_key():
    s = _src(PROV_LIB, "bkshading_relay_provision_gather_remote_snippet").stdout
    for key in ("BIN_X=", "UNIT_SHA=", "ENV_FPS=", "GPHOTO2=", "ENABLED="):
        assert key in s, (key, s)
    assert "systemctl start" not in s and "systemctl restart" not in s


# =============================================================================================
# setup-device.sh + verify-device.sh wiring
# =============================================================================================
def test_setup_device_provisions_the_relay_enable_only_before_the_ro_flip():
    s = _read(SETUP)
    body = _noncomment(s)
    i_relay = body.find("bkshading_relay_provision_install")
    i_step18 = s.find("# STEP 18: Configure read-only")
    assert i_relay >= 0, "setup-device.sh must call bkshading_relay_provision_install"
    assert s.find("bkshading_relay_provision_install", s.find("STEP 17c")) < i_step18, \
        "the relay step must sit in the rw window, before STEP 18"
    assert "bkshading_relay_expected_enable_state" in body, "the enable-state must follow the rig mode"
    assert "--rig-mode" in s and "--relay-binary" in s
    assert "bkshading-linux-amd64" in s, "the relay binary comes from the bkshading CI artifact"
    assert not re.search(r"systemctl (start|restart) bkshading-relay", body), "enable-only"
    assert "RELAY_PROBLEM" in body and re.search(r"MISSING=.*RELAY_PROBLEM", body), \
        "a relay failure is RECORDED and STEP 19 refuses Setup Complete"


def test_setup_device_default_rig_mode_is_test():
    s = _read(SETUP)
    assert re.search(r'RIG_MODE_ARG="\$\{CAMERA_BOX_RIG_MODE:-test\}"', s), \
        "the default rig mode must be test (development steady state, the passive issue-1311 direction)"


def test_verify_device_has_the_ao_relay_check_before_q():
    s = _read(VERIFY)
    header = s[: s.find("set -euo pipefail")]
    assert "(ao)" in header, "document (ao) in the header Checks list"
    usage = s[s.find("usage()"):]
    assert "(ao)" in usage[: usage.find("\nEOF")], "document (ao) in usage()"
    i_ao = s.find("\n# (ao) ")
    i_q = s.find("\n# (q) .bak cruft drift")
    assert 0 <= i_ao < i_q, "(ao) must sit before (q), which stays last"
    block = s[i_ao:i_q]
    assert "bkshading_relay_provision_verdict" in block
    assert "bkshading_relay_expected_enable_state" in block
    assert "rig_mode_from_painter_snapshot" in block, "auto mode reads cam2's painter state"
    assert 'warn "' not in block, "(ao) is a hard gate"


if __name__ == "__main__":
    import sys

    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    failed = 0
    for fn in fns:
        try:
            fn()
            print("ok   %s" % fn.__name__)
        except Exception as e:  # noqa: BLE001 - surfaced, never swallowed
            failed += 1
            print("FAIL %s: %s" % (fn.__name__, e))
    print("\n%d/%d passed" % (len(fns) - failed, len(fns)))
    sys.exit(1 if failed else 0)
