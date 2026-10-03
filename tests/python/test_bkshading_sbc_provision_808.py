#!/usr/bin/env python3
"""bkshading SBC / handheld provisioning — the LAST milestone (issue 808).

The owner architecture (comment 5356048130 path 2, "cieľový stav"; Design v3 comment 5664682477 +
ROZHODNUTÉ 5664746806, 14.9.2026) puts a handheld camera on a separately powered zero-class arm64
SBC with WiFi — the board is device-agnostic (Raspberry Pi Zero 2 W, Radxa ZERO 3W, or Orange Pi Zero 2W — the
ordered prototype), powered from
the camera cage's V-mount 5 V USB splitter (never a power bank / PiSugar / raw 15 V D-tap). The
camera plugs USB into the SBC's host port (PTP), which runs the SAME `bkshading-relay` component the
camboxes run — a "mini-cambox without video". The strih aggregation
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
  - `scripts/bkshading-deploy-relay.sh` gains `--arch amd64|arm64` (selects the artifact), so the
    arm64 relay has a real deploy path.

Slice B of the handheld milestone (owner ruling 5948648089, main design 5971558113): the SBC root goes
READ-ONLY, the same as the camboxes. `--install` writes the read-only fstab from the ONE shared lib
`scripts/lib/ro-root.sh` (the cambox tmpfs set, the board's own other mounts kept), makes journald
volatile, masks armbian-ramlog and systemd-networkd-persistent-storage, and remounts an already-ro root rw for its own writes and back.
`--check` grades the root mode (ro = OK). The deploy reads the target's own root mode instead of a
flag: an ro root gets the remount cycle, an rw root none, an unreadable one refuses.

These stdlib-only + pyyaml structural/behavioural tests run in the `python-tests` CI job (no Rust
toolchain, no root, no apt, no real systemd — the impure ops are overridden to fakes into a temp
root). Runnable directly (`python3 tests/python/test_bkshading_sbc_provision_808.py`) or under pytest.
"""
import hashlib
import json
import os
import re
import shutil
import stat
import struct
import subprocess
import sys
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
# WiFi-link state classifier (pure; --check reads it) — the handheld is wireless, a cambox is wired
# ---------------------------------------------------------------------------------------------
def _wifi_state(root, glob="wl*"):
    src = '. "%s"\nbkshading_sbc_wifi_link_state "$A1" "$A2"' % LIB
    env = dict(os.environ, A1=root, A2=glob)
    r = subprocess.run(["bash", "-c", src], capture_output=True, text=True, env=env)
    return r.returncode, r.stdout.strip()


def test_wifi_link_state_up_down_none():
    with tempfile.TemporaryDirectory() as tmp:
        # a joined wireless box -> up
        up = _make_net_sysfs(os.path.join(tmp, "u"), {"wlan0": "up", "eth0": "up"})
        assert _wifi_state(up) == (0, "up")
        # a wireless box present but not associated -> down (any non-"up" operstate)
        down = _make_net_sysfs(os.path.join(tmp, "d"), {"wlan0": "down"})
        assert _wifi_state(down) == (0, "down")
        dorm = _make_net_sysfs(os.path.join(tmp, "dm"), {"wlp2s0": "dormant"})
        assert _wifi_state(dorm) == (0, "down")
        # NO wireless interface at all (a wired cambox) -> none (the --check SKIP signal), never error
        none = _make_net_sysfs(os.path.join(tmp, "n"), {"eth0": "up", "lo": "unknown"})
        assert _wifi_state(none) == (0, "none")
        # a missing sysfs root -> none, never an error
        assert _wifi_state(os.path.join(tmp, "nope")) == (0, "none")
        # two wireless ifaces, one up -> up
        two = _make_net_sysfs(os.path.join(tmp, "t"), {"wlan0": "down", "wlan1": "up"})
        assert _wifi_state(two) == (0, "up")
        # an associated interface whose driver leaves operstate "unknown" but carrier=1 (the
        # out-of-tree uwe5622 on the Orange Pi Zero 2W) counts as UP, never a false FAIL.
        unk = _make_net_sysfs(os.path.join(tmp, "u2"),
                              {"wlan0": {"operstate": "unknown", "carrier": "1"}})
        assert _wifi_state(unk) == (0, "up")
        # a genuinely-down link: operstate down AND carrier 0 -> down
        dn = _make_net_sysfs(os.path.join(tmp, "d2"),
                             {"wlan0": {"operstate": "down", "carrier": "0"}})
        assert _wifi_state(dn) == (0, "down")


def _first_wifi_iface(root, glob="wl*"):
    src = '. "%s"\nbkshading_sbc_first_wifi_iface "$A1" "$A2"' % LIB
    env = dict(os.environ, A1=root, A2=glob)
    r = subprocess.run(["bash", "-c", src], capture_output=True, text=True, env=env)
    return r.returncode, r.stdout.strip()


def test_first_wifi_iface_names_the_real_interface():
    with tempfile.TemporaryDirectory() as tmp:
        # a non-standard name (wlp2s0) is what the remediation should print, not a hard-coded wlan0
        r = _make_net_sysfs(os.path.join(tmp, "a"), {"eth0": "up", "wlp2s0": "down"})
        assert _first_wifi_iface(r) == (0, "wlp2s0")
        # no wireless iface -> empty, never an error
        r2 = _make_net_sysfs(os.path.join(tmp, "b"), {"eth0": "up"})
        assert _first_wifi_iface(r2) == (0, "")
        # missing tree -> empty, never an error
        assert _first_wifi_iface(os.path.join(tmp, "nope")) == (0, "")


def test_wifi_ssid_from_iw_parser():
    iw = (
        "Connected to aa:bb:cc:dd:ee:ff (on wlan0)\n"
        "\tSSID: rig-5g\n"
        "\tfreq: 5180\n"
    )
    assert _bash_arg(LIB, "bkshading_sbc_wifi_ssid_from_iw", iw)[1] == "rig-5g"
    # no SSID line -> empty, never an error
    assert _bash_arg(LIB, "bkshading_sbc_wifi_ssid_from_iw", "not connected")[1] == ""
    assert _bash_arg(LIB, "bkshading_sbc_wifi_ssid_from_iw", "")[1] == ""


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


def _fake_systemctl(record_path, fail_on=None, disabled_units=()):
    d = tempfile.mkdtemp()
    p = os.path.join(d, "systemctl")
    fail = ('if [ "$1" = "%s" ]; then exit 1; fi\n' % fail_on) if fail_on else ""
    # issue 808 WiFi rows: a unit listed here answers is-enabled with "disabled".
    disabled = "".join(
        'if [ "$1" = "is-enabled" ] && [ "$2" = "%s" ]; then echo disabled; exit 1; fi\n' % u
        for u in disabled_units
    )
    with open(p, "w", encoding="utf-8") as f:
        f.write(
            "#!/usr/bin/env bash\n"
            'printf "%%s\\n" "$*" >> "%s"\n' % record_path
            + disabled
            + 'if [ "$1" = "is-enabled" ]; then echo enabled; fi\n'
            + fail
        )
    os.chmod(p, 0o755)
    return p


# ---------------------------------------------------------------------------------------------
# issue 808 WiFi roam + heal fixtures (design 5972548198): the board's netplan files, the tool stubs
# ---------------------------------------------------------------------------------------------
WIFI_SSID = "newlevel.media"
# A passphrase with every awkward character for a shell or a YAML reader: a double quote, a dollar,
# a space, a backslash and a single quote. It must survive the migration bit-exact and never leak.
WIFI_PASS = "p\"a$s s\\w0rd'x"
WIFI_GW = "10.77.8.1"
GOOD_BSSID = "92:0d:ab:03:67:07"
FAR_BSSID = "aa:0d:ab:03:6f:af"
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


def _expected_psk(ssid=WIFI_SSID, password=WIFI_PASS):
    # IEEE 802.11i PSK = PBKDF2-HMAC-SHA1(passphrase, ssid, 4096, 32): exactly what wpa_passphrase
    # computes. Independent of the tool under test, so a wrong derivation cannot pass.
    return hashlib.pbkdf2_hmac("sha1", password.encode(), ssid.encode(), 4096, 32).hex()


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
    # The WiFi driver module behind the fake /sys/class/net/wlan0: unloading it removes wlan0 (kept
    # aside as .wlan0-unloaded with its device/driver/module links); loading it brings wlan0 back
    # `iface_delay_s` later, from a detached process, as a real driver's probe does. `modprobe_rc`
    # fails every call, `modprobe_load_rc` only the load.
    net = os.environ["BKSHADING_WIFI_HEAL_SYSFS_NET"]
    live, gone = os.path.join(net, "wlan0"), os.path.join(net, ".wlan0-unloaded")
    if st.get("modprobe_rc"):
        sys.exit(int(st["modprobe_rc"]))
    if "-r" in args:
        if os.path.isdir(live):
            os.rename(live, gone)
        if st.get("term_heal_on_unload") and not st.get("term_sent"):
            # the heal pass is ended right here, between the unload and the load (systemd's
            # SIGTERM at TimeoutStartSec): modprobe's parent is `timeout`, whose parent is the heal
            st["term_sent"] = True
            save()
            with open("/proc/%d/stat" % os.getppid()) as f:
                heal_pid = int(f.read().rsplit(")", 1)[1].split()[1])
            os.kill(heal_pid, 15)
        sys.exit(0)
    if st.get("modprobe_load_rc"):
        sys.exit(int(st["modprobe_load_rc"]))
    st.update(st.get("on_reload", {}))
    save()
    if os.path.isdir(gone):
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
        sys.exit(0)
    if args[:1] == ["start"]:
        net = os.environ.get("BKSHADING_WIFI_HEAL_SYSFS_NET")
        if net and not os.path.isdir(os.path.join(net, "wlan0")):
            # the unit Requires= the wlan0 device: no interface, no supplicant
            st["start_without_iface"] = st.get("start_without_iface", 0) + 1
            save()
            sys.exit(1)
        st["wpa_active"] = True
        st.update(st.get("on_start", {}))
        save()
        sys.exit(int(st.get("start_rc", 0)))
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


