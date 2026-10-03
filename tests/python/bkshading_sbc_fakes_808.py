"""Shared fakes for the handheld SBC tests (issue 808): the board-tool stubs and the heal harness.

`test_bkshading_sbc_provision_808.py` (provisioning + --check) and `test_bkshading_wifi_heal_808.py`
(the WiFi heal) both run the REAL scripts against these fakes, so they live in one place. Not a test
module (no `test_` prefix, pytest never collects it): the test files import it after putting this
directory on sys.path. Stdlib only.
"""
import json
import os
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
LIB = os.path.join(REPO, "scripts", "lib", "bkshading-sbc-runtime.sh")
README = os.path.join(REPO, "bkshading", "README.md")
HEAL_SCRIPT = os.path.join(REPO, "scripts", "bkshading-wifi-heal.sh")
HEAL_SERVICE = os.path.join(REPO, "systemd", "bkshading-wifi-heal.service")
HEAL_TIMER = os.path.join(REPO, "systemd", "bkshading-wifi-heal.timer")

# the board's WiFi as the stubs answer it: the DHCP gateway, the strong AP and the far one
WIFI_GW = "10.77.8.1"
GOOD_BSSID = "92:0d:ab:03:67:07"
FAR_BSSID = "aa:0d:ab:03:6f:af"

REAL_WPA_PASSPHRASE = shutil.which("wpa_passphrase") or ""

