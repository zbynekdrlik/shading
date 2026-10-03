#!/usr/bin/env python3
"""The handheld SBC's WiFi heal (issue 808, design 5972548198): scripts/bkshading-wifi-heal.sh, its
pure decisions in scripts/lib/bkshading-sbc-runtime.sh and systemd/bkshading-wifi-heal.{service,timer}.

The heal pings the DHCP gateway on wlan0 and reassociates / restarts the supplicant after misses
(the reachability rungs, which wait while wpa_state is not COMPLETED). Its stuck rung starts a
stopped supplicant, and reloads the WiFi driver after driver-refused associations or a hung
supplicant (live on handheld-1, 3.10.2026: the uwe5622 driver refused every association until
sprdwl_ng was reloaded).

The tests run the REAL script with PATH = a stub dir only (the board-tool stubs and the heal
harness live in bkshading_sbc_fakes_808.py, shared with the provisioning tests), so a missing stub
can never reach the machine's real wpa_cli, journalctl or modprobe. This file holds the
reachability rungs, the unit bounds and the docs pins; the stuck rung's tests are in
test_bkshading_wifi_heal_stuck_808.py. Split out of test_bkshading_sbc_provision_808.py. Runs in the
`python-tests` CI job; runnable directly (`python3 tests/python/test_bkshading_wifi_heal_808.py`) or
under pytest.
"""

import os
import re
import shutil
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from bkshading_sbc_fakes_808 import (  # noqa: E402
    FAR_BSSID,
    GOOD_BSSID,
    HEAL_SCRIPT,
    HEAL_SERVICE,
    HEAL_TIMER,
    LIB,
    README,
    REPO,
    _heal_env,
    _heal_misses,
    _heal_pass,
    _heal_tools,
    _lib_call,
    _net_state,
    _read,
    _unit_value,
)


def test_heal_decision_table():
    table = [
        # prev, wpa_state, reachable -> action misses
        ("0", "COMPLETED", "yes", "none 0"),
        ("4", "COMPLETED", "yes", "none 0"),
        ("0", "COMPLETED", "no", "none 1"),
        ("1", "COMPLETED", "no", "none 2"),
        ("2", "COMPLETED", "no", "reassociate 3"),
        ("3", "COMPLETED", "no", "none 4"),
        ("4", "COMPLETED", "no", "none 5"),
        ("5", "COMPLETED", "no", "restart 0"),
        ("17", "COMPLETED", "no", "restart 0"),   # a stale count past 2N acts at once
        ("2", "SCANNING", "no", "none 0"),        # the supplicant is working: never act
        ("5", "ASSOCIATING", "no", "none 0"),
        ("5", "", "no", "none 0"),                # no supplicant answer = not COMPLETED
        ("junk", "COMPLETED", "no", "none 1"),    # a damaged count reads as 0
        ("08", "COMPLETED", "no", "restart 0"),   # decimal 8 (octal would be an arithmetic error)
        ("02", "COMPLETED", "no", "reassociate 3"),
    ]
    for prev, state, reach, want in table:
        got = _lib_call('bkshading_sbc_wifi_heal_decide "$A" "$B" "$C"',
                        env={"A": prev, "B": state, "C": reach}).strip()
        assert got == want, (prev, state, reach, got, want)


