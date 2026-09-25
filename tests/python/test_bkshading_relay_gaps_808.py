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
import sys
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


def test_newest_success_filter_orders_by_created_at_and_drops_failures():
    # The ONE jq program the resolver hands to gh's built-in --jq (no standalone jq on a cambox).
    r = _src(RESOLVE_LIB, 'printf "%s" "$J" | jq -r "$(ci_run_newest_success_filter)"',
             env={"J": json.dumps(RUNS)})
    assert r.returncode == 0, r.stderr
    rows = [ln.split() for ln in r.stdout.splitlines()]
    assert [x[0] for x in rows] == ["300", "200", "100"], r.stdout
    assert rows[0] == ["300", "2026-09-25T17:34:20Z", "ccc"], rows


def test_resolver_uses_only_gh_builtin_jq():
    body = _noncomment(_read(RESOLVE_LIB))
    assert not re.search(r"(^|[|;&]\s*)jq\b", body, re.M), "no standalone jq: a cambox has gh but no jq"
    assert "--jq" in body


def _fake_gh(tmp, runs, artifacts_by_run, log):
    """A fake gh: `run list` prints RUNS through its --jq program (like gh's built-in jq);
    `api .../runs/<id>/artifacts` lists that run's artifacts; `run view <id>` prints a headSha;
    `run download <id> -n <a> --dir <d>` writes <d>/bkshading-relay."""
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
        'if [ "$1 $2" = "run list" ]; then\n'
        '  q=""; for a in "$@"; do [ "${prev:-}" = "--jq" ] && q="$a"; prev="$a"; done\n'
        '  if [ -n "$q" ]; then jq -r "$q" "__RUNS__"; else cat "__RUNS__"; fi; exit 0\n'
        "fi\n"
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
        '  *"remount,ro"*) [ "__ROFAIL__" = 1 ] && { echo "mount: /: mount point is busy." >&2; exit 1; } ;;\n'
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
    for was, want in (("active", "start"), ("activating", "start"), ("reloading", "start"),
                      ("inactive", "none"), ("failed", "none"), ("deactivating", "none"),
                      ("unknown", "none"), ("", "unreadable")):
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
    # an unresolvable source box: the roster itself is unknown -> every box unknown, except EVENT
    for dev, mode, want in (("cam3", "test", "unknown"), ("cam2", "test", "unknown"),
                            ("cam3", "unknown", "unknown"), ("cam3", "event", "enabled")):
        r = _src(PROV_LIB, 'bkshading_relay_expected_enable_state "$D" "$M" "" cam2',
                 env={"D": dev, "M": mode})
        assert r.stdout.strip() == want, ("empty source", dev, mode, r.stdout)
    assert _src(PROV_LIB, "bkshading_relay_roster_painter_box").stdout.strip() == "cam2"


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


# =============================================================================================
# Review round 1 (issue 808): unreadable/activating state, every restore path, a signal mid-deploy,
# the holder probe without lsof, and the relay binary fetch.
# =============================================================================================
def _deploy_env_custom(tmp, remote_sha, was_active="active", ssh_extra="", scp_body=None):
    """Like _deploy_env, plus extra ssh `case` arms (checked first) and an optional scp body."""
    env, log = _deploy_env(tmp, remote_sha, was_active=was_active)
    ssh = env["BKSHADING_DEPLOY_SSH"]
    body = _read(ssh).replace('case "$cmd" in\n', 'case "$cmd" in\n' + ssh_extra, 1)
    _write_exec(ssh, body)
    if scp_body is not None:
        _write_exec(env["BKSHADING_DEPLOY_SCP"],
                    '#!/usr/bin/env bash\nprintf "SCP %s\\n" "$*" >> "' + log + '"\n' + scp_body)
    return env, log


def test_unreadable_relay_state_refuses_before_touching_the_box():
    with tempfile.TemporaryDirectory() as tmp:
        b, sha = _bin(tmp)
        env, log = _deploy_env_custom(tmp, sha, was_active="")
        r = _run_deploy(["--host", "10.77.9.66", "--binary", b], env)
        assert r.returncode != 0, "an unreadable relay state must refuse:\n" + r.stdout + r.stderr
        assert re.search(r"could not read .*state", r.stderr), r.stderr
        calls = _read(log)
        assert "remount,rw" not in calls and "SCP" not in calls and "systemctl stop" not in calls, calls


