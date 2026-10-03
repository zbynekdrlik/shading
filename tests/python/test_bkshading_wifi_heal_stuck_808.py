#!/usr/bin/env python3
"""The handheld SBC's WiFi heal, its STUCK rung (issue 808; live on handheld-1, 3.10.2026): the
uwe5622 driver refused every association until sprdwl_ng was reloaded. scripts/bkshading-wifi-heal.sh
starts a stopped supplicant (no driver reload while wlan0 is there), and after 3 passes of
driver-refused associations or a hung supplicant reloads the WiFi driver -- the pure decisions in
scripts/lib/bkshading-sbc-runtime.sh (bkshading_sbc_wifi_heal_stuck_decide / _driver_plan).

Covers the journal cursor (each refusal counted once), the stopped-vs-hung split, the remembered
driver module, the load / reload plans, the restore trap and the fresh-context review findings on
them. Split out of test_bkshading_wifi_heal_808.py (the reachability rungs, the unit bounds and the
docs pins); the stubs and the heal harness live in bkshading_sbc_fakes_808.py. Runs in the
`python-tests` CI job; runnable directly or under pytest.
"""
import os
import re
import shutil
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from bkshading_sbc_fakes_808 import (  # noqa: E402
    GOOD_BSSID,
    _get_state,
    _heal_env,
    _heal_pass,
    _heal_tools,
    _lib_call,
    _net_state,
    _read,
    _set_state,
)


def test_heal_stuck_decision_table():
    # Live on handheld-1 (3.10.2026): after a run of forced reassociations the uwe5622 driver
    # answered every connect with "Association request to the driver failed" -- wpa_state never
    # reached COMPLETED, so the reachability heal never acted. Neither a supplicant restart nor a
    # link down/up revived it; reloading the driver module (sprdwl_ng) did, COMPLETED in 6 s.
    table = [
        # prev stuck, wpa_state, driver-failed lines since the last pass -> action stuck
        ("0", "COMPLETED", "0", "none 0"),
        ("2", "COMPLETED", "4", "none 0"),          # a working link resets the count
        ("0", "SCANNING", "0", "none 0"),           # out of range / plain scanning: never stuck
        ("2", "DISCONNECTED", "0", "none 0"),
        ("0", "DISCONNECTED", "2", "none 1"),       # the driver refused an association
        ("1", "SCANNING", "1", "none 2"),
        ("2", "ASSOCIATING", "3", "reload-driver 0"),
        ("0", "?", "0", "none 1"),                  # the supplicant does not answer (hung)
        ("2", "?", "0", "reload-driver 0"),
        ("9", "DISCONNECTED", "1", "reload-driver 0"),  # a stale count past the limit acts at once
        ("junk", "DISCONNECTED", "1", "none 1"),    # a damaged count reads as 0
        ("1", "DISCONNECTED", "junk", "none 0"),    # an unreadable journal count is no evidence
    ]
    for prev, state, failed, want in table:
        got = _lib_call('bkshading_sbc_wifi_heal_stuck_decide "$A" "$B" "$C"',
                        env={"A": prev, "B": state, "C": failed}).strip()
        assert got == want, (prev, state, failed, got, want)
    assert _lib_call("bkshading_sbc_wifi_heal_stuck_limit").strip() == "3"
    assert _lib_call("bkshading_sbc_wifi_heal_driver_failed_text").strip() == \
        "Association request to the driver failed"


def _stuck(env):
    return _read(os.path.join(env["BKSHADING_WIFI_HEAL_STATE_DIR"], "stuck")).strip()


