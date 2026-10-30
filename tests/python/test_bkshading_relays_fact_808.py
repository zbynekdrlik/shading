"""issue 808 (owner ruling 5977504457, 4.10.2026): one panel block per cambox, and a handheld SBC is the
CONTROL of the cambox its video reaches, never its own panel. Owner 26.10.2026: "ale v pp urcite nie je 7
kamier!!!" / "cela zostava kamier je snv plus pp ked sme na spolocnej akcii, inak su rozdelene medzi dva zbory"
-- a box's panel lists ITS venue's cameras (`STRIH_CAMERAS`: strih-snv 2 3 5, strih-pp 1 4 6 7).

The provisioning seed `strih_bkshading_config_text` (setup-strih step 16c) still wrote cam1 + three
standalone `handheld-1..3` panels. It now renders cam1..cam7 from the box fact `STRIH_BKSHADING_RELAYS`:
`none` or space-separated `N=<transport>@<host>:<port>` overrides of the default `cambox-relay` at
`camN.lan:8771`. The expected panels below are the live `/etc/bkshading/bkshading.toml` of each box,
read 26.10.2026 (strih-pp minus its three stale handheld panels, which the ruling removes).

Issue 1431 (joint event: strih.lan is the ONE switch): every cambox relay registers itself with the strih
behind strih.lan as `camN`, and the service's camera list is static, so BOTH strih boxes now declare all seven
cameras. A venue camera is `register:camN` with its literal relay as `fallback_address`; a camera of the other
venue is `visiting = true` and the panel lists it only once its relay registered (still venue-only at a normal
event). A handheld override (`N=sbc-relay@...`) stays exactly as before.
"""
import os
import subprocess
import tomllib
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
BOXES = REPO / "scripts" / "strih-boxes"
SETUP = REPO / "scripts" / "setup-strih.sh"


@pytest.fixture
def boxes(tmp_path):
    d = tmp_path / "boxes"
    d.mkdir()
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text().replace("STRIH_NDI_RUNTIME_PEER=TODO_OWNER",
                                                       "STRIH_NDI_RUNTIME_PEER=10.77.9.61"))
    return d


def _bash(body, boxes, *args):
    env = {k: v for k, v in os.environ.items() if not k.startswith("STRIH_") and k != "OBS_FLEET"}
    env["STRIH_BOXES_DIR"] = str(boxes)
    script = 'set -euo pipefail\n. scripts/lib/strih-box-facts.sh\n. scripts/lib/strih-provision.sh\n' + body
    return subprocess.run(["bash", "-c", script, "harness", *args], capture_output=True, text=True, env=env,
                          cwd=str(REPO), timeout=60)


def _expected(cams, addresses):
    """The venue cameras first (register-first, the literal relay as the fallback; a handheld override as it
    is), then every other camera as a visiting block; each keeps its cambox preview + grab fps (issue 1431)."""
    want = []
    for n in cams:
        transport, address = addresses[n]
        block = {"id": "cam%d" % n, "label": "Cam %d" % n, "transport": transport}
        if transport == "cambox-relay":
            block.update({"address": "register:cam%d" % n, "fallback_address": address})
        else:
            block["address"] = address
        block.update({"ndi_preview": "CAM%d (usb)" % n, "grab_fps": 60})
        want.append(block)
    for n in range(1, 8):
        if n not in cams:
            want.append({"id": "cam%d" % n, "label": "Cam %d" % n, "transport": "cambox-relay",
                         "address": "register:cam%d" % n, "visiting": True,
                         "ndi_preview": "CAM%d (usb)" % n, "grab_fps": 60})
    return want


SNV_LIVE = _expected((2, 3, 5), {n: ("cambox-relay", "cam%d.lan:8771" % n) for n in range(1, 8)}
                     | {3: ("sbc-relay", "register:handheld-1")})
PP_LIVE = _expected((1, 4, 6, 7), {n: ("cambox-relay", "10.77.9.6%d:8771" % n) for n in range(1, 8)}
                    | {4: ("sbc-relay", "register:handheld-1")})


# --- the fact ---------------------------------------------------------------------------------------------------