def test_activating_relay_is_stopped_and_started_again():
    with tempfile.TemporaryDirectory() as tmp:
        b, sha = _bin(tmp)
        env, log = _deploy_env_custom(tmp, sha, was_active="activating")
        r = _run_deploy(["--host", "10.77.9.66", "--binary", b], env)
        assert r.returncode == 0, r.stdout + r.stderr
        calls = _read(log)
        assert calls.find("systemctl stop bkshading-relay") < calls.find("SCP ") < calls.find("systemctl start bkshading-relay"), calls


def test_scp_failure_restores_ro_root_and_the_active_relay():
    with tempfile.TemporaryDirectory() as tmp:
        b, sha = _bin(tmp)
        env, log = _deploy_env_custom(tmp, sha, was_active="active", scp_body="exit 7\n")
        r = _run_deploy(["--host", "10.77.9.66", "--binary", b], env)
        assert r.returncode != 0
        calls = _read(log)
        assert calls.find("SCP ") < calls.find("remount,ro /") < calls.find("systemctl start bkshading-relay"), calls
        assert "mv -f" not in calls, "a failed scp must never move anything over the relay binary"


def test_rw_remount_failure_restores_the_active_relay():
    with tempfile.TemporaryDirectory() as tmp:
        b, sha = _bin(tmp)
        env, log = _deploy_env_custom(tmp, sha, was_active="active",
                                      ssh_extra='  *"mount -o remount,rw"*) exit 32 ;;\n')
        r = _run_deploy(["--host", "10.77.9.66", "--binary", b], env)
        assert r.returncode != 0
        calls = _read(log)
        assert "SCP" not in calls and "systemctl start bkshading-relay" in calls, calls


def test_sha_mismatch_leaves_the_relay_stopped_but_restores_ro():
    with tempfile.TemporaryDirectory() as tmp:
        b, _sha = _bin(tmp)
        env, log = _deploy_env_custom(tmp, "0" * 64, was_active="active")
        r = _run_deploy(["--host", "10.77.9.66", "--binary", b], env)
        assert r.returncode != 0 and "mismatch" in r.stderr, r.stderr
        assert "STOPPED" in r.stderr, "the unverified binary must never be started:\n" + r.stderr
        calls = _read(log)
        assert "remount,ro /" in calls, calls
        assert "systemctl start bkshading-relay" not in calls, calls


def test_ssh_failure_during_ro_remount_is_reported_as_ssh_not_busy():
    with tempfile.TemporaryDirectory() as tmp:
        b, sha = _bin(tmp)
        env, log = _deploy_env_custom(tmp, sha, was_active="inactive",
                                      ssh_extra='  *"remount,ro"*) exit 255 ;;\n')
        r = _run_deploy(["--host", "10.77.9.66", "--binary", b], env)
        assert r.returncode != 0
        assert "ssh to 10.77.9.66 FAILED during the ro remount" in r.stderr, r.stderr
        assert "lsof +L1" not in _read(log), "an ssh failure is not a busy mount -- no holder hunt"


def test_a_signal_mid_scp_still_restores_ro_root_and_the_relay():
    import signal
    import time

    with tempfile.TemporaryDirectory() as tmp:
        b, sha = _bin(tmp)
        env, log = _deploy_env_custom(tmp, sha, was_active="active", scp_body="sleep 2\nexit 0\n")
        e = dict(os.environ)
        e.update(env)
        p = subprocess.Popen(["bash", DEPLOY, "--host", "10.77.9.66", "--binary", b],
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=e)
        for _ in range(100):
            if os.path.exists(log) and "SCP " in _read(log):
                break
            time.sleep(0.05)
        p.send_signal(signal.SIGTERM)
        _out, err = p.communicate(timeout=30)
        assert p.returncode != 0, err
        calls = _read(log)
        assert "mv -f" not in calls, calls
        assert calls.find("SCP ") < calls.find("remount,ro /") < calls.find("systemctl start bkshading-relay"), \
            "the EXIT trap must restore the ro root and the relay:\n" + calls


