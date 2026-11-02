#!/usr/bin/env python3
"""scripts/release.py (shading#1): the version gate, the --version contract, asset staging and the
publish step. Real git repos and real files in a tmp dir; the only fake is `gh` (the GitHub API,
an external network service) as a script on PATH that logs its argv."""
import hashlib
import importlib.util
import json
import os
import stat
import subprocess
import tarfile

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
SPEC = importlib.util.spec_from_file_location("release", os.path.join(REPO, "scripts", "release.py"))
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)

SHA = "0123456789abcdef0123456789abcdef01234567"


def cargo_toml(version):
    return f'[workspace]\nmembers = []\n\n[workspace.package]\nversion = "{version}"\n'


def git(repo, *args):
    subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True)


def make_repo(tmp_path, base_version, head_version):
    repo = tmp_path / "repo"
    repo.mkdir()
    git(repo, "init", "-q", "-b", "main")
    git(repo, "config", "user.email", "t@example.invalid")
    git(repo, "config", "user.name", "t")
    if base_version is None:
        (repo / "README").write_text("x\n")
    else:
        (repo / "Cargo.toml").write_text(cargo_toml(base_version))
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", "base")
    git(repo, "checkout", "-qb", "dev")
    (repo / "Cargo.toml").write_text(cargo_toml(head_version))
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "--allow-empty", "-m", "head")
    return str(repo)


# --- version ---------------------------------------------------------------------------------

def test_parse_version_reads_the_workspace_package_version():
    assert release.parse_version(cargo_toml("1.2.3")) == "1.2.3"


@pytest.mark.parametrize("text", ['[workspace]\nmembers = []\n', cargo_toml("1.2"), cargo_toml("1.2.3-rc1")])
def test_parse_version_refuses_a_missing_or_non_plain_version(text):
    with pytest.raises(release.ReleaseError):
        release.parse_version(text)


def test_the_real_workspace_version_parses():
    assert release.SEMVER.match(release.workspace_version())


def test_version_check_passes_a_higher_dev(tmp_path):
    repo = make_repo(tmp_path, "0.9.0", "0.10.0")  # numeric, not string, comparison
    assert release.version_check("main", repo=repo) == "ok: 0.10.0 > 0.9.0 (main)"


@pytest.mark.parametrize("base,head", [("0.1.0", "0.1.0"), ("0.2.0", "0.1.9"), ("1.0.0", "0.99.99")])
def test_version_check_refuses_an_equal_or_lower_dev(tmp_path, base, head):
    repo = make_repo(tmp_path, base, head)
    with pytest.raises(release.ReleaseError, match="bump"):
        release.version_check("main", repo=repo)


def test_version_check_passes_the_first_release(tmp_path):
    repo = make_repo(tmp_path, None, "0.1.0")
    assert "first release" in release.version_check("main", repo=repo)


def test_version_check_fails_on_an_unknown_ref(tmp_path):
    repo = make_repo(tmp_path, "0.1.0", "0.2.0")
    with pytest.raises(release.ReleaseError, match="git show"):
        release.version_check("no-such-ref", repo=repo)


# --- binary-version --------------------------------------------------------------------------

def fake_binary(tmp_path, output, rc=0):
    path = tmp_path / "bin"
    path.write_text(f"#!/bin/sh\nprintf '%s' '{output}'\nexit {rc}\n")
    path.chmod(0o755)
    return str(path)


def workspace(tmp_path, version):
    repo = tmp_path / "ws"
    repo.mkdir()
    (repo / "Cargo.toml").write_text(cargo_toml(version))
    return str(repo)


def test_binary_version_accepts_the_last_word_equal_to_the_workspace_version(tmp_path):
    binary = fake_binary(tmp_path, "bkshading-relay 0.4.2\n")
    assert release.binary_version(binary, repo=workspace(tmp_path, "0.4.2")) == "ok: bkshading-relay 0.4.2"


@pytest.mark.parametrize(
    "output,rc",
    [
        ("bkshading-relay 0.4.1\n", 0),  # stale version
        ("0.4.2 bkshading-relay\n", 0),  # version not the last word
        ("bkshading-relay 0.4.2\nextra\n", 0),  # two lines
        ("bkshading-relay 0.4.2\n", 1),  # failing exit
    ],
)
def test_binary_version_refuses_a_broken_contract(tmp_path, output, rc):
    binary = fake_binary(tmp_path, output, rc)
    with pytest.raises(release.ReleaseError):
        release.binary_version(binary, repo=workspace(tmp_path, "0.4.2"))


