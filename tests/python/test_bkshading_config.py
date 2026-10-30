#!/usr/bin/env python3
"""M1 validation of the bkshading service config schema (issue 808).

Parses the shipped example config and asserts the shape the Rust `ServiceConfig` expects:
each camera has an id/label/transport/address, `transport` is one of the owner-approved
values (USB relay / SBC relay / ethernet REST — never Bluetooth), and `ndi_preview` is
optional (a camera without a feed may omit it). Keeps the TOML the Rust deserialiser reads
honest without a cargo build. Runnable directly or under pytest.
"""
import os

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib  # type: ignore

HERE = os.path.dirname(os.path.abspath(__file__))
EXAMPLE = os.path.join(HERE, "..", "..", "bkshading", "service", "bkshading.example.toml")

VALID_TRANSPORTS = {"cambox-relay", "sbc-relay", "ethernet-rest"}


def _load():
    with open(EXAMPLE, "rb") as fh:
        return tomllib.load(fh)


def test_example_config_parses_and_has_cameras():
    cfg = _load()
    assert "bind" in cfg
    assert isinstance(cfg.get("camera"), list) and cfg["camera"], "expected [[camera]] tables"


def test_every_camera_has_required_fields_and_valid_transport():
    cfg = _load()
    ids = set()
    for cam in cfg["camera"]:
        for field in ("id", "label", "transport", "address"):
            assert field in cam, f"camera missing '{field}': {cam}"
        assert cam["transport"] in VALID_TRANSPORTS, f"bad transport: {cam['transport']}"
        assert "bluetooth" not in cam["transport"].lower(), "Bluetooth is not a valid transport"
        assert cam["id"] not in ids, f"duplicate camera id {cam['id']}"
        ids.add(cam["id"])


def test_the_panel_is_cam1_to_cam7_and_no_handheld_panel():
    # Owner ruling 5977504457 (4.10.2026): the panel lists cam1..cam7, one block per cambox; a
    # handheld SBC is the CONTROL of the cambox its video reaches, never its own panel.
    cfg = _load()
    assert [c["id"] for c in cfg["camera"]] == ["cam%d" % n for n in range(1, 8)]
    assert not any(c["id"].startswith("handheld") for c in cfg["camera"])


def test_a_handheld_controls_a_cambox_block_that_keeps_its_preview():
    # The example shows the SNV state: cam3's camera is the handheld with the Teradek, so the cam3
    # block is driven by the handheld-1 SBC relay and still previews CAM3 (usb).
    cfg = _load()
    cams = {c["id"]: c for c in cfg["camera"]}
    assert cams["cam3"]["transport"] == "sbc-relay"
    assert cams["cam3"]["address"] == "register:handheld-1"
    for n in range(1, 8):
        assert cams["cam%d" % n]["ndi_preview"] == "CAM%d (usb)" % n


def test_preview_table_is_valid_when_present():
    # M2: the optional [preview] table tunes the live preview. When present it must carry a
    # sane fps and quality (the Rust PreviewConfig defaults them, but the shipped example
    # documents them, so keep the example honest).
    cfg = _load()
    if "preview" in cfg:
        prev = cfg["preview"]
        assert isinstance(prev.get("fps"), (int, float)) and prev["fps"] > 0
        assert 0 < prev.get("jpeg_quality", 55) <= 100


def test_every_cambox_declares_grab_fps():
    # issue 809: every cambox is grabbed at 60 fps; the example documents the grab-mode field so the
    # panel can warn on a camera-fps mismatch (an sbc-relay block too: its video is the cambox grab).
    cfg = _load()
    assert all(c.get("grab_fps") == 60 for c in cfg["camera"])


def _run():
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for fn in fns:
        fn()
        print(f"ok  {fn.__name__}")
    print(f"\n{len(fns)} passed")


if __name__ == "__main__":
    _run()