def test_holder_probe_names_the_holder_without_lsof():
    lib = os.path.join(REPO, "scripts", "lib", "bkshading-deploy-runtime.sh")
    probe = _src(lib, "bkshading_deploy_ro_holder_probe_cmd").stdout
    assert "lsof +L1" in probe and "/proc/" in probe and "(deleted)" in probe
    with tempfile.TemporaryDirectory() as tmp:
        for tool in ("readlink", "cat", "sort", "head", "tr", "grep", "awk"):
            os.symlink(subprocess.run(["bash", "-c", "command -v " + tool], capture_output=True,
                                      text=True, check=True).stdout.strip(), os.path.join(tmp, tool))
        r = subprocess.run(["/bin/bash", "-c", probe], capture_output=True, text=True,
                           env={"PATH": tmp})
        assert r.returncode == 0, r.stderr
        assert r.stdout.startswith("COMMAND PID"), "the /proc fallback emits lsof columns:\n" + r.stdout
        # whatever it finds parses with the pure holder summary (no crash, one line)
        s = _src(lib, 'bkshading_deploy_ro_holders "$T"', env={"T": r.stdout})
        assert s.returncode == 0 and "\n" not in s.stdout.strip()


def _fetch(plan, tmp, extra_env=None):
    e = {"BKSHADING_RELAY_GH": os.path.join(tmp, "fake-gh")}
    if extra_env:
        e.update(extra_env)
    return _src(PROV_LIB, 'bkshading_relay_provision_fetch_binary "$P" "$D" zbynekdrlik/camera-box main',
                env=dict(e, P=plan, D=os.path.join(tmp, "dl")))


def test_fetch_binary_follows_the_plan():
    with tempfile.TemporaryDirectory() as tmp:
        glog = os.path.join(tmp, "gh.log")
        _fake_gh(tmp, RUNS, {300: [ARTIFACT], 200: [ARTIFACT]}, glog)
        loc = os.path.join(tmp, "staged-relay")
        with open(loc, "w") as f:
            f.write("R")
        r = _fetch("local:" + loc, tmp)
        assert r.returncode == 0 and r.stdout.strip() == loc, r.stdout + r.stderr
        r = _fetch("run:200", tmp)
        assert r.returncode == 0 and r.stdout.strip().endswith("/dl/bkshading-relay"), r.stdout + r.stderr
        assert re.search(r"run download 200\b", _read(glog))
        r = _fetch("latest", tmp)
        assert r.returncode == 0, r.stdout + r.stderr
        assert re.search(r"run download 300\b", _read(glog)), "latest = the newest run carrying it"
        r = _fetch("none", tmp)
        assert r.returncode != 0 and "--relay-binary" in r.stdout, r.stdout
        curl = os.path.join(tmp, "fake-curl")
        _write_exec(curl, '#!/usr/bin/env bash\nwhile [ $# -gt 0 ]; do [ "$1" = -o ] && printf R > "$2"; shift; done\n')
        r = _fetch("url:https://h/relay", tmp, {"BKSHADING_RELAY_CURL": curl})
        assert r.returncode == 0 and r.stdout.strip().endswith("/dl/bkshading-relay"), r.stdout + r.stderr
        _write_exec(curl, "#!/usr/bin/env bash\nexit 22\n")
        r = _fetch("url:https://h/relay", tmp, {"BKSHADING_RELAY_CURL": curl})
        assert r.returncode != 0 and "failed" in r.stdout, r.stdout


# =============================================================================================
# Review round 2 (issue 808): a dead terminal, a signal inside the restore, the exe-held holder,
# an unreadable artifact list, and the standalone install CLI following the rig mode.
# =============================================================================================
def _deploy_popen(tmp, env, b, stderr):
    e = dict(os.environ)
    e.update(env)
    return subprocess.Popen(["bash", DEPLOY, "--host", "10.77.9.66", "--binary", b],
                            stdout=subprocess.DEVNULL, stderr=stderr, env=e)


def _wait_for(log, needle, tries=200):
    import time
    for _ in range(tries):
        if os.path.exists(log) and needle in _read(log):
            return
        time.sleep(0.05)
    raise AssertionError("never saw %r in %s" % (needle, log))


