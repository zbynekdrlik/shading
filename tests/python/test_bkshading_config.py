#!/usr/bin/env python3
"""M1 validation of the bkshading service config schema (issue 808).

Parses the shipped example config and asserts the shape the Rust `ServiceConfig` expects:
each camera has an id/label/transport/address, `transport` is one of the owner-approved
values (USB relay / SBC relay / ethernet REST — never Bluetooth), and `ndi_preview` is
optional (a handheld without a feed omits it). Keeps the TOML the Rust deserialiser reads
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


def test_ndi_preview_is_optional():
    cfg = _load()
    cams = {c["id"]: c for c in cfg["camera"]}
    # cam1 has a preview; the handheld omits it (params-only block).
    assert "ndi_preview" in cams["cam1"]
    assert "ndi_preview" not in cams["handheld-1"]


def test_preview_table_is_valid_when_present():
    # M2: the optional [preview] table tunes the live preview. When present it must carry a
    # sane fps and quality (the Rust PreviewConfig defaults them, but the shipped example
    # documents them, so keep the example honest).
    cfg = _load()
    if "preview" in cfg:
        prev = cfg["preview"]
        assert isinstance(prev.get("fps"), (int, float)) and prev["fps"] > 0
        assert 0 < prev.get("jpeg_quality", 55) <= 100


def test_cam1_declares_grab_fps_and_handheld_omits_it():
    # issue 809: cam1 is grabbed at 60 fps; the example documents the grab-mode field so the
    # panel can warn on a camera-fps mismatch. grab_fps is optional (handheld omits it).
    cfg = _load()
    cams = {c["id"]: c for c in cfg["camera"]}
    assert cams["cam1"].get("grab_fps") == 60
    assert "grab_fps" not in cams["handheld-1"]


def _run():
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for fn in fns:
        fn()
        print(f"ok  {fn.__name__}")
    print(f"\n{len(fns)} passed")


if __name__ == "__main__":
    _run()
