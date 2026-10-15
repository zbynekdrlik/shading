"""Shared fakes for the handheld SBC tests (issue 808): the board-tool stubs, the heal harness and
the provision harness.

`test_bkshading_sbc_provision_808.py` (relay, read-only root, deploy, CI),
`test_bkshading_sbc_wifi_takeover_808.py` (netplan migration, supplicant conf, WiFi --check rows) and
`test_bkshading_wifi_heal_808.py` (the WiFi heal) run the REAL scripts against these fakes, so they
live in one place. Not a test module (no `test_` prefix, pytest never collects it): the test files
import it after putting this directory on sys.path. Stdlib only.
"""
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile

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
def break_cursor():
    # a later command in the heal's restore window fails: the cursor path becomes a non-empty dir,
    # which `rm -f` cannot remove (an errexit inside the armed window)
    cpath = os.path.join(os.environ["BKSHADING_WIFI_HEAL_STATE_DIR"], "journal-cursor")
    if os.path.exists(cpath) and not os.path.isdir(cpath):
        os.remove(cpath)
    os.makedirs(cpath, exist_ok=True)
    open(os.path.join(cpath, "x"), "w").close()
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
            break_cursor()
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
        if st.get("break_cursor_on_is_active"):
            break_cursor()
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


def _heal_env(tmp, state, extra_tools=(), driver_module=None, iface_present=True,
              module_loaded=None):
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
        "BKSHADING_WIFI_HEAL_SYSFS_NET": _fake_wifi_sysfs(tmp, driver_module, iface_present,
                                                         module_loaded),
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


def _fake_wifi_sysfs(tmp, driver_module, iface_present=True, module_loaded=None):
    """A fake /sys/class/net with wlan0, whose device/driver/module links to <tmp>/sys-module/<name>
    (the fake /sys/module: sprdwl_ng on the Orange Pi Zero 2W) -- or no module link (a built-in
    driver). iface_present=False: the module is unloaded, so wlan0 and the module dir are gone
    (kept aside as .wlan0-unloaded / .<name>-unloaded, which the modprobe stub brings back on a
    load). module_loaded (default: iface_present) = False keeps the module dir aside; True keeps it
    loaded without wlan0 (a load that never brought wlan0 back)."""
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
    if module_loaded is None:
        module_loaded = iface_present
    if not iface_present:
        os.rename(os.path.join(net, "wlan0"), os.path.join(net, ".wlan0-unloaded"))
    if not module_loaded:
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


# ---------------------------------------------------------------------------------------------
# the provision harness: the real scripts/bkshading-provision-sbc.sh run into a temp root, with a
# fake systemctl / findmnt / mount, a fake ELF relay binary, the board's netplan files and a fake
# /sys/class/net (shared by the provisioning and the WiFi takeover tests)
# ---------------------------------------------------------------------------------------------
SCRIPT = os.path.join(REPO, "scripts", "bkshading-provision-sbc.sh")
UNIT_NAME = "bkshading-relay.service"


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


def _fake_systemctl(record_path, fail_on=None, disabled_units=()):
    d = tempfile.mkdtemp()
    p = os.path.join(d, "systemctl")
    fail = ('if [ "$1" = "%s" ]; then exit 1; fi\n' % fail_on) if fail_on else ""
    # issue 808 WiFi rows: a unit listed here answers is-enabled with "disabled".
    disabled = "".join(
        'if [ "$1" = "is-enabled" ] && [ "$2" = "%s" ]; then echo disabled; exit 1; fi\n' % u
        for u in disabled_units
    )
    # issue 808 --check masked rows: a unit this fake masked earlier answers like a real masked
    # unit ("masked", exit 1)
    masked = ('if [ "$1" = "is-enabled" ] && grep -qxF "mask $2" "%s"; then echo masked; exit 1; fi\n'
              % record_path)
    with open(p, "w", encoding="utf-8") as f:
        f.write(
            "#!/usr/bin/env bash\n"
            'printf "%%s\\n" "$*" >> "%s"\n' % record_path
            + disabled
            + masked
            + 'if [ "$1" = "is-enabled" ]; then echo enabled; fi\n'
            + fail
        )
    os.chmod(p, 0o755)
    return p


# ---------------------------------------------------------------------------------------------
# issue 808 WiFi roam + heal fixtures (design 5972548198): the board's netplan files (the board-tool
# stubs are in bkshading_sbc_fakes_808.py)
# ---------------------------------------------------------------------------------------------
WIFI_SSID = "newlevel.media"
# A passphrase with every awkward character for a shell or a YAML reader: a double quote, a dollar,
# a space, a backslash and a single quote. It must survive the migration bit-exact and never leak.
WIFI_PASS = "p\"a$s s\\w0rd'x"
NETPLAN_ETH = (
    "network:\n  version: 2\n  renderer: networkd\n  ethernets:\n    all-eth-interfaces:\n"
    '      match:\n        name: "e*"\n      dhcp4: yes\n      dhcp6: yes\n'
)
NETPLAN_USB0 = "network:\n  version: 2\n  ethernets:\n    usb0:\n      addresses: [10.55.0.2/24]\n"
WIFI_YAML_NAME = "30-wifis-dhcp.yaml"