def test_hup_with_a_dead_terminal_still_restores_the_box():
    import signal
    with tempfile.TemporaryDirectory() as tmp:
        b, sha = _bin(tmp)
        env, log = _deploy_env_custom(tmp, sha, was_active="active", scp_body="sleep 2\nexit 0\n")
        with open("/dev/full", "w") as full:
            p = _deploy_popen(tmp, env, b, stderr=full)
            _wait_for(log, "SCP ")
            p.send_signal(signal.SIGHUP)
            p.wait(timeout=30)
        assert p.returncode != 0
        calls = _read(log)
        assert calls.find("SCP ") < calls.find("remount,ro /") < calls.find("systemctl start bkshading-relay"), \
            "a HUP with every stderr write failing must still restore the ro root + the relay:\n" + calls


def test_a_signal_inside_the_restore_runs_the_restore_again():
    import signal
    with tempfile.TemporaryDirectory() as tmp:
        b, sha = _bin(tmp)
        env, log = _deploy_env_custom(tmp, sha, was_active="active",
                                      ssh_extra='  *"remount,ro"*) sleep 2; exit 0 ;;\n')
        p = _deploy_popen(tmp, env, b, stderr=subprocess.PIPE)
        _wait_for(log, "remount,ro")
        p.send_signal(signal.SIGTERM)
        _out, err = p.communicate(timeout=30)
        assert p.returncode != 0
        assert b"interrupted -- running it again" in err, err
        calls = _read(log)
        assert calls.rfind("remount,ro /") < calls.rfind("systemctl start bkshading-relay"), \
            "the re-run restore must start the relay that was active before:\n" + calls


def test_a_second_ctrl_c_cannot_kill_the_rerun_restore():
    # Ctrl-C from a terminal hits the whole process group; sshpass forwards it to ssh even when the
    # deploy shell ignores it. The trap's re-run restore therefore runs in its own session (setsid).
    import signal
    with tempfile.TemporaryDirectory() as tmp:
        b, sha = _bin(tmp)
        done_log = os.path.join(tmp, "ro-done.log")
        env, log = _deploy_env_custom(tmp, sha, was_active="active",
                                      ssh_extra='  *"remount,ro"*) sleep 2; echo RO-DONE >> "%s"; exit 0 ;;\n' % done_log)
        # A stand-in for the real sshpass: it handles SIGINT itself (so the deploy's SIG_IGN is NOT
        # inherited by ssh) and forwards it, exactly the behaviour that let a second Ctrl-C through.
        fake_sshpass = os.path.join(tmp, "fake-sshpass")
        _write_exec(fake_sshpass,
                    "#!%s\nimport signal, subprocess, sys\n"
                    "p = subprocess.Popen(sys.argv[1:], preexec_fn=lambda: signal.signal(signal.SIGINT, signal.SIG_DFL))\n"
                    "signal.signal(signal.SIGINT, lambda s, f: p.send_signal(s))\n"
                    "rc = p.wait()\nsys.exit(128 - rc if rc < 0 else rc)\n" % sys.executable)
        env["BKSHADING_DEPLOY_SSHPASS_PREFIX"] = fake_sshpass
        e = dict(os.environ)
        e.update(env)
        p = subprocess.Popen(["bash", DEPLOY, "--host", "10.77.9.66", "--binary", b],
                             stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env=e,
                             start_new_session=True)
        _wait_for(log, "remount,ro")
        os.killpg(p.pid, signal.SIGINT)          # the first Ctrl-C: kills the normal-path remount
        import time
        for _ in range(200):                     # wait for the trap's re-run remount
            if _read(log).count("remount,ro /") >= 2:
                break
            time.sleep(0.05)
        os.killpg(p.pid, signal.SIGINT)          # the second Ctrl-C: must not reach the re-run
        _out, err = p.communicate(timeout=30)
        calls = _read(log)
        assert calls.count("remount,ro /") >= 2, calls
        # the first remount was killed by Ctrl-C; the re-run one must COMPLETE despite the second
        assert os.path.exists(done_log) and "RO-DONE" in _read(done_log), \
            "a second Ctrl-C killed the re-run ro remount:\n" + calls + err.decode()
        assert calls.rfind("remount,ro /") < calls.rfind("systemctl start bkshading-relay"), calls


