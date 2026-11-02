# Handover: shading (2026-11-02)

## Status

This project owns shading (bkshading): the service, the relay on the camboxes, the handheld SBC and the panel. It splits from `zbynekdrlik/camera-box` under ticket airuleset#1280. The repository starts empty on purpose.

The code arrives from camera-box later, with history. The OBS import comes first. Open shading tickets camera-box#1402 and camera-box#1452 are parked until 2026-11-04, and the import waits until they close, or their fixes land here.

## Machines

- dev1: this project's account, with no sudo. Ask airuleset for build tools by peer.
- The boxes: camera-box installs your pinned release. You do not deploy to them yourself.

## What else acts on the machines

- camera-box: pins a release, deploys it and runs the whole-system gates. `deploy-fleet.sh`, `setup-device.sh`, `verify-device.sh`, `setup-strih.sh`, `verify-strih.sh` and `rig-mode.sh` stay there and call into your release.
- The E2E test-camera settings use the relay's own keys, so that interface is a pinned contract.

## How to run a test

Nothing to run yet. The tests arrive with the import.

## Site facts

Never commit a secret. Private facts go in `~/.config/shading/`.

## Runners

None registered. camera-box CI built this code before.

## Backlog

Owner-requested:

- Handle shading as its own project (2026-11-02).

Proposed by a session, not yet asked for by the owner: nothing.

## Knowledge left in other projects

- camera-box: camera-box#1487, `bkshading/README.md`, the shading rule files, camera-box#1402, camera-box#1452.
- airuleset: airuleset#1280.
