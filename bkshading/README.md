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
  camera preview on top and the shading parameters below. Opened at `strih.lan` locally, and
  remotely via a password-protected cloudflare proxy (see "Remote access (cloudflare)" below).

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
  cambox NDI source + provisioning libndi on the strih box + full FourCC coverage. (The runtime
  LIFECYCLE half is already in code: the receiver acquires ONE process-shared, load-once NDI
  runtime — `preview/shared_runtime.rs` keep-alive slot — so a single camera's reconnect can
  never run the process-global SDK destroy under other live receivers.)
- Automating the E2E camera pre-run shutter checklist (meaningful once the shading camera is
  physically cabled to the relay box). Cloudflare remote access with password
  protection is DONE (see "Remote access (cloudflare)" below), and the SBC handheld provisioning is
  DONE (see "SBC / handheld image" below).

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

## Remote access (cloudflare) (issue 808)

The panel is LAN-only by default (`strih.lan:8770`). Remote access goes through a
**password-protected cloudflare proxy** — the owner's decision (issue 808 comment 5355836067;
NOT tailscale). Provisioning mirrors `bkshading-provision-relay.sh`:

- `systemd/bkshading-cloudflared.service` — runs `cloudflared tunnel --no-autoupdate --config
  /etc/bkshading/cloudflared-config.yml run`. The tunnel is **outbound-only** (opens no inbound
  port); the connector holds ONLY its credentials JSON, referenced by path in the config (0600,
  placed by the owner from `cloudflared tunnel create` — **never committed**).
- `scripts/bkshading-provision-cloudflared.sh --check|--install` — idempotent, fail-loud,
  ENABLE-ONLY (`daemon-reload` + `enable`, never `start`/`restart` — defer to reboot). `--install`
  installs `cloudflared` (if missing), composes `/etc/bkshading/cloudflared-config.yml` (config-file
  mode: tunnel name, credentials-file reference, ingress `hostname → http://localhost:8770` + the
  catch-all 404), installs + enables the unit. Cross-platform: Linux installs the systemd unit; on
  the Windows-first strih PC it documents `cloudflared service install` + `%USERPROFILE%\.cloudflared\`.
- `scripts/lib/bkshading-cloudflared-runtime.sh` — source-only pure helpers (paths / unit / config
  path + the config.yml composer + the service origin, whose port is cross-checked against the
  service's own `default_bind` so the tunnel points where the panel listens — ONE source of truth).
- `tests/python/test_bkshading_cloudflared_provision_808.py` — cross-checks unit/script/lib
  no-drift, asserts NO secret is committed (references-by-path only), enable-only, and the
  Access-enforcement gate; drives an install→check end-to-end into a temp root (fake
  cloudflared/systemctl — Tier-0 runnable).

**Operator auth story (password protection).** The password is enforced at the **Cloudflare Access**
layer on the public hostname — NOT in the service (the owner put the protection at the proxy). The
recommended policy for a small operator team is **One-Time PIN**: allowed operator emails receive a
login code (no shared password to leak, per-operator revocable). Because a tunnel exposes a PUBLIC
hostname, `--install` **REFUSES without `--access-confirmed`** and `--check` **FAILS without the
Access marker** — a naked, unprotected public tunnel can never be the "provisioned" state.

```
scripts/bkshading-provision-cloudflared.sh --install \
  --hostname shading.example.org --tunnel church-shading \
  --credentials-file /etc/bkshading/church-shading.json --access-confirmed