def test_ssh_transport_rc_other_than_1_is_not_a_busy_mount():
    with tempfile.TemporaryDirectory() as tmp:
        b, sha = _bin(tmp)
        env, log = _deploy_env_custom(tmp, sha, was_active="inactive",
                                      ssh_extra='  *"remount,ro"*) exit 5 ;;\n')
        r = _run_deploy(["--host", "10.77.9.66", "--binary", b], env)
        assert r.returncode != 0 and "FAILED during the ro remount (rc 5)" in r.stderr, r.stderr
        assert "lsof +L1" not in _read(log)


def test_holder_probe_names_an_exe_held_binary_from_a_fake_proc_tree():
    lib = os.path.join(REPO, "scripts", "lib", "bkshading-deploy-runtime.sh")
    with tempfile.TemporaryDirectory() as tmp:
        proc = os.path.join(tmp, "proc")
        pid = os.path.join(proc, "4242")
        os.makedirs(os.path.join(pid, "fd"))
        with open(os.path.join(pid, "comm"), "w") as f:
            f.write("bkshading-relay\n")
        # noise that always reads (deleted) but never holds / -- must not crowd the holder out
        other = os.path.join(proc, "77")
        os.makedirs(os.path.join(other, "fd"))
        with open(os.path.join(other, "comm"), "w") as f:
            f.write("npm exec x\n")
        os.symlink("/memfd:doublemapper (deleted)", os.path.join(other, "fd", "9"))
        os.symlink("/opt/old tool (deleted)", os.path.join(other, "exe"))
        os.symlink("/usr/local/bin/bkshading-relay (deleted)", os.path.join(pid, "exe"))
        with open(os.path.join(pid, "maps"), "w") as f:
            f.write("7f00-7f01 r-xp 00000000 08:02 1234 /usr/lib/libold.so (deleted)\n"
                    "7f02-7f03 r--p 00000000 08:02 99 /usr/lib/libfine.so\n")
        os.symlink("/var/log/x (deleted)", os.path.join(pid, "fd", "3"))
        bindir = os.path.join(tmp, "bin")
        os.makedirs(bindir)
        for tool in ("readlink", "cat", "sort", "head", "awk", "tr", "grep"):
            os.symlink(subprocess.run(["bash", "-c", "command -v " + tool], capture_output=True,
                                      text=True, check=True).stdout.strip(), os.path.join(bindir, tool))
        probe = _src(lib, 'bkshading_deploy_ro_holder_probe_cmd "$R"', env={"R": proc}).stdout
        r = subprocess.run(["/bin/bash", "-c", probe], capture_output=True, text=True, env={"PATH": bindir})
        assert r.returncode == 0, r.stderr
        s = _src(lib, 'bkshading_deploy_ro_holders "$T"', env={"T": r.stdout}).stdout.strip()
        assert "bkshading-relay[4242] /usr/local/bin/bkshading-relay" in s, (s, r.stdout)
        assert "/usr/lib/libold.so" in s and "/var/log/x" in s, s
        assert "libfine" not in s and "memfd" not in s, s
        assert "npm_exec_x[77] /opt/old tool" in s, "a spaced process name must keep the columns: %s" % s


def test_an_unreadable_artifact_list_stops_the_resolver():
    with tempfile.TemporaryDirectory() as tmp:
        log = os.path.join(tmp, "gh.log")
        gh = _fake_gh(tmp, RUNS, {300: [ARTIFACT], 200: [ARTIFACT], 100: [ARTIFACT]}, log)
        body = _read(gh).replace('if [ "$1" = "api" ]; then\n',
                                 'if [ "$1" = "api" ]; then\n  case "$2" in *runs/300/*) echo "HTTP 502: Bad Gateway" >&2; exit 1 ;; esac\n', 1)
        _write_exec(gh, body)
        r = _src(RESOLVE_LIB, "ci_run_latest_success zbynekdrlik/camera-box main ci.yml %s" % ARTIFACT,
                 env={"CI_RUN_RESOLVE_GH": gh})
        assert r.returncode != 0 and r.stdout.strip() == "", \
            "one failed artifact lookup must never fall back to an older run: %r" % r.stdout
        assert "UNREADABLE" in r.stderr and "HTTP 502: Bad Gateway" in r.stderr, r.stderr