def test_check_skips_wifi_on_wired_box():
    # A wired box (a cambox — the reused relay unit runs there too) has no wl* interface; --check
    # must SKIP the WiFi item, never FAIL it. Provision fully with a wired-only sysfs tree.
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root, net_ifaces={"eth0": "up"})
        assert r.returncode == 0, (r.stdout, r.stderr)
        r2, _c2, _b2 = _run_provision("--check", root, net_ifaces={"eth0": "up"})
        assert r2.returncode == 0, (r2.stdout, r2.stderr)
        assert re.search(r"skip", r2.stdout + r2.stderr, re.I), \
            "a wired box (no wl*) must report the WiFi check SKIPPED, not FAILED"
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_check_fails_when_wifi_down():
    # A wireless handheld whose WiFi is not up must FAIL --check with a join remediation — the whole
    # topology depends on the link. Everything else provisioned OK, only the link is down.
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root, net_ifaces={"wlan0": "down"})
        assert r.returncode == 0, (r.stdout, r.stderr)  # install does not gate on the live link
        r2, _c2, _b2 = _run_provision("--check", root, net_ifaces={"wlan0": "down"})
        assert r2.returncode != 0, "a down WiFi link must fail --check"
        out = r2.stdout + r2.stderr
        assert re.search(r"wifi|wl|link", out, re.I), "the failure must name the WiFi link"
        assert re.search(r"nmcli|ssid|join", out, re.I), \
            "the failure must carry a join remediation"
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_unknown_arg_exits_2():
    r = subprocess.run(["bash", SCRIPT, "--bogus"], capture_output=True, text=True)
    assert r.returncode == 2, r.returncode


# ---------------------------------------------------------------------------------------------
# slice B: the read-only root, the same canon as the camboxes (ROZHODNUTÉ 5948648089)
# ---------------------------------------------------------------------------------------------
RO_LIB = os.path.join(REPO, "scripts", "lib", "ro-root.sh")


def _read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


def _lib_fstab_text(original):
    src = '. "%s"\nro_root_fstab_text "$U" ext4 "$ORIG"' % RO_LIB
    r = subprocess.run(["bash", "-c", src], capture_output=True, text=True,
                       env=dict(os.environ, U=ROOT_UUID, ORIG=original), check=True)
    return r.stdout


def test_install_writes_the_ro_fstab_from_the_shared_lib():
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        paths = _sbc_paths(root)
        written = _read(paths["fstab"])
        assert written == _lib_fstab_text(ARMBIAN_FSTAB), "the fstab must be the lib's text:\n" + written
        assert "UUID=%s / ext4 ro 0 1\n" % ROOT_UUID in written
        assert "tmpfs /var/cache tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=512M 0 0" in written
        # the pristine original is kept once, like the cambox's fstab.bak
        assert _read(paths["fstab"] + ".bak") == ARMBIAN_FSTAB
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_rerun_keeps_the_original_backup_and_the_same_fstab():
    root = tempfile.mkdtemp()
    try:
        r1, _c, _b = _run_provision("--install", root)
        assert r1.returncode == 0, (r1.stdout, r1.stderr)
        first = _read(_sbc_paths(root)["fstab"])
        r2, _c2, _b2 = _run_provision("--install", root, root_opts=RO_OPTS)
        assert r2.returncode == 0, (r2.stdout, r2.stderr)
        assert _read(_sbc_paths(root)["fstab"]) == first, "a re-run writes the same fstab"
        assert _read(_sbc_paths(root)["fstab"] + ".bak") == ARMBIAN_FSTAB, \
            "a re-run must never overwrite the original backup with the rewritten fstab"
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_keeps_a_boards_own_mount():
    pios = (
        "PARTUUID=1234abcd-01  /boot/firmware  vfat    defaults          0       2\n"
        "PARTUUID=1234abcd-02  /               ext4    defaults,noatime  0       1\n"
    )
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root, fstab_text=pios)
        assert r.returncode == 0, (r.stdout, r.stderr)
        written = _read(_sbc_paths(root)["fstab"])
        assert "PARTUUID=1234abcd-01  /boot/firmware  vfat    defaults          0       2\n" in written
        assert "PARTUUID=1234abcd-02" not in written
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_makes_journald_volatile():
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        jd = _sbc_paths(root)["journald"]
        assert not os.path.exists(os.path.join(jd, "10-persistent.conf")), \
            "the debug Storage=persistent drop-in must be removed"
        dropins = sorted(os.listdir(jd))
        assert dropins, "a volatile journald drop-in must be written"
        body = "".join(_read(os.path.join(jd, d)) for d in dropins)
        assert "[Journal]" in body and "Storage=volatile" in body, body
        assert "Storage=persistent" not in body
        # it sorts after any other drop-in, so nothing overrides it
        assert dropins[-1].startswith("99-"), dropins
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_masks_armbian_ramlog_and_stays_enable_only():
    root = tempfile.mkdtemp()
    try:
        r, calls, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        log = _read(calls)
        assert "mask armbian-ramlog.service" in log, log
        assert "start" not in log and "restart" not in log, "enable-only:\n" + log
        assert not os.path.exists(_sbc_paths(root)["mount_log"]), \
            "an rw root is never remounted (the ro fstab takes effect at reboot)"
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_masks_networkd_persistent_storage_on_the_ro_root():
    # Live on handheld-1 (3.10.2026, the first boot on the read-only root): the Debian trixie
    # systemd-networkd-persistent-storage.service (`networkctl persistent-storage yes`) failed with
    # io.systemd.Network.StorageReadOnly and left the board `degraded`. networkd keeps its state in
    # /run on a read-only root, so --install masks the unit like armbian-ramlog.
    root = tempfile.mkdtemp()
    try:
        r, calls, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        log = _read(calls)
        assert "mask systemd-networkd-persistent-storage.service" in log, log
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_masks_the_fake_hwclock_save_service_and_its_timer():
    # Live on handheld-1 (3.10.2026, the second boot on the read-only root): Armbian's hourly
    # fake-hwclock-save.timer ran `fake-hwclock save`, which cannot write /etc/fake-hwclock.data on a
    # read-only root, and left the board `degraded`. The boot-time load (fake-hwclock-load) only
    # READS the file and stays; NTP sets the real time right after boot.
    root = tempfile.mkdtemp()
    try:
        r, calls, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        log = _read(calls)
        assert "mask fake-hwclock-save.service" in log, log
        assert "mask fake-hwclock-save.timer" in log, log
        assert "mask fake-hwclock-load.service" not in log, "the boot-time load only reads"
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_on_a_ro_root_remounts_rw_then_back_ro():
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root, root_opts=RO_OPTS)
        assert r.returncode == 0, (r.stdout, r.stderr)
        log = _read(_sbc_paths(root)["mount_log"]).splitlines()
        assert log == ["-o remount,rw /", "-o remount,ro /"], log
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_failure_on_a_ro_root_still_puts_it_back_read_only():
    # A re-run on the read-only root remounts rw for its own writes. When a step fails midway
    # (here `systemctl enable`), the EXIT trap must still remount ro -- never a root left rw.
    for fail_on in ("enable", "mask"):
        root = tempfile.mkdtemp()
        try:
            r, calls, _b = _run_provision("--install", root, root_opts=RO_OPTS,
                                          systemctl_fail_on=fail_on)
            assert r.returncode != 0, (fail_on, r.stdout, r.stderr)
            log = _read(_sbc_paths(root)["mount_log"]).splitlines()
            assert log == ["-o remount,rw /", "-o remount,ro /"], (fail_on, log)
            # the D-Bus-activated writers are stopped before the ro remount (the EBUSY class)
            assert "stop packagekit unattended-upgrades" in _read(calls), _read(calls)
        finally:
            shutil.rmtree(root, ignore_errors=True)


def test_install_refuses_an_unreadable_root():
    for opts, uuid in (("", ROOT_UUID), (RW_OPTS, "")):
        root = tempfile.mkdtemp()
        try:
            r, calls, _b = _run_provision("--install", root, root_opts=opts, root_uuid=uuid)
            assert r.returncode == 1, (opts, uuid, r.returncode, r.stdout, r.stderr)
            assert _read(_sbc_paths(root)["fstab"]) == ARMBIAN_FSTAB, "fstab untouched"
            assert not os.path.exists(_sbc_paths(root)["fstab"] + ".bak")
        finally:
            shutil.rmtree(root, ignore_errors=True)