def _wifi_yaml(ssid=WIFI_SSID, password=WIFI_PASS, country="SK"):
    """The Armbian preset shape (live handheld-1). JSON strings are valid YAML double-quoted scalars,
    so the awkward passphrase is encoded the way a careful writer encodes it."""
    return (
        "network:\n  version: 2\n  renderer: networkd\n  wifis:\n    wlan0:\n"
        "      dhcp4: yes\n      dhcp6: yes\n      regulatory-domain: %s\n"
        "      access-points:\n        %s:\n          password: %s\n"
        % (country, json.dumps(ssid), json.dumps(password))
    )


def _armbian_netplan():
    return {"10-dhcp-all-interfaces.yaml": NETPLAN_ETH, WIFI_YAML_NAME: _wifi_yaml(),
            "40-usb0.yaml": NETPLAN_USB0}


def _make_net_sysfs(base, ifaces):
    """Build a fake /sys/class/net tree. `ifaces` maps iface name -> either an operstate string, or a
    dict {"operstate": <str>, "carrier": <str>} to also write a `carrier` file. Returns the root path
    (injected via BKSHADING_SBC_NET_SYSFS) so the WiFi-link check reads a controlled tree instead of
    the CI runner's real interfaces."""
    root = os.path.join(base, "net-sysfs")
    os.makedirs(root, exist_ok=True)
    for name, state in ifaces.items():
        d = os.path.join(root, name)
        os.makedirs(d, exist_ok=True)
        if isinstance(state, dict):
            operstate = state["operstate"]
            carrier = state.get("carrier")
        else:
            operstate, carrier = state, None
        with open(os.path.join(d, "operstate"), "w") as f:
            f.write(operstate + "\n")
        if carrier is not None:
            with open(os.path.join(d, "carrier"), "w") as f:
                f.write(carrier + "\n")
    return root


ROOT_UUID = "7c1e2c5a-0000-4b6c-9a1d-123456789abc"
# The live handheld-1 shape (Armbian community trixie, one partition mmcblk0p1): an rw root + a /tmp
# tmpfs. A Raspberry Pi OS board would add its own /boot/firmware line, which the ro fstab keeps.
ARMBIAN_FSTAB = (
    "UUID=%s / ext4 defaults,noatime,commit=120,errors=remount-ro 0 1\n"
    "tmpfs /tmp tmpfs defaults,nosuid 0 0\n" % ROOT_UUID
)
RW_OPTS = "rw,noatime,commit=120,errors=remount-ro"
RO_OPTS = "ro,noatime,commit=120,errors=remount-ro"


def _fake_findmnt(dirpath):
    """A findmnt answering the three root reads the provision makes, from env (FAKE_ROOT_*)."""
    p = os.path.join(dirpath, "findmnt")
    with open(p, "w", encoding="utf-8") as f:
        f.write(
            "#!/usr/bin/env bash\n"
            'case "$*" in\n'
            '  *OPTIONS*) printf "%s\\n" "${FAKE_ROOT_OPTS-}" ;;\n'
            '  *UUID*) printf "%s\\n" "${FAKE_ROOT_UUID-}" ;;\n'
            '  *FSTYPE*) printf "%s\\n" "${FAKE_ROOT_FSTYPE-}" ;;\n'
            "esac\n"
        )
    os.chmod(p, 0o755)
    return p


def _fake_mount(dirpath, record_path):
    p = os.path.join(dirpath, "mount")
    with open(p, "w", encoding="utf-8") as f:
        f.write('#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s"\n' % record_path)
    os.chmod(p, 0o755)
    return p


def _sbc_paths(root):
    return {
        "fstab": os.path.join(root, "etc", "fstab"),
        "journald": os.path.join(root, "etc", "systemd", "journald.conf.d"),
        "mount_log": os.path.join(root, "mount-calls.log"),
        "cambox_marker": os.path.join(root, "usr", "local", "bin", "camera-box"),
        "netplan": os.path.join(root, "etc", "netplan"),
        "wpa_conf": os.path.join(root, "etc", "wpa_supplicant", "wpa_supplicant-wlan0.conf"),
        "networkd": os.path.join(root, "etc", "systemd", "network"),
        "heal_dir": os.path.join(root, "usr", "local", "lib", "bkshading"),
        "unit_dir": os.path.join(root, "systemd-system"),
        "tool_log": os.path.join(root, "tool-calls.log"),
        "net_state": os.path.join(root, "fake-net.json"),
    }


