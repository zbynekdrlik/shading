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

The Linux binaries are built on GitHub's `ubuntu-latest` and link dynamically against its glibc:
v0.1.0 needs GLIBC_2.39 for the relay (amd64 and arm64) and GLIBC_2.34 for the service, the same
floor camera-box's own CI builds had. A box with an older glibc cannot run them.

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
  - `BKSHADING_CAMERA` — optional fixed camera address `host[:port]` (`--camera`); unset = find
    the camera on its `cdc_ncm` link (below).
- **Camera transport (since v0.2.0, shading#6 — breaking).** The relay no longer uses `gphoto2`
  or PTP. It drives the camera through the official Blackmagic REST API, over the camera's own
  USB-C network link (CDC-NCM, Camera OS 9.x), with no authentication:
  `http://<camera>:80/control/api/v1/<endpoint>`.
  - `gphoto2` is no longer a runtime dependency of the relay, and the `--gphoto2` flag is gone. A
    unit that passes it fails to start; camera-box's unit does not pass it.
  - **Finding the camera.** By default the relay looks at every network interface whose kernel
    driver is `cdc_ncm` (the kernel names it `enx<mac>`) on a Blackmagic USB device (vendor id
    `1edb`; a 2.5G dongle or a tethered phone on the same driver is skipped). It takes the DHCP
    server address from systemd-networkd's lease `/run/systemd/netif/leases/<ifindex>`
    (`SERVER_ADDRESS`, else `ROUTER`). Without a lease file it falls back to the other host of the
    link's own /30 (`ip -4 -o addr show dev <iface>`). The camera is that DHCP server: it hands the box one address of a /30 (cam5:
    camera `172.25.77.161`, box `.162`).
  - **Confirming and naming it (v0.2.2).** `GET system/product` names the camera when the firmware
    serves it. Otherwise (the Pocket 6K at fw 8.6 answers 404) the camera is confirmed by `GET
    lens/iris` 200 carrying a real `apertureStop`, else `GET /system` 204 (or a 2xx with an empty or
    JSON body; an HTML page is not a camera). A 500 or 503 from `system/product` is not taken as
    "not served": the next read probes again. A camera found this way is named by the USB product string of the
    vendor-1edb device behind the link (`Pocket Cinema Camera 6K`), else "Blackmagic camera".
  - **Camera-side prerequisite:** the camera serves REST only with **Blackmagic Camera Setup →
    Network access → "Web media manager" enabled** (live on the 6K at fw 8.6). Without it the link
    comes up and DHCP works, but nothing answers HTTP, and the relay reads the camera offline.
  - **Override.** `BKSHADING_CAMERA=<host[:port]>` (or `--camera`) skips discovery and uses that
    address.