def test_fetch_reports_the_gh_error():
    with tempfile.TemporaryDirectory() as tmp:
        gh = os.path.join(tmp, "fake-gh")
        _write_exec(gh, '#!/usr/bin/env bash\necho "HTTP 404: artifact not found" >&2\nexit 1\n')
        r = _fetch("run:77", tmp)
        assert r.returncode != 0 and "HTTP 404: artifact not found" in r.stdout, r.stdout


def _provision_cli(tmp, args, device):
    env, calls = _prov_env(tmp)
    env["BKSHADING_RELAY_DEVICE_NAME"] = device
    e = dict(os.environ)
    e.pop("CAMERA_BOX_RIG_MODE", None)
    e.update(env)
    r = subprocess.run(["bash", PROVISION] + args, capture_output=True, text=True, env=e)
    return r, calls


def test_install_cli_follows_the_rig_mode():
    src = _source_box()
    with tempfile.TemporaryDirectory() as tmp:
        # a roster box with no mode given: refuse -- guessing would re-arm or kill its relay
        r, calls = _provision_cli(tmp, ["--install"], src)
        assert r.returncode == 2 and "--rig-mode" in r.stderr, r.stdout + r.stderr
        assert not os.path.exists(calls), "nothing may be installed without a mode"
    with tempfile.TemporaryDirectory() as tmp:
        r, calls = _provision_cli(tmp, ["--install", "--rig-mode", "test"], src)
        assert r.returncode == 0, r.stdout + r.stderr
        log = _read(calls)
        assert re.search(r"^disable bkshading-relay.service$", log, re.M), "TEST source box: disabled\n" + log
        assert not re.search(r"^enable ", log, re.M), log
    with tempfile.TemporaryDirectory() as tmp:
        r, calls = _provision_cli(tmp, ["--install", "--rig-mode", "event"], src)
        assert r.returncode == 0 and re.search(r"^enable bkshading-relay.service$", _read(calls), re.M), r.stderr
    with tempfile.TemporaryDirectory() as tmp:
        r, calls = _provision_cli(tmp, ["--install"], _non_roster_box())
        assert r.returncode == 0 and re.search(r"^enable bkshading-relay.service$", _read(calls), re.M), r.stderr
    with tempfile.TemporaryDirectory() as tmp:
        r, _calls = _provision_cli(tmp, ["--install", "--rig-mode", "live"], src)
        assert r.returncode == 2, r.returncode


def test_check_cli_fails_an_enabled_source_box_in_test_mode():
    src = _source_box()
    with tempfile.TemporaryDirectory() as tmp:
        r, _calls = _provision_cli(tmp, ["--install", "--rig-mode", "event"], src)
        assert r.returncode == 0, r.stderr
        os.makedirs(os.path.join(tmp, "bin"), exist_ok=True)
        with open(os.path.join(tmp, "bin", "bkshading-relay"), "w") as f:
            f.write("R")
        os.chmod(os.path.join(tmp, "bin", "bkshading-relay"), 0o755)
        ok, _ = _provision_cli(tmp, ["--check", "--rig-mode", "event"], src)
        assert ok.returncode == 0, ok.stdout + ok.stderr
        bad, _ = _provision_cli(tmp, ["--check", "--rig-mode", "test"], src)
        assert bad.returncode != 0 and "wants it disabled" in bad.stderr, bad.stderr