# One python stub serving every board tool the WiFi takeover + heal call. It logs its argv to
# FAKE_TOOL_LOG and answers from the JSON state at FAKE_NET_STATE (which reassociate/restart update),
# so the REAL script text runs end to end. `wpa_passphrase` runs the real binary when this machine
# has one, else emulates it exactly (the passphrase on stdin when no 2nd argument is given, the
# 8..63 length rule, and the `#psk="<passphrase>"` comment line the script must strip).
TOOL_STUB = r'''#!__PY__
import hashlib, json, os, subprocess, sys
name = os.path.basename(sys.argv[0])
args = sys.argv[1:]
with open(os.environ["FAKE_TOOL_LOG"], "a") as f:
    f.write(name + " " + " ".join(args) + "\n")
if name == "wpa_passphrase":
    real = "__REAL_WPA_PASSPHRASE__"
    if real:
        sys.exit(subprocess.run([real] + args).returncode)
    ssid = args[0]
    if len(args) > 1:
        pw = args[1].encode()
    else:
        sys.stderr.write("# reading passphrase from stdin\n")
        pw = sys.stdin.buffer.readline().split(b"\n")[0].split(b"\r")[0]
    if not 8 <= len(pw) <= 63:
        print("Passphrase must be 8..63 characters")
        sys.exit(1)
    psk = hashlib.pbkdf2_hmac("sha1", pw, ssid.encode(), 4096, 32).hex()
    print("network={")
    print('\tssid="%s"' % ssid)
    print('\t#psk="%s"' % pw.decode())
    print("\tpsk=%s" % psk)
    print("}")
    sys.exit(0)
state_path = os.environ["FAKE_NET_STATE"]
with open(state_path) as f:
    st = json.load(f)
def save():
    with open(state_path, "w") as f:
        json.dump(st, f)
REFUSED = "wlan0: Association request to the driver failed"
def supplicant_activity():
    # what the supplicant writes to its journal while the board is off the WiFi: ordinary lines,
    # and `driver_failed` driver-refused associations among them (the uwe5622 wedge)
    journal = st.setdefault("journal", [])
    journal.append("wlan0: CTRL-EVENT-SCAN-STARTED")
    for _ in range(int(st.get("driver_failed", 0))):
        journal.append("wlan0: Trying to associate with 92:0d:ab:03:67:07 (SSID='newlevel.media' freq=5180 MHz)")
        journal.append(REFUSED)
        journal.append("wlan0: CTRL-EVENT-ASSOC-REJECT bssid=92:0d:ab:03:67:07 status_code=1")
if name == "ip":
    if "route" in args and "default" in args and st.get("gateway"):
        print("default via %s dev wlan0 proto dhcp src 10.77.9.165 metric 600" % st["gateway"])
    sys.exit(0)
if name == "ping":
    if st.get("ping_rc") is not None:
        sys.exit(int(st["ping_rc"]))  # a ping that fails for another reason than a lost reply
    ok = bool(st.get("reachable")) and bool(st.get("gateway")) and args[-1:] == [st["gateway"]]
    sys.exit(0 if ok else 1)
if name == "wpa_cli":
    cmd = args[-1] if args else ""
    if st.get("wpa_active") is False:
        sys.exit(255)  # the supplicant unit is stopped: nothing behind the control socket
    if st.get("wpa_hang"):
        import time
        time.sleep(30)  # a wedged supplicant: the caller must bound the call
    if st.get("wpa_state") is None:
        sys.exit(255)  # no supplicant behind the control socket
    if cmd == "status":
        # a reassociate whose new association shows only after `pending_delay` more status reads
        # (wpa_supplicant keeps COMPLETED on the old BSSID while it scans)
        if "pending" in st:
            if st.get("pending_delay", 0) > 0:
                st["pending_delay"] -= 1
            else:
                st.update(st.pop("pending"))
            save()
        print("bssid=%s\nfreq=2437\nssid=newlevel.media\nid=0\nmode=station\nkey_mgmt=WPA2-PSK\n"
              "wpa_state=%s\nip_address=10.77.9.165" % (st["bssid"], st["wpa_state"]))
    elif cmd == "signal_poll":
        print("RSSI=%s\nLINKSPEED=65\nNOISE=9999\nFREQUENCY=2437" % st["rssi"])
    elif cmd == "reassociate":
        if "on_reassociate_delayed" in st:
            st["pending"] = st["on_reassociate_delayed"]
            st["pending_delay"] = int(st.get("reassociate_delay", 0))
        st.update(st.get("on_reassociate", {}))
        save()
        print("OK")
    sys.exit(0)
if name == "journalctl":
    # The supplicant's journal, served ONLY for the exact calls the heal must make (anything else
    # prints nothing, so a changed call can never count refusals by accident):
    #   count read, first: -u <unit> --cursor-file=<state>/journal-cursor --since <FAKE_JOURNAL_SINCE> -o cat --no-pager
    #   count read, next:  -u <unit> --cursor-file=<state>/journal-cursor -o cat --no-pager
    #   cursor to the end: -u <unit> --cursor-file=<state>/journal-cursor -n 1 -o cat --no-pager
    # A count read first appends the supplicant's lines since the last pass. The cursor file holds
    # the index of the last line served (journalctl writes the cursor of the last entry it showed);
    # a cursor it cannot read fails like the real one ("Failed to seek to cursor", exit 1). The
    # --since window starts at `journal_since_from` (default 0: every line is "recent", so a
    # caller that re-reads the window instead of following the cursor counts lines twice).
    cpath = os.path.join(os.environ["BKSHADING_WIFI_HEAL_STATE_DIR"], "journal-cursor")
    head = ["-u", "wpa_supplicant@wlan0.service", "--cursor-file=" + cpath]
    tail = ["-o", "cat", "--no-pager"]
    if args[:3] != head or args[-3:] != tail:
        sys.exit(0)
    rest = args[3:-3]
    journal = st.setdefault("journal", [])
    has_cursor = os.path.exists(cpath) and os.path.getsize(cpath) > 0
    if rest == ["-n", "1"]:
        # like the real journalctl (systemd 255, probed): with a cursor file present, -n 1 shows the
        # ONE entry after the cursor, not the journal's last one -- the caller must drop it first
        if has_cursor:
            text = open(cpath).read().strip()
            if not text.startswith("stub-cursor="):
                sys.stderr.write("Failed to seek to cursor: Invalid argument\n")
                sys.exit(1)
            at = int(text.split("=", 1)[1]) + 1
            out = journal[at:at + 1]
            if out:
                with open(cpath, "w") as f:
                    f.write("stub-cursor=%d\n" % at)
            for line in out:
                print(line)
            sys.exit(0)
        out = journal[-1:]
    else:
        supplicant_activity()
        save()
        if has_cursor:
            if rest:
                sys.stderr.write("stub journalctl: --since next to a cursor: %r\n" % rest)
                sys.exit(3)
            text = open(cpath).read().strip()
            if not text.startswith("stub-cursor="):
                sys.stderr.write("Failed to seek to cursor: Invalid argument\n")
                sys.exit(1)
            out = journal[int(text.split("=", 1)[1]) + 1:]
        elif rest == ["--since", os.environ["FAKE_JOURNAL_SINCE"]]:
            out = journal[int(st.get("journal_since_from", 0)):]
        else:
            sys.exit(0)
    for line in out:
        print(line)
    if out:
        with open(cpath, "w") as f:
            f.write("stub-cursor=%d\n" % (len(journal) - 1))
    sys.exit(0)
if name == "modprobe":
    # The WiFi driver module, loaded while <fake /sys/module>/<name> exists. Unloading it removes
    # that dir and wlan0 (both kept aside as .<name>-unloaded / .wlan0-unloaded, links intact);
    # loading an unloaded module brings its dir back at once and wlan0 `iface_delay_s` later, from a
    # detached process, as a real driver's probe does (never when `load_no_iface` is set: a driver
    # that loads but never creates wlan0). Loading a module that is already loaded does nothing,
    # like the real modprobe. `modprobe_rc` fails every call, `modprobe_load_rc` only the load.
    net = os.environ["BKSHADING_WIFI_HEAL_SYSFS_NET"]
    mods = os.environ["BKSHADING_WIFI_HEAL_SYSFS_MODULE"]
    live, gone = os.path.join(net, "wlan0"), os.path.join(net, ".wlan0-unloaded")
    mod = args[-1] if args else ""
    mod_live, mod_gone = os.path.join(mods, mod), os.path.join(mods, "." + mod + "-unloaded")
    if st.get("modprobe_rc"):
        sys.exit(int(st["modprobe_rc"]))
    if "-r" in args:
        if os.path.isdir(mod_live):
            os.rename(mod_live, mod_gone)
        if os.path.isdir(live):
            os.rename(live, gone)
        sig = int(st.get("signal_heal_on_unload") or 0)
        if sig and not st.get("signal_sent"):
            # the heal pass is ended right here, between the unload and the load (systemd's
            # SIGTERM at TimeoutStartSec, a Ctrl-C): signal the heal script's own bash, found by its
            # command line among this process's ancestors -- never a guess at a parent
            st["signal_sent"] = True
            save()
            pid = os.getppid()
            while pid > 1:
                with open("/proc/%d/cmdline" % pid, "rb") as f:
                    cmd = f.read()
                if b"bkshading-wifi-heal.sh" in cmd:
                    os.kill(pid, sig)
                    sys.exit(0)
                with open("/proc/%d/stat" % pid) as f:
                    pid = int(f.read().rsplit(")", 1)[1].split()[1])
            sys.stderr.write("stub modprobe: no bkshading-wifi-heal.sh among my ancestors\n")
            sys.exit(97)
        sys.exit(0)
    if st.get("modprobe_load_rc"):
        sys.exit(int(st["modprobe_load_rc"]))
    if os.path.isdir(mod_live):
        sys.exit(0)
    if os.path.isdir(mod_gone):
        os.rename(mod_gone, mod_live)
    st.update(st.get("on_reload", {}))
    save()
    if os.path.isdir(gone) and not st.get("load_no_iface"):
        subprocess.Popen(
            [sys.executable, "-c",
             "import os, sys, time; time.sleep(float(sys.argv[1])); os.rename(sys.argv[2], sys.argv[3])",
             str(st.get("iface_delay_s", 1)), gone, live],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            start_new_session=True)
    sys.exit(0)
if name == "systemctl":
    if args[:1] == ["restart"]:
        st.update(st.get("on_restart", {}))
        save()
        sys.exit(int(st.get("restart_rc", 0)))
    if args[:1] == ["stop"]:
        # the old supplicant keeps writing its refusals until it terminates
        supplicant_activity()
        st["wpa_active"] = False
        save()
        if st.get("break_cursor_on_stop"):
            # the next command in the reload window fails: the cursor path becomes a non-empty dir,
            # which `rm -f` cannot remove (an errexit inside the stop..start window)
            cpath = os.path.join(os.environ["BKSHADING_WIFI_HEAL_STATE_DIR"], "journal-cursor")
            if os.path.exists(cpath) and not os.path.isdir(cpath):
                os.remove(cpath)
            os.makedirs(cpath, exist_ok=True)
            open(os.path.join(cpath, "x"), "w").close()
        sys.exit(0)
    if args[:1] == ["start"]:
        # A simplification of the real unit (Requires= the wlan0 device): a real start waits on the
        # device job until the caller's timeout kills the client; here a start without wlan0 just
        # fails. `--no-block` only queues the start: it answers 0 at once either way.
        queued = "--no-block" in args
        net = os.environ.get("BKSHADING_WIFI_HEAL_SYSFS_NET")
        if net and not os.path.isdir(os.path.join(net, "wlan0")):
            st["start_without_iface"] = st.get("start_without_iface", 0) + 1
            save()
            sys.exit(0 if queued else 1)
        st["wpa_active"] = True
        st.update(st.get("on_start", {}))
        save()
        sys.exit(0 if queued else int(st.get("start_rc", 0)))
    if args[:1] == ["is-active"]:
        # the supplicant unit's word: `unit_word` when the test sets one, else active/inactive
        word = st.get("unit_word") or ("active" if st.get("wpa_active", True) else "inactive")
        print(word)
        sys.exit(0 if word == "active" else 3)
    if args[:1] == ["is-enabled"]:
        print("enabled")
    sys.exit(0)
sys.exit(2)
'''