def _run_provision(mode, root, bin_machine=AARCH64, make_bin=True, net_ifaces=None,
                   root_opts=None, root_uuid=ROOT_UUID, fstab_text=ARMBIAN_FSTAB,
                   persistent_dropin=True, cambox=False, systemctl_fail_on=None,
                   netplan_files=None, net_state=None, disabled_units=(), netplan_other=None,
                   env_extra=None):
    sysd = os.path.join(root, "systemd-system")
    binp = os.path.join(root, "bin", "bkshading-relay")
    calls = os.path.join(root, "systemctl-calls.log")
    sc = _fake_systemctl(calls, fail_on=systemctl_fail_on, disabled_units=disabled_units)
    paths = _sbc_paths(root)
    # issue 808 WiFi: the board's own netplan files, seeded once (a re-run keeps whatever an
    # earlier --install moved aside); default = the live handheld-1 set incl. the WiFi preset.
    if not os.path.isdir(paths["netplan"]):
        os.makedirs(paths["netplan"])
        for name, text in (_armbian_netplan() if netplan_files is None else netplan_files).items():
            with open(os.path.join(paths["netplan"], name), "w", encoding="utf-8") as f:
                f.write(text)
    with open(paths["net_state"], "w", encoding="utf-8") as f:
        json.dump(_net_state() if net_state is None else net_state, f)
    stubs = _write_tool_stubs(os.path.join(root, "fake-board-tools"))
    # netplan also reads /run/netplan and /lib/netplan: point those at temp dirs (never the test
    # machine's own netplan), optionally seeded with YAML files.
    other_dirs = [os.path.join(root, "run-netplan"), os.path.join(root, "lib-netplan")]
    for d, files in zip(other_dirs, netplan_other or ({}, {})):
        if files and not os.path.isdir(d):
            os.makedirs(d)
            for name, text in files.items():
                with open(os.path.join(d, name), "w", encoding="utf-8") as f:
                    f.write(text)
    if make_bin:
        os.makedirs(os.path.dirname(binp), exist_ok=True)
        _fake_elf(binp, bin_machine)
    if net_ifaces is None:
        # default: a joined wireless handheld (wl* operstate=up) so --check is deterministic and
        # green regardless of the CI runner's real interfaces.
        net_ifaces = {"wlan0": "up", "eth0": "up"}
    net_root = _make_net_sysfs(root, net_ifaces)
    if root_opts is None:
        # an --install normally runs on the still-rw board; a --check after the reboot sees ro.
        root_opts = RW_OPTS if mode == "--install" else RO_OPTS
    # Seed the board's own files once (a re-run keeps whatever the first --install wrote).
    if not os.path.exists(paths["fstab"]):
        os.makedirs(os.path.dirname(paths["fstab"]), exist_ok=True)
        with open(paths["fstab"], "w", encoding="utf-8") as f:
            f.write(fstab_text)
    os.makedirs(paths["journald"], exist_ok=True)
    if persistent_dropin and not os.path.exists(os.path.join(root, ".seeded-journald")):
        with open(os.path.join(paths["journald"], "10-persistent.conf"), "w") as f:
            f.write("[Journal]\nStorage=persistent\n")
        open(os.path.join(root, ".seeded-journald"), "w").close()
    if cambox:
        os.makedirs(os.path.dirname(paths["cambox_marker"]), exist_ok=True)
        _fake_elf(paths["cambox_marker"], X86_64)
    tools = os.path.join(root, "fake-tools")
    os.makedirs(tools, exist_ok=True)
    env = dict(
        os.environ,
        BKSHADING_SBC_UNIT_DEST=os.path.join(sysd, UNIT_NAME),
        BKSHADING_SBC_BIN=binp,
        BKSHADING_SBC_GPHOTO2="true",  # exists -> command -v succeeds, apt skipped
        BKSHADING_SBC_SYSTEMCTL=sc,
        BKSHADING_SBC_NET_SYSFS=net_root,
        BKSHADING_SBC_FSTAB=paths["fstab"],
        BKSHADING_SBC_JOURNALD_DIR=paths["journald"],
        BKSHADING_SBC_FINDMNT=_fake_findmnt(tools),
        BKSHADING_SBC_MOUNT=_fake_mount(tools, paths["mount_log"]),
        BKSHADING_SBC_CAMBOX_MARKER=paths["cambox_marker"],
        BKSHADING_SBC_NETPLAN_DIR=paths["netplan"],
        BKSHADING_SBC_WPA_CONF_DIR=os.path.dirname(paths["wpa_conf"]),
        BKSHADING_SBC_NETWORKD_DIR=paths["networkd"],
        BKSHADING_SBC_HEAL_DIR=paths["heal_dir"],
        BKSHADING_SBC_PYTHON=sys.executable,
        BKSHADING_SBC_WPA_PASSPHRASE=stubs["wpa_passphrase"],
        BKSHADING_SBC_WPA_CLI=stubs["wpa_cli"],
        BKSHADING_SBC_IP=stubs["ip"],
        BKSHADING_SBC_PING=stubs["ping"],
        FAKE_TOOL_LOG=paths["tool_log"],
        FAKE_NET_STATE=paths["net_state"],
        BKSHADING_SBC_NETPLAN_OTHER_DIRS=" ".join(other_dirs),
        FAKE_ROOT_OPTS=root_opts,
        FAKE_ROOT_UUID=root_uuid,
        FAKE_ROOT_FSTYPE="ext4",
    )
    env.update(env_extra or {})
    r = subprocess.run(["bash", SCRIPT, mode], capture_output=True, text=True, env=env)
    return r, calls, binp
