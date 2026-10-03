---
paths:
  - "scripts/bkshading-provision-sbc.sh"
  - "scripts/lib/bkshading-sbc-runtime.sh"
  - "scripts/lib/ro-root.sh"
  - "scripts/bkshading-deploy-relay.sh"
  - "scripts/lib/bkshading-deploy-runtime.sh"
  - "tests/python/test_bkshading_sbc_provision_808.py"
  - "tests/python/test_ro_root_808.py"
---

# bkshading handheld SBC — provisioning, the read-only root, the arm64 relay deploy (issue 808)

Split out of `.claude/rules/bkshading.md` (which keeps the service, relay and panel knowledge).

## SBC / handheld provisioning (the last milestone; Design v3, owner 14.9.2026)
A handheld camera runs the SAME `bkshading-relay` on **a separately powered zero-class arm64 SBC
with WiFi**: camera USB → SBC host port (PTP/gphoto2), SBC on the rig WiFi. **The board is
DEVICE-AGNOSTIC** (owner has not finally chosen — Design v3, comment 5664682477 + ROZHODNUTÉ
5664746806): a Raspberry **Pi Zero 2 W**, a **Radxa ZERO 3W**, or an **Orange Pi Zero 2W** (the
ordered prototype). **The box's power supply is the OWNER's own business (ruling 17.9.2026, #808
comment 5711321335: „napajanie si normalne ja riesim") — never a design item, never a bench item,
never a question to him.** The only electrical fact the docs state: **all candidate boards are
5 V-only — 5 V on the board's power port, never raw 15 V / D-tap.** (Background only: PTP makes the
camera the USB *device*, so the camera itself is not the box's power source.) The service already understands the
handheld (`Transport::SbcRelay`, `handheld-1..3` / `transport="sbc-relay"` in
`bkshading.example.toml`, a params-only block — no NDI preview). The box side is
`scripts/bkshading-provision-sbc.sh` (+ pure lib `scripts/lib/bkshading-sbc-runtime.sh`), mirroring
the relay/cloudflared provisioning canon but with two deliberate deltas + one gotcha:
- **Do NOT hard-code one board's port topology.** Per-board (all one USB cable to the camera + a 5 V
  feed the owner arranges): **Pi Zero 2 W** = micro-USB OTG host + a separate micro-USB 5 V power-in
  (2.4 GHz WiFi only → needs a 2.4 GHz SSID on site); **Radxa ZERO 3W** = USB3-C host + OTG-C 5 V-in
  (dual-band); **Orange Pi Zero 2W** = two USB-C (host-vs-power is revision-dependent — probe both;
  dual-band).
- **The SBC REUSES `systemd/bkshading-relay.service` UNCHANGED** (owner: "the SAME relay component")
  and writes **NO `CAMERA_BOX_CAPTURE_FPS` env** — an SBC has no camera-box appliance to derive from
  and a handheld has no grab comparison, so the unit's `EnvironmentFile=-` degrades gracefully
  (relay → `capture_fps=None` → service static config). Do NOT reuse `bkshading-provision-relay.sh`
  (its whole job is deriving that env from `camera-box.service.d` drop-ins, which an SBC lacks).
- **A `systemd/bkshading-relay.service` UNIT-FILE change never rides any binary deploy** (neither
  `bkshading-deploy-relay.sh` nor the fleet post-merge deploy touch `/etc/systemd/system/`) — it
  needs its own manual re-provision per box: `mount -o remount,rw /` → write the unit →
  `systemctl daemon-reload && systemctl restart bkshading-relay` → read back
  `systemctl show -p Restart,RestartUSec,ActiveState` → `mount -o remount,ro /`. Done live
  2026-09-04 on cam1+cam2 for the issue-1228 `Restart=on-failure`/`RestartSec=5` unit (cam2 had
  drifted on an older `Restart=always`/3 provisioning). Symptom of forgetting this: repo unit and
  `systemctl cat` disagree after a green release.
