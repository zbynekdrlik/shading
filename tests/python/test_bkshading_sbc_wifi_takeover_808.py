#!/usr/bin/env python3
"""The handheld SBC's WiFi takeover (issue 808, design 5972548198): `scripts/bkshading-provision-sbc.sh
--install` moves wlan0 from netplan to its own wpa_supplicant@wlan0 (bgscan, the migrated SSID and the
wpa_passphrase PSK, never the passphrase on an argv or in a log), a networkd DHCP file and a restart
drop-in, installs the heal, and only then moves the netplan WiFi YAML aside; `--check` grades the WiFi
rows. The pure conf/netplan helpers of scripts/lib/bkshading-sbc-runtime.sh and the netplan reader
scripts/bkshading_sbc_netplan_wifi.py are tested here too.

Split out of test_bkshading_sbc_provision_808.py (which keeps the relay, read-only root, deploy and
CI tests); the provision harness and the board-tool stubs live in bkshading_sbc_fakes_808.py, the
heal's own tests in test_bkshading_wifi_heal_808.py. Runs in the `python-tests` CI job; runnable
directly (`python3 tests/python/test_bkshading_sbc_wifi_takeover_808.py`) or under pytest.
"""
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from bkshading_sbc_fakes_808 import (  # noqa: E402
    ARMBIAN_FSTAB,
    FAR_BSSID,
    GOOD_BSSID,
    HEAL_SCRIPT,
    HEAL_SERVICE,
    HEAL_TIMER,
    LIB,
    NETPLAN_ETH,
    NETPLAN_USB0,
    REPO,
    RO_OPTS,
    UNIT_NAME,
    WIFI_GW,
    WIFI_PASS,
    WIFI_SSID,
    WIFI_YAML_NAME,
    _armbian_netplan,
    _heal_env,
    _heal_misses,
    _heal_pass,
    _lib_call,
    _net_state,
    _read,
    _run_provision,
    _sbc_paths,
    _wifi_yaml,
)



def _expected_psk(ssid=WIFI_SSID, password=WIFI_PASS):
    # IEEE 802.11i PSK = PBKDF2-HMAC-SHA1(passphrase, ssid, 4096, 32): exactly what wpa_passphrase
    # computes. Independent of the tool under test, so a wrong derivation cannot pass.
    return hashlib.pbkdf2_hmac("sha1", password.encode(), ssid.encode(), 4096, 32).hex()
# ---------------------------------------------------------------------------------------------
# issue 808 WiFi roam + heal (design 5972548198): the handheld sat on a dead far AP after a reboot
# with wpa_state=COMPLETED and a DHCP lease; netplan has no bgscan and nothing checked traffic.
# ---------------------------------------------------------------------------------------------
NETPLAN_READER = os.path.join(REPO, "scripts", "bkshading_sbc_netplan_wifi.py")
BGSCAN_LINE = 'bgscan="simple:30:-65:300"'


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