def test_install_refuses_on_a_cambox():
    root = tempfile.mkdtemp()
    try:
        r, calls, _b = _run_provision("--install", root, cambox=True)
        assert r.returncode == 2, (r.returncode, r.stdout, r.stderr)
        assert "setup-device.sh" in r.stderr, r.stderr
        assert _read(_sbc_paths(root)["fstab"]) == ARMBIAN_FSTAB, "a cambox fstab is never rewritten"
        assert not os.path.exists(calls), "nothing is enabled or masked on a cambox"
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_check_grades_the_root_mode():
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        ok, _c, _b = _run_provision("--check", root, root_opts=RO_OPTS)
        assert ok.returncode == 0, (ok.stdout, ok.stderr)
        assert re.search(r"OK: root filesystem is read-only", ok.stdout), ok.stdout
        rw, _c, _b = _run_provision("--check", root, root_opts=RW_OPTS)
        assert rw.returncode == 1, (rw.stdout, rw.stderr)
        assert re.search(r"FAIL: root filesystem is read-WRITE.*reboot after --install", rw.stderr), rw.stderr
        unk, _c, _b = _run_provision("--check", root, root_opts="")
        assert unk.returncode == 1, (unk.stdout, unk.stderr)
        assert re.search(r"FAIL: .*root", unk.stderr), unk.stderr
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_provision_sources_the_shared_ro_root_lib():
    assert '. "$HERE/lib/ro-root.sh"' in _read(SCRIPT)


# ---------------------------------------------------------------------------------------------
# issue 808 WiFi roam + heal (design 5972548198): the handheld sat on a dead far AP after a reboot
# with wpa_state=COMPLETED and a DHCP lease; netplan has no bgscan and nothing checked traffic.
# ---------------------------------------------------------------------------------------------
HEAL_SCRIPT = os.path.join(REPO, "scripts", "bkshading-wifi-heal.sh")
NETPLAN_READER = os.path.join(REPO, "scripts", "bkshading_sbc_netplan_wifi.py")
HEAL_SERVICE = os.path.join(REPO, "systemd", "bkshading-wifi-heal.service")
HEAL_TIMER = os.path.join(REPO, "systemd", "bkshading-wifi-heal.timer")
BGSCAN_LINE = 'bgscan="simple:30:-65:300"'


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


def test_wifi_constants_pin_the_design():
    assert _lib_call("bkshading_sbc_wifi_iface").strip() == "wlan0"
    assert _lib_call("bkshading_sbc_wpa_unit").strip() == "wpa_supplicant@wlan0.service"
    assert _lib_call("bkshading_sbc_wpa_conf_name").strip() == "wpa_supplicant-wlan0.conf"
    assert _lib_call("bkshading_sbc_wpa_ctrl_dir").strip() == "/run/wpa_supplicant"
    assert _lib_call("bkshading_sbc_wpa_key_mgmt").strip() == "WPA-PSK WPA-PSK-SHA256"
    # main ROZHODNUTÉ 5973115530: no SAE -- the conf carries the hex PSK, which WPA3 cannot use,
    # and newlevel.media is a WPA2/WPA3 transition network where listing SAE can keep the board off.
    assert "SAE" not in _lib_call("bkshading_sbc_wpa_key_mgmt")
    assert _lib_call("bkshading_sbc_wpa_ieee80211w").strip() == "1"
    assert _lib_call("bkshading_sbc_bgscan_line").strip() == BGSCAN_LINE
    assert _lib_call("bkshading_sbc_wifi_heal_miss_limit").strip() == "3"
    assert _lib_call("bkshading_sbc_wifi_heal_interval_s").strip() == "20"


def test_wpa_conf_text_shape():
    psk = "ab" * 32
    out = _lib_call('bkshading_sbc_wpa_conf_text SK "$S" "$P"', env={"S": WIFI_SSID, "P": psk})
    lines = [ln.strip() for ln in out.splitlines()]
    assert "ctrl_interface=/run/wpa_supplicant" in lines
    assert "country=SK" in lines
    block = lines[lines.index("network={"):]
    assert block[:7] == ["network={", 'ssid="newlevel.media"', "key_mgmt=WPA-PSK WPA-PSK-SHA256",
                         "ieee80211w=1", BGSCAN_LINE, "psk=" + psk, "}"], block
    assert not any(ln.startswith("#psk") for ln in lines)
    # no regulatory domain in the YAML -> no country line, never a guessed one
    none = _lib_call('bkshading_sbc_wpa_conf_text "" "$S" "$P"', env={"S": WIFI_SSID, "P": psk})
    assert "country=" not in none
    # two access points -> two network blocks
    two = _lib_call('bkshading_sbc_wpa_conf_text SK a1a1a1a1 "$P" b2b2b2b2 "$P"', env={"P": psk})
    assert two.count("network={") == 2 and '\tssid="a1a1a1a1"' in two and '\tssid="b2b2b2b2"' in two


def test_wpa_ssid_value_quotes_printable_ascii_and_hexes_the_rest():
    def ssid(s):
        return _lib_call('bkshading_sbc_wpa_ssid_value "$S"', env={"S": s}).strip()
    assert ssid("newlevel.media") == '"newlevel.media"'
    assert ssid("rig 5G") == '"rig 5G"'
    # a double quote, a non-ASCII byte -> the hex form wpa_supplicant also reads
    assert ssid('a"b') == "612262"
    assert ssid("Café") == "436166c3a9"
    # a long run of one byte stays whole (od -v: never a `*` repeat line)
    assert ssid("é" * 12) == "c3a9" * 12


def test_psk_extractor_takes_the_hex_never_the_plaintext_comment():
    out = 'network={\n\tssid="x"\n\t#psk="%s"\n\tpsk=%s\n}\n' % (WIFI_PASS, "AB" * 32)
    assert _lib_call('bkshading_sbc_psk_hex_from_wpa_passphrase "$O"', env={"O": out}).strip() \
        == "ab" * 32
    # the error text wpa_passphrase prints for a bad length -> nothing, never a guess
    assert _lib_call('bkshading_sbc_psk_hex_from_wpa_passphrase "$O"',
                     env={"O": "Passphrase must be 8..63 characters\n"}).strip() == ""
    assert _lib_call('bkshading_sbc_psk_hex_from_wpa_passphrase ""').strip() == ""


def test_default_gateway_parser():
    def gw(text):
        return _lib_call('bkshading_sbc_default_gw_from_route "$T"', env={"T": text}).strip()
    assert gw("default via 10.77.8.1 dev wlan0 proto dhcp src 10.77.9.165 metric 600\n") \
        == "10.77.8.1"
    assert gw("default via 192.168.1.254 dev wlan0 proto dhcp metric 600\n"
              "default via 10.0.0.1 dev wlan0 metric 700\n") == "192.168.1.254"
    assert gw("default dev wlan0 scope link\n") == ""
    assert gw("") == ""