# --- stage -----------------------------------------------------------------------------------

def elf(machine):
    return b"\x7fELF" + bytes(14) + machine.to_bytes(2, "little") + b"payload"


def make_build(tmp_path, **override):
    build = tmp_path / "build"
    content = {
        release.RELAY_AMD64: elf(62),
        release.RELAY_ARM64: elf(183),
        release.SERVICE_LINUX: elf(62),
        release.SERVICE_WINDOWS: b"MZ" + bytes(30),
    }
    content.update(override)
    for name, src in release.ASSETS.items():
        if content[name] is None:
            continue
        path = build / src
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content[name])
        path.chmod(0o644)  # download-artifact drops the exec bit
    web = tmp_path / "web"
    web.mkdir()
    (web / "index.html").write_text("<html></html>\n")
    return str(build), str(web)


def test_stage_writes_every_asset_and_a_matching_sha256(tmp_path):
    build, web = make_build(tmp_path)
    dist = tmp_path / "dist"
    names = release.stage(build, str(dist), web_dir=web)
    assert names == sorted([*release.ASSETS, *(n + ".sha256" for n in release.ASSETS)])
    for name in release.ASSETS:
        data = (dist / name).read_bytes()
        line = (dist / (name + ".sha256")).read_text()
        assert line == f"{hashlib.sha256(data).hexdigest()}  {name}\n"
    for name in (release.RELAY_AMD64, release.RELAY_ARM64, release.SERVICE_WINDOWS):
        assert (dist / name).stat().st_mode & stat.S_IXUSR, name


def test_stage_tarball_holds_the_executable_service_and_its_web_assets(tmp_path):
    build, web = make_build(tmp_path)
    dist = tmp_path / "dist"
    release.stage(build, str(dist), web_dir=web)
    with tarfile.open(dist / release.SERVICE_LINUX) as tar:
        members = {m.name: m for m in tar.getmembers()}
    assert set(members) == {
        "bkshading-service/bkshading",
        "bkshading-service/web",
        "bkshading-service/web/index.html",
    }
    assert members["bkshading-service/bkshading"].mode & 0o111 == 0o111


@pytest.mark.parametrize(
    "asset,data",
    [
        (release.RELAY_ARM64, elf(62)),  # an amd64 relay shipped as arm64
        (release.RELAY_AMD64, elf(183)),
        (release.SERVICE_WINDOWS, elf(62)),
        (release.SERVICE_LINUX, b"#!/bin/sh\n"),
    ],
)
def test_stage_refuses_a_binary_built_for_the_wrong_machine(tmp_path, asset, data):
    build, web = make_build(tmp_path, **{asset: data})
    with pytest.raises(release.ReleaseError, match="machine"):
        release.stage(build, str(tmp_path / "dist"), web_dir=web)


def test_stage_refuses_a_missing_binary(tmp_path):
    build, web = make_build(tmp_path, **{release.RELAY_ARM64: None})
    with pytest.raises(release.ReleaseError, match="missing"):
        release.stage(build, str(tmp_path / "dist"), web_dir=web)


def test_the_real_panel_web_dir_exists():
    assert os.path.isfile(os.path.join(release.WEB_DIR, "index.html"))


# --- publish ---------------------------------------------------------------------------------

def fake_gh(tmp_path, monkeypatch, view_rc, view_out="", view_err=""):
    """A `gh` on PATH: `release view` answers as given, every call is logged as a JSON line."""
    bindir = tmp_path / "fakebin"
    bindir.mkdir()
    log = tmp_path / "gh.log"
    script = bindir / "gh"
    script.write_text(
        "#!/usr/bin/env python3\n"
        "import json, sys\n"
        f"open({str(log)!r}, 'a').write(json.dumps(sys.argv[1:]) + '\\n')\n"
        "if sys.argv[1:3] == ['release', 'view']:\n"
        f"    sys.stdout.write({view_out!r}); sys.stderr.write({view_err!r}); sys.exit({view_rc})\n"
        "sys.exit(0)\n"
    )
    script.chmod(0o755)
    monkeypatch.setenv("PATH", f"{bindir}{os.pathsep}{os.environ['PATH']}")
    return log