def test_heal_service_timeouts_cover_the_longest_pass():
    # Every bounded step of the longest pass (a driver reload), from the lib's constants. The unit's
    # TimeoutStartSec must cover it and its comment must state the same figures (it said "up to
    # 15 s settle" while the settled after-read alone takes up to 23 s).
    def const(fn):
        return int(_lib_call(fn).strip())
    tool = const("bkshading_sbc_wifi_tool_timeout_s")
    sysctl = const("bkshading_sbc_wifi_heal_systemctl_timeout_s")
    iface_wait = const("bkshading_sbc_wifi_heal_iface_wait_s")
    settle = const("bkshading_sbc_wifi_heal_settle_s")
    snapshot = 2 * tool                    # wpa_cli status + signal_poll
    # the last read starts just inside the settle bound, after a 1 s sleep, + 1 s of SECONDS rounding
    settled = settle + 1 + snapshot + 1
    reload_pass = (snapshot                # the "before" read
                   + tool                  # systemctl is-active (the supplicant does not answer)
                   + tool                  # the journal count read
                   + sysctl                # systemctl stop
                   + tool                  # the journal cursor to the end
                   + 2 * sysctl            # modprobe -r, modprobe
                   + iface_wait            # wlan0 comes back
                   + sysctl                # systemctl start
                   + settled)
    assert reload_pass <= int(_unit_value(HEAL_SERVICE, "TimeoutStartSec")), reload_pass
    unit = _read(HEAL_SERVICE)
    assert "= %d s" % reload_pass in unit, "the unit comment must state the computed bound"
    assert "up to %d s" % settled in unit, "the unit comment must state the settled after-read bound"
    # the start of a stopped supplicant with wlan0 gone runs its own driver steps (no stop)
    start_pass = (snapshot                # the "before" read
                  + tool                  # systemctl is-active
                  + tool                  # the journal count read
                  + 2 * sysctl            # modprobe -r, modprobe (a loaded module without wlan0)
                  + iface_wait            # wlan0 comes back
                  + tool                  # the journal cursor to the end
                  + sysctl                # systemctl start
                  + settled)
    assert start_pass <= int(_unit_value(HEAL_SERVICE, "TimeoutStartSec")), start_pass
    # a pass ended mid-reload runs the trap (load + wlan0 wait + start) inside TimeoutStopSec
    assert 2 * sysctl + iface_wait <= int(_unit_value(HEAL_SERVICE, "TimeoutStopSec"))
    # the script takes the bounds from the lib (the wlan0 wait overridable for tests only), and
    # the reload and the start make exactly the calls counted above
    script = _read(HEAL_SCRIPT)
    assert 'SYSTEMCTL_TIMEOUT_S="$(bkshading_sbc_wifi_heal_systemctl_timeout_s)"' in script
    assert '"${BKSHADING_WIFI_HEAL_IFACE_WAIT_S:-}" "$(bkshading_sbc_wifi_heal_iface_wait_s)"' in script

    def body(head, end):
        return script.split(head, 1)[1].split(end, 1)[0]
    reload_case = body("\n  reload-driver)\n", "\nesac\n")
    start_case = body("\n  start)\n", "\n  reload-driver)\n")
    plan_fn = body("\napply_driver_plan() {\n", "\n}\n")
    load_fn = body("\nload_driver() {\n", "\n}\n")
    assert reload_case.count('timeout "$SYSTEMCTL_TIMEOUT_S"') == 2, "stop, start"
    assert reload_case.count("apply_driver_plan") == 1
    assert start_case.count('timeout "$SYSTEMCTL_TIMEOUT_S"') == 1, "start"
    assert start_case.count("apply_driver_plan") == 1
    assert plan_fn.count('timeout "$SYSTEMCTL_TIMEOUT_S"') == 1, "modprobe -r"
    assert plan_fn.count("load_driver") == 2, "after the unload, or alone"
    assert load_fn.count('timeout "$SYSTEMCTL_TIMEOUT_S"') == 1 and "wait_for_iface" in load_fn


SBC_RULE = os.path.join(REPO, ".claude", "rules", "bkshading-sbc.md")