- **The SBC root goes READ-ONLY, the same as the camboxes (ROZHODNUTÉ 5948648089, design
  5971558113).** The box is unplugged after each ~3 h use; a power cut mid-write can corrupt the
  microSD root. `--install` (enable-only, effective at the next reboot):
  - writes `ro_root_fstab_text` from `scripts/lib/ro-root.sh`: the cambox root line + the cambox
    tmpfs set, the board's own other mounts kept verbatim (a Pi OS `/boot/firmware`), the original
    saved ONCE to `fstab.bak`;
  - makes journald volatile (`99-bkshading-volatile.conf`, `Storage=volatile`) after removing the
    bench-debug `10-persistent.conf`; a single-partition SBC has no journal partition;
  - masks `armbian-ramlog` and `systemd-networkd-persistent-storage` (the Debian trixie unit runs
    `networkctl persistent-storage yes`, fails `StorageReadOnly` on a ro root and leaves the board
    `degraded`; live on handheld-1, 3.10.2026 — networkd keeps its state in /run without it), and
    `fake-hwclock-save` + its hourly timer (cannot write `/etc/fake-hwclock.data`; the boot-time
    `fake-hwclock-load` only reads it and stays);
  - on a root that is already ro (a re-run), remounts rw for its own writes and back;
  - reads the root first (an unreadable root refuses untouched) and refuses on a cambox
    (`/usr/local/bin/camera-box` present), whose root is `setup-device.sh`'s.
  `--check` grades the root: `ro` = OK, `rw` = FAIL "reboot after --install".
- **ONE read-only canon for the provisioning scripts: `scripts/lib/ro-root.sh`.** `setup-device.sh`
  STEP 18 writes its root line and every tmpfs line through it, per line, because the cambox fstab
  also carries the EFI line and the issue-1309 journal-partition line between `/var/log` and
  `/var/tmp`. The cambox fstab stays byte-identical: `tests/python/test_ro_root_808.py` runs the
  lifted STEP 18 heredoc against the golden `tests/fixtures/ro_root_fstab_808/`. The root reading
  is `ro_root_mount_mode`: setup-device's `root_mount_is_readonly` calls it, verify-device.sh keeps
  its own copy (parity-pinned). Change a tmpfs line in the LIB, and only on purpose: it changes
  every cambox at its next provisioning. NOT covered: the image builders. `create-usb-linux.sh`
  writes the first-boot (rw) fstab that STEP 18 later replaces; `build-image.sh`'s overlay image
  still carries its own tmpfs set.
- **Deploy uses `bkshading-deploy-relay.sh --arch arm64`.** `--arch arm64` fetches the
  `bkshading-relay-linux-arm64` artifact. The remount follows the TARGET's own root
  (`bkshading_deploy_root_opts_cmd` = `findmnt -no OPTIONS /` with a `/proc/mounts` fallback, decided
  by the pure `bkshading_deploy_root_remount_action`):
  - `ro` → the `remount,rw → swap → remount,ro` cycle (every cambox, a read-only SBC);
  - `rw` → no remount (a board before its first read-only reboot);
  - anything else → refuse before the box is touched, exactly like an unreadable relay state.
  One path for cambox and SBC; the old per-deploy flag is gone (an unknown argument now).
- **CROSS-BUILD GOTCHA — only the RELAY cross-builds to aarch64 trivially; the SERVICE does NOT.**
  The relay is pure Rust (axum/tokio/serde/clap; **no reqwest/rustls/ring, no libndi** on the relay
  side), so the CI `bkshading` job cross-compiles it for `aarch64-unknown-linux-gnu` with just
  `rustup target add` + the `gcc-aarch64-linux-gnu` linker + `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER`.
  Target = aarch64 (NOT armhf): every candidate board is ARMv8 (Cortex-A53) with a 64-bit stock
  image; a 32-bit `armv7-unknown-linux-gnueabihf` build is one extra matrix entry only if a legacy
  handheld needs it. **Do NOT naively add a service ARM cross-build** — the service pulls
  `ring`/`rustls` (reqwest) + the libndi FFI, which do NOT cross-link with a bare gcc linker; the
  service is Windows/amd64 only (it runs on the strih PC), so there is deliberately no service ARM
  artifact.
- **`--check` verifies the deployed relay binary is actually AArch64** (an ELF `e_machine` read via
  `od` — offset 18, 2 bytes LE, AArch64=183 / x86-64=62; pure helpers in `bkshading-sbc-runtime.sh`,
  Tier-0 testable with a 20-byte fake-ELF fixture) so a mis-deployed amd64 binary is caught here,
  not at reboot with an opaque `Exec format error`.
