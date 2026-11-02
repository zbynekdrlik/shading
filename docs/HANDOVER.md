# Handover: shading (2026-11-02)

## Status

This project owns the shading (bkshading) code: the service, the relay that runs on the camboxes and the handheld SBC, the panel, their tests and their releases. It split from `zbynekdrlik/camera-box` under ticket airuleset#1280 and shading#1.

The code came over with its history on 2.11.2026 (camera-box `dev` @ 390ae50, path-filtered). Split agreed with camera-box, the intercom model: shading owns code, tests and releases; camera-box keeps the deploy and provisioning scripts, the systemd units, the rig gates and the SBC flash script, and pulls the pinned release. `docs/CONTRACT.md` is what camera-box reads. Open camera-box#1402 (relay fps keys) and camera-box#1452 (PP aperture writes) are code that now lives here.

## Machines

- dev1: this project's account, with no sudo. Ask airuleset for build tools by peer.
- The boxes: camera-box installs your pinned release. You do not deploy to them yourself.

## What else acts on the machines

- camera-box: pins a release (tag + `.sha256`), deploys it and runs the whole-system gates. `deploy-fleet.sh`, `setup-device.sh`, `verify-device.sh`, `setup-strih.sh`, `verify-strih.sh`, `rig-mode.sh` and every `scripts/bkshading-*` stay there and call into your release.
- The E2E test-camera settings use the relay's own keys, so that interface is a pinned contract.

## How to run a test

- Rust: `cargo fmt --all --check`, `cargo clippy -p bkshading-proto -p bkshading-relay -p bkshading --all-targets -- -D warnings` (and `-p bkshading --no-default-features`), `cargo test -p bkshading-proto -p bkshading-relay -p bkshading`, `cargo test -p bkshading --no-default-features`.
- Python: `ruff check scripts tests`, `python3 -m pytest -q tests/python`.
- Panel E2E (Playwright, chromium): `bkshading/service/tests/e2e`, `npm install`, then `BKSHADING_BIN=<release bkshading> npx playwright test`.

## Site facts

Never commit a secret. Private facts go in `~/.config/shading/`.

## Runners

None registered; CI uses GitHub-hosted runners (`.github/workflows/ci.yml`). A push to `main` publishes the release `v<workspace version>`.

## Backlog

Owner-requested:

- Handle shading as its own project (2026-11-02).

Proposed by a session, not yet asked for by the owner: nothing.

## Knowledge left in other projects

- camera-box: camera-box#1487, camera-box#1402, camera-box#1452; the deploy/provisioning rules (`fleet-relay-upgrade.md`, `camera-test-settings.md`) and its copy of the shading rules until it deletes `bkshading/`.
- airuleset: airuleset#1280.