def test_wpa_field_parser():
    status = "bssid=92:0d:ab:03:67:07\nwpa_state_hint=X\nwpa_state=COMPLETED\nssid=newlevel.media\n"
    def field(text, key):
        return _lib_call('bkshading_sbc_wpa_field "$T" "$K"', env={"T": text, "K": key}).strip()
    assert field(status, "bssid") == GOOD_BSSID
    assert field(status, "wpa_state") == "COMPLETED"  # exact key, never the wpa_state_hint line
    assert field("RSSI=-73\r\nLINKSPEED=65\r\n", "RSSI") == "-73"
    assert field(status, "freq") == ""
    assert field("", "bssid") == ""


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
    # known), $2 modprobe present, $3 wlan0 present.
    table = [
        ("sprdwl_ng", "yes", "yes", "reload"),       # loaded: unload + load
        ("sprdwl_ng", "yes", "no", "load"),          # wlan0 gone = the module is not loaded
        ("sprdwl_ng", "no", "yes", "no-modprobe"),
        ("sprdwl_ng", "no", "no", "no-modprobe"),
        ("", "yes", "yes", "builtin"),               # wlan0 with no module link
        ("", "no", "yes", "builtin"),
        ("", "yes", "no", "unknown"),                # wlan0 gone and no module name recorded
        ("../evil", "yes", "yes", "builtin"),        # not a module name: none
        ("a b", "yes", "no", "unknown"),
    ]
    for mod, has_modprobe, iface, want in table:
        got = _lib_call('bkshading_sbc_wifi_heal_driver_plan "$A" "$B" "$C"',
                        env={"A": mod, "B": has_modprobe, "C": iface}).strip()
        assert got == want, (mod, has_modprobe, iface, got, want)


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
    # systemd ends a pass at TimeoutStartSec with SIGTERM. Between `modprobe -r` and the start the
    # board has no driver and no supplicant: the reload's trap loads the module and starts the
    # supplicant before the pass exits.
    tmp = tempfile.mkdtemp()
    try:
        state = _net_state(wpa_state="DISCONNECTED", reachable=False, driver_failed=1,
                           term_heal_on_unload=True,
                           on_reload={"wpa_state": "COMPLETED", "reachable": True,
                                      "driver_failed": 0})
        env = _heal_env(tmp, state, extra_tools=("journalctl", "modprobe"),
                        driver_module="sprdwl_ng")
        for _ in range(2):
            assert _heal_pass(env).returncode == 0
        r = _heal_pass(env)
        assert r.returncode == 143, (r.returncode, r.stdout, r.stderr)
        assert "in the middle of the driver reload" in r.stderr, r.stderr
        assert _driver_acts(env) == ["systemctl stop wpa_supplicant@wlan0.service",
                                     "modprobe -r sprdwl_ng", "modprobe sprdwl_ng",
                                     "systemctl start wpa_supplicant@wlan0.service"], _heal_tools(env)
        assert os.path.isdir(os.path.join(env["BKSHADING_WIFI_HEAL_SYSFS_NET"], "wlan0"))
        assert _get_state(env)["wpa_active"] is True
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


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
    # a pass ended mid-reload runs the trap (load + wlan0 wait + start) inside TimeoutStopSec
    assert 2 * sysctl + iface_wait <= int(_unit_value(HEAL_SERVICE, "TimeoutStopSec"))
    # the script takes the bounds from the lib, and the reload makes exactly the calls counted above
    script = _read(HEAL_SCRIPT)
    assert 'SYSTEMCTL_TIMEOUT_S="$(bkshading_sbc_wifi_heal_systemctl_timeout_s)"' in script
    assert 'IFACE_WAIT_S="$(bkshading_sbc_wifi_heal_iface_wait_s)"' in script
    reload_case = script.split("\n  reload-driver)\n", 1)[1].split("\nesac\n", 1)[0]
    assert reload_case.count('timeout "$SYSTEMCTL_TIMEOUT_S"') == 3, "stop, modprobe -r, start"
    load_fn = script.split("\nload_driver() {\n", 1)[1].split("\n}\n", 1)[0]
    assert load_fn.count('timeout "$SYSTEMCTL_TIMEOUT_S"') == 1 and "wait_for_iface" in load_fn


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


def test_networkd_file_matches_the_settings_netplan_generated():
    text = _lib_call("bkshading_sbc_networkd_wifi_content")
    body = [ln for ln in text.splitlines() if ln and not ln.startswith("#")]
    assert body == ["[Match]", "Name=wlan0", "[Network]", "DHCP=yes", "LinkLocalAddressing=ipv6",
                    "[DHCP]", "RouteMetric=600", "UseMTU=true"], body
    assert _lib_call("bkshading_sbc_networkd_wifi_name").strip() == "05-bkshading-wlan0.network"


# --- the netplan WiFi reader (scripts/bkshading_sbc_netplan_wifi.py) ---
def _netplan_dir(files):
    d = tempfile.mkdtemp()
    for name, text in files.items():
        with open(os.path.join(d, name), "w", encoding="utf-8") as f:
            f.write(text)
    return d


def _reader(netplan_dir, iface="wlan0"):
    r = subprocess.run([sys.executable, NETPLAN_READER, netplan_dir, iface], capture_output=True)
    fields = r.stdout.split(b"\0")[:-1] if r.stdout else []
    pairs = [(fields[i].decode(), fields[i + 1].decode()) for i in range(0, len(fields), 2)]
    return r.returncode, pairs, r.stderr.decode()


def test_netplan_reader_reads_the_armbian_preset_bit_exact():
    d = _netplan_dir(_armbian_netplan())
    try:
        rc, pairs, err = _reader(d)
        assert rc == 0, err
        # dhcp = the DHCP= netplan itself generates for this YAML (dhcp4 + dhcp6 -> yes)
        assert pairs == [("file", os.path.join(d, WIFI_YAML_NAME)), ("country", "SK"),
                         ("dhcp", "yes"), ("ssid", WIFI_SSID), ("pass", WIFI_PASS)], pairs
        assert WIFI_PASS not in err
    finally:
        shutil.rmtree(d, ignore_errors=True)


def test_netplan_reader_keeps_raw_scalar_text_like_netplan():
    # unquoted scalars stay their text (never an int / an octal / a bool), as netplan reads them
    for raw in ("12345678", "0x1A2B3C4D", "012345678", "yesyesyes"):
        y = _wifi_yaml().replace(json.dumps(WIFI_PASS), raw)
        d = _netplan_dir({WIFI_YAML_NAME: y})
        try:
            rc, pairs, err = _reader(d)
            assert rc == 0, (raw, err)
            assert ("pass", raw) in pairs, (raw, pairs)
        finally:
            shutil.rmtree(d, ignore_errors=True)


def test_netplan_reader_finds_nothing_or_refuses_what_it_cannot_carry():
    d = _netplan_dir({"10-dhcp-all-interfaces.yaml": NETPLAN_ETH, "40-usb0.yaml": NETPLAN_USB0})
    try:
        assert _reader(d)[0] == 3, "no WiFi YAML -> 3 (the provision refuses or keeps its conf)"
    finally:
        shutil.rmtree(d, ignore_errors=True)
    bad = {
        "the WiFi shares its file with ethernet": NETPLAN_ETH.rstrip("\n") + "\n" + _wifi_yaml().split(
            "network:\n  version: 2\n  renderer: networkd\n", 1)[1],
        "an open network": (
            "network:\n  version: 2\n  renderer: networkd\n  wifis:\n    wlan0:\n"
            '      dhcp4: yes\n      access-points:\n        "open-cafe": {}\n'),
        "static addresses": _wifi_yaml().replace("      dhcp4: yes\n",
                                                 "      dhcp4: no\n      addresses: [10.0.0.9/24]\n"),
        "an enterprise network": _wifi_yaml().replace(
            "          password: %s\n" % json.dumps(WIFI_PASS),
            "          auth:\n            key-management: eap\n            password: x\n"),
    }
    for why, text in bad.items():
        d = _netplan_dir({WIFI_YAML_NAME: text})
        try:
            rc, pairs, err = _reader(d)
            assert rc == 2, (why, rc, pairs, err)
            assert "cannot be migrated" in err, (why, err)
            assert WIFI_PASS not in err and pairs == [], why
        finally:
            shutil.rmtree(d, ignore_errors=True)
    d = _netplan_dir({WIFI_YAML_NAME: _wifi_yaml(), "50-again.yaml": _wifi_yaml()})
    try:
        rc, _pairs, err = _reader(d)
        assert rc == 2 and "more than one netplan file" in err, err
    finally:
        shutil.rmtree(d, ignore_errors=True)


# --- --install: the takeover ---
def _all_files_containing(root, needle, skip=()):
    hits = []
    for dirpath, _dirs, files in os.walk(root):
        for fn in files:
            p = os.path.join(dirpath, fn)
            if p in skip:
                continue
            with open(p, "rb") as f:
                if needle.encode() in f.read():
                    hits.append(p)
    return hits