def _run_ao(block_out, env_extra):
    """Execute the REAL (ao) block sliced from verify-device.sh with ssh stubbed to print BLOCK_OUT."""
    s = _read(VERIFY)
    block = s[s.find("\n# (ao) "):s.find("\n# (an) ")]
    prelude = (
        'ok() { echo "OK $1"; }\nfail() { echo "FAIL $1"; }\n'
        'ssh_box() { printf "%s\\n" "$AO_BLOCK"; }\n'
        'ssh_ip() { printf "%s\\n" "$AO_PAINTER"; }\n'
    )
    src = 'set -euo pipefail\n. "%s"\n. "%s"\n. "%s"\n%s%s' % (
        os.path.join(REPO, "scripts", "camera-set.sh"), PROV_LIB,
        os.path.join(REPO, "scripts", "lib", "rig-mode-state.sh"), prelude, block)
    e = dict(os.environ, AO_BLOCK=block_out, AO_PAINTER="")
    e.pop("CAMERA_BOX_RIG_MODE", None)
    e.update(env_extra)
    return subprocess.run(["bash", "-c", src], capture_output=True, text=True, env=e)


def _good_block(enabled):
    return "BIN_X=yes\nUNIT_SHA=%s\nENV_FPS=60\nGPHOTO2=yes\nENABLED=%s\n" % (_unit_sha(), enabled)


def test_ao_block_passes_a_non_roster_box_enabled_in_any_mode():
    r = _run_ao(_good_block("enabled"), {"CAMERA_NAME": _non_roster_box(), "CAMERA_BOX_RIG_MODE": "test"})
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith("OK ") and "FAIL" not in r.stdout, r.stdout + r.stderr


def _source_box():
    r = subprocess.run(["bash", "-c", '. "%s"\ncamera_source_box' % os.path.join(REPO, "scripts", "camera-set.sh")],
                       capture_output=True, text=True, check=True)
    return r.stdout.strip()


def _non_roster_box():
    # any resolvable cambox that is neither the source box nor cam2
    src = _source_box()
    for cam in ("cam3", "cam4", "cam5", "cam6", "cam7", "cam1"):
        if cam not in (src, "cam2"):
            return cam
    raise AssertionError("no non-roster cambox")


def test_ao_block_fails_the_source_box_enabled_in_test_mode():
    src = _source_box()
    assert src and src != "cam2", src
    r = _run_ao(_good_block("enabled"), {"CAMERA_NAME": src, "CAMERA_BOX_RIG_MODE": "test"})
    assert r.returncode == 0, r.stderr
    assert "FAIL bkshading relay:" in r.stdout and "disabled" in r.stdout, r.stdout


def test_ao_block_fails_the_painter_box_enabled_in_test_mode():
    r = _run_ao(_good_block("enabled"), {"CAMERA_NAME": "cam2", "CAMERA_BOX_RIG_MODE": "test"})
    assert "FAIL bkshading relay:" in r.stdout and "disabled" in r.stdout, r.stdout


def test_ao_block_reads_event_mode_off_the_cam2_painter():
    snap = "RIG_MODE_PROBE_OK\nPID_PRESENT|0\nPID_ALIVE|0\nSVC_ENABLED|0\nSVC_ACTIVE|0\n"
    ok = _run_ao(_good_block("enabled"), {"CAMERA_NAME": "cam2", "AO_PAINTER": snap})
    assert ok.stdout.startswith("OK ") and "'event'" in ok.stdout, ok.stdout + ok.stderr
    bad = _run_ao(_good_block("disabled"), {"CAMERA_NAME": "cam2", "AO_PAINTER": snap})
    assert "FAIL bkshading relay:" in bad.stdout and "enabled" in bad.stdout, bad.stdout


def test_ao_block_fails_a_roster_box_when_the_mode_is_unreadable():
    # no RIG_MODE and the cam2 painter probe answers nothing -> UNKNOWN -> FAIL for cam2
    r = _run_ao(_good_block("disabled"), {"CAMERA_NAME": "cam2"})
    assert r.returncode == 0, r.stderr
    assert "FAIL bkshading relay:" in r.stdout and "rig mode unreadable" in r.stdout, r.stdout


def test_ao_block_reads_test_mode_off_the_cam2_painter():
    snap = "RIG_MODE_PROBE_OK\nPID_PRESENT|0\nPID_ALIVE|0\nSVC_ENABLED|1\nSVC_ACTIVE|1\n"
    r = _run_ao(_good_block("disabled"), {"CAMERA_NAME": "cam2", "AO_PAINTER": snap})
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith("OK ") and "'test'" in r.stdout, r.stdout + r.stderr


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
