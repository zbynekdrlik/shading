#!/usr/bin/env python3
"""bkshading service: real NDI preview is the DEFAULT build (issue 1157, owner decision 2026-08-24).

The M2 follow-up shipped the real libndi preview receiver behind `--features ndi` with the service
member's `[features] default = []` (the STUB test-pattern source was the default). The owner then
decided (issue 1157 comment 5393834171, možnosť 1 — in line with the `features-default-on` rule)
that the real NDI receive path must be ON in the DEFAULT build. This test pins that decision plus
the two invariants that keep it honest:

  1. the SERVICE member's `[features].default` includes `"ndi"`, and the `ndi` feature is still the
     `dep:libloading` seam (a runtime dynamic load, so this still compiles on CI without libndi);
  2. CI still proves the `--no-default-features` (stub / libndi-free) build path — which the old
     `default = []` build proved for free and would otherwise lose ALL coverage once ndi is default;
  3. the COLOR_FORMAT misnomer stays renamed (`COLOR_FORMAT_BGRX_BGRA`, never `_UYVY_BGRA`) — value
     0 is `NDIlib_recv_color_format_BGRX_BGRA` per the installed SDK header.

Pure `tomllib` + `yaml` parse -- no cargo build, so it runs in the `python-tests` CI job (no Rust
toolchain, per the local-build ban #557). Runnable directly or under pytest.
"""
import os
import re

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib  # type: ignore

import yaml

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
SERVICE_MANIFEST = os.path.join(REPO, "bkshading", "service", "Cargo.toml")
NDI_SOURCE = os.path.join(REPO, "bkshading", "service", "src", "preview", "ndi_source.rs")
CI_YML = os.path.join(REPO, ".github", "workflows", "ci.yml")


def _load_toml(path):
    with open(path, "rb") as fh:
        return tomllib.load(fh)


def _load_ci():
    with open(CI_YML) as fh:
        return yaml.safe_load(fh)


def _job_step_runs(job):
    return "\n".join(s.get("run", "") for s in job.get("steps", []))


def test_service_default_features_include_ndi():
    """The core owner decision: the real NDI receive path is ON in the default build."""
    manifest = _load_toml(SERVICE_MANIFEST)
    default = manifest.get("features", {}).get("default", [])
    assert "ndi" in default, (
        "bkshading/service Cargo.toml [features].default must include \"ndi\" (owner decision "
        f"2026-08-24, issue 1157 — features-default-on); got default = {default!r}"
    )


def test_ndi_feature_is_still_the_libloading_seam():
    """`ndi` must remain the libloading runtime-load seam (compiles on CI without libndi)."""
    manifest = _load_toml(SERVICE_MANIFEST)
    features = manifest.get("features", {})
    assert features.get("ndi") == ["dep:libloading"], (
        "the `ndi` feature must still map to [\"dep:libloading\"] (a RUNTIME dynamic load, so the "
        f"default-on build still compiles on CI with no libndi installed); got {features.get('ndi')!r}"
    )


def test_libloading_stays_optional():
    """libloading stays an optional dep activated by the (now default) `ndi` feature -- the
    standard optional-dep idiom; it must not become an unconditional dependency."""
    manifest = _load_toml(SERVICE_MANIFEST)
    dep = manifest.get("dependencies", {}).get("libloading")
    assert isinstance(dep, dict) and dep.get("optional") is True, (
        "libloading must stay `optional = true` (activated by the default `ndi` feature), not an "
        f"unconditional dependency; got {dep!r}"
    )


def test_ci_still_proves_the_no_default_features_stub_path():
    """Now that ndi is the default, the plain `cargo ... -p bkshading` steps cover the REAL path;
    CI must still compile/test the `--no-default-features` (stub / libndi-free) path so it can't
    bit-rot -- the coverage the old `default = []` build provided for free."""
    ci = _load_ci()
    runs = _job_step_runs(ci["jobs"]["bkshading"])
    assert re.search(r"cargo (?:clippy|test)[^\n]*-p bkshading\b[^\n]*--no-default-features", runs) or \
        re.search(r"cargo (?:clippy|test)[^\n]*--no-default-features[^\n]*-p bkshading\b", runs), (
        "the bkshading CI job must keep a `--no-default-features` clippy/test step for the service "
        "so the stub (libndi-free) path stays proven once ndi is the default build"
    )


def test_ci_windows_check_covers_the_default_ndi_path():
    """The strih ship target is Windows; with ndi default, the plain `cargo check -p bkshading`
    on the windows job is now the real-ndi compile of the exact deployed config."""
    ci = _load_ci()
    runs = _job_step_runs(ci["jobs"]["bkshading-windows"])
    assert re.search(r"cargo check[^\n]*-p bkshading\b", runs), (
        "the bkshading-windows job must `cargo check -p bkshading` (now the default = ndi compile "
        "of the strih ship target)"
    )


def test_color_format_misnomer_stays_renamed():
    """value 0 is NDIlib_recv_color_format_BGRX_BGRA per the installed SDK header (issue 1157
    deferral 2, resolved in the #808 SBC lane) -- the misnamed constant must never come back."""
    src = open(NDI_SOURCE, encoding="utf-8").read()
    assert "COLOR_FORMAT_UYVY_BGRA" not in src, (
        "the misnamed COLOR_FORMAT_UYVY_BGRA must stay renamed -- value 0 is BGRX_BGRA per "
        "Processing.NDI.Recv.h"
    )
    assert "COLOR_FORMAT_BGRX_BGRA" in src, (
        "ndi_source.rs must define/use COLOR_FORMAT_BGRX_BGRA (the correct name for value 0)"
    )


def _run():
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    passed = 0
    for fn in fns:
        try:
            fn()
        except Exception as exc:  # surface RED clearly when run directly
            print(f"FAIL {fn.__name__}: {exc}")
            continue
        print(f"ok  {fn.__name__}")
        passed += 1
    print(f"\n{passed}/{len(fns)} passed")
    return passed == len(fns)


if __name__ == "__main__":
    import sys

    sys.exit(0 if _run() else 1)
