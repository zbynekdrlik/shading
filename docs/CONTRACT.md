# shading ↔ camera-box contract

shading owns the bkshading code, its tests and its releases. camera-box pins a release, deploys it
to the boxes and runs the whole-system gates (shading#1, agreed 2.11.2026, the intercom model).
Everything below is what camera-box reads. Changing any of it is a breaking change: bump the
version, update this file, and tell camera-box before the release.

Tests that pin this contract here: `bkshading/relay/tests/contract.rs`,
`bkshading/service/tests/check_config.rs`, `tests/python/test_release.py`.

## Releases

- A release is the GitHub Release `vX.Y.Z` of `zbynekdrlik/shading`, where `X.Y.Z` is
  `[workspace.package].version` in `Cargo.toml`. All three crates carry that one version.
- It is published only by the `release` job of CI, on a push to `main`, after every gate passed
  (`bkshading`, `bkshading-windows`, `coverage`, `python-tests`). The tag points at that commit.
- **Published assets never change.** A re-run leaves a complete release alone; a partial release
  fails the job and is fixed by hand. Pin a tag and a sha256, never `latest`.
- A tag always points at the commit whose gates built its assets: a `main` push whose version is
  already tagged on another commit fails instead of releasing.
- Assets, each with a `<asset>.sha256` in `sha256sum` format (`<hex>  <asset>`):

| Asset | Content | Runs on |
|---|---|---|
| `bkshading-relay-linux-amd64` | relay binary, x86-64 ELF | camboxes |
| `bkshading-relay-linux-arm64` | relay binary, AArch64 ELF | handheld SBC |
| `bkshading-service-linux-amd64.tar.gz` | `bkshading-service/bkshading` (executable) + `bkshading-service/web/` | strih (Linux) |
| `bkshading-service-windows-amd64.exe` | service binary, PE | Windows strih PC, if any still runs it |

The panel web assets are also embedded in the service binary; `web/` is a copy, not needed at
runtime.

## Relay (`bkshading-relay`)

- **Version:** `bkshading-relay --version` prints exactly one line, `bkshading-relay X.Y.Z`. The
  last word is the version (camera-box `scripts/lib/bkshading-relay-version.sh`).
- **Listen:** `0.0.0.0:8771` by default (`--bind`). HTTP routes: `GET /api/state`,
  `PUT /api/params`, `GET /api/detect`, `GET /healthz`.
- **Install path:** camera-box installs it as `/usr/local/bin/bkshading-relay` and owns the unit
  (`systemd/bkshading-relay.service` stays in camera-box).
- **Environment** (camera-box writes it into the `relay.env` the unit loads):
  - `BKSHADING_REGISTER_URL`, `BKSHADING_RELAY_ID` — announce the relay to the service
    (`--register-url`, `--relay-id`).
  - `CAMERA_BOX_CAPTURE_FPS` — the box's grab rate, reported to the service.
  - `BKSHADING_RELAY_MIN_READ_INTERVAL_MS` — tunes the camera read floor (default 10 000 ms; it
    can never be switched off).
- **gphoto2 keys.** The relay drives the camera through the `gphoto2` CLI with these key names,
  and camera-box's E2E test-camera step uses the same names directly while the relay is stopped.
  Names and raw gphoto2 values stay byte-stable:

| Key | Meaning | camera-box E2E |
|---|---|---|
| `iso` | ISO | read + set |
| `d002` | shutter angle × 100 | read + set |
| `d004` | white balance, Kelvin | read + set |
| `f-number` | aperture | read + set (optional) |
| `d005` | tint | read + set (optional) |
| `d006` | project fps × 100 | read only |
| `d007` | off-speed fps | never read |
| `d003` | manual focus distance (relay reads it best-effort) | — |

- **Read-only root.** The relay writes nothing to disk at runtime, so it runs on a read-only root.
  The deploy procedure stays camera-box's (`scripts/bkshading-deploy-relay.sh`): rig-busy gated,
  stop a running relay before the swap, stage then atomic `mv`, close the root read-only through
  camera-box's `ro_window_close_cmds`, start the relay again only if it ran before, and leave it
  stopped if the root did not close. A release changes none of that.

## Service (`bkshading`)

- **Version:** `bkshading --version` prints `bkshading X.Y.Z`.
- **Listen:** `0.0.0.0:8770` by default (config `bind`, or `--bind`). Health probes use a route the
  service serves: `GET /api/version` (never `/api/state`, which is the relay's).
- **Config check:** `bkshading --check-config <file>` loads the file exactly as a start would. It
  prints `ok: <N> cameras` and exits 0, or exits non-zero with the file path and the reason on
  stderr. It starts nothing and binds no port. camera-box runs it from the pinned release on its
  generated strih seed (`strih_bkshading_config_text`).
  - An unknown table is ignored, not refused: a misspelled `[[cameras]]` (the table is
    `[[camera]]`) loads as `ok: 0 cameras`. Compare the printed count, not only the exit code.
- **Relay registration:** `POST /api/relays/register` — a relay announces itself with its id
  (`register:<id>` addresses in the config).

## Not shading's

camera-box keeps the deploy and provisioning layer: every `scripts/bkshading-*` script and lib,
the `systemd/bkshading-*` units, the rig gates (`bkshading-e2e-pause.sh`, the preflight, the
relay mode switch), `deploy-fleet.sh`, the SBC flash script and the strih seed generator.