def test_heal_reloads_a_driver_that_refuses_every_association():
    tmp = tempfile.mkdtemp()
    try:
        state = _net_state(wpa_state="DISCONNECTED", reachable=False, driver_failed=3,
                           on_reload={"wpa_state": "COMPLETED", "bssid": GOOD_BSSID, "rssi": -63,
                                      "reachable": True, "driver_failed": 0})
        env = _heal_env(tmp, state, extra_tools=("journalctl", "modprobe"),
                        driver_module="sprdwl_ng")
        outs = []
        for i in range(3):
            r = _heal_pass(env)
            assert r.returncode == 0, (i, r.stdout, r.stderr)
            outs.append(r.stdout)
        tools = _heal_tools(env)
        acts = [t for t in tools if t.startswith(("systemctl stop", "systemctl start", "modprobe"))]
        assert acts == ["systemctl stop wpa_supplicant@wlan0.service", "modprobe -r sprdwl_ng",
                        "modprobe sprdwl_ng", "systemctl start wpa_supplicant@wlan0.service"], tools
        assert "driver refused" in outs[0] and "1 of 3" in outs[0], outs[0]
        assert re.search(r"reload the WiFi driver sprdwl_ng on wlan0 after 3 stuck passes .*"
                         r"after bssid=%s signal=-63 dBm wpa_state=COMPLETED; result=ok"
                         % re.escape(GOOD_BSSID), outs[2]), outs[2]
        assert _stuck(env) == "0", "a reload starts the count over"
        r4 = _heal_pass(env)
        assert "answers again" in r4.stdout and "reload the WiFi driver sprdwl_ng" in r4.stdout, r4.stdout
        assert not any("reboot" in t for t in tools)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_never_reloads_while_the_board_merely_scans():
    # out of range of every AP (an outdoor venue without the SSID): scanning forever is not stuck
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(wpa_state="SCANNING", reachable=False, driver_failed=0),
                        extra_tools=("journalctl", "modprobe"), driver_module="sprdwl_ng")
        for _ in range(6):
            r = _heal_pass(env)
            assert r.returncode == 0, (r.stdout, r.stderr)
        tools = _heal_tools(env)
        assert not any(t.startswith(("modprobe", "systemctl stop", "systemctl start")) for t in tools), tools
        assert _stuck(env) == "0"
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_restarts_a_hung_supplicant_through_the_reload_path():
    # the supplicant stops answering wpa_cli (hung, not crashed): after 3 passes the heal stops it,
    # reloads the driver and starts it again (systemctl stop kills a hung one).
    tmp = tempfile.mkdtemp()
    try:
        state = _net_state(wpa_state=None, on_reload={"wpa_state": "COMPLETED", "reachable": True})
        env = _heal_env(tmp, state, extra_tools=("journalctl", "modprobe"),
                        driver_module="sprdwl_ng")
        for _ in range(3):
            r = _heal_pass(env)
            assert r.returncode == 0, (r.stdout, r.stderr)
        tools = _heal_tools(env)
        assert "systemctl stop wpa_supplicant@wlan0.service" in tools, tools
        assert "systemctl start wpa_supplicant@wlan0.service" in tools, tools
        assert "reload the WiFi driver sprdwl_ng" in r.stdout, r.stdout
        # the unit read `active`: a running supplicant that does not answer is hung, so it takes
        # the reload path (a STOPPED unit is only started, see the issue-808 review tests)
        assert "systemctl is-active wpa_supplicant@wlan0.service" in tools, tools
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_restarts_the_supplicant_alone_for_a_builtin_driver_or_without_modprobe():
    for extra, module in ((("journalctl", "modprobe"), None), (("journalctl",), "sprdwl_ng")):
        tmp = tempfile.mkdtemp()
        try:
            state = _net_state(wpa_state="DISCONNECTED", reachable=False, driver_failed=2)
            env = _heal_env(tmp, state, extra_tools=extra, driver_module=module)
            for _ in range(3):
                r = _heal_pass(env)
                assert r.returncode == 0, (extra, module, r.stdout, r.stderr)
            tools = _heal_tools(env)
            assert not any(t.startswith("modprobe") for t in tools), tools
            assert "systemctl stop wpa_supplicant@wlan0.service" in tools, tools
            assert "systemctl start wpa_supplicant@wlan0.service" in tools, tools
            assert "no driver reload" in r.stdout, r.stdout
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


# ---------------------------------------------------------------------------------------------
# issue 808 stuck-rung review (fresh-context review of the driver-reload rung)
# ---------------------------------------------------------------------------------------------
JOURNAL_COUNT = "journalctl -u wpa_supplicant@wlan0.service --cursor-file=%s%s -o cat --no-pager"


def _journal_calls(env):
    return [t for t in _heal_tools(env) if t.startswith("journalctl")]


def _cursor_path(env):
    return os.path.join(env["BKSHADING_WIFI_HEAL_STATE_DIR"], "journal-cursor")


