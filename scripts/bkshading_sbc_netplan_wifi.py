#!/usr/bin/env python3
"""Read the handheld SBC's netplan WiFi config for the wpa_supplicant takeover (issue 808).

`scripts/bkshading-provision-sbc.sh --install` takes the handheld's WiFi over from netplan (design
5972548198): netplan 1.1 has no `bgscan` key, so its generated supplicant config never roams to a
stronger AP. The SSID, passphrase and regulatory domain are MIGRATED from the board's own netplan
WiFi YAML -- no credential ever crosses dev1, an argv or a log. This helper is the one YAML reader
of that migration (bash cannot parse YAML); the bash side derives the PSK and writes the conf.

It parses with PyYAML's BaseLoader, which keeps every scalar as its raw text, the same as netplan's
own libyaml reader: a passphrase `12345678` or `0x1A2B3C4D` stays that text and is never turned into
an int, and `dhcp4: yes` stays the string `yes`. PyYAML is a dependency of the `netplan.io` package,
so it is present on every board this helper has a netplan YAML to read on.

Output (stdout, for the provision script only): NUL-terminated `tag value` pairs --
    file <path>   country <ISO code or empty>   then per access point:  ssid <ssid>  pass <passphrase>
Exit codes: 0 = one WiFi YAML found; 3 = no netplan YAML defines the interface; 2 = a shape this
migration does not carry over (the reason on stderr, which never carries a passphrase). The script
refuses rather than drop a setting it would lose by moving the YAML aside.

Usage: bkshading_sbc_netplan_wifi.py <netplan-dir> <iface>
"""
import glob
import os
import re
import sys

import yaml

# Keys a migrated `network:` mapping, interface and access point may carry. Anything else would be
# silently lost when the YAML is moved aside, so it refuses instead.
NETWORK_KEYS = {"version", "renderer", "wifis"}
IFACE_KEYS = {"access-points", "regulatory-domain", "dhcp4", "dhcp6", "renderer"}
AP_KEYS = {"password", "auth", "mode"}
AUTH_KEYS = {"key-management", "password"}
TRUE_WORDS = {"true", "yes", "on", "y"}
COUNTRY_RE = re.compile(r"^[A-Z]{2}$")


class Unsupported(Exception):
    """A netplan WiFi shape the migration does not carry over (never carries a secret)."""


def _load(path):
    with open(path, encoding="utf-8") as f:
        return yaml.load(f, Loader=yaml.BaseLoader)  # noqa: S506 - BaseLoader builds only str/list/dict


def find_wifi_yamls(netplan_dir, iface):
    """[(path, document)] of every *.yaml in netplan_dir whose network.wifis defines iface."""
    found = []
    for path in sorted(glob.glob(os.path.join(netplan_dir, "*.yaml"))):
        try:
            doc = _load(path)
        except yaml.YAMLError as e:
            raise Unsupported("%s is not valid YAML (%s)" % (path, e.__class__.__name__)) from None
        net = doc.get("network") if isinstance(doc, dict) else None
        wifis = net.get("wifis") if isinstance(net, dict) else None
        if isinstance(wifis, dict) and iface in wifis:
            found.append((path, doc))
    return found


def _password_of(ssid, ap):
    if ap is None or ap == "":
        raise Unsupported("access point %r has no password (an open network is not migrated)" % ssid)
    if not isinstance(ap, dict):
        raise Unsupported("access point %r is not a mapping" % ssid)
    extra = sorted(set(ap) - AP_KEYS)
    if extra:
        raise Unsupported("access point %r carries %s, which the migration does not carry over" % (ssid, ", ".join(extra)))
    if ap.get("mode", "infrastructure") != "infrastructure":
        raise Unsupported("access point %r is mode %r, not infrastructure" % (ssid, ap.get("mode")))
    auth = ap.get("auth")
    password = ap.get("password")
    if auth is not None:
        if not isinstance(auth, dict):
            raise Unsupported("access point %r has an auth block that is not a mapping" % ssid)
        extra = sorted(set(auth) - AUTH_KEYS)
        if extra:
            raise Unsupported("access point %r auth carries %s (only a PSK network is migrated)" % (ssid, ", ".join(extra)))
        if auth.get("key-management", "psk") != "psk":
            raise Unsupported("access point %r is key-management %r (only psk is migrated)" % (ssid, auth.get("key-management")))
        if password is not None and auth.get("password") is not None:
            raise Unsupported("access point %r sets a password twice" % ssid)
        password = password if password is not None else auth.get("password")
    if not isinstance(password, str) or password == "":
        raise Unsupported("access point %r has no password (an open network is not migrated)" % ssid)
    return password


def extract(netplan_dir, iface):
    """The migration's view of the one netplan YAML that defines iface, or None when none does.

    Returns {"file", "country", "aps": [(ssid, passphrase), ...]}. Raises Unsupported on a shape the
    migration cannot carry over whole."""
    found = find_wifi_yamls(netplan_dir, iface)
    if not found:
        return None
    if len(found) > 1:
        raise Unsupported("more than one netplan file defines wifis.%s: %s" % (iface, ", ".join(p for p, _ in found)))
    path, doc = found[0]
    net = doc["network"]
    extra = sorted(set(net) - NETWORK_KEYS)
    if extra:
        raise Unsupported("%s also configures %s -- move the WiFi into its own YAML first" % (path, ", ".join(extra)))
    if sorted(net["wifis"]) != [iface]:
        raise Unsupported("%s also configures the WiFi interface(s) %s" % (path, ", ".join(sorted(set(net["wifis"]) - {iface}))))
    if net.get("renderer", "networkd") != "networkd" or not isinstance(net["wifis"][iface], dict):
        raise Unsupported("%s is not a networkd WiFi mapping for %s" % (path, iface))
    cfg = net["wifis"][iface]
    extra = sorted(set(cfg) - IFACE_KEYS)
    if extra:
        raise Unsupported("%s: wifis.%s carries %s, which the migration does not carry over" % (path, iface, ", ".join(extra)))
    if cfg.get("renderer", "networkd") != "networkd":
        raise Unsupported("%s: wifis.%s is not rendered by networkd" % (path, iface))
    if str(cfg.get("dhcp4", "")).lower() not in TRUE_WORDS:
        raise Unsupported("%s: wifis.%s does not use DHCP (dhcp4), which the migration writes" % (path, iface))
    country = cfg.get("regulatory-domain", "")
    if not isinstance(country, str) or (country and not COUNTRY_RE.match(country.upper())):
        raise Unsupported("%s: wifis.%s regulatory-domain is not a two-letter country code" % (path, iface))
    aps = cfg.get("access-points")
    if not isinstance(aps, dict) or not aps:
        raise Unsupported("%s: wifis.%s has no access-points" % (path, iface))
    return {
        "file": path,
        "country": country.upper(),
        "aps": [(ssid, _password_of(ssid, ap)) for ssid, ap in aps.items()],
    }


def _emit(tag, value):
    sys.stdout.write(tag + "\0" + value + "\0")


def main(argv):
    if len(argv) != 3:
        sys.stderr.write("usage: bkshading_sbc_netplan_wifi.py <netplan-dir> <iface>\n")
        return 2
    try:
        found = extract(argv[1], argv[2])
    except Unsupported as e:
        sys.stderr.write("ERROR: the netplan WiFi config cannot be migrated: %s\n" % e)
        return 2
    if found is None:
        return 3
    _emit("file", found["file"])
    _emit("country", found["country"])
    for ssid, password in found["aps"]:
        _emit("ssid", ssid)
        _emit("pass", password)
    sys.stdout.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
