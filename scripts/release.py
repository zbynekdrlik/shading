#!/usr/bin/env python3
"""shading release machinery (shading#1): the CI gates and the publish step of a release.

Subcommands (each exits non-zero with a message naming what is wrong):
  version-check BASE_REF   dev's [workspace.package] version must be strictly above BASE_REF's
                           (a BASE_REF without a Cargo.toml passes: the first release).
  binary-version BIN       BIN --version must print one line whose LAST word is the workspace
                           version (camera-box reads the relay's that way, docs/CONTRACT.md).
  stage BUILD DIST         check the CI binaries in BUILD are built for their asset's machine
                           (ELF e_machine / PE), copy them into DIST under their
                           release asset names (executable), the Linux service as a tarball with
                           its web/ assets, and write one `<asset>.sha256` (sha256sum format) each.
  publish DIST             create release v<version> at $GITHUB_SHA with every file in DIST.
                           Published assets never change (the intercom rule): a release that
                           already carries every asset is left alone (a CI re-run), one that
                           carries only some fails for a person to look at, none = publish.

Asset names are a contract with camera-box: see ASSETS and docs/CONTRACT.md.
"""
import hashlib
import os
import re
import shutil
import subprocess
import sys
import tarfile

import tomllib

REPO = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
SEMVER = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")

# Release asset name -> path of its source file inside the CI download dir (BUILD).
RELAY_AMD64 = "bkshading-relay-linux-amd64"
RELAY_ARM64 = "bkshading-relay-linux-arm64"
SERVICE_LINUX = "bkshading-service-linux-amd64.tar.gz"
SERVICE_WINDOWS = "bkshading-service-windows-amd64.exe"
ASSETS = {
    RELAY_AMD64: os.path.join("linux", "release", "bkshading-relay"),
    RELAY_ARM64: os.path.join(
        "linux", "aarch64-unknown-linux-gnu", "release", "bkshading-relay"
    ),
    SERVICE_LINUX: os.path.join("linux", "release", "bkshading"),
    SERVICE_WINDOWS: os.path.join("windows", "bkshading.exe"),
}
WEB_DIR = os.path.join(REPO, "bkshading", "service", "web")


class ReleaseError(Exception):
    pass


def parse_version(text):
    """[workspace.package].version of a Cargo.toml text, validated as plain X.Y.Z."""
    data = tomllib.loads(text)
    try:
        version = data["workspace"]["package"]["version"]
    except KeyError as exc:
        raise ReleaseError("Cargo.toml has no [workspace.package].version") from exc
    if not isinstance(version, str) or not SEMVER.match(version):
        raise ReleaseError(f"workspace version {version!r} is not X.Y.Z")
    return version


def version_key(version):
    return tuple(int(p) for p in SEMVER.match(version).groups())


def workspace_version(repo=REPO):
    with open(os.path.join(repo, "Cargo.toml"), encoding="utf-8") as fh:
        return parse_version(fh.read())


def version_check(base_ref, repo=REPO):
    head = workspace_version(repo)
    shown = subprocess.run(
        ["git", "show", f"{base_ref}:Cargo.toml"],
        cwd=repo, capture_output=True, text=True, check=False,
    )
    if shown.returncode != 0:
        # Only a missing file is a first release; any other git failure is an error.
        if "does not exist" in shown.stderr or "exists on disk, but not in" in shown.stderr:
            return f"ok: {head} (no Cargo.toml on {base_ref}: first release)"
        raise ReleaseError(f"git show {base_ref}:Cargo.toml failed: {shown.stderr.strip()}")
    base = parse_version(shown.stdout)
    if version_key(head) <= version_key(base):
        raise ReleaseError(
            f"workspace version {head} is not above {base_ref}'s {base}: bump "
            "[workspace.package].version in Cargo.toml"
        )
    return f"ok: {head} > {base} ({base_ref})"


def binary_version(binary, repo=REPO):
    expected = workspace_version(repo)
    out = subprocess.run([binary, "--version"], capture_output=True, text=True, check=False)
    if out.returncode != 0:
        raise ReleaseError(f"{binary} --version exited {out.returncode}: {out.stderr.strip()}")
    lines = out.stdout.strip().splitlines()
    if len(lines) != 1:
        raise ReleaseError(f"{binary} --version must print exactly one line, got {lines!r}")
    words = lines[0].split()
    if not words or words[-1] != expected:
        raise ReleaseError(
            f"{binary} --version last word must be {expected}, got {lines[0]!r}"
        )
    return f"ok: {lines[0]}"