def test_heal_counts_each_refusal_once_through_the_journal_cursor():
    # A --since window on every pass (25 s on a 20 s cadence) counted every refusal twice. The
    # heal follows a cursor instead: --since only on the first read, then only lines after it.
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(wpa_state="DISCONNECTED", reachable=False, driver_failed=1),
                        extra_tools=("journalctl",))
        r1 = _heal_pass(env)
        assert r1.returncode == 0, (r1.stdout, r1.stderr)
        assert _stuck(env) == "1" and "refused 1 association" in r1.stdout, r1.stdout
        # the driver stops refusing; the board still scans
        _set_state(env, driver_failed=0)
        r2 = _heal_pass(env)
        assert r2.returncode == 0, (r2.stdout, r2.stderr)
        assert _stuck(env) == "0", "the refusal of the first pass was counted again"
        cursor = _cursor_path(env)
        assert _journal_calls(env) == [
            JOURNAL_COUNT % (cursor, " --since " + env["FAKE_JOURNAL_SINCE"]),
            JOURNAL_COUNT % (cursor, ""),
        ], _heal_tools(env)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_counts_no_refusal_from_before_a_driver_reload():
    # The old supplicant keeps refusing until `systemctl stop` returns; a pass after the reload
    # judges the reloaded driver only, so the cursor moves to the journal's end after the stop.
    tmp = tempfile.mkdtemp()
    try:
        state = _net_state(wpa_state="DISCONNECTED", reachable=False, driver_failed=2,
                           on_reload={"driver_failed": 0})
        env = _heal_env(tmp, state, extra_tools=("journalctl", "modprobe"),
                        driver_module="sprdwl_ng")
        for _ in range(3):
            r = _heal_pass(env)
            assert r.returncode == 0, (r.stdout, r.stderr)
        assert "reload the WiFi driver sprdwl_ng" in r.stdout, r.stdout
        tools = _heal_tools(env)
        cursor = _cursor_path(env)
        to_end = JOURNAL_COUNT % (cursor, " -n 1")
        assert to_end in tools, tools
        assert tools.index("systemctl stop wpa_supplicant@wlan0.service") < tools.index(to_end) \
            < tools.index("modprobe -r sprdwl_ng"), tools
        r4 = _heal_pass(env)
        assert r4.returncode == 0, (r4.stdout, r4.stderr)
        assert _stuck(env) == "0" and "stuck pass" not in r4.stdout, r4.stdout
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_drops_the_journal_cursor_on_a_working_link():
    # A cursor kept through an hour of COMPLETED would count that hour's refusals at the next drop.
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(wpa_state="DISCONNECTED", reachable=False, driver_failed=1),
                        extra_tools=("journalctl",))
        assert _heal_pass(env).returncode == 0
        assert os.path.exists(_cursor_path(env)) and _stuck(env) == "1"
        _set_state(env, wpa_state="COMPLETED", reachable=True, driver_failed=0)
        assert _heal_pass(env).returncode == 0
        assert not os.path.exists(_cursor_path(env)), "a COMPLETED pass drops the cursor"
        # refusals written while the link worked, older than the next pass's --since window
        st = _get_state(env)
        st["journal"] += ["wlan0: Association request to the driver failed"] * 2
        _set_state(env, journal=st["journal"], journal_since_from=len(st["journal"]),
                   wpa_state="DISCONNECTED", reachable=False)
        r3 = _heal_pass(env)
        assert r3.returncode == 0, (r3.stdout, r3.stderr)
        assert _stuck(env) == "0", "refusals from before the drop were counted"
        assert _journal_calls(env)[-1] == JOURNAL_COUNT % (
            _cursor_path(env), " --since " + env["FAKE_JOURNAL_SINCE"]), _heal_tools(env)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_drops_a_cursor_journald_cannot_seek():
    # journalctl exits 1 on a cursor it cannot seek ("Failed to seek to cursor", live on dev1):
    # kept, that file would blind the count forever; dropped, the next pass reads --since again.
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(wpa_state="DISCONNECTED", reachable=False, driver_failed=1),
                        extra_tools=("journalctl",))
        os.makedirs(env["BKSHADING_WIFI_HEAL_STATE_DIR"])
        with open(_cursor_path(env), "w") as f:
            f.write("s=gone;i=1\n")
        r1 = _heal_pass(env)
        assert r1.returncode == 0, (r1.stdout, r1.stderr)
        assert _stuck(env) == "0", "an unreadable journal is no evidence"
        assert not os.path.exists(_cursor_path(env))
        r2 = _heal_pass(env)
        assert r2.returncode == 0, (r2.stdout, r2.stderr)
        assert _stuck(env) == "1", r2.stdout
        assert _journal_calls(env)[-1] == JOURNAL_COUNT % (
            _cursor_path(env), " --since " + env["FAKE_JOURNAL_SINCE"]), _heal_tools(env)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_waits_for_wlan0_before_it_starts_the_supplicant():
    # `modprobe -r` removes wlan0; the reloaded driver brings it back a moment later (the stub's
    # detached probe, 2 s). The supplicant starts only once wlan0 is back, and the pass leaves the
    # wait as soon as it is: a wait that ignored wlan0's return would sit out the whole bound.
    import time
    tmp = tempfile.mkdtemp()
    try:
        state = _net_state(wpa_state="DISCONNECTED", reachable=False, driver_failed=1,
                           iface_delay_s=2,
                           on_reload={"wpa_state": "COMPLETED", "reachable": True,
                                      "driver_failed": 0})
        env = _heal_env(tmp, state, extra_tools=("journalctl", "modprobe"),
                        driver_module="sprdwl_ng")
        for _ in range(2):
            assert _heal_pass(env).returncode == 0
        t0 = time.monotonic()
        r = _heal_pass(env)
        took = time.monotonic() - t0
        assert r.returncode == 0 and "result=ok" in r.stdout, (r.stdout, r.stderr)
        assert "start_without_iface" not in _get_state(env), "the supplicant started without wlan0"
        assert os.path.isdir(os.path.join(env["BKSHADING_WIFI_HEAL_SYSFS_NET"], "wlan0"))
        assert 2 <= took < 10, "the pass waited %.1f s for a wlan0 back after 2 s" % took
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_reports_a_failed_driver_reload():
    # `modprobe -r` refuses (the module is in use): no load follows, the supplicant is started
    # again on the still-loaded driver, and the pass says result=FAILED and exits 1.
    tmp = tempfile.mkdtemp()
    try:
        state = _net_state(wpa_state="DISCONNECTED", reachable=False, driver_failed=1,
                           modprobe_rc=1)
        env = _heal_env(tmp, state, extra_tools=("journalctl", "modprobe"),
                        driver_module="sprdwl_ng")
        for _ in range(2):
            assert _heal_pass(env).returncode == 0
        r = _heal_pass(env)
        assert r.returncode == 1, (r.stdout, r.stderr)
        assert "reload the WiFi driver sprdwl_ng" in r.stdout and "result=FAILED" in r.stdout, r.stdout
        acts = [t for t in _heal_tools(env)
                if t.startswith(("systemctl stop", "systemctl start", "modprobe"))]
        assert acts == ["systemctl stop wpa_supplicant@wlan0.service", "modprobe -r sprdwl_ng",
                        "systemctl start wpa_supplicant@wlan0.service"], acts
        assert _get_state(env)["wpa_active"] is True, "the supplicant must not be left stopped"
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_stuck_decision_reads_the_supplicant_unit_state():
    # "?" (no wpa_cli answer) is either a STOPPED unit or a HUNG one. Live 3.10.2026 the
    # supervisor's `systemctl stop` read as hung and cost a driver reload; a stopped unit only
    # needs a start. The unit word is read only when wpa_cli does not answer.
    table = [
        # prev stuck, wpa_state, refused lines, `systemctl is-active` word -> action stuck
        ("0", "?", "0", "inactive", "start 0"),       # stopped: start it, no reload
        ("2", "?", "0", "failed", "start 0"),         # given up: start it, no reload
        ("0", "?", "0", "active", "none 1"),          # running but silent = hung
        ("2", "?", "0", "active", "reload-driver 0"),
        ("1", "?", "0", "activating", "none 2"),      # systemd mid auto-restart: still counted
        ("1", "?", "0", "", "none 2"),                # an unreadable word: never a start
        ("2", "DISCONNECTED", "1", "inactive", "reload-driver 0"),  # answered: the word is moot
        ("1", "COMPLETED", "0", "inactive", "none 0"),
    ]
    for prev, state, failed, word, want in table:
        got = _lib_call('bkshading_sbc_wifi_heal_stuck_decide "$A" "$B" "$C" "$D"',
                        env={"A": prev, "B": state, "C": failed, "D": word}).strip()
        assert got == want, (prev, state, failed, word, got, want)