def _write_tool_stubs(dirpath, names=("wpa_passphrase", "wpa_cli", "ip", "ping")):
    os.makedirs(dirpath, exist_ok=True)
    body = TOOL_STUB.replace("__PY__", sys.executable).replace(
        "__REAL_WPA_PASSPHRASE__", REAL_WPA_PASSPHRASE)
    out = {}
    for n in names:
        p = os.path.join(dirpath, n)
        with open(p, "w", encoding="utf-8") as f:
            f.write(body)
        os.chmod(p, 0o755)
        out[n] = p
    return out


def _net_state(**kw):
    st = {"wpa_state": "COMPLETED", "bssid": GOOD_BSSID, "rssi": -63, "gateway": WIFI_GW,
          "reachable": True}
    st.update(kw)
    return st


def _read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


def _lib_call(snippet, env=None):
    src = '. "%s"\n%s' % (LIB, snippet)
    e = dict(os.environ)
    e.update(env or {})
    r = subprocess.run(["bash", "-c", "set -euo pipefail\n" + src], capture_output=True, text=True,
                       env=e)
    assert r.returncode == 0, (snippet, r.returncode, r.stdout, r.stderr)
    return r.stdout


def _unit_value(path, key):
    for line in _read(path).splitlines():
        if line.startswith(key + "="):
            return line.split("=", 1)[1].strip()
    return None