def test_heal_docs_name_the_stuck_rung_exception():
    # Review findings 4-7 + 12: the docs still said the heal never acts on a link that is not
    # COMPLETED -- the stuck rung (a refusing driver, a stopped or hung supplicant) does.
    stale = {
        HEAL_SERVICE: ["Never acts while wpa_state is not COMPLETED"],
        HEAL_SCRIPT: ["while it is not COMPLETED it does nothing"],
        README: ["never while\n  the supplicant is still connecting", "never while the supplicant is still connecting"],
        LIB: ["the heal does nothing while wpa_state is\n# not COMPLETED",
              "the WiFi heal only acts on a COMPLETED link",
              "and nothing while wpa_state is not COMPLETED",
              "key_mgmt + PMF as netplan generated them"],
    }
    for path, phrases in stale.items():
        text = _read(path)
        for phrase in phrases:
            assert phrase not in text, (path, phrase)
    assert "stuck rung" in _read(HEAL_SERVICE)
    assert "stuck rung" in _read(HEAL_SCRIPT).split("set -euo pipefail", 1)[1].split("HERE=", 1)[0]
    readme = _read(README)
    assert "Association request to the driver failed" in readme and "driver reload" in readme
    dropin = _lib_call("bkshading_sbc_wpa_restart_dropin_content")
    assert "COMPLETED" not in dropin and "stopped" in dropin, dropin
    assert "deliberately" in _read(LIB).split("bkshading_sbc_wpa_key_mgmt()", 1)[0][-1200:], \
        "the key_mgmt comment says it differs from netplan on purpose (no SAE)"
    rule = _read(SBC_RULE)
    testing = rule.split("**Testing the heal:**", 1)[1].split("\n- ", 1)[0]
    for word in ("journalctl", "modprobe", "BKSHADING_WIFI_HEAL_SYSFS_NET"):
        assert word in testing, word
    # the live handheld-1 facts (3.10.2026) the module resolution rests on
    for fact in ("/sys/module/sprdwl_ng", "unisoc_wifi", "uwe5622_bsp_sdio"):
        assert fact in rule, fact


# --- the heal units + the heal script, run for real against stubs on PATH ---
def test_heal_units_match_the_lib():
    interval = _lib_call("bkshading_sbc_wifi_heal_interval_s").strip()
    assert _unit_value(HEAL_TIMER, "OnUnitActiveSec") == interval + "s"
    acc = _unit_value(HEAL_TIMER, "AccuracySec")
    assert acc is not None and int(acc.rstrip("s")) < int(interval), \
        "the default 1 min AccuracySec would stretch the cadence"
    assert "WantedBy=timers.target" in _read(HEAL_TIMER)
    install_dir = _lib_call("bkshading_sbc_wifi_heal_install_dir").strip()
    assert _unit_value(HEAL_SERVICE, "ExecStart") == install_dir + "/bkshading-wifi-heal.sh"
    assert _unit_value(HEAL_SERVICE, "Type") == "oneshot"
    # no reboot, no ifdown loop: no such command on any non-comment line of the heal
    code = [ln for ln in _read(HEAL_SCRIPT).splitlines() if not ln.lstrip().startswith("#")]
    for ln in code:
        assert not re.search(r"\b(reboot|shutdown|poweroff|ifdown|ip link set)\b", ln), ln