- **`--check` also verifies the WiFi link is up** — a pure `bkshading_sbc_wifi_link_state
  <sysfs-root> <iface-glob>` reads `/sys/class/net/wl*/operstate` and returns `up`/`down`/`none`
  (`BKSHADING_SBC_NET_SYSFS` injects a fake tree for Tier-0). **`operstate` is the primary signal
  but NOT the only one — `carrier==1` also counts as up.** Some drivers (notably the **Orange Pi
  Zero 2W's out-of-tree `uwe5622`**) leave `operstate` at `"unknown"`/`"dormant"` while genuinely
  associated, so operstate-alone would false-FAIL the very prototype board; a genuinely-down link is
  `operstate down` AND no carrier. **Band-agnostic on purpose** (a
  2.4 GHz-only Pi Zero 2 W is as valid as a dual-band board — the check only proves a link, never a
  band/SSID); an optional best-effort `iw`-parsed SSID enriches the OK line (`bkshading_sbc_wifi_ssid_from_iw`,
  never gating). A **wired box with no `wl*` interface (the cambox class, which runs the SAME reused
  unit) SKIPs the WiFi check — never FAILs**; a down wireless link FAILs with a `nmcli device wifi
  connect …` join remediation (noting a 2.4 GHz-only board needs a 2.4 GHz SSID on site).
- The physical bring-up (flash the arm64 image, headless WiFi, then deploy + `--install` + reboot)
  is the owner's/supervisor's rig step. Transports stay USB-PTP (gphoto2/libusb) / USB-Eth REST —
  NEVER Bluetooth; a gphoto2 camera is a USB device, not a network link, so the netplan `enx*`
  CDC-NCM trap (#1155) does not touch the handheld.

### Supervisor bench checklist (run when the prototype board arrives — the HARDWARE half, UNVERIFIED in code lanes)
The code lane ships the provisioning + `--check`; these live-hardware steps are the supervisor's:
1. ~~PD / power-role listener on the camera's USB-C port~~ — **DROPPED (owner ruling 17.9.2026):
   the box's power supply is the owner's business; nothing about power is measured, designed or
   asked.** The board arrives powered by whatever the owner arranged (5 V on its power port).
2. **`gphoto2 --auto-detect` on the real board** — the camera enumerates on the SBC's USB host port
   and PTP control works (the relay's transport).
3. **WiFi join** — the board is on the rig WiFi SSID; on a **2.4 GHz-only** board bring up a 2.4 GHz
   SSID on site first. Then `scripts/bkshading-provision-sbc.sh --check` reports the link `up`.
4. **Per-board port/power probe** — **Orange Pi Zero 2W:** which USB-C is host vs power is
   **revision-dependent — probe both**; its WiFi driver is **out-of-tree (`uwe5622`)**, so verify
   `wl*` comes up on the vendor/Armbian image **before** `--install`. **Radxa ZERO 3W:** USB3-C =
   host, OTG-C = 5 V-in. **Pi Zero 2 W:** micro-USB OTG = host, separate micro-USB = 5 V-in.
5. **End-to-end** — deploy the aarch64 relay, `--install`, reboot, add the `handheld-N` record, and
   confirm the strih `bkshading` service sees the handheld live (params-only block).
6. **Read-only root (the final bench step, owner ruling 5948648089)** — copy the current `scripts/`
   dir (the new `lib/ro-root.sh` comes with it), rerun `--install` and reboot the board. Then:
   - `findmnt -no OPTIONS /` starts with `ro`;
   - `systemctl --failed` is empty;
   - `--check` is all OK (the root row included);
   - the relay reads the camera and a kelvin write round-trips;
   - a relay redeploy logs `root on <host>: ro -> remount rw for the swap, back to ro after`.
   A unit that writes `/var/lib` at start fails on a ro root (`provisioning-scripts.md`, the
   `StateDirectory=` section): fix it in the SBC provisioning, never by leaving the root rw.
   The microSD card is an ordinary brand A1 card, never an endurance card (ruling 5948648089:
   ~3 h of use a week, so wear is no argument).
   The tmpfs sizes are caps, not reservations, but the 512M `/var/cache` cap equals all the RAM of
   a 512 MB board (Pi Zero 2 W): an `apt-get` on such a board can run it out of memory. Run apt on
   the 2 GB Orange Pi without worry; on a 512 MB board, keep apt runs small (one package).