def _heal_env(tmp, state, extra_tools=(), driver_module=None, iface_present=True):
    stubdir = os.path.join(tmp, "stubs")
    _write_tool_stubs(stubdir, names=("wpa_cli", "ip", "ping", "systemctl") + tuple(extra_tools))
    # PATH is the stub dir ONLY (ci-testing-gotchas, issue 1371): a missing stub can never fall
    # through to the real wpa_cli / ping of the machine running the test.
    for tool in ("dirname", "mkdir", "mv", "sleep", "timeout", "rm"):
        os.symlink(shutil.which(tool), os.path.join(stubdir, tool))
    statef = os.path.join(tmp, "net.json")
    with open(statef, "w", encoding="utf-8") as f:
        json.dump(state, f)
    return {
        "PATH": stubdir,
        "HOME": tmp,
        "FAKE_NET_STATE": statef,
        "FAKE_TOOL_LOG": os.path.join(tmp, "tools.log"),
        # the --since of the heal's first journal read: one timer interval + 5 s of slack
        "FAKE_JOURNAL_SINCE": "-%ds" % (int(_lib_call("bkshading_sbc_wifi_heal_interval_s")) + 5),
        "BKSHADING_WIFI_HEAL_STATE_DIR": os.path.join(tmp, "run"),
        "BKSHADING_WIFI_HEAL_SETTLE_S": "1",
        "BKSHADING_WIFI_HEAL_SYSFS_NET": _fake_wifi_sysfs(tmp, driver_module, iface_present),
        "BKSHADING_WIFI_HEAL_SYSFS_MODULE": os.path.join(tmp, "sys-module"),
    }