- **The camera link — camera-box installs this `.network` file** on every box that runs the relay
  (camboxes and the handheld SBC). It must be on the box and `systemd-networkd` enabled for
  discovery to work. The link comes up DOWN and NetworkManager leaves it unmanaged; networkd
  brings it up. Leases live in `/run`, so the read-only root is fine.
  - DHCP only: no default route, no DNS, no NTP, nothing else from the camera, and the box never
    waits for the link at boot.
  - The `05-` prefix sorts it before netplan's `10-netplan-*` files. No other netplan, networkd or
    NetworkManager match on `enx*` may claim this link (camera-box's issue-1155 netplan trap).
  - It matches the driver AND Blackmagic's USB vendor id. A `Driver=cdc_ncm` match alone would
    also claim a cdc_ncm uplink (an RTL8156 2.5G dongle, a dock) and strip its route and DNS.

```ini
# /etc/systemd/network/05-bkshading-camera.network
# shading#6: the Blackmagic camera's USB-C network link (CDC-NCM). The camera is the DHCP server
# of a /30; the bkshading relay reads its address from this link's lease.
[Match]
Driver=cdc_ncm
Property=ID_VENDOR_ID=1edb

[Link]
RequiredForOnline=no

[Network]
DHCP=ipv4
LinkLocalAddressing=no
IPv6AcceptRA=no

[DHCPv4]
UseRoutes=no
UseGateway=no
UseDNS=no
UseNTP=no
UseHostname=no
UseDomains=no
```

- **REST endpoints.** The relay uses these endpoints, and camera-box's E2E test-camera step should
  read and set the camera through the same ones while the relay is stopped; the gphoto2 keys are
  gone. Field names are Blackmagic's (`RESTAPIforBlackmagicCameras.pdf`).

| Endpoint | JSON | Relay use | Was (gphoto2) |
|---|---|---|---|
| `system/product` | `{deviceName, productName, softwareVersion}` | read: proves a camera answers; `productName` is the reported model | `--auto-detect` |
| `video/iso` | `{iso}` | read + set | `iso` |
| `video/supportedISOs` | `{supportedISOs: [int]}` | read (the ISO choices) | `iso` choices |
| `video/shutter` | `{shutterSpeed}` (1/N s) or `{shutterAngle}` (degrees × 100) | read + set, in the unit the camera reports | `d002` (angle × 100) |
| `video/supportedShutters` | `{shutterSpeeds}` or `{shutterAngles}` | read (the shutter choices) | — |
| `video/whiteBalance` | `{whiteBalance}` (Kelvin) | read + set | `d004` |
| `video/whiteBalance/description` | `{whiteBalance: {min, max}}` | read (the Kelvin range) | `d004` range |
| `video/whiteBalance/doAuto` | no body | set (`autoWb: true`) | — |
| `video/whiteBalanceTint` | `{whiteBalanceTint}` | read + set | `d005` |
| `lens/iris` | `{apertureStop, normalised, apertureNumber, continuousApertureAutoExposure}` | read + set, **set `apertureStop` only** (the other two answer 204 and do not move the iris, live on cam5); `normalised` is read as `params.irisRangePos` (v0.2.3) | `f-number` / `0x5007` |
| `lens/iris/description` | `{controllable, apertureStop: {min, max}}` | read (the aperture grid's ends) | `f-number` choices |
| `lens/focus` | `{normalised}` | read only | `d003` |
| `lens/zoom` | `{focalLength, normalised}` | read only (v0.2.5): `focalLength` keys what the relay learns about the lens's aperture steps; a body without it is fine | — |
| `system/format` | `{codec, frameRate, offSpeedFrameRate, min/maxOffSpeedFrameRate, recordResolution, …}` | read; set `{frameRate}` | `d006` / `d007` |
| `system/supportedFormats` | `{supportedFormats: [{codecs, frameRates, recordResolution}]}` | read (the settable frame rates) | `d006` choices |

- **What changes for a reader of `GET /api/state`:**
  - `params.focusDistance` (the raw PTP `d003`) is always `null` now. The focus position is the
    new `params.focusNorm` (`lens/focus normalised`, 0..1, read only).
    - camera-box's issue-1238 preflight prints its `d003=` line only when `focusDistance` is
      present, so it prints nothing until it reads `focusNorm` instead.
  - `params.apertureAv` is always the camera's TRUE f-stop (`lens/iris apertureStop`). The
    display shows it as it is.
  - **New field `params.irisRangePos` (v0.2.3, additive, read only).** It is `lens/iris
    normalised`: the iris's place in the lens's own range, a number from 0 to 1.
    - `0` = the iris is wide open (the lens's smallest f-number at the current zoom). `1` = the
      iris is fully closed. Values between are fractional. A camera value outside 0..1 is clamped.
    - `null` when the camera does not give `normalised`.
    - It is the lens's ends even when the firmware has no `lens/iris/description` (Pocket 6K at
      fw 8.6). Live on cam3 at 24 mm, wide open: `{"apertureStop":4.0,"normalised":0.0}`. A
      variable-aperture zoom moves its wide end with the focal length, and `normalised` follows it.
    - The relay never writes it: a `normalised` write answers 204 and does not move the iris.
    - An older relay does not send it. A reader that does not know it ignores it.
    - **v0.2.5 (additive meaning, same field):** it also reads `0` / `1` while the iris sits on an
      end the relay LEARNED at the current focal length (below, `apertureStep`), even when the
      camera's `normalised` says otherwise. The Pocket 4K measures `normalised` against the
      `lens/iris/description` range, so at a zoom's wide end it never reaches 0 (cam5 at 108 mm:
      0.0387 at its widest 4.4, description min 4.1). Away from a learned end it is the camera's
      value as before. A reader that treats 0 / 1 as "at that end" needs no change.
    - The panel disables `-` at `0` and `+` at `1`, and a slider release past a reached end sends
      nothing.
  - `caps.fNumberChoices` is NOT the lens's steps. It is only the positions the absolute aperture
    slider writes: the nominal 1/3-stop marks between the ends of `lens/iris/description` (the
    first and last entries are those ends). The camera snaps such a write to its nearest own step.
    - Lens steps depend on the lens AND the zoom. cam5 at 108 mm lands on 4.4 4.8 5.2 5.6 6.2
      6.7 7.3 8.0 while its description says min 4.1. The PP 6K has 1/6 steps near open (2.8
      2.9 3.1 3.2 3.3 3.5). No grid can list them, so +/- never step this grid: see
      `apertureStep` below.
    - `params.apertureNorm` is the slider position nearest the true f-stop.
  - `caps.fpsChoices` are the frame rates the camera lists for its current codec and resolution,
    × 100 (`"23.98"` = 2398).
  - `params.sensorFps100` is the off-speed rate (`offSpeedFrameRate` × 100).
  - `camera` is the product name (`Blackmagic Pocket Cinema Camera 4K`). It was the gphoto2
    `--auto-detect` model.
- **Not every firmware serves every endpoint.** Every capability endpoint is optional. One the
  firmware does not serve (404/501) is logged once at INFO. Any other failure (a 500 while
  booting, a body that does not decode) is logged every cycle, and is never taken for "not served".
  The relay falls back to:
  - ISO: the standard Pocket list 100 .. 25600 in 1/3 steps;
  - shutter: the standard denominators no faster than the frame rate;
  - white balance: 2500 .. 10000 K;
  - lens range: unknown.
    - The absolute slider uses f/2.8 .. f/22, widened to every f-stop the lens has reported on
      this link.
    - A slider position past the real end of the lens may be refused with 400. The relay reports
      that as `notApplied: apertureNorm`, not as an error.
    - For `apertureStep`, the iris's `irisRangePos` (`lens/iris normalised`) gives the ends first:
      a press toward the end the iris is at is `notApplied` at once, with no probe (v0.2.3). A
      camera without `normalised` still probes: a press the iris does not follow means the end of
      the lens, and a probe the camera refuses with 400 is the end of the lens too; since v0.2.5
      both report `notApplied: apertureStep`. With a KNOWN range, a 400 is a refused write (502),
      as before.

  What two bodies serve, read live:

| Endpoint | Pocket 4K fw 9.8 (SNV cam5) | Pocket 6K fw 8.6 (SNV cam3) |
|---|---|---|
| `system/product` | 200 | **404** |
| `system` | — | 204 |
| `lens/iris`, `lens/focus`, `lens/zoom` | 200 | 200 |
| `lens/iris/description` | 200 | **404** |
| `video/iso`, `video/whiteBalance`, `video/whiteBalanceTint` | 200 (PUT landed live) | 200 |
| `video/shutter`, `video/gain` | manual | 200 |
| `video/supportedISOs`, `video/supportedShutters` | manual | **404** |
| `video/whiteBalance/description`, `video/shutter/description` | manual | **404** |
| `system/format`, `system/supportedFormats` | manual | 200 |
| `video/ndFilter`, `system/supportedCodecFormats`, `system/supportedVideoFormats` | manual | **501** |

  "manual" = documented, not read live on that body. The 6K's own `/control/documentation.html`
  lists only the paths it serves.

- **Verified live vs. from the manual.** `system/product`, `lens/iris` GET and PUT
  `{apertureStop}`, `lens/zoom` and `lens/focus` were read on cam5 (Pocket 4K, fw 9.8). The 6K at
  fw 8.6 (cam3) answered every endpoint in the table above, and the iris walk landed on each of
  its 1/6-stop steps. The following come from Blackmagic's manual and are UNVERIFIED on a camera until the
  first live run:
  - the `PUT system/format` body with `frameRate` alone;
  - `doAuto` (its read-back may still show the Kelvin from before auto WB finished);
  - `PUT video/shutter`;
  - the `supportedFormats` / `supportedShutters` shapes;
  - systemd-networkd's lease path on the box;
  - the `Property=ID_VENDOR_ID` match (`1edb` itself was read live).
- **What changes for a writer of `PUT /api/params`:**
  - **New field `apertureStep` (v0.2.1, additive).** Its SIGN is the direction (`1` = a larger
    f-number, `-1` = smaller); the size is ignored. It moves the aperture by exactly ONE of the
    lens's own steps.
    - The relay reads the TRUE current f-stop and writes a target 1/16 stop away.
    - It waits up to 0.5 s for the iris (1.2 s before v0.2.5; live, the iris reads back moved
      within 340 ms on the 4K and ~220 ms on the 6K). If the iris has not moved, it widens the target by
      another 1/16 stop, until the iris lands on a different f-stop or the target reaches the
      `lens/iris/description` min or max.
    - The increment is smaller than half of two adjacent native gaps, so the first landing is
      always the next native step: none is skipped, on a 1/3- or 1/6-stop lens, at any zoom.
    - **At a reported end (v0.2.3):** when `lens/iris normalised` says the iris is already at the
      end the press heads for (`-1` at `0`, `+1` at `1`, within 0.001), the relay writes NOTHING
      and answers at once with `notApplied: ["apertureStep"]` and the same f-stop.
    - At the end of the lens found by probing (the target reached the description min/max, six
      dead probes or a 400 without a description) a press answers with the same f-stop. That is
      not an error. **Since v0.2.5 it is `notApplied: ["apertureStep"]`** (before, it was quiet,
      and the panel got no feedback).
    - **Learned per focal length (v0.2.5, additive).** The relay reads `lens/zoom focalLength` with
      each press's start and remembers, per focal length, every f-stop it saw, the pairs of stops
      a walk proved adjacent, and the ends it found. A press whose next stop is such a proven
      neighbour (no other known stop between them) writes that stop directly: ONE PUT, about
      0.4 s. If the iris does not take it, the relay walks the lens as above. A press toward a
      learned end is refused at once with `notApplied: ["apertureStep"]` and no PUT, and
      `irisRangePos` reads 0 / 1 there. Another focal length has its own memory; a camera without
      `lens/zoom` shares one (a zoom change it cannot see is then caught only by a stop read
      between two remembered neighbours, or a direct write that lands elsewhere). The memory is
      cleared whenever the camera link drops.
    - An iris that follows none of up to 6 probes with range left, or a lens whose description
      says `controllable: false`, is reported as `notApplied: ["apertureStep"]`. A
      `controllable: false` lens is not probed at all.
    - **Cost (v0.2.5):** each probe the iris does not follow waits 0.5 s; the landing about
      0.4 s. cam5 at 108 mm 5.2 -> 5.6 is about 1 s, 4.4 -> 5.2 about 2 s, a known neighbour about
      0.4 s; its zoom-shifted end (4.4 against a description min of 4.1) about 2 s the first time
      and at once after. The worst case is an iris that follows nothing: 6 waits, about 3 s.
    - **Queued presses:** a caller is answered right after its OWN press (worst about 4.5 s: an
      earlier absolute iris write waited out, then 6 dead probes), or once its drain has run 6 s.
      SETs still queued then run IN ORDER on a background drain that keeps the gate shut. So the
      service's 10 s write client never waits on a later press. Their results, `notApplied`
      included, reach the panel through `/api/state`.
    - An older relay ignores the field.
  - `apertureNorm` stays the absolute slider write. It snaps to the lens, which is fine for a
    drag.
  - **The answer shape:**
    - `200 {"applied": <number of camera PUTs>, "state": <RelayState>}`. `state` is read back
      from the camera after the writes (after ~1 s when the iris was written with
      `apertureNorm`), and its `notApplied` lists the keys the camera did not take (`apertureNorm`,
      `apertureStep`, `iso`, `shutter`, `kelvin`, `tint`, `fps`).
    - `202 {"queued": true}` when the SET queued behind a write in flight. It is applied in order
      by that write; the next `/api/state` shows it.
    - `422 <text starting "fps-not-settable:">` for an `fps` the camera does not list. Nothing was
      written.
    - `502 <text>` when the camera is offline, did not answer, or refused a write. The text names
      the PUT and the camera's own status and body.
  - `fps` is accepted only for a rate in `caps.fpsChoices`; any other value answers 422
    `fps-not-settable`.
  - `autoWb: true` now runs the camera's auto white balance.
  - A camera that refuses a write (e.g. 403 while auto exposure owns the iris) answers 502 with
    the camera's own status and text.
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
