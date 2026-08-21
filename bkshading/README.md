# bkshading — remote camera shading control (issue 808)

Multiplatform Rust service + relay for controlling Blackmagic camera **shading/grading**
(aperture, ISO, white balance, shutter, fps) from an operator web panel, local **and**
remote. Implements the owner architecture decided 2026-08-20 (issue 808 comments
5355836067 / 5356048130 / 5356062847).

## Architecture

```
 BMPCC (USB) ── cambox (cam1) ── bkshading-relay ──┐
 BMPCC (USB) ── SBC (handheld) ─ bkshading-relay ──┤  LAN   ┌── bkshading (strih PC)
 BMPCC (USB-Eth REST) ─────────────────────────────┴──HTTP──┤   axum web panel  ── operator
                                                            └── 4+4 blocks (preview + params)
```

- **Transports (owner decision): USB via a relay, or USB-Ethernet REST. Never Bluetooth.**
- One **relay** per camera runs on the box its USB is plugged into (a cambox, or a mini SBC
  on a handheld cage — the SAME component, ARM-friendly). It drives the camera over USB-PTP
  via the `gphoto2` CLI and exposes its shading over a small HTTP API.
- The **service** runs on the strih PC (Windows first, Linux after the frame-loss P0),
  aggregates every relay, and serves ONE operator web panel: 4+4 blocks stacked, each with a
  camera preview on top and the shading parameters below. Opened at `strih.lan` — local and
  (later) remote via a password-protected cloudflare proxy.

## Crates

| Crate | What |
|---|---|
| `bkshading-proto` | Shared wire types + the byte-verified Blackmagic PTP mapping (ported from the dev2 MVP `pybridge/mapping.py`). Pure, no IO — one source of truth for both sides. |
| `bkshading-relay` | The cambox/SBC USB relay. `gphoto2` transport behind a trait, small HTTP API (`/healthz`, `/api/state`, `/api/detect`, `PUT /api/params`). |
| `bkshading` | The aggregation service + operator web panel (`GET /`, `/api/cameras`, `PUT /api/cameras/<id>/params`, and the M2 live preview `GET /api/cameras/<id>/preview.jpg`). |

## M1 scope (this milestone)

- The three crates, wired into the workspace + a dedicated CI build job (the appliance build
  path is unchanged — see the root `Cargo.toml` note).
- The 4+4 responsive web panel skeleton (preview **placeholder**; params: aperture, ISO,
  WB, shutter, fps) with the service version shown in the DOM (version-on-dashboard).
- Config-driven camera list (id, transport, address, optional NDI preview). A camera with no
  preview renders a params-only block.
- The relay's `gphoto2` read + write logic, fully unit-tested with a fake runner (no camera).

## M2 scope (live preview)

- Live NDI camera preview into each 4+4 block's top area (replaces the M1 placeholder). The
  service subscribes to each configured camera's NDI **low-bandwidth** stream, decimates to a
  few fps, JPEG-encodes, and serves the latest frame at `GET /api/cameras/<id>/preview.jpg`;
  the web UI reloads an `<img>` a few times a second.
- A `PreviewSource` seam: the default (and CI) source is a **stub test pattern**, so the
  service compiles and runs with no libndi and no camera. The real libndi receiver (bandwidth
  LOWEST, mirroring the appliance `src/ndi.rs`) is behind `--features ndi`, **off by default**
  and unverified against a live source in this lane (see "Deferred").
- Pure, CI-tested logic: frame decimation, RGB→JPEG encode, colour conversion, per-camera
  routing/store. Tuned via the optional `[preview]` config table (`fps`, `jpeg_quality`, ...).

## Deferred to M2+

- Verifying the **real** libndi preview receive (`--features ndi`) end-to-end against a live
  cambox NDI source + provisioning libndi on the strih box + full FourCC coverage.
- cloudflare remote with password protection; the SBC handheld image; and automating the E2E
  camera pre-run shutter checklist (now unblocked by the relay provisioning below).

## Relay provisioning (issue 808) — systemd unit + gphoto2 + the issue-809 env

The relay BINARY already reads `CAMERA_BOX_CAPTURE_FPS` from its env and reports it as
`RelayState.capture_fps` (the service's issue-809 grab derive consumes it), but nothing ran it on a
cambox. Provisioning is a standalone, supervisor-run script (mirrors `bkshading-provision-ndi.sh`):

- `systemd/bkshading-relay.service` — runs `/usr/local/bin/bkshading-relay --bind 0.0.0.0:8771` as
  root (raw USB-PTP), with `EnvironmentFile=-/etc/bkshading/relay.env` (the `-` makes it OPTIONAL:
  an unprovisioned box reports no capture fps and the service falls back to the static `grab_fps`
  config, never a wrong value).
- `scripts/bkshading-provision-relay.sh --check|--install` — idempotent, fail-loud, ENABLE-ONLY
  (`daemon-reload` + `enable`, never `start`/`restart` — defer to reboot, per
  `.claude/rules/provisioning-scripts.md`). `--install` installs `gphoto2` (apt, if missing),
  DERIVES `CAMERA_BOX_CAPTURE_FPS` from the box's own `camera-box.service.d` drop-ins (mirroring
  `src/capture.rs requested_capture_denominator` — the default `60` when no drop-in overrides it,
  so the reported rate matches what the box actually grabs at — ONE source of truth, not a hard-coded
  duplicate), writes `/etc/bkshading/relay.env`, installs + enables the unit.
- `scripts/lib/bkshading-relay-runtime.sh` — source-only pure helpers (paths/port + the capture-fps
  derive + env-file body); `tests/python/test_bkshading_relay_provision_808.py` cross-checks the
  unit/script/helper so they cannot drift and drives an install→check end-to-end into a temp root
  (fake systemctl/gphoto2 — no root/apt/systemd needed, Tier-0 runnable).
- **The relay BINARY deploy** (the CI-built `bkshading-relay` → `/usr/local/bin`) and the **live
  verify against a real camera** are the supervisor's rig steps (this lane has no rig access).

## Running (once built on CI)

```
scripts/bkshading-provision-relay.sh --install  # on the cambox/SBC: gphoto2 + unit + env; enable
bkshading-relay --bind 0.0.0.0:8771            # what the unit runs (needs gphoto2 installed)
bkshading --config bkshading/service/bkshading.example.toml   # on the strih PC
```