def test_heal_reassociates_after_three_misses_then_restarts_after_three_more():
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(reachable=False, bssid=FAR_BSSID, rssi=-73))
        outs = []
        for i in range(6):
            r = _heal_pass(env)
            assert r.returncode == 0, (i, r.stdout, r.stderr)
            outs.append(r.stdout)
        tools = _heal_tools(env)
        reassoc = [t for t in tools if t.endswith(" reassociate")]
        restart = [t for t in tools if t.startswith("systemctl restart")]
        assert len(reassoc) == 1 and reassoc[0].startswith("wpa_cli -p /run/wpa_supplicant -i wlan0"), tools
        assert restart == ["systemctl restart wpa_supplicant@wlan0.service"], tools
        assert "miss 1 of 3" in outs[0] and outs[1] == ""
        assert re.search(r"wpa_cli reassociate on wlan0 after 3 consecutive misses \(gateway 10\.77\.8\.1\); "
                         r"before bssid=%s signal=-73 dBm; after bssid=%s signal=-73 dBm wpa_state=COMPLETED; "
                         r"result=OK" % (re.escape(FAR_BSSID), re.escape(FAR_BSSID)), outs[2]), outs[2]
        assert outs[3] == "" and outs[4] == ""
        assert re.search(r"systemctl restart wpa_supplicant@wlan0\.service on wlan0 after 6 consecutive "
                         r"misses .*result=ok", outs[5]), outs[5]
        assert _heal_misses(env) == "0", "a restart starts the count over"
        assert not any("reboot" in t for t in tools)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_reassociate_moves_to_the_strong_ap_and_logs_before_and_after():
    tmp = tempfile.mkdtemp()
    try:
        state = _net_state(reachable=False, bssid=FAR_BSSID, rssi=-73,
                           on_reassociate={"bssid": GOOD_BSSID, "rssi": -63, "reachable": True})
        env = _heal_env(tmp, state)
        for _ in range(3):
            r = _heal_pass(env)
            assert r.returncode == 0, (r.stdout, r.stderr)
        assert re.search(r"before bssid=%s signal=-73 dBm; after bssid=%s signal=-63 dBm"
                         % (re.escape(FAR_BSSID), re.escape(GOOD_BSSID)), r.stdout), r.stdout
        assert len([ln for ln in r.stdout.splitlines() if ln.strip()]) == 1, "ONE line per action"
        r4 = _heal_pass(env)
        assert "answers again after 3 consecutive misses" in r4.stdout, r4.stdout
        assert _heal_misses(env) == "0"
        assert _heal_pass(env).stdout == "", "a healthy pass is quiet"
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_never_acts_while_the_supplicant_is_not_completed():
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(wpa_state="SCANNING", reachable=False))
        os.makedirs(env["BKSHADING_WIFI_HEAL_STATE_DIR"])
        with open(os.path.join(env["BKSHADING_WIFI_HEAL_STATE_DIR"], "misses"), "w") as f:
            f.write("5\n")
        r = _heal_pass(env)
        assert r.returncode == 0, (r.stdout, r.stderr)
        tools = _heal_tools(env)
        assert not any(t.startswith("ping") or t.endswith("reassociate") or
                       t.startswith("systemctl") for t in tools), tools
        assert _heal_misses(env) == "0"
        assert "wpa_state=SCANNING" in r.stdout and "reset from 5" in r.stdout, r.stdout
        # no supplicant behind the socket at all: also no action
        other = os.path.join(tmp, "no-supplicant")
        os.makedirs(other)
        env2 = _heal_env(other, _net_state(wpa_state=None))
        r2 = _heal_pass(env2)
        assert r2.returncode == 0 and _heal_misses(env2) == "0", (r2.stdout, r2.stderr)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_counts_a_missing_dhcp_gateway_as_a_miss_without_pinging():
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(gateway=None))
        r = _heal_pass(env)
        assert r.returncode == 0, (r.stdout, r.stderr)
        assert _heal_misses(env) == "1"
        assert "no DHCP default route on wlan0" in r.stdout, r.stdout
        assert "did not answer" not in r.stdout, "no ping was sent, so none can be unanswered"
        assert not any(t.startswith("ping") for t in _heal_tools(env))
        ips = [t for t in _heal_tools(env) if t.startswith("ip ")]
        assert ips == ["ip -4 route show default dev wlan0"], ips
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_damaged_counter_reads_as_zero_and_a_failed_restart_exits_nonzero():
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(reachable=False, restart_rc=1,
                                        on_restart={"wpa_state": "DISCONNECTED"}))
        os.makedirs(env["BKSHADING_WIFI_HEAL_STATE_DIR"])
        mf = os.path.join(env["BKSHADING_WIFI_HEAL_STATE_DIR"], "misses")
        with open(mf, "w") as f:
            f.write("garbage\n")
        r = _heal_pass(env)
        assert r.returncode == 0 and _heal_misses(env) == "1", (r.stdout, r.stderr)
        with open(mf, "w") as f:
            f.write("5\n")
        r2 = _heal_pass(env)
        assert r2.returncode == 1, (r2.stdout, r2.stderr)
        assert "result=FAILED" in r2.stdout and "wpa_state=DISCONNECTED" in r2.stdout, r2.stdout
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_never_acts_when_a_tool_is_missing_or_ping_errors():
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state())
        os.remove(os.path.join(env["PATH"], "ping"))
        for i in range(6):
            r = _heal_pass(env)
            assert r.returncode == 1, (i, r.stdout, r.stderr)
            assert "ping" in r.stderr and "not found" in r.stderr, r.stderr
        tools = _heal_tools(env)
        assert not any(t.endswith("reassociate") or t.startswith("systemctl") for t in tools), tools
        assert not os.path.exists(os.path.join(env["BKSHADING_WIFI_HEAL_STATE_DIR"], "misses")) \
            or _heal_misses(env) == "0"
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(ping_rc=2))
        for i in range(6):
            r = _heal_pass(env)
            assert r.returncode == 1, (i, r.stdout, r.stderr)
        assert "exit 2" in r.stderr, r.stderr
        assert not any(t.endswith("reassociate") or t.startswith("systemctl")
                       for t in _heal_tools(env))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_waits_for_the_new_association_before_reading_after():
    # wpa_cli reassociate returns at once and the supplicant stays COMPLETED on the old BSSID while
    # it scans: an "after" read at once repeats "before" even when the roam succeeds.
    tmp = tempfile.mkdtemp()
    try:
        state = _net_state(reachable=False, bssid=FAR_BSSID, rssi=-73, reassociate_delay=2,
                           on_reassociate_delayed={"bssid": GOOD_BSSID, "rssi": -63,
                                                   "reachable": True})
        env = _heal_env(tmp, state)
        env["BKSHADING_WIFI_HEAL_SETTLE_S"] = "6"
        for _ in range(3):
            r = _heal_pass(env)
            assert r.returncode == 0, (r.stdout, r.stderr)
        assert re.search(r"before bssid=%s signal=-73 dBm; after bssid=%s signal=-63 dBm"
                         % (re.escape(FAR_BSSID), re.escape(GOOD_BSSID)), r.stdout), r.stdout
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_logs_the_recovery_after_a_restart():
    tmp = tempfile.mkdtemp()
    try:
        state = _net_state(reachable=False, on_restart={"reachable": True, "bssid": GOOD_BSSID})
        env = _heal_env(tmp, state)
        for _ in range(6):
            assert _heal_pass(env).returncode == 0
        r = _heal_pass(env)
        assert re.search(r"answers again .*systemctl restart wpa_supplicant@wlan0", r.stdout), r.stdout
        assert _heal_pass(env).stdout == "", "the recovery is logged once"
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_bounds_a_wedged_wpa_cli():
    import time
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(wpa_hang=True))
        env["BKSHADING_WIFI_HEAL_TOOL_TIMEOUT_S"] = "1"
        t0 = time.monotonic()
        r = _heal_pass(env)
        took = time.monotonic() - t0
        assert r.returncode == 0, (r.stdout, r.stderr)
        assert took < 10, "a wedged wpa_cli must be killed, not waited out (%.1f s)" % took
        assert _heal_misses(env) == "0", "an unanswered supplicant is not COMPLETED: no action"
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_waits_out_a_long_scan_before_reading_after():
    # a full 2.4 + 5 GHz scan with passive DFS channels can take longer than 5 s; the supplicant
    # stays COMPLETED on the old BSSID all that time
    tmp = tempfile.mkdtemp()
    try:
        state = _net_state(reachable=False, bssid=FAR_BSSID, rssi=-73, reassociate_delay=7,
                           on_reassociate_delayed={"bssid": GOOD_BSSID, "rssi": -63,
                                                   "reachable": True})
        env = _heal_env(tmp, state)
        env["BKSHADING_WIFI_HEAL_SETTLE_S"] = "12"
        for _ in range(3):
            r = _heal_pass(env)
            assert r.returncode == 0, (r.stdout, r.stderr)
        assert re.search(r"after bssid=%s signal=-63 dBm" % re.escape(GOOD_BSSID), r.stdout), r.stdout
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def test_heal_names_a_supplicant_that_does_not_answer():
    tmp = tempfile.mkdtemp()
    try:
        env = _heal_env(tmp, _net_state(wpa_state=None))
        os.makedirs(env["BKSHADING_WIFI_HEAL_STATE_DIR"])
        with open(os.path.join(env["BKSHADING_WIFI_HEAL_STATE_DIR"], "misses"), "w") as f:
            f.write("2\n")
        r = _heal_pass(env)
        assert r.returncode == 0, (r.stdout, r.stderr)
        assert "does not answer" in r.stdout and "is working" not in r.stdout, r.stdout
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


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