def calls(log):
    if not log.exists():
        return []
    return [json.loads(line) for line in log.read_text().splitlines()]


def staged_dist(tmp_path):
    build, web = make_build(tmp_path)
    dist = tmp_path / "dist"
    release.stage(build, str(dist), web_dir=web)
    return str(dist)


def test_publish_creates_the_release_at_the_commit_with_every_asset(tmp_path, monkeypatch):
    dist = staged_dist(tmp_path)
    log = fake_gh(tmp_path, monkeypatch, 1, view_err="release not found\n")
    out = release.publish(dist, repo=workspace(tmp_path, "0.1.0"), env={"GITHUB_SHA": SHA})
    assert out == f"published v0.1.0 at {SHA} with 8 files"
    create = calls(log)[-1]
    assert create[:7] == ["release", "create", "v0.1.0", "--target", SHA, "--title", "v0.1.0"]
    assert sorted(os.path.basename(f) for f in create[9:]) == sorted(os.listdir(dist))


def test_publish_leaves_a_complete_release_unchanged(tmp_path, monkeypatch):
    dist = staged_dist(tmp_path)
    names = "\n".join(os.listdir(dist)) + "\n"
    log = fake_gh(tmp_path, monkeypatch, 0, view_out=names)
    out = release.publish(dist, repo=workspace(tmp_path, "0.1.0"), env={"GITHUB_SHA": SHA})
    assert "left unchanged" in out
    assert not any(c[:2] == ["release", "create"] for c in calls(log))


def test_publish_refuses_to_touch_a_partial_release(tmp_path, monkeypatch):
    dist = staged_dist(tmp_path)
    log = fake_gh(tmp_path, monkeypatch, 0, view_out=release.RELAY_AMD64 + "\n")
    with pytest.raises(release.ReleaseError, match="never overwrite"):
        release.publish(dist, repo=workspace(tmp_path, "0.1.0"), env={"GITHUB_SHA": SHA})
    assert not any(c[:2] == ["release", "create"] for c in calls(log))


def test_publish_fails_on_a_gh_error_other_than_not_found(tmp_path, monkeypatch):
    dist = staged_dist(tmp_path)
    log = fake_gh(tmp_path, monkeypatch, 1, view_err="HTTP 502: Bad Gateway\n")
    with pytest.raises(release.ReleaseError, match="502"):
        release.publish(dist, repo=workspace(tmp_path, "0.1.0"), env={"GITHUB_SHA": SHA})
    assert not any(c[:2] == ["release", "create"] for c in calls(log))


@pytest.mark.parametrize("sha", ["", "main", SHA[:12]])
def test_publish_refuses_a_missing_or_short_commit(tmp_path, monkeypatch, sha):
    dist = staged_dist(tmp_path)
    fake_gh(tmp_path, monkeypatch, 1, view_err="release not found\n")
    with pytest.raises(release.ReleaseError, match="GITHUB_SHA"):
        release.publish(dist, repo=workspace(tmp_path, "0.1.0"), env={"GITHUB_SHA": sha})


def test_publish_refuses_a_dist_with_an_extra_or_missing_file(tmp_path, monkeypatch):
    dist = staged_dist(tmp_path)
    fake_gh(tmp_path, monkeypatch, 1, view_err="release not found\n")
    os.remove(os.path.join(dist, release.RELAY_ARM64 + ".sha256"))
    with pytest.raises(release.ReleaseError, match="exactly"):
        release.publish(dist, repo=workspace(tmp_path, "0.1.0"), env={"GITHUB_SHA": SHA})


def test_cli_exits_2_on_bad_usage_and_1_on_a_release_error(tmp_path):
    script = os.path.join(REPO, "scripts", "release.py")
    assert subprocess.run(["python3", script], capture_output=True, check=False).returncode == 2
    out = subprocess.run(
        ["python3", script, "stage", str(tmp_path / "nothing"), str(tmp_path / "dist")],
        capture_output=True, text=True, check=False,
    )
    assert out.returncode == 1
    assert "missing CI binaries" in out.stderr