def test_install_migrates_the_netplan_wifi_into_its_own_supplicant_conf():
    root = tempfile.mkdtemp()
    try:
        r, calls, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        paths = _sbc_paths(root)
        conf = _read(paths["wpa_conf"])
        assert stat.S_IMODE(os.stat(paths["wpa_conf"]).st_mode) == 0o600
        lines = [ln.strip() for ln in conf.splitlines()]
        assert "ctrl_interface=/run/wpa_supplicant" in lines
        assert "country=SK" in lines, "the country comes from the netplan regulatory-domain"
        assert 'ssid="newlevel.media"' in lines
        assert "key_mgmt=WPA-PSK WPA-PSK-SHA256" in lines and "ieee80211w=1" in lines
        assert BGSCAN_LINE in lines
        assert "psk=" + _expected_psk() in lines, "the PSK must equal wpa_passphrase's derivation"
        assert WIFI_PASS not in conf and "#psk" not in conf, "never the plaintext passphrase"
        # the netplan WiFi YAML moved aside once, byte-identical; ethernet + usb0 stay
        np = paths["netplan"]
        assert not os.path.exists(os.path.join(np, WIFI_YAML_NAME))
        assert _read(os.path.join(np, WIFI_YAML_NAME + ".bak")) == _wifi_yaml()
        assert _read(os.path.join(np, "10-dhcp-all-interfaces.yaml")) == NETPLAN_ETH
        assert _read(os.path.join(np, "40-usb0.yaml")) == NETPLAN_USB0
        # DHCP on wlan0 through networkd, the settings netplan generated
        net = _read(os.path.join(paths["networkd"], "05-bkshading-wlan0.network"))
        for want in ("Name=wlan0", "DHCP=yes", "LinkLocalAddressing=ipv6", "RouteMetric=600",
                     "UseMTU=true"):
            assert want in net, (want, net)
        # the heal: script + lib beside it + both units, byte-identical to the checkout
        heal = paths["heal_dir"]
        assert _read(os.path.join(heal, "bkshading-wifi-heal.sh")) == _read(HEAL_SCRIPT)
        assert os.access(os.path.join(heal, "bkshading-wifi-heal.sh"), os.X_OK)
        assert _read(os.path.join(heal, "lib", "bkshading-sbc-runtime.sh")) == _read(LIB)
        for unit in (HEAL_SERVICE, HEAL_TIMER):
            assert _read(os.path.join(paths["unit_dir"], os.path.basename(unit))) == _read(unit)
        # enable-only
        log = _read(calls)
        assert "enable wpa_supplicant@wlan0.service" in log, log
        assert "enable bkshading-wifi-heal.timer" in log, log
        assert "start" not in log and "restart" not in log, "enable-only:\n" + log
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_never_leaks_the_passphrase_or_the_psk():
    root = tempfile.mkdtemp()
    try:
        r, calls, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        paths = _sbc_paths(root)
        psk = _expected_psk()
        for secret in (WIFI_PASS, psk):
            assert secret not in r.stdout and secret not in r.stderr
        # the passphrase reaches no argv: wpa_passphrase gets the SSID only, the passphrase on stdin
        tool_log = _read(paths["tool_log"])
        assert "wpa_passphrase newlevel.media\n" in tool_log, tool_log
        # on disk: the passphrase only in the original YAML kept as .bak (YAML-escaped there, so the
        # decoded text is nowhere and its escape-free tail only in the .bak), the PSK only in the conf
        bak = os.path.join(paths["netplan"], WIFI_YAML_NAME + ".bak")
        assert _all_files_containing(root, WIFI_PASS) == []
        assert _all_files_containing(root, json.dumps(WIFI_PASS)) == [bak]
        assert _all_files_containing(root, "w0rd'x") == [bak]
        assert _all_files_containing(root, psk) == [paths["wpa_conf"]]
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_rerun_keeps_the_conf_and_moves_nothing_again():
    root = tempfile.mkdtemp()
    try:
        r1, _c, _b = _run_provision("--install", root)
        assert r1.returncode == 0, (r1.stdout, r1.stderr)
        paths = _sbc_paths(root)
        conf1 = _read(paths["wpa_conf"])
        bak = os.path.join(paths["netplan"], WIFI_YAML_NAME + ".bak")
        bak1 = _read(bak)
        os.remove(paths["tool_log"])
        r2, calls2, _b2 = _run_provision("--install", root, root_opts=RO_OPTS)
        assert r2.returncode == 0, (r2.stdout, r2.stderr)
        assert _read(paths["wpa_conf"]) == conf1, "a re-run keeps the conf byte-identical"
        assert _read(bak) == bak1 and not os.path.exists(bak + ".bak")
        assert "already exists -- kept" in r2.stdout, r2.stdout
        tool_log = _read(paths["tool_log"]) if os.path.exists(paths["tool_log"]) else ""
        assert "wpa_passphrase" not in tool_log, "a kept conf derives nothing again"
        assert "enable bkshading-wifi-heal.timer" in _read(calls2)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_refuses_with_no_netplan_wifi_and_no_conf():
    root = tempfile.mkdtemp()
    try:
        r, calls, _b = _run_provision(
            "--install", root, root_opts=RO_OPTS,
            netplan_files={"10-dhcp-all-interfaces.yaml": NETPLAN_ETH, "40-usb0.yaml": NETPLAN_USB0})
        assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
        assert re.search(r"no WiFi config to migrate", r.stderr), r.stderr
        assert "wpa_supplicant-wlan0.conf" in r.stderr and "30-wifis-dhcp.yaml" in r.stderr
        assert "nothing changed" in r.stderr
        paths = _sbc_paths(root)
        assert _read(paths["fstab"]) == ARMBIAN_FSTAB, "refused before any write"
        assert not os.path.exists(os.path.join(paths["unit_dir"], UNIT_NAME))
        assert not os.path.exists(calls), "nothing enabled or masked"
        assert not os.path.exists(paths["mount_log"]), "an ro root is not even remounted"
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_refuses_a_netplan_wifi_it_cannot_carry_whole():
    shared = NETPLAN_ETH.rstrip("\n") + "\n" + _wifi_yaml().split(
        "network:\n  version: 2\n  renderer: networkd\n", 1)[1]
    root = tempfile.mkdtemp()
    try:
        r, calls, _b = _run_provision("--install", root, netplan_files={"01-all.yaml": shared})
        assert r.returncode == 1, (r.stdout, r.stderr)
        # the shell's own refusal for the reader's exit 2, never the "nothing to migrate" one
        assert "cannot be migrated (reason above)" in r.stderr, r.stderr
        assert "no WiFi config to migrate" not in r.stderr, r.stderr
        assert "nothing changed" in r.stderr, r.stderr
        assert WIFI_PASS not in r.stdout + r.stderr
        paths = _sbc_paths(root)
        assert _read(os.path.join(paths["netplan"], "01-all.yaml")) == shared
        assert not os.path.exists(paths["wpa_conf"]) and not os.path.exists(calls)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_accepts_a_ready_64_hex_psk_in_netplan():
    hexpsk = "A1" * 32
    root = tempfile.mkdtemp()
    try:
        files = _armbian_netplan()
        files[WIFI_YAML_NAME] = _wifi_yaml(password=hexpsk)
        r, _c, _b = _run_provision("--install", root, netplan_files=files)
        assert r.returncode == 0, (r.stdout, r.stderr)
        paths = _sbc_paths(root)
        assert "psk=" + hexpsk.lower() in _read(paths["wpa_conf"])
        assert "wpa_passphrase" not in (_read(paths["tool_log"]) if os.path.exists(paths["tool_log"]) else "")
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_on_a_wired_box_skips_the_wifi_takeover():
    root = tempfile.mkdtemp()
    try:
        r, calls, _b = _run_provision("--install", root, net_ifaces={"eth0": "up"})
        assert r.returncode == 0, (r.stdout, r.stderr)
        paths = _sbc_paths(root)
        assert "WiFi takeover + heal skipped" in r.stdout, r.stdout
        assert not os.path.exists(paths["wpa_conf"])
        assert os.path.exists(os.path.join(paths["netplan"], WIFI_YAML_NAME)), "netplan untouched"
        assert not os.path.exists(paths["heal_dir"])
        log = _read(calls)
        assert "wpa_supplicant" not in log and "wifi-heal" not in log, log
    finally:
        shutil.rmtree(root, ignore_errors=True)


# --- --check: the new rows ---
def test_check_wifi_rows_ok_after_install():
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        ok, _c2, _b2 = _run_provision("--check", root)
        assert ok.returncode == 0, (ok.stdout, ok.stderr)
        assert "OK: wpa_supplicant@wlan0.service enabled" in ok.stdout
        assert "OK: %s carries %s" % (_sbc_paths(root)["wpa_conf"], BGSCAN_LINE) in ok.stdout
        assert "OK: WiFi heal installed + bkshading-wifi-heal.timer enabled" in ok.stdout
        assert re.search(r"OK: gateway 10\.77\.8\.1 answers a ping on wlan0 \(bssid=%s signal=-63"
                         % re.escape(GOOD_BSSID), ok.stdout), ok.stdout
        pings = [ln for ln in _read(_sbc_paths(root)["tool_log"]).splitlines()
                 if ln.startswith("ping ")]
        assert pings and all("-I wlan0" in p and p.endswith(" " + WIFI_GW) for p in pings), pings
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_check_fails_when_the_gateway_does_not_answer_naming_bssid_and_signal():
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        dead = _net_state(reachable=False, bssid=FAR_BSSID, rssi=-73)
        bad, _c2, _b2 = _run_provision("--check", root, net_state=dead)
        assert bad.returncode == 1, (bad.stdout, bad.stderr)
        assert re.search(r"FAIL: gateway 10\.77\.8\.1 does not answer a ping on wlan0 "
                         r"\(bssid=%s signal=-73 dBm" % re.escape(FAR_BSSID), bad.stderr), bad.stderr
        nogw, _c3, _b3 = _run_provision("--check", root,
                                        net_state=_net_state(gateway=None, bssid=FAR_BSSID))
        assert nogw.returncode == 1
        assert re.search(r"FAIL: no DHCP default gateway on wlan0 \(bssid=%s"
                         % re.escape(FAR_BSSID), nogw.stderr), nogw.stderr
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_check_fails_on_a_conf_without_bgscan_disabled_units_or_a_stale_heal():
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        paths = _sbc_paths(root)
        conf = _read(paths["wpa_conf"])
        with open(paths["wpa_conf"], "w", encoding="utf-8") as f:
            f.write(conf.replace("\t" + BGSCAN_LINE + "\n", ""))
        nob, _c2, _b2 = _run_provision("--check", root)
        assert nob.returncode == 1 and "has no %s line" % BGSCAN_LINE in nob.stderr, nob.stderr
        with open(paths["wpa_conf"], "w", encoding="utf-8") as f:
            f.write(conf)
        dis, _c3, _b3 = _run_provision(
            "--check", root,
            disabled_units=("wpa_supplicant@wlan0.service", "bkshading-wifi-heal.timer"))
        assert dis.returncode == 1
        assert "FAIL: wpa_supplicant@wlan0.service not enabled" in dis.stderr, dis.stderr
        assert "FAIL: bkshading-wifi-heal.timer not enabled" in dis.stderr, dis.stderr
        with open(os.path.join(paths["heal_dir"], "bkshading-wifi-heal.sh"), "a") as f:
            f.write("# drift\n")
        stale, _c4, _b4 = _run_provision("--check", root)
        assert stale.returncode == 1
        assert re.search(r"FAIL: WiFi heal missing or differs from this checkout: .*bkshading-wifi-heal\.sh",
                         stale.stderr), stale.stderr
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_check_skips_every_wifi_row_on_a_wired_box():
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root, net_ifaces={"eth0": "up"})
        assert r.returncode == 0, (r.stdout, r.stderr)
        ok, _c2, _b2 = _run_provision("--check", root, net_ifaces={"eth0": "up"})
        assert ok.returncode == 0, (ok.stdout, ok.stderr)
        assert "the wpa_supplicant, bgscan, heal and gateway rows are skipped" in ok.stdout
        assert not os.path.exists(_sbc_paths(root)["tool_log"]), "no ping / wpa_cli on a wired box"
    finally:
        shutil.rmtree(root, ignore_errors=True)


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
    """A fake /sys/class/net with wlan0, whose device/driver/module links to a dir named after
    the driver module (sprdwl_ng on the Orange Pi Zero 2W) -- or no module link (a built-in driver).
    iface_present=False: the module is unloaded, wlan0 is gone (kept aside as .wlan0-unloaded, which
    the modprobe stub brings back on a load)."""
    net = os.path.join(tmp, "sys-class-net")
    dev = os.path.join(tmp, "sys-devices", "wlan0-dev")
    drv = os.path.join(tmp, "sys-bus", "drivers", "wlan-driver")
    os.makedirs(os.path.join(net), exist_ok=True)
    os.makedirs(os.path.join(dev, "net"), exist_ok=True)
    os.makedirs(drv, exist_ok=True)
    os.makedirs(os.path.join(net, "wlan0"), exist_ok=True)
    os.symlink(dev, os.path.join(net, "wlan0", "device"))
    os.symlink(drv, os.path.join(dev, "driver"))
    if driver_module:
        mod = os.path.join(tmp, "sys-module", driver_module)
        os.makedirs(mod, exist_ok=True)
        os.symlink(mod, os.path.join(drv, "module"))
    if not iface_present:
        os.rename(os.path.join(net, "wlan0"), os.path.join(net, ".wlan0-unloaded"))
    return net


