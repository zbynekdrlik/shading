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
- WS push of the aggregate; cloudflare remote with password protection.
- Installing `gphoto2` on the camboxes (a RUNTIME dep; not present on cam1 yet — verified
  read-only) + provisioning hooks (`setup-device.sh`/`verify-device.sh`), the SBC handheld
  image, and automating the E2E camera pre-run shutter checklist.

## Running (once built on CI)

```
bkshading-relay --bind 0.0.0.0:8771            # on the cambox/SBC (needs gphoto2 installed)
bkshading --config bkshading/service/bkshading.example.toml   # on the strih PC
```