def test_heal_starts_a_stopped_supplicant_without_a_driver_reload():
    for word in ("inactive", "failed"):
        tmp = tempfile.mkdtemp()
        try:
            env = _heal_env(tmp, _net_state(wpa_active=False, unit_word=word,
                                            on_start={"unit_word": None}),
                            extra_tools=("journalctl", "modprobe"), driver_module="sprdwl_ng")
            r = _heal_pass(env)
            assert r.returncode == 0, (word, r.stdout, r.stderr)
            tools = _heal_tools(env)
            assert "systemctl is-active wpa_supplicant@wlan0.service" in tools, tools
            acts = [t for t in tools
                    if t.startswith(("systemctl stop", "systemctl start", "modprobe"))]
            assert acts == ["systemctl start wpa_supplicant@wlan0.service"], (word, acts)
            assert re.search(
                r"start wpa_supplicant@wlan0\.service on wlan0 \(it was %s; no driver reload\); "
                r"after bssid=%s signal=-63 dBm wpa_state=COMPLETED; result=ok"
                % (word, re.escape(GOOD_BSSID)), r.stdout), r.stdout
            assert _stuck(env) == "0", "a start is no stuck pass"
            r2 = _heal_pass(env)
            assert "answers again" in r2.stdout and "the last action: start" in r2.stdout, r2.stdout
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


