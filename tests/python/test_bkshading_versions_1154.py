#!/usr/bin/env python3
"""bkshading crate versions must inherit the single root workspace version (issue 1154).

Before this fix, the three bkshading members (`bkshading/proto|relay|service`) each
hard-coded their own `[package].version` string, while the repo's version-bump discipline
(and the three literal `^version = "X"` readers -- camera-box-version-gate.sh:169,
recording-e2e.sh:903, rig-status.py) only ever touch the ROOT `Cargo.toml`. So a root-only
bump left the members stale, and that stale version leaked into the bkshading panel DOM /
`/api/version` / `RelayState.version` (a version-on-dashboard lie).

The fix is Cargo's native `[workspace.package].version` inheritance: ONE literal version at
the root, inherited by the root appliance package AND all three members via
`version.workspace = true`. This test pins the INVARIANT that guarantees a single bump
propagates everywhere -- no crate (root appliance included) is allowed to hard-code its own
version string; each must inherit the one workspace version.

Pure `tomllib` parse -- no cargo build, so it runs in the `python-tests` CI job (no Rust
toolchain). A supplementary cargo-metadata check (skipped when cargo is absent) proves the
resolved value-level uniformity where a toolchain is available. Runnable directly or under
pytest.
"""
import json
import os
import re
import shutil
import subprocess

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib  # type: ignore

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
ROOT_MANIFEST = os.path.join(REPO, "Cargo.toml")
MEMBER_MANIFESTS = {
    "bkshading-proto": os.path.join(REPO, "bkshading", "proto", "Cargo.toml"),
    "bkshading-relay": os.path.join(REPO, "bkshading", "relay", "Cargo.toml"),
    "bkshading": os.path.join(REPO, "bkshading", "service", "Cargo.toml"),
}

# A dev version like "1.7.0-dev.521" (or a plain "1.7.0" release).
_VERSION_RE = re.compile(r"^\d+\.\d+\.\d+(?:-[.A-Za-z0-9]+)?$")

# The value tomllib yields for `version.workspace = true` / `version = { workspace = true }`.
_INHERIT = {"workspace": True}


def _load(path):
    with open(path, "rb") as fh:
        return tomllib.load(fh)


def test_root_defines_exactly_one_workspace_package_version():
    """The single source of truth: `[workspace.package].version` at the root."""
    root = _load(ROOT_MANIFEST)
    ws_pkg = root.get("workspace", {}).get("package", {})
    assert "version" in ws_pkg, (
        "root Cargo.toml must define [workspace.package].version as the ONE source of "
        "truth for the appliance + bkshading crates (issue 1154)"
    )
    version = ws_pkg["version"]
    assert isinstance(version, str) and _VERSION_RE.match(version), (
        f"[workspace.package].version must be a concrete version string, got {version!r}"
    )


def test_root_appliance_package_inherits_the_workspace_version():
    """The appliance package must NOT hard-code its own version -- it inherits, so a
    single bump of [workspace.package].version moves the appliance too."""
    root = _load(ROOT_MANIFEST)
    pkg_version = root["package"]["version"]
    assert pkg_version == _INHERIT, (
        "root [package].version must be `version.workspace = true` (inherit), not a "
        f"hard-coded literal, so it can never drift; got {pkg_version!r}"
    )


def test_every_bkshading_member_inherits_the_workspace_version():
    """No bkshading member may hard-code a version -- each inherits the workspace one,
    which is exactly what stops the #1154 drift from ever recurring."""
    for name, path in MEMBER_MANIFESTS.items():
        member = _load(path)
        pkg_version = member["package"]["version"]
        assert pkg_version == _INHERIT, (
            f"{name} ({os.path.relpath(path, REPO)}) [package].version must be "
            f"`version.workspace = true` (inherit), not a hard-coded literal; got "
            f"{pkg_version!r}"
        )


def test_no_manifest_hardcodes_a_literal_package_version():
    """Belt-and-suspenders: across the appliance + all three members, the ONLY literal
    version string lives in root [workspace.package]."""
    literals = []
    for path in [ROOT_MANIFEST, *MEMBER_MANIFESTS.values()]:
        data = _load(path)
        v = data.get("package", {}).get("version")
        if isinstance(v, str):
            literals.append((os.path.relpath(path, REPO), v))
    assert not literals, (
        "no [package].version may be a hard-coded literal -- all four crates must "
        f"inherit the single [workspace.package].version; found literals: {literals}"
    )


def test_cargo_metadata_resolves_a_uniform_version():
    """Supplementary value-level proof: where a cargo toolchain exists, every workspace
    package resolves to the SAME version, equal to [workspace.package].version. Skipped in
    the python-tests CI job (no Rust toolchain); runs locally + wherever cargo is present.
    `cargo metadata` is Tier-0-allowed (no compile)."""
    if shutil.which("cargo") is None:
        import pytest

        pytest.skip("cargo not on PATH (e.g. the python-tests CI job) -- structural "
                    "tomllib tests above are authoritative")

    expected = _load(ROOT_MANIFEST)["workspace"]["package"]["version"]
    out = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1",
         "--manifest-path", ROOT_MANIFEST],
        capture_output=True, text=True, cwd=REPO,
    )
    assert out.returncode == 0, (
        f"cargo metadata failed (rc={out.returncode}) -- a malformed workspace manifest "
        f"would break the resolved version too:\n{out.stderr}"
    )
    meta = json.loads(out.stdout)
    resolved = {pkg["name"]: pkg["version"] for pkg in meta["packages"]}
    # Every crate in this workspace (appliance + 3 bkshading members) is present.
    for name in ["camera-box", *MEMBER_MANIFESTS.keys()]:
        assert name in resolved, f"{name} missing from cargo metadata packages"
        assert resolved[name] == expected, (
            f"{name} resolved to {resolved[name]!r}, expected the single workspace "
            f"version {expected!r} -- versions have drifted"
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