def sha256_line(path):
    digest = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            digest.update(chunk)
    return f"{digest.hexdigest()}  {os.path.basename(path)}\n"


# The machine each asset must be built for: ELF e_machine (62 = x86-64, 183 = AArch64) or "pe".
MACHINE = {RELAY_AMD64: 62, RELAY_ARM64: 183, SERVICE_LINUX: 62, SERVICE_WINDOWS: "pe"}


def binary_machine(path):
    with open(path, "rb") as fh:
        head = fh.read(20)
    if head[:4] == b"\x7fELF" and len(head) >= 20:
        return int.from_bytes(head[18:20], "little")
    if head[:2] == b"MZ":
        return "pe"
    return None


def _executable(info):
    info.mode = 0o755 if info.isfile() and info.name.endswith("/bkshading") else info.mode
    return info


def stage(build, dist, web_dir=WEB_DIR):
    missing = [src for src in ASSETS.values() if not os.path.isfile(os.path.join(build, src))]
    if missing:
        raise ReleaseError(f"missing CI binaries in {build}: {missing}")
    if not os.path.isdir(web_dir):
        raise ReleaseError(f"panel web assets not found at {web_dir}")
    for name, src in ASSETS.items():
        path = os.path.join(build, src)
        got = binary_machine(path)
        if got != MACHINE[name]:
            raise ReleaseError(f"{src} is machine {got!r}, {name} needs {MACHINE[name]!r}")
    os.makedirs(dist, exist_ok=True)
    for name, src in ASSETS.items():
        dest = os.path.join(dist, name)
        if name == SERVICE_LINUX:
            # One flat dir in the tarball: `bkshading` + `web/` (the layout camera-box installs).
            # The binary is stored executable: download-artifact drops the exec bit.
            with tarfile.open(dest, "w:gz") as tar:
                tar.add(
                    os.path.join(build, src),
                    arcname="bkshading-service/bkshading",
                    filter=_executable,
                )
                tar.add(web_dir, arcname="bkshading-service/web")
        else:
            shutil.copyfile(os.path.join(build, src), dest)
            os.chmod(dest, 0o755)
        with open(dest + ".sha256", "w", encoding="utf-8") as fh:
            fh.write(sha256_line(dest))
    return sorted(os.listdir(dist))


def publish(dist, repo=REPO, env=None):
    env = os.environ if env is None else env
    version = workspace_version(repo)
    tag = f"v{version}"
    sha = env.get("GITHUB_SHA", "")
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ReleaseError(f"GITHUB_SHA must be a 40-hex commit, got {sha!r}")
    expected = sorted([*ASSETS, *(n + ".sha256" for n in ASSETS)])
    present = sorted(os.listdir(dist))
    if present != expected:
        raise ReleaseError(f"{dist} must hold exactly {expected}, has {present}")
    view = subprocess.run(
        ["gh", "release", "view", tag, "--json", "assets", "-q", ".assets[].name"],
        capture_output=True, text=True, check=False,
    )
    if view.returncode == 0:
        published = set(view.stdout.split())
        if published >= set(expected):
            return f"{tag} already carries every asset: left unchanged"
        raise ReleaseError(
            f"release {tag} exists with only {sorted(published)}: fix it by hand, never overwrite"
        )
    # Only "release not found" means no release; any other gh error must not read as empty.
    if view.stderr.strip() != "release not found":
        raise ReleaseError(f"gh release view {tag} failed: {view.stderr.strip()}")
    notes = (
        f"shading {tag}. Assets and their `.sha256` files are the pinned contract camera-box "
        "deploys (docs/CONTRACT.md)."
    )
    files = [os.path.join(dist, name) for name in present]
    subprocess.run(
        ["gh", "release", "create", tag, "--target", sha, "--title", tag, "--notes", notes,
         *files],
        check=True,
    )
    return f"published {tag} at {sha} with {len(files)} files"


def main(argv):
    commands = {
        "version-check": (version_check, 1),
        "binary-version": (binary_version, 1),
        "stage": (stage, 2),
        "publish": (publish, 1),
    }
    if len(argv) < 2 or argv[1] not in commands or len(argv) - 2 != commands[argv[1]][1]:
        print(__doc__, file=sys.stderr)
        return 2
    fn, _ = commands[argv[1]]
    try:
        result = fn(*argv[2:])
    except ReleaseError as exc:
        print(f"release.py {argv[1]}: {exc}", file=sys.stderr)
        return 1
    print(result if isinstance(result, str) else "\n".join(result))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