def _heal_pass(env, script=HEAL_SCRIPT):
    return subprocess.run(["/bin/bash", script], env=env, capture_output=True, text=True)


def _heal_misses(env):
    return _read(os.path.join(env["BKSHADING_WIFI_HEAL_STATE_DIR"], "misses")).strip()


def _heal_tools(env):
    p = env["FAKE_TOOL_LOG"]
    return _read(p).splitlines() if os.path.exists(p) else []


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


def test_heal_runs_from_the_installed_layout():
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        installed = os.path.join(_sbc_paths(root)["heal_dir"], "bkshading-wifi-heal.sh")
        run_dir = os.path.join(root, "heal-run")
        os.makedirs(run_dir)
        env = _heal_env(run_dir, _net_state(reachable=False))
        h = _heal_pass(env, script=installed)
        assert h.returncode == 0, (h.stdout, h.stderr)
        assert _heal_misses(env) == "1", "the installed script finds its lib and counts a miss"
    finally:
        shutil.rmtree(root, ignore_errors=True)


# ---------------------------------------------------------------------------------------------
# issue 808 WiFi review round 1 (the fresh-context review on the lane): the regressions it found
# ---------------------------------------------------------------------------------------------
SHARED_WIFI_ETH = NETPLAN_ETH.rstrip("\n") + "\n" + _wifi_yaml().split(
    "network:\n  version: 2\n  renderer: networkd\n", 1)[1]


def test_install_refuses_an_unmigratable_yaml_even_when_a_conf_exists():
    # The reader's exit code was lost under set -e: with a conf already on the box --install exited
    # 0 and left a netplan YAML defining wlan0 -> two supplicants on wlan0 after the reboot.
    root = tempfile.mkdtemp()
    try:
        paths = _sbc_paths(root)
        os.makedirs(os.path.dirname(paths["wpa_conf"]))
        with open(paths["wpa_conf"], "w") as f:
            f.write("ctrl_interface=/run/wpa_supplicant\n")
        r, calls, _b = _run_provision("--install", root, netplan_files={"01-all.yaml": SHARED_WIFI_ETH})
        assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
        assert "cannot be migrated (reason above)" in r.stderr, r.stderr
        assert _read(os.path.join(paths["netplan"], "01-all.yaml")) == SHARED_WIFI_ETH
        assert not os.path.exists(calls), "refused before anything is enabled"
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_refuses_when_the_netplan_reader_cannot_run():
    root = tempfile.mkdtemp()
    try:
        r, calls, _b = _run_provision("--install", root,
                                      env_extra={"BKSHADING_SBC_PYTHON": shutil.which("false")})
        assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
        assert "could not read the netplan YAML" in r.stderr, r.stderr
        assert _read(_sbc_paths(root)["fstab"]) == ARMBIAN_FSTAB and not os.path.exists(calls)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_refuses_without_the_tools_the_heal_needs():
    # a missing ping made every heal pass a "miss": a self-made reassociate + restart on a healthy link
    for tool in ("BKSHADING_SBC_PING", "BKSHADING_SBC_IP", "BKSHADING_SBC_WPA_CLI"):
        root = tempfile.mkdtemp()
        try:
            missing = os.path.join(root, "no-such-tool")
            r, calls, _b = _run_provision("--install", root, env_extra={tool: missing})
            assert r.returncode == 1, (tool, r.stdout, r.stderr)
            assert missing in r.stderr and "nothing changed" in r.stderr, (tool, r.stderr)
            assert not os.path.exists(calls) and not os.path.exists(_sbc_paths(root)["wpa_conf"])
        finally:
            shutil.rmtree(root, ignore_errors=True)


