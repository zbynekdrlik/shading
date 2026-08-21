#!/usr/bin/env python3
"""bkshading NDI-runtime provisioning + cross-platform discovery (issue 1157).

M2 (issue 808) shipped the real libndi preview receiver behind `--features ndi`, but its
`NdiLib::load()` was copied verbatim from the appliance's Linux-only `src/ndi.rs` — so it could
never load on the strih PC (Windows), the service's ship target, where the runtime is
`Processing.NDI.Lib.x64.dll` (documented at scripts/bundle-state-server.py). #1157 makes discovery
cross-platform via a pure, unit-tested `ndi_paths` module, and adds a provision/verify script
(+ a sourceable helper) for the libndi runtime.

These stdlib-only structural tests run in the `python-tests` CI job (no Rust toolchain):
 - the provision script + helper parse (`bash -n`) and the pure helper's outputs are correct;
 - the shell helper's Linux dirs/names + the documented Windows DLL AGREE with the Rust
   `ndi_paths.rs` discovery tables, so the two sources of truth cannot silently drift.
Runnable directly (`python3 tests/python/test_bkshading_ndi_provision_1157.py`) or under pytest.
"""
import os
import shutil
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
SCRIPT = os.path.join(REPO, "scripts", "bkshading-provision-ndi.sh")
LIB = os.path.join(REPO, "scripts", "lib", "bkshading-ndi-runtime.sh")
NDI_PATHS_RS = os.path.join(
    REPO, "bkshading", "service", "src", "preview", "ndi_paths.rs"
)

WINDOWS_DLL = r"C:\Program Files\NDI\NDI 6 Tools\Runtime\Processing.NDI.Lib.x64.dll"
WINDOWS_RUNTIME_DIR = r"C:\Program Files\NDI\NDI 6 Tools\Runtime"
LINUX_DIRS = ["/usr/lib/ndi", "/usr/local/lib/ndi", "/opt/ndi/lib"]
LINUX_NAMES = ["libndi.so.6", "libndi.so.5", "libndi.so"]
ENV_VARS = ["NDI_RUNTIME_DIR_V6", "NDI_RUNTIME_DIR_V5", "NDI_RUNTIME_DIR"]


def _bash(snippet):
    """Source the helper, run `snippet`, return stripped stdout (raises on nonzero)."""
    src = '. "%s"\n%s' % (LIB, snippet)
    out = subprocess.run(
        ["bash", "-c", src], capture_output=True, text=True, check=True
    )
    return out.stdout


def _first_match(names, listing):
    """Return (returncode, stdout) of bkshading_ndi_first_match — args passed via env (no quoting pain)."""
    src = '. "%s"\nbkshading_ndi_first_match "$BK_NAMES" "$BK_LISTING"' % LIB
    env = dict(os.environ, BK_NAMES=names, BK_LISTING=listing)
    r = subprocess.run(["bash", "-c", src], capture_output=True, text=True, env=env)
    return r.returncode, r.stdout.strip()


def test_scripts_exist_and_parse():
    assert os.path.isfile(SCRIPT), SCRIPT
    assert os.path.isfile(LIB), LIB
    for p in (SCRIPT, LIB):
        r = subprocess.run(["bash", "-n", p], capture_output=True, text=True)
        assert r.returncode == 0, "bash -n %s: %s" % (p, r.stderr)


def test_helper_linux_dirs_names_env():
    assert _bash("bkshading_ndi_linux_dirs").split() == LINUX_DIRS
    assert _bash("bkshading_ndi_linux_names").split() == LINUX_NAMES
    assert _bash("bkshading_ndi_env_vars").split() == ENV_VARS


def test_helper_windows_dll_is_the_documented_path():
    dll = _bash("bkshading_ndi_windows_dll").strip()
    assert dll == WINDOWS_DLL, dll
    assert "Processing.NDI.Lib.x64.dll" in dll
    assert "NDI 6 Tools" in dll


def test_helper_first_match_prefers_by_name_order():
    rc, m = _first_match(" ".join(LINUX_NAMES), "libndi.so\nlibndi.so.6")
    assert rc == 0 and m == "libndi.so.6", (rc, m)
    rc2, m2 = _first_match(" ".join(LINUX_NAMES), "foo\nlibndi.so")
    assert rc2 == 0 and m2 == "libndi.so", (rc2, m2)


def test_helper_first_match_no_match_is_nonzero_and_empty():
    rc, m = _first_match("libndi.so.6", "nope\nother")
    assert rc != 0, "no-match must return nonzero"
    assert m == "", m


def test_rust_and_shell_agree_no_drift():
    with open(NDI_PATHS_RS, encoding="utf-8") as f:
        rs = f.read()
    # Windows discovery: the runtime DLL name + the documented NDI Tools runtime dir.
    assert "Processing.NDI.Lib.x64.dll" in rs
    assert WINDOWS_RUNTIME_DIR in rs
    # Linux discovery: the same appliance dirs + names the shell helper lists.
    for d in LINUX_DIRS:
        assert d in rs, d
    for n in LINUX_NAMES:
        assert n in rs, n
    for e in ENV_VARS:
        assert e in rs, e


def test_check_finds_a_fixture_runtime_dir():
    d = tempfile.mkdtemp()
    try:
        open(os.path.join(d, "libndi.so.6"), "w").close()
        env = dict(os.environ, NDI_RUNTIME_DIR_V6=d)
        r = subprocess.run(
            ["bash", SCRIPT, "--check"], capture_output=True, text=True, env=env
        )
        assert r.returncode == 0, r.stderr
        assert "discoverable" in r.stdout
        assert d in r.stdout
    finally:
        shutil.rmtree(d, ignore_errors=True)


def test_unknown_arg_exits_2():
    r = subprocess.run(
        ["bash", SCRIPT, "--bogus"], capture_output=True, text=True
    )
    assert r.returncode == 2, r.returncode


if __name__ == "__main__":
    for _name, _fn in sorted(globals().items()):
        if _name.startswith("test_") and callable(_fn):
            _fn()
            print("ok %s" % _name)
    print("all passed")