def _set_state(env, **kw):
    """Change the stubs' board state between two heal passes."""
    with open(env["FAKE_NET_STATE"], encoding="utf-8") as f:
        st = json.load(f)
    st.update(kw)
    with open(env["FAKE_NET_STATE"], "w", encoding="utf-8") as f:
        json.dump(st, f)
    return st


def _get_state(env):
    with open(env["FAKE_NET_STATE"], encoding="utf-8") as f:
        return json.load(f)


def _fake_wifi_sysfs(tmp, driver_module, iface_present=True):
    """A fake /sys/class/net with wlan0, whose device/driver/module links to <tmp>/sys-module/<name>
    (the fake /sys/module: sprdwl_ng on the Orange Pi Zero 2W) -- or no module link (a built-in
    driver). iface_present=False: the module is unloaded, so wlan0 and the module dir are gone
    (kept aside as .wlan0-unloaded / .<name>-unloaded, which the modprobe stub brings back on a
    load)."""
    net = os.path.join(tmp, "sys-class-net")
    dev = os.path.join(tmp, "sys-devices", "wlan0-dev")
    drv = os.path.join(tmp, "sys-bus", "drivers", "wlan-driver")
    mods = os.path.join(tmp, "sys-module")
    os.makedirs(os.path.join(net), exist_ok=True)
    os.makedirs(os.path.join(dev, "net"), exist_ok=True)
    os.makedirs(drv, exist_ok=True)
    os.makedirs(mods, exist_ok=True)
    os.makedirs(os.path.join(net, "wlan0"), exist_ok=True)
    os.symlink(dev, os.path.join(net, "wlan0", "device"))
    os.symlink(drv, os.path.join(dev, "driver"))
    if driver_module:
        mod = os.path.join(mods, driver_module)
        os.makedirs(mod, exist_ok=True)
        os.symlink(mod, os.path.join(drv, "module"))
    if not iface_present:
        os.rename(os.path.join(net, "wlan0"), os.path.join(net, ".wlan0-unloaded"))
        if driver_module:
            os.rename(os.path.join(mods, driver_module),
                      os.path.join(mods, "." + driver_module + "-unloaded"))
    return net


def _heal_pass(env, script=HEAL_SCRIPT):
    return subprocess.run(["/bin/bash", script], env=env, capture_output=True, text=True)


def _heal_misses(env):
    return _read(os.path.join(env["BKSHADING_WIFI_HEAL_STATE_DIR"], "misses")).strip()


def _heal_tools(env):
    p = env["FAKE_TOOL_LOG"]
    return _read(p).splitlines() if os.path.exists(p) else []
