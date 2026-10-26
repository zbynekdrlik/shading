"""issue 808 (owner ruling 5977504457, 4.10.2026): one panel block per cambox, and a handheld SBC is the
CONTROL of the cambox its video reaches, never its own panel. Owner 26.10.2026: "ale v pp urcite nie je 7
kamier!!!" / "cela zostava kamier je snv plus pp ked sme na spolocnej akcii, inak su rozdelene medzi dva zbory"
-- a box's panel lists ITS venue's cameras (`STRIH_CAMERAS`: strih-snv 2 3 5, strih-pp 1 4 6 7).

The provisioning seed `strih_bkshading_config_text` (setup-strih step 16c) still wrote cam1 + three
standalone `handheld-1..3` panels. It now renders cam1..cam7 from the box fact `STRIH_BKSHADING_RELAYS`:
`none` or space-separated `N=<transport>@<host>:<port>` overrides of the default `cambox-relay` at
`camN.lan:8771`. The expected panels below are the live `/etc/bkshading/bkshading.toml` of each box,
read 26.10.2026 (strih-pp minus its three stale handheld panels, which the ruling removes).
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
    """One block per venue camera: every one keeps its cambox preview + grab fps, the transport + address vary."""
    want = []
    for n in cams:
        transport, address = addresses[n]
        want.append({"id": "cam%d" % n, "label": "Cam %d" % n, "transport": transport, "address": address,
                     "ndi_preview": "CAM%d (usb)" % n, "grab_fps": 60})
    return want


SNV_LIVE = _expected((2, 3, 5), {n: ("cambox-relay", "cam%d.lan:8771" % n) for n in range(1, 8)}
                     | {3: ("sbc-relay", "handheld-1.lan:8771")})
PP_LIVE = _expected((1, 4, 6, 7), {n: ("cambox-relay", "10.77.9.6%d:8771" % n) for n in range(1, 8)}
                    | {4: ("sbc-relay", "10.77.8.70:8771")})


# --- the fact ---------------------------------------------------------------------------------------------------

@pytest.mark.parametrize("box,want", [
    ("strih-snv", "3=sbc-relay@handheld-1.lan:8771"),
    ("strih-pp", "1=cambox-relay@10.77.9.61:8771 4=sbc-relay@10.77.8.70:8771 6=cambox-relay@10.77.9.66:8771 "
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
    assert "handheld-1\"" not in text and 'id = "handheld' not in text, "a handheld is never its own panel"


def test_an_sbc_relay_block_names_the_ruling(boxes):
    text, _cfg = _seed(boxes, "strih-snv")
    block = text.split('id = "cam3"')[1].split("[[camera]]")[0]
    assert "# a handheld SBC relay controls this cambox's camera" in block, block


def test_explicit_args_override_the_bind_and_the_relays(boxes):
    _t, cfg = _seed(boxes, "strih-snv", "127.0.0.1:9999", "5=sbc-relay@handheld-2.lan:8771")
    assert cfg["bind"] == "127.0.0.1:9999"
    cams = {c["id"]: c for c in cfg["camera"]}
    assert cams["cam5"]["transport"] == "sbc-relay" and cams["cam5"]["address"] == "handheld-2.lan:8771"
    assert cams["cam3"]["transport"] == "cambox-relay" and cams["cam3"]["address"] == "cam3.lan:8771"
    assert sorted(cams) == ["cam2", "cam3", "cam5"], "only the box's own venue cameras"
    _t, cfg = _seed(boxes, "strih-snv", "0.0.0.0:8770", "none")
    assert all(c["transport"] == "cambox-relay" for c in cfg["camera"])


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