def test_heal_driver_plan_table():
    # What the stuck rung can do with the WiFi driver: $1 the module behind wlan0 (empty = none
    # known), $2 modprobe present, $3 wlan0 present, $4 the module loaded (/sys/module/<name>).
    table = [
        ("sprdwl_ng", "yes", "yes", "yes", "reload"),   # loaded: unload + load
        ("sprdwl_ng", "yes", "no", "no", "load"),       # not loaded (a load that failed): load
        ("sprdwl_ng", "yes", "no", "yes", "reload"),    # loaded but no wlan0: a load is a no-op
        ("sprdwl_ng", "no", "yes", "yes", "no-modprobe"),
        ("sprdwl_ng", "no", "no", "no", "no-modprobe"),
        ("", "yes", "yes", "no", "builtin"),            # wlan0 with no module link
        ("", "no", "yes", "no", "builtin"),
        ("", "yes", "no", "no", "unknown"),             # wlan0 gone and no module name recorded
        ("../evil", "yes", "yes", "no", "builtin"),     # not a module name: none
        ("a b", "yes", "no", "no", "unknown"),
        ("-r", "yes", "yes", "yes", "builtin"),         # modprobe would read it as an option
    ]
    for mod, has_modprobe, iface, loaded, want in table:
        got = _lib_call('bkshading_sbc_wifi_heal_driver_plan "$A" "$B" "$C" "$D"',
                        env={"A": mod, "B": has_modprobe, "C": iface, "D": loaded}).strip()
        assert got == want, (mod, has_modprobe, iface, loaded, got, want)


def _seed_driver_module(env, name):
    os.makedirs(env["BKSHADING_WIFI_HEAL_STATE_DIR"], exist_ok=True)
    with open(os.path.join(env["BKSHADING_WIFI_HEAL_STATE_DIR"], "driver-module"), "w") as f:
        f.write(name + "\n")


def _driver_acts(env):
    return [t for t in _heal_tools(env)
            if t.startswith(("systemctl stop", "systemctl start", "modprobe"))]