def test_install_moves_the_netplan_yaml_aside_only_after_the_units_are_enabled():
    # a failure between the YAML move and the enable left a board with no WiFi after the reboot
    root = tempfile.mkdtemp()
    try:
        r, _calls, _b = _run_provision("--install", root, systemctl_fail_on="enable")
        assert r.returncode != 0, (r.stdout, r.stderr)
        np = _sbc_paths(root)["netplan"]
        assert _read(os.path.join(np, WIFI_YAML_NAME)) == _wifi_yaml(), "netplan keeps the WiFi"
        assert not os.path.exists(os.path.join(np, WIFI_YAML_NAME + ".bak"))
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_rerun_after_a_partial_install_finishes_the_move():
    # the conf was written but the YAML was not moved: an IDENTICAL migration is finished, not refused
    root = tempfile.mkdtemp()
    try:
        r1, _c, _b = _run_provision("--install", root)
        assert r1.returncode == 0, (r1.stdout, r1.stderr)
        paths = _sbc_paths(root)
        np = paths["netplan"]
        os.rename(os.path.join(np, WIFI_YAML_NAME + ".bak"), os.path.join(np, WIFI_YAML_NAME))
        conf1 = _read(paths["wpa_conf"])
        r2, _c2, _b2 = _run_provision("--install", root)
        assert r2.returncode == 0, (r2.stdout, r2.stderr)
        assert _read(paths["wpa_conf"]) == conf1
        assert not os.path.exists(os.path.join(np, WIFI_YAML_NAME))
        assert _read(os.path.join(np, WIFI_YAML_NAME + ".bak")) == _wifi_yaml()
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_refuses_when_the_netplan_yaml_and_the_conf_differ():
    # a netplan WiFi changed after the takeover must never be dropped silently
    root = tempfile.mkdtemp()
    try:
        r1, _c, _b = _run_provision("--install", root)
        assert r1.returncode == 0, (r1.stdout, r1.stderr)
        paths = _sbc_paths(root)
        np = paths["netplan"]
        os.remove(os.path.join(np, WIFI_YAML_NAME + ".bak"))
        changed = _wifi_yaml(password="an0ther-passphrase")
        with open(os.path.join(np, WIFI_YAML_NAME), "w") as f:
            f.write(changed)
        conf1 = _read(paths["wpa_conf"])
        r2, _c2, _b2 = _run_provision("--install", root)
        assert r2.returncode == 1, (r2.stdout, r2.stderr)
        assert paths["wpa_conf"] in r2.stderr and os.path.join(np, WIFI_YAML_NAME) in r2.stderr
        assert "differ" in r2.stderr, r2.stderr
        assert "an0ther-passphrase" not in r2.stdout + r2.stderr
        assert _read(paths["wpa_conf"]) == conf1 and _read(os.path.join(np, WIFI_YAML_NAME)) == changed
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_install_enables_networkd_and_a_supplicant_restart_dropin():
    # a crashed wpa_supplicant@wlan0 (no Restart= in Debian's unit) left the board off the WiFi
    root = tempfile.mkdtemp()
    try:
        r, calls, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        dropin = os.path.join(_sbc_paths(root)["unit_dir"], "wpa_supplicant@wlan0.service.d",
                              "bkshading-restart.conf")
        body = _read(dropin)
        assert "[Service]" in body and "Restart=on-failure" in body and "RestartSec=" in body, body
        assert body == _lib_call("bkshading_sbc_wpa_restart_dropin_content")
        log = _read(calls)
        assert "enable systemd-networkd.service" in log, log
        assert "start" not in log, log
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_networkd_dhcp_follows_the_netplan_dhcp6():
    # netplan writes DHCP=ipv4 for a WiFi without dhcp6; the takeover must not add DHCPv6
    assert "DHCP=ipv4" in _lib_call("bkshading_sbc_networkd_wifi_content ipv4")
    assert "DHCP=yes" in _lib_call("bkshading_sbc_networkd_wifi_content yes")
    root = tempfile.mkdtemp()
    try:
        files = _armbian_netplan()
        files[WIFI_YAML_NAME] = _wifi_yaml().replace("      dhcp6: yes\n", "")
        r, _c, _b = _run_provision("--install", root, netplan_files=files)
        assert r.returncode == 0, (r.stdout, r.stderr)
        net = _read(os.path.join(_sbc_paths(root)["networkd"], "05-bkshading-wlan0.network"))
        assert "DHCP=ipv4" in net and "DHCP=yes" not in net, net
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_netplan_reader_refuses_a_wifi_it_cannot_move_aside():
    # netplan also reads /run/netplan and /lib/netplan; a wlan0 there survives the takeover
    etc = _netplan_dir({"10-dhcp-all-interfaces.yaml": NETPLAN_ETH})
    lib = _netplan_dir({"90-vendor-wifi.yaml": _wifi_yaml()})
    try:
        r = subprocess.run([sys.executable, NETPLAN_READER, etc, "wlan0", lib], capture_output=True)
        assert r.returncode == 2, (r.returncode, r.stderr)
        assert b"90-vendor-wifi.yaml" in r.stderr and b"cannot move" in r.stderr, r.stderr
    finally:
        shutil.rmtree(etc, ignore_errors=True)
        shutil.rmtree(lib, ignore_errors=True)
    # a /lib file shadowed by an /etc file that STAYS (not the migrated one) never goes live
    etc = _netplan_dir({WIFI_YAML_NAME: _wifi_yaml(), "90-vendor.yaml": NETPLAN_USB0})
    lib = _netplan_dir({"90-vendor.yaml": _wifi_yaml(password="shadowed-pass")})
    try:
        rc, pairs, err = _reader_dirs(etc, lib)
        assert rc == 0, err
        assert ("pass", WIFI_PASS) in pairs and ("file", os.path.join(etc, WIFI_YAML_NAME)) in pairs
    finally:
        shutil.rmtree(etc, ignore_errors=True)
        shutil.rmtree(lib, ignore_errors=True)
    # the MIGRATED file shadows only until it is moved aside: a same-name /lib file would then go
    # live, so it is refused, whatever it defines
    for text in (_wifi_yaml(password="shadowed-pass"), NETPLAN_USB0):
        etc = _netplan_dir({WIFI_YAML_NAME: _wifi_yaml()})
        lib = _netplan_dir({WIFI_YAML_NAME: text})
        try:
            rc, pairs, err = _reader_dirs(etc, lib)
            assert rc == 2 and "un-hide" in err, (rc, err)
            assert pairs == [] and "shadowed-pass" not in err
        finally:
            shutil.rmtree(etc, ignore_errors=True)
            shutil.rmtree(lib, ignore_errors=True)


def _reader_dirs(etc, *others):
    r = subprocess.run([sys.executable, NETPLAN_READER, etc, "wlan0"] + list(others),
                       capture_output=True)
    fields = r.stdout.split(b"\0")[:-1] if r.stdout else []
    pairs = [(fields[i].decode(), fields[i + 1].decode()) for i in range(0, len(fields), 2)]
    return r.returncode, pairs, r.stderr.decode()


def test_check_fails_on_a_readable_conf_a_netplan_wlan0_or_a_missing_tool():
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        paths = _sbc_paths(root)
        os.chmod(paths["wpa_conf"], 0o644)
        ro, _c2, _b2 = _run_provision("--check", root)
        assert ro.returncode == 1 and re.search(r"FAIL: .*mode 644.*0600", ro.stderr), ro.stderr
        os.chmod(paths["wpa_conf"], 0o600)
        np = paths["netplan"]
        os.rename(os.path.join(np, WIFI_YAML_NAME + ".bak"), os.path.join(np, WIFI_YAML_NAME))
        two, _c3, _b3 = _run_provision("--check", root)
        assert two.returncode == 1, two.stdout
        assert re.search(r"FAIL: netplan still defines wlan0", two.stderr), two.stderr
        os.rename(os.path.join(np, WIFI_YAML_NAME), os.path.join(np, WIFI_YAML_NAME + ".bak"))
        noping, _c4, _b4 = _run_provision(
            "--check", root, env_extra={"BKSHADING_SBC_PING": os.path.join(root, "no-ping")})
        assert noping.returncode == 1
        assert re.search(r"FAIL: .*no-ping.* not found", noping.stderr), noping.stderr
        assert "does not answer" not in noping.stderr, "a missing tool is not a dead gateway"
        os.remove(os.path.join(paths["unit_dir"], "wpa_supplicant@wlan0.service.d",
                               "bkshading-restart.conf"))
        nodrop, _c5, _b5 = _run_provision("--check", root)
        assert nodrop.returncode == 1 and "bkshading-restart.conf" in nodrop.stderr, nodrop.stderr
    finally:
        shutil.rmtree(root, ignore_errors=True)


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


# ---------------------------------------------------------------------------------------------
# issue 808 WiFi review round 2
# ---------------------------------------------------------------------------------------------
def test_install_refuses_when_moving_the_yaml_would_unhide_a_same_name_file():
    root = tempfile.mkdtemp()
    try:
        r, calls, _b = _run_provision(
            "--install", root, netplan_other=({}, {WIFI_YAML_NAME: _wifi_yaml(password="lib-pass1")}))
        assert r.returncode == 1, (r.stdout, r.stderr)
        assert "un-hide" in r.stderr and "nothing changed" in r.stderr, r.stderr
        assert "netplan runs no supplicant" not in r.stdout
        assert os.path.exists(os.path.join(_sbc_paths(root)["netplan"], WIFI_YAML_NAME))
        assert not os.path.exists(calls)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def _reader_spy(root):
    """A BKSHADING_SBC_PYTHON that runs the real reader and keeps a copy of everything it printed."""
    p = os.path.join(root, "python-spy")
    with open(p, "w", encoding="utf-8") as f:
        f.write("#!%s\nimport subprocess, sys\n"
                "r = subprocess.run([%r] + sys.argv[1:], stdout=subprocess.PIPE)\n"
                "open(%r, 'ab').write(r.stdout)\n"
                "sys.stdout.buffer.write(r.stdout)\nsys.exit(r.returncode)\n"
                % (sys.executable, sys.executable, os.path.join(root, "reader-out.bin")))
    os.chmod(p, 0o755)
    return p, os.path.join(root, "reader-out.bin")


def test_check_reads_netplan_without_loading_any_passphrase():
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        np = _sbc_paths(root)["netplan"]
        os.rename(os.path.join(np, WIFI_YAML_NAME + ".bak"), os.path.join(np, WIFI_YAML_NAME))
        spy, out = _reader_spy(root)
        c, _c2, _b2 = _run_provision("--check", root, env_extra={"BKSHADING_SBC_PYTHON": spy})
        assert re.search(r"FAIL: netplan still defines wlan0", c.stderr), c.stderr
        seen = open(out, "rb").read()
        assert WIFI_YAML_NAME.encode() in seen, seen
        assert WIFI_PASS.encode() not in seen and b"pass\0" not in seen, seen
    finally:
        shutil.rmtree(root, ignore_errors=True)
    # the names-only reader mode, directly
    d = _netplan_dir(_armbian_netplan())
    try:
        r = subprocess.run([sys.executable, NETPLAN_READER, "--names-only", d, "wlan0"],
                           capture_output=True)
        assert r.returncode == 0, r.stderr
        assert r.stdout == b"file\0" + os.path.join(d, WIFI_YAML_NAME).encode() + b"\0", r.stdout
    finally:
        shutil.rmtree(d, ignore_errors=True)