@pytest.mark.parametrize("box,want", [
    ("strih-snv", "3=sbc-relay@register:handheld-1"),
    ("strih-pp", "1=cambox-relay@10.77.9.61:8771 4=sbc-relay@register:handheld-1 6=cambox-relay@10.77.9.66:8771 "
                 "7=cambox-relay@10.77.9.67:8771"),
    ("stream-snv", "none"),
    ("stream-pp", "none"),
])
def test_every_box_declares_its_shading_relays(boxes, box, want):
    r = _bash('strih_box_load "$1" || exit 9\nstrih_lx_bkshading_relays', boxes, box)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == want


@pytest.mark.parametrize("box,new,word", [
    ("strih-snv", "8=sbc-relay@x.lan:8771", "1..7"),
    ("strih-snv", "0=sbc-relay@x.lan:8771", "1..7"),
    ("strih-snv", "3=sbc-relay@a.lan:8771 3=cambox-relay@b.lan:8771", "twice"),
    ("strih-snv", "3=bluetooth@a.lan:8771", "transport"),
    ("strih-snv", "3=sbc-relay@a.lan", "host:port"),
    ("strih-snv", "3=sbc-relay@-x.lan:8771", "host:port"),
    ("strih-snv", "3=sbc-relay@a.lan:99999", "host:port"),
    ("strih-snv", "none 3=sbc-relay@a.lan:8771", "N=transport@host:port"),
    ("strih-snv", "", "empty"),
    ("stream-snv", "3=sbc-relay@a.lan:8771", "stream role"),
    ("strih-snv", "4=sbc-relay@a.lan:8771", "STRIH_CAMERAS"),
    # owner 28.10.2026: a register id only names an sbc-relay, and only a plain hostname-like id.
    ("strih-snv", "3=cambox-relay@register:cam3", "register:<id>"),
    ("strih-snv", "3=sbc-relay@register:", "register:<id>"),
    ("strih-snv", "3=sbc-relay@register:hand held", "N=transport@host:port"),
    ("strih-snv", "3=sbc-relay@register:h\u00e1ndheld", "unsafe character"),
    ("strih-snv", "3=sbc-relay@register:" + "a" * 65, "register:<id>"),
])
def test_the_loader_refuses_a_wrong_relay_list(boxes, box, new, word):
    f = boxes / ("%s.env" % box)
    lines = f.read_text().splitlines()
    i = [n for n, ln in enumerate(lines) if ln.startswith("STRIH_BKSHADING_RELAYS=")]
    assert len(i) == 1, "the fact file must declare STRIH_BKSHADING_RELAYS once"
    lines[i[0]] = "STRIH_BKSHADING_RELAYS=" + new
    f.write_text("\n".join(lines) + "\n")
    r = _bash('strih_box_load "$1"', boxes, box)
    assert r.returncode != 0
    assert "STRIH_BKSHADING_RELAYS" in r.stderr and word in r.stderr, r.stderr


# --- the seed ---------------------------------------------------------------------------------------------------

def _seed(boxes, box, *args):
    r = _bash('strih_box_load "$1" || exit 9\nshift\nstrih_bkshading_config_text "$@"', boxes, box, *args)
    assert r.returncode == 0, r.stderr
    return r.stdout, tomllib.loads(r.stdout)


@pytest.mark.parametrize("box,want", [("strih-snv", SNV_LIVE), ("strih-pp", PP_LIVE)])
def test_the_seed_writes_the_live_panel_of_the_box(boxes, box, want):
    text, cfg = _seed(boxes, box)
    assert cfg["bind"] == "0.0.0.0:8770"
    assert cfg["preview"] == {"fps": 3.0, "jpeg_quality": 55, "capture_timeout_ms": 1000,
                              "reconnect_backoff_ms": 2000}
    assert cfg["camera"] == want
    # A handheld is never its own panel block (its registration id may appear in an address, owner 28.10.2026).
    assert 'id = "handheld' not in text, "a handheld is never its own panel"


def test_an_sbc_relay_block_names_the_ruling(boxes):
    text, _cfg = _seed(boxes, "strih-snv")
    block = text.split('id = "cam3"')[1].split("[[camera]]")[0]
    assert "# a handheld SBC relay controls this cambox's camera" in block, block