def test_heal_persists_the_driver_module_while_wlan0_has_it():
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(), driver_module="sprdwl_ng")
        assert _heal_pass(env).returncode == 0
        assert _read(os.path.join(env["BKSHADING_WIFI_HEAL_STATE_DIR"], "driver-module")) \
            .strip() == "sprdwl_ng"
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_loads_a_remembered_driver_when_wlan0_is_gone():
    # wlan0 and its module link are gone (an earlier pass unloaded the module and its load
    # failed): the remembered name loads the driver (no -r of a module that is not loaded).
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(wpa_state=None, on_reload={"wpa_state": "COMPLETED"}),
                        extra_tools=("journalctl", "modprobe"), driver_module="sprdwl_ng",
                        iface_present=False)
        _seed_driver_module(env, "sprdwl_ng")
        for _ in range(3):
            r = _heal_pass(env)
            assert r.returncode == 0, (r.stdout, r.stderr)
        assert _driver_acts(env) == ["systemctl stop wpa_supplicant@wlan0.service",
                                     "modprobe sprdwl_ng",
                                     "systemctl start wpa_supplicant@wlan0.service"], _heal_tools(env)
        assert re.search(r"load the WiFi driver sprdwl_ng \(wlan0 was gone\) on wlan0 after 3 stuck "
                         r"passes .*result=ok", r.stdout), r.stdout
        assert "built-in" not in r.stdout
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_loads_the_driver_before_it_starts_a_stopped_supplicant_without_wlan0():
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(wpa_active=False), extra_tools=("journalctl", "modprobe"),
                        driver_module="sprdwl_ng", iface_present=False)
        _seed_driver_module(env, "sprdwl_ng")
        r = _heal_pass(env)
        assert r.returncode == 0, (r.stdout, r.stderr)
        assert _driver_acts(env) == ["modprobe sprdwl_ng",
                                     "systemctl start wpa_supplicant@wlan0.service"], _heal_tools(env)
        assert re.search(r"load the WiFi driver sprdwl_ng and start wpa_supplicant@wlan0\.service on "
                         r"wlan0 \(it was inactive; wlan0 was gone\); .*result=ok", r.stdout), r.stdout
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_recovers_from_a_driver_load_that_failed_after_the_unload():
    # The review's red case: `modprobe -r` ran, the load failed -- wlan0 and its module link are
    # gone. The next pass must still know the module and load it, never "a built-in driver".
    tmp = tempfile.mkdtemp()
    try:
        state = _net_state(wpa_state="DISCONNECTED", reachable=False, driver_failed=1,
                           modprobe_load_rc=1,
                           on_reload={"wpa_state": "COMPLETED", "reachable": True,
                                      "driver_failed": 0})
        env = _heal_env(tmp, state, extra_tools=("journalctl", "modprobe"),
                        driver_module="sprdwl_ng")
        for _ in range(2):
            assert _heal_pass(env).returncode == 0
        r3 = _heal_pass(env)
        assert r3.returncode == 1 and "result=FAILED" in r3.stdout, (r3.stdout, r3.stderr)
        assert not os.path.isdir(os.path.join(env["BKSHADING_WIFI_HEAL_SYSFS_NET"], "wlan0"))
        _set_state(env, modprobe_load_rc=0)
        r4 = _heal_pass(env)
        assert r4.returncode == 0, (r4.stdout, r4.stderr)
        assert "load the WiFi driver sprdwl_ng" in r4.stdout and "built-in" not in r4.stdout, r4.stdout
        assert "result=ok" in r4.stdout, r4.stdout
        assert os.path.isdir(os.path.join(env["BKSHADING_WIFI_HEAL_SYSFS_NET"], "wlan0"))
        assert _get_state(env)["wpa_active"] is True
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_finishes_the_reload_when_the_pass_is_killed_in_the_middle():
    # systemd ends a pass at TimeoutStartSec with SIGTERM (a terminal run: Ctrl-C = SIGINT).
    # Between `modprobe -r` and the start the board has no driver and no supplicant: the reload's
    # trap loads the module and queues the supplicant start (--no-block: a `systemctl stop` of the
    # heal would hold a blocking start behind its own stop job) before the pass exits.
    for sig, rc in ((15, 143), (2, 130)):
        tmp = tempfile.mkdtemp()
        try:
            state = _net_state(wpa_state="DISCONNECTED", reachable=False, driver_failed=1,
                               signal_heal_on_unload=sig,
                               on_reload={"wpa_state": "COMPLETED", "reachable": True,
                                          "driver_failed": 0})
            env = _heal_env(tmp, state, extra_tools=("journalctl", "modprobe"),
                            driver_module="sprdwl_ng")
            for _ in range(2):
                assert _heal_pass(env).returncode == 0
            r = _heal_pass(env)
            assert r.returncode == rc, (sig, r.returncode, r.stdout, r.stderr)
            assert "in the middle of the driver reload" in r.stderr, r.stderr
            assert _driver_acts(env) == [
                "systemctl stop wpa_supplicant@wlan0.service", "modprobe -r sprdwl_ng",
                "modprobe sprdwl_ng", "systemctl start --no-block wpa_supplicant@wlan0.service",
            ], (sig, _heal_tools(env))
            assert os.path.isdir(os.path.join(env["BKSHADING_WIFI_HEAL_SYSFS_NET"], "wlan0"))
            assert _get_state(env)["wpa_active"] is True
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