scripts/bkshading-provision-cloudflared.sh --check
```

- **The live Cloudflare Zero Trust steps are the owner's** (this lane has no rig/account access):
  `cloudflared tunnel login` + `create` (produces the credentials JSON — place it at the referenced
  path, `chmod 0600`), `cloudflared tunnel route dns <name> <hostname>` (the DNS record), and the
  Cloudflare Access application + One-Time-PIN policy on the hostname. The tunnel flow needs **no**
  Cloudflare API token. Deploying the `cloudflared` binary + the live remote-access verify are the
  supervisor's steps.

## SBC / handheld image (issue 808)

A **handheld** camera (no cambox, video over wireless HDMI) is shaded by running the SAME
`bkshading-relay` on **a separately powered zero-class arm64 SBC with WiFi on the cage** — the
board is device-agnostic (Design v3, owner 14.9.2026): a Raspberry **Pi Zero 2 W**, a **Radxa
ZERO 3W**, or an **Orange Pi Zero 2W** (the ordered prototype). The camera plugs into the SBC's
**USB host port** (PTP over `gphoto2`) and the box is powered from the camera cage's V-mount **5 V
USB splitter** (5 V USB-C/USB-A out, or a D-tap→USB-C **5 V** cable) — **never a power bank, never
PiSugar, never raw 15 V D-tap; all candidate boards are 5 V-only**. Because PTP makes the camera
the USB *device* (the power sink), the box can never be powered BY the camera — hence its own 5 V
feed. The relay exposes the camera's shading to the service exactly like a cambox does (owner
architecture, issue 808 comment 5356048130 path 2). The service already treats it as just another
camera (`transport = "sbc-relay"`, the `handheld-1..3` records in `bkshading.example.toml`, a
params-only block — a handheld has no NDI feed). This milestone provisions the box side.

- **Per-board port/power topology (all one USB cable to the camera + a 5 V feed from the splitter):**
  - **Pi Zero 2 W** — micro-USB OTG host → camera; a separate micro-USB 5 V power-in from the
    splitter. 2.4 GHz WiFi only, so the rig needs a 2.4 GHz SSID on site.
  - **Radxa ZERO 3W** — USB 3.0 Type-C host → camera; OTG-C 5 V-in from the splitter. Dual-band WiFi.
  - **Orange Pi Zero 2W** — two USB-C ports (host vs power is revision-dependent — probe both);
    5 V-in from the splitter on the power port. Dual-band WiFi.
- **CI cross-builds the ARM binary.** The `bkshading` job cross-compiles the relay for
  **`aarch64-unknown-linux-gnu`** (rustup target + the `gcc-aarch64-linux-gnu` linker; the relay is
  pure Rust — axum/tokio/serde/clap, no C link deps — so the cross-link is trivial) and uploads it
  as the relay-only **`bkshading-relay-linux-arm64`** artifact. **Target choice — aarch64, not
  armhf:** every candidate board is a Cortex-A53 (ARMv8-A, 64-bit) with a 64-bit stock arm64 image
  (Raspberry Pi OS / Debian / Armbian); the relay's footprint is a few MB (well under the 512 MB /
  2 GB budget on 64-bit), and aarch64-gnu is the best-supported Rust cross target with glibc
  matching every candidate image. A 32-bit `armv7-unknown-linux-gnueabihf` build is one extra CI
  matrix entry to add IF a legacy 32-bit handheld ever needs it — not the default.
- **`scripts/bkshading-provision-sbc.sh --check|--install`** (+ pure lib
  `scripts/lib/bkshading-sbc-runtime.sh`) — idempotent, fail-loud, ENABLE-ONLY (`daemon-reload` +
  `enable`, never `start`/`restart` — defer to reboot, per `.claude/rules/provisioning-scripts.md`).
  `--install` installs `gphoto2` (apt, if missing) and installs + enables the **reused**
  `systemd/bkshading-relay.service` (the SBC runs the SAME unit). It writes **NO**
  `CAMERA_BOX_CAPTURE_FPS` env: an SBC has no camera-box appliance to derive from and a handheld has
  no grab comparison, so the relay's `EnvironmentFile=-` degrades gracefully (relay reports
  `capture_fps=None` → the service uses its static config, never a wrong value). `--check` verifies
  `gphoto2` + the unit installed (byte-match) + enabled + the relay binary present **and actually
  aarch64** (an ELF `e_machine` read — a mis-deployed amd64 binary is caught here, not at reboot
  with an opaque `Exec format error`) + **the WiFi link is up** (reads `/sys/class/net/wl*/operstate`
  — band-agnostic; a wired box with no `wl*` interface, e.g. a cambox, SKIPs this check, never
  FAILs; a down link FAILs with a `nmcli device wifi connect …` join remediation).
- **Deploy the ARM relay:** `scripts/bkshading-deploy-relay.sh --host <sbc> --arch arm64
  --no-remount` — `--arch arm64` fetches the `bkshading-relay-linux-arm64` artifact; `--no-remount`
  skips the read-only-root swap (a cambox appliance has a read-only root; a stock arm64 SBC image —
  Raspberry Pi OS / Debian / Armbian — root is read-write). The scp + sha256 byte-verify +
  ENABLE-ONLY discipline is otherwise identical to the cambox path.
- **The physical box bring-up is the owner's / supervisor's step** (no rig access in this lane):
  flash the board's 64-bit arm64 image (Raspberry Pi OS Lite / Debian / Armbian) with headless WiFi
  + ssh, join the rig WiFi SSID (a 2.4 GHz-only board needs a 2.4 GHz SSID on site), give the box a
  stable `handheld-N` hostname via a static DHCP lease on router_snv, then deploy + `--install` +
  reboot, then add the `handheld-N` `[[camera]]` record to the service config. **Transports stay
  USB-relay / USB-Ethernet REST only — never Bluetooth** (owner hard rule): the SBC drives the
  camera over USB-PTP (`gphoto2`/libusb), so it never enumerates as a network link (the netplan
  `enx*` CDC-NCM trap that bites camboxes does not touch the handheld).

```
# on the SBC (after flashing the arm64 image + deploying the aarch64 relay):
scripts/bkshading-provision-sbc.sh --install   # gphoto2 + reused relay unit; enable (defer to reboot)
scripts/bkshading-provision-sbc.sh --check     # verify gphoto2 + unit enabled + aarch64 binary + WiFi link
```

## Running (once built on CI)

```
scripts/bkshading-provision-relay.sh --install --rig-mode test  # on the cambox: gphoto2 + unit + env; enable-state per rig mode (setup-device.sh does this on a fresh install)
bkshading-relay --bind 0.0.0.0:8771            # what the unit runs (needs gphoto2 installed)
bkshading --config bkshading/service/bkshading.example.toml   # on the strih PC
```