def test_explicit_args_override_the_bind_and_the_relays(boxes):
    _t, cfg = _seed(boxes, "strih-snv", "127.0.0.1:9999", "5=sbc-relay@handheld-2.lan:8771")
    assert cfg["bind"] == "127.0.0.1:9999"
    cams = {c["id"]: c for c in cfg["camera"]}
    assert cams["cam5"]["transport"] == "sbc-relay" and cams["cam5"]["address"] == "handheld-2.lan:8771"
    assert "fallback_address" not in cams["cam5"], "a handheld named by its address has no fallback"
    assert cams["cam3"]["transport"] == "cambox-relay" and cams["cam3"]["address"] == "register:cam3"
    assert cams["cam3"]["fallback_address"] == "cam3.lan:8771"
    home = sorted(i for i, c in cams.items() if not c.get("visiting"))
    assert home == ["cam2", "cam3", "cam5"], "the box's own venue cameras are the ones always listed"
    _t, cfg = _seed(boxes, "strih-snv", "0.0.0.0:8770", "none")
    assert all(c["transport"] == "cambox-relay" for c in cfg["camera"])


@pytest.mark.parametrize("box,home", [("strih-snv", (2, 3, 5)), ("strih-pp", (1, 4, 6, 7))])
def test_both_strih_boxes_declare_all_seven_cameras_1431(boxes, box, home):
    # Issue 1431: strih.lan picks the strih; a visiting cambox registers there as camN, so its block must exist.
    _t, cfg = _seed(boxes, box)
    cams = {c["id"]: c for c in cfg["camera"]}
    assert sorted(cams) == ["cam%d" % n for n in range(1, 8)]
    for n in range(1, 8):
        c = cams["cam%d" % n]
        assert c.get("visiting", False) is (n not in home), c
        if c.get("visiting"):
            assert c["address"] == "register:cam%d" % n and "fallback_address" not in c, c
    # The venue cameras come first, in STRIH_CAMERAS order (the panel order).
    assert [c["id"] for c in cfg["camera"]][: len(home)] == ["cam%d" % n for n in home]


@pytest.mark.parametrize("bad", ["9=sbc-relay@a.lan:8771", "3=sbc-relay@a.lan", "3=wifi@a.lan:8771",
                                 "3=sbc-relay@a.lan:1 3=sbc-relay@b.lan:2"])
def test_the_seed_refuses_a_wrong_relay_list(boxes, bad):
    r = _bash('strih_box_load strih-snv || exit 9\nstrih_bkshading_config_text 0.0.0.0:8770 "$1"', boxes, bad)
    assert r.returncode != 0
    assert r.stdout == "", "a refused relay list must never print a partial config"


def test_setup_names_the_seeded_panel():
    s = SETUP.read_text()
    assert "cam1 + handhelds" not in s
    assert "seeded /etc/bkshading/bkshading.toml (the box's STRIH_CAMERAS" in s


def test_a_handheld_named_by_its_registration_id_is_seeded_as_register_808(boxes):
    # Owner 28.10.2026: the handheld's WiFi address is never a fact; the panel config names its id.
    _t, cfg = _seed(boxes, "strih-snv", "0.0.0.0:8770", "3=sbc-relay@register:handheld-1")
    cams = {c["id"]: c for c in cfg["camera"]}
    assert cams["cam3"]["transport"] == "sbc-relay"
    assert cams["cam3"]["address"] == "register:handheld-1"


def test_the_visiting_blocks_follow_the_fact_files_not_a_literal_range_1431(boxes):
    # A camera no strih venue lists gets no block; the visiting set is read from the fact files.
    f = boxes / "strih-pp.env"
    text = f.read_text().replace("STRIH_CAMERAS=1 4 6 7", "STRIH_CAMERAS=1 4 6")
    f.write_text(text.replace(" 7=cambox-relay@10.77.9.67:8771", ""))
    r = _bash('strih_box_load strih-snv || exit 9\nstrih_box_fleet_cameras', boxes)
    assert r.returncode == 0 and r.stdout.strip() == "1 2 3 4 5 6", r.stdout + r.stderr
    _t, cfg = _seed(boxes, "strih-snv")
    assert sorted(c["id"] for c in cfg["camera"]) == ["cam%d" % n for n in (1, 2, 3, 4, 5, 6)], cfg["camera"]