def test_heal_finishes_the_reload_when_a_command_fails_in_the_middle():
    # The EXIT arm: a command that fails under `set -e` between the stop and the start (here the
    # journal cursor cannot be dropped) ends the pass; the trap still starts the supplicant.
    tmp = tempfile.mkdtemp()
    try:
        state = _net_state(wpa_state="DISCONNECTED", reachable=False, driver_failed=1,
                           break_cursor_on_stop=True)
        env = _heal_env(tmp, state, extra_tools=("journalctl", "modprobe"),
                        driver_module="sprdwl_ng")
        for _ in range(2):
            assert _heal_pass(env).returncode == 0
        r = _heal_pass(env)
        assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
        assert "in the middle of the driver reload or supplicant start (exit 1)" in r.stderr, r.stderr
        # a reload plan: the trap does load the driver, and says so
        assert "loading the driver sprdwl_ng and queueing the start" in r.stderr, r.stderr
        acts = _driver_acts(env)
        assert acts[0] == "systemctl stop wpa_supplicant@wlan0.service", acts
        assert acts[-1] == "systemctl start --no-block wpa_supplicant@wlan0.service", acts
        assert "modprobe -r sprdwl_ng" not in acts, "the failure came before the unload"
        assert _get_state(env)["wpa_active"] is True
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_reloads_a_loaded_driver_whose_wlan0_never_came_back():
    # The review's second red case: `modprobe` loaded the module but wlan0 never appeared. A
    # load-only plan would run `modprobe` on a loaded module (a no-op) on every later pass, and the
    # board would stay off the WiFi until a reboot. A loaded module without wlan0 is reloaded.
    tmp = tempfile.mkdtemp()
    try:
        state = _net_state(wpa_state="DISCONNECTED", reachable=False, driver_failed=1,
                           load_no_iface=True,
                           on_reload={"wpa_state": "COMPLETED", "reachable": True,
                                      "driver_failed": 0})
        env = _heal_env(tmp, state, extra_tools=("journalctl", "modprobe"),
                        driver_module="sprdwl_ng")
        env["BKSHADING_WIFI_HEAL_IFACE_WAIT_S"] = "2"
        for _ in range(2):
            assert _heal_pass(env).returncode == 0
        r3 = _heal_pass(env)
        assert r3.returncode == 1 and "result=FAILED" in r3.stdout, (r3.stdout, r3.stderr)
        mods = env["BKSHADING_WIFI_HEAL_SYSFS_MODULE"]
        assert os.path.isdir(os.path.join(mods, "sprdwl_ng")), "the module is loaded"
        assert not os.path.isdir(os.path.join(env["BKSHADING_WIFI_HEAL_SYSFS_NET"], "wlan0"))
        _set_state(env, load_no_iface=False)
        # the wait ends the moment wlan0 is back (~1 s here): the full bound costs nothing
        env.pop("BKSHADING_WIFI_HEAL_IFACE_WAIT_S")
        before = len(_heal_tools(env))
        r4 = _heal_pass(env)
        assert r4.returncode == 0 and "result=ok" in r4.stdout, (r4.stdout, r4.stderr)
        acts = [t for t in _heal_tools(env)[before:]
                if t.startswith(("systemctl stop", "systemctl start", "modprobe"))]
        assert acts == ["modprobe -r sprdwl_ng", "modprobe sprdwl_ng",
                        "systemctl start wpa_supplicant@wlan0.service"], acts
        assert os.path.isdir(os.path.join(env["BKSHADING_WIFI_HEAL_SYSFS_NET"], "wlan0"))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_finishes_the_start_when_the_pass_is_killed_after_the_unload():
    # The start of a stopped supplicant with wlan0 gone and the module loaded unloads it first.
    # A pass ended between that unload and the load is restored by the same trap as a reload.
    tmp = tempfile.mkdtemp()
    try:
        state = _net_state(wpa_active=False, signal_heal_on_unload=15)
        env = _heal_env(tmp, state, extra_tools=("journalctl", "modprobe"),
                        driver_module="sprdwl_ng", iface_present=False, module_loaded=True)
        _seed_driver_module(env, "sprdwl_ng")
        r = _heal_pass(env)
        assert r.returncode == 143, (r.returncode, r.stdout, r.stderr)
        assert "in the middle of the driver reload" in r.stderr, r.stderr
        assert _driver_acts(env) == ["modprobe -r sprdwl_ng", "modprobe sprdwl_ng",
                                     "systemctl start --no-block wpa_supplicant@wlan0.service"], \
            _heal_tools(env)
        assert os.path.isdir(os.path.join(env["BKSHADING_WIFI_HEAL_SYSFS_NET"], "wlan0"))
        assert _get_state(env)["wpa_active"] is True
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_restore_trap_on_a_start_pass_without_a_driver_step():
    # Every start pass arms the restore trap, also one with wlan0 present (no driver step). A pass
    # ended inside it (here: a command failing under errexit) only queues the start it was making,
    # and its message says so -- it loads nothing, so it must not claim to.
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(wpa_active=False, break_cursor_on_is_active=True),
                        extra_tools=("journalctl", "modprobe"), driver_module="sprdwl_ng")
        r = _heal_pass(env)
        assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
        assert "in the middle of the driver reload or supplicant start (exit 1)" in r.stderr, r.stderr
        assert "queueing the start of wpa_supplicant@wlan0.service" in r.stderr, r.stderr
        assert "loading the driver" not in r.stderr, r.stderr
        assert _driver_acts(env) == ["systemctl start --no-block wpa_supplicant@wlan0.service"], \
            _heal_tools(env)
        assert _get_state(env)["wpa_active"] is True
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_never_uses_a_remembered_module_on_a_board_whose_wlan0_has_none():
    # A remembered name (from a board state that is gone) must not reach modprobe while wlan0
    # exists without a module link: that is a built-in driver.
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(wpa_state="DISCONNECTED", reachable=False, driver_failed=1),
                        extra_tools=("journalctl", "modprobe"), driver_module=None)
        _seed_driver_module(env, "sprdwl_ng")
        for _ in range(3):
            r = _heal_pass(env)
            assert r.returncode == 0, (r.stdout, r.stderr)
        assert "no driver reload: a built-in driver" in r.stdout, r.stdout
        assert not any(t.startswith("modprobe") for t in _heal_tools(env)), _heal_tools(env)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_resets_the_stuck_state_on_a_completed_pass_it_cannot_judge():
    # A COMPLETED link resets the stuck count and drops the journal cursor even when the pass
    # cannot judge reachability (ping exit 2 -> exit 1): a later drop must not count refusals
    # from before the working stretch, or start one stuck pass from the limit.
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(ping_rc=2), extra_tools=("journalctl",))
        state_dir = env["BKSHADING_WIFI_HEAL_STATE_DIR"]
        os.makedirs(state_dir)
        with open(os.path.join(state_dir, "stuck"), "w") as f:
            f.write("2\n")
        with open(os.path.join(state_dir, "journal-cursor"), "w") as f:
            f.write("stub-cursor=0\n")
        r = _heal_pass(env)
        assert r.returncode == 1, (r.stdout, r.stderr)
        assert _stuck(env) == "0", "a COMPLETED pass resets the stuck count"
        assert not os.path.exists(os.path.join(state_dir, "journal-cursor"))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_refused_count_ignores_other_supplicant_lines():
    text = "\n".join([
        "wlan0: CTRL-EVENT-SCAN-STARTED",
        "wlan0: Association request to the driver failed",
        "wlan0: CTRL-EVENT-ASSOC-REJECT bssid=92:0d:ab:03:67:07 status_code=1",
        "wlan0: Trying to associate with 92:0d:ab:03:67:07",
        "wlan0: Association request to the driver failed",
        "",
    ])
    assert _lib_call('bkshading_sbc_wifi_heal_count_refused "$A"', env={"A": text}).strip() == "2"
    assert _lib_call('bkshading_sbc_wifi_heal_count_refused ""').strip() == "0"


if __name__ == "__main__":
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