def test_install_rerun_compares_the_wifi_identity_not_the_conf_text():
    # a partial re-run must not turn into a "differs" refusal because a comment or a constant of
    # the lib changed since the conf was written (the pending key_mgmt ruling, a reworded header)
    root = tempfile.mkdtemp()
    try:
        r1, _c, _b = _run_provision("--install", root)
        assert r1.returncode == 0, (r1.stdout, r1.stderr)
        paths = _sbc_paths(root)
        np = paths["netplan"]
        os.rename(os.path.join(np, WIFI_YAML_NAME + ".bak"), os.path.join(np, WIFI_YAML_NAME))
        conf = _read(paths["wpa_conf"]).replace(
            "# Written by", "# (an older header)\n# Written by").replace(
            "key_mgmt=WPA-PSK WPA-PSK-SHA256", "key_mgmt=WPA-PSK")
        with open(paths["wpa_conf"], "w") as f:
            f.write(conf)
        r2, _c2, _b2 = _run_provision("--install", root)
        assert r2.returncode == 0, (r2.stdout, r2.stderr)
        assert _read(paths["wpa_conf"]) == conf, "a kept conf is never rewritten"
        assert os.path.exists(os.path.join(np, WIFI_YAML_NAME + ".bak"))
        # ... and --check names the drift from the lib's settings (non-secret lines only)
        c, _c3, _b3 = _run_provision("--check", root)
        assert c.returncode == 1, c.stdout
        assert re.search(r"FAIL: .*wpa_supplicant-wlan0\.conf.*key_mgmt=WPA-PSK WPA-PSK-SHA256",
                         c.stderr), c.stderr
        assert _expected_psk() not in c.stdout + c.stderr
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_check_settings_drift_remediation_names_the_remount():
    # following it means two moves on the (usually read-only) root
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root)
        assert r.returncode == 0, (r.stdout, r.stderr)
        paths = _sbc_paths(root)
        with open(paths["wpa_conf"]) as f:
            conf = f.read()
        with open(paths["wpa_conf"], "w") as f:
            f.write(conf.replace("ieee80211w=1", "ieee80211w=2"))
        c, _c2, _b2 = _run_provision("--check", root)
        assert c.returncode == 1 and "lacks the lib's ieee80211w=1" in c.stderr, c.stderr
        assert "mount -o remount,rw /" in c.stderr, c.stderr
    finally:
        shutil.rmtree(root, ignore_errors=True)


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


def test_install_tool_refusal_names_the_remount_on_a_read_only_root():
    root = tempfile.mkdtemp()
    try:
        r, _c, _b = _run_provision("--install", root, root_opts=RO_OPTS,
                                   env_extra={"BKSHADING_SBC_PING": os.path.join(root, "no-ping")})
        assert r.returncode == 1, (r.stdout, r.stderr)
        assert "mount -o remount,rw /" in r.stderr and "apt-get install" in r.stderr, r.stderr
        assert not os.path.exists(_sbc_paths(root)["mount_log"]), "the refusal itself remounts nothing"
    finally:
        shutil.rmtree(root, ignore_errors=True)


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
# deploy extension: --arch arm64 selects the arm64 artifact; the remount follows the target's root
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


def test_deploy_no_remount_flag_is_gone():
    # The remount follows the target's own root now (a read-only SBC root, issue 808 slice B), so the
    # old per-deploy flag is a dead MVP flag: it is refused as an unknown argument.
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, "bkshading-relay")
        _fake_elf(fake_bin, AARCH64)
        r = _run_deploy(["--host", "10.77.9.60", "--arch", "arm64", "--no-remount",
                         "--binary", fake_bin, "--dry-run"])
        assert r.returncode == 2, (r.returncode, r.stdout, r.stderr)
        assert "unknown argument" in r.stderr, r.stderr


def test_deploy_dry_run_arm64_plans_the_root_mode_read():
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, "bkshading-relay")
        _fake_elf(fake_bin, AARCH64)
        r = _run_deploy(["--host", "10.77.9.60", "--arch", "arm64", "--binary", fake_bin, "--dry-run"])
        assert r.returncode == 0, (r.stdout, r.stderr)
        out = r.stdout + r.stderr
        assert "10.77.9.60" in out and BIN_PATH in out
        assert "findmnt" in out, "the plan must say the root mode is read from the target"
        assert "remount,rw" in out and "remount,ro" in out, "the ro-root cycle is planned for an ro root"
        assert "WARNING" not in out, "no rw-root footgun warning is left: the target decides"
        assert "bkshading-provision-sbc.sh" in out, "an arm64 deploy points at the SBC provisioning"
        # still enable-only.
        assert re.search(r"(NOT start|enable-only|reboot)", out, re.I)


def _fake_deploy_env(tmp, remote_sha, root_opts=RW_OPTS):
    log = os.path.join(tmp, "calls.log")
    fake_ssh = os.path.join(tmp, "fake-ssh")
    ssh_body = (
        "#!/usr/bin/env bash\n"
        'printf "SSH %s\\n" "$*" >> "__LOG__"\n'
        'cmd="${!#}"\n'
        "case \"$cmd\" in\n"
        # issue 808: the deploy reads the relay state first and refuses on an unreadable read, so
        # the fake SBC answers it like a real one whose relay is stopped.
        '  *"is-active"*) printf "inactive\\n" ;;\n'
        # issue 808 slice B: the deploy reads the target's root mode; an empty answer refuses.
        '  *"findmnt"*) printf "__ROOT__\\n" ;;\n'
        '  *sha256sum*) printf "__SHA__\\n" ;;\n'
        '  *"test -x"*) printf "yes\\n" ;;\n'
        "  *) : ;;\n"
        "esac\n"
        "exit 0\n"
    ).replace("__LOG__", log).replace("__SHA__", remote_sha).replace("__ROOT__", root_opts)
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
    # issue 1271 rig-busy guard: the deploy script runs `obs_phase2.py rig-busy-check` against the
    # LIVE strih/stream OBS before touching a cambox. A "fake" deploy must never read the real rig
    # (16.9.2026: this test went red on dev1 purely because a PR E2E was recording on both boxes),
    # so point the guard's script dir at a stub that answers "not busy" -- the existing
    # BKSHADING_DEPLOY_OBS_PHASE2_DIR seam, no deploy-script change.
    fake_phase2 = os.path.join(tmp, "obs_phase2.py")
    with open(fake_phase2, "w") as f:
        f.write(
            "#!/usr/bin/env python3\n"
            "import json, sys\n"
            'print(json.dumps({"busy": False, "diagnostics": []}))\n'
        )
    os.chmod(fake_phase2, 0o755)
    env = {
        "BKSHADING_DEPLOY_SSH": fake_ssh,
        "BKSHADING_DEPLOY_SCP": fake_scp,
        "BKSHADING_DEPLOY_SSHPASS_PREFIX": "",
        "BKSHADING_DEPLOY_OBS_PHASE2_DIR": tmp,
    }
    return env, log


def _fake_deploy_run(root_opts):
    with tempfile.TemporaryDirectory() as tmp:
        fake_bin = os.path.join(tmp, "bkshading-relay")
        _fake_elf(fake_bin, AARCH64)
        local_sha = subprocess.run(
            ["sha256sum", fake_bin], capture_output=True, text=True, check=True
        ).stdout.split()[0]
        env, log = _fake_deploy_env(tmp, local_sha, root_opts=root_opts)
        r = _run_deploy(["--host", "10.77.9.60", "--arch", "arm64", "--binary", fake_bin], env=env)
        calls = open(log).read() if os.path.exists(log) else ""
        return r, calls


def test_fake_deploy_to_a_rw_root_sbc_never_remounts():
    r, calls = _fake_deploy_run(RW_OPTS)
    assert r.returncode == 0, (r.stdout, r.stderr)
    assert "findmnt" in calls, "the deploy must READ the target's root mode"
    assert ("root@10.77.9.60:" + BIN_PATH) in calls, "must scp to the relay bin path"
    assert "sha256sum" in calls, "must byte-verify"
    assert "remount,rw" not in calls and "remount,ro" not in calls, \
        "an rw root (a board before its first ro reboot) is never remounted"
    assert "WARNING" not in r.stderr, "an rw SBC root before its ro reboot is normal:\n" + r.stderr
    assert "systemctl start" not in calls and "systemctl restart" not in calls, \
        "deploy is enable-only"


def test_fake_deploy_to_a_ro_root_sbc_remounts_rw_then_ro():
    r, calls = _fake_deploy_run(RO_OPTS)
    assert r.returncode == 0, (r.stdout, r.stderr)
    i_root = calls.find("findmnt")
    i_rw = calls.find("remount,rw /")
    i_scp = calls.find("root@10.77.9.60:" + BIN_PATH)
    i_ro = calls.find("remount,ro /")
    assert 0 <= i_root < i_rw < i_scp < i_ro, "a read-only SBC root gets the cambox cycle:\n" + calls
    assert "root back to read-only" in r.stdout, r.stdout


def test_fake_deploy_refuses_an_unreadable_root_before_touching_the_sbc():
    r, calls = _fake_deploy_run("")
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    assert re.search(r"could not read the root mount", r.stderr), r.stderr
    assert "remount" not in calls and "SCP" not in calls and "systemctl stop" not in calls, calls


def test_no_doc_or_usage_line_still_passes_the_removed_flag():
    for f in (SCRIPT, DEPLOY_SCRIPT, README, os.path.join(REPO, ".claude", "rules", "bkshading.md"),
              os.path.join(REPO, ".claude", "rules", "bkshading-sbc.md")):
        assert "--no-remount" not in _read(f), "%s still mentions the removed flag" % f


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
    # Device-AGNOSTIC (the board is not finally decided, owner 14.9.2026): the README names the ROLE,
    # not one vendor. Pinned so a future re-hardcode to a single device name is a RED test.
    assert "zero-class arm64 SBC" in txt, "README must name the device-agnostic SBC class"
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
