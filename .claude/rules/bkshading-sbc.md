---
paths:
  - "scripts/bkshading-provision-sbc.sh"
  - "scripts/lib/bkshading-sbc-runtime.sh"
  - "scripts/lib/ro-root.sh"
  - "scripts/bkshading-deploy-relay.sh"
  - "scripts/lib/bkshading-deploy-runtime.sh"
  - "tests/python/test_bkshading_sbc_provision_808.py"
  - "tests/python/test_ro_root_808.py"
  - "scripts/bkshading-wifi-heal.sh"
  - "scripts/bkshading_sbc_netplan_wifi.py"
  - "scripts/lib/bkshading-sbc-wifi-probe.sh"
  - "systemd/bkshading-wifi-heal.service"
  - "systemd/bkshading-wifi-heal.timer"
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
  `--check` grades the root: `ro` = OK, `rw` = FAIL "reboot after --install". It also grades each
  unit of `bkshading_sbc_masked_units`: `systemctl is-enabled` must print `masked` (a real masked
  unit exits 1 with that word; a unit the image does not ship reads `masked` once `mask` linked it
  to /dev/null), else FAIL "re-run --install". Both rows apply to every SBC, wired or WiFi.
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
- **The WiFi belongs to `wpa_supplicant@wlan0`, roams by `bgscan` and heals a dead AP (design
  5972548198, live on handheld-1 3.10.2026).** After a read-only reboot the two strong APs rejected
  the board (they still held its PMF association), it joined the far AP at -73 dBm, got DHCP, and
  passed no traffic while `wpa_state=COMPLETED`. Two gaps: netplan 1.1 has NO `bgscan` key (its
  generated supplicant roams only on a LOST link), and nothing checks that a COMPLETED link carries
  traffic (`operstate`/carrier read `up`). `--install` on a board with a `wl*` radio:
  - MIGRATES the SSID + passphrase + `regulatory-domain` + DHCP out of the board's own netplan WiFi
    YAML (`scripts/bkshading_sbc_netplan_wifi.py`, PyYAML **BaseLoader** — every scalar stays its
    raw text like netplan's libyaml reader, so `password: 12345678` or `012345678` never becomes an
    int/octal) and writes `/etc/wpa_supplicant/wpa_supplicant-wlan0.conf` (0600): ctrl_interface in
    `/run`, `country=`, `key_mgmt`, `ieee80211w=1`, `bgscan="simple:30:-65:300"` and the 64-hex PSK.
    All values are named constants with reasons in `scripts/lib/bkshading-sbc-runtime.sh`.
  - The passphrase goes to `wpa_passphrase <ssid>` on STDIN (no second argument), never on an
    argv; its output echoes the passphrase in a `#psk="..."` comment line, which
    `bkshading_sbc_psk_hex_from_wpa_passphrase` drops (it takes only the `psk=<64 hex>` line). A
    netplan password that already IS 64 hex is used as-is (wpa_passphrase refuses that length).
    The YAML reader's stderr never carries a passphrase (a YAML parse error prints its class only:
    PyYAML's message quotes the offending line).
  - **The reader's exit code needs `set +e` INSIDE its process substitution.** The script runs
    under `set -euo pipefail`, and the `<( … )` subshell inherits errexit: a failing reader ended the
    subshell before `printf 'rc\0%s\0' "$?"` ran, so every reader failure read as "nothing to
    migrate" (review finding: with a conf present, `--install` then exited 0 and left netplan's
    wlan0 in place). Any `mapfile < <(cmd; printf rc)` pattern in a `set -e` script needs the same.
  - Writes `/etc/systemd/network/05-bkshading-wlan0.network` with the settings netplan generated
    (`05-` sorts before every `10-netplan-*` file; `DHCP=yes` for dhcp4 + dhcp6, `ipv4` for dhcp4
    alone — never a DHCPv6 the YAML did not ask for) and a `wpa_supplicant@wlan0.service.d/
    bkshading-restart.conf` drop-in (`Restart=on-failure`: Debian's unit has no `Restart=`; systemd
    brings a CRASHED supplicant back in 5 s, faster than the heal's stuck rung, which starts a
    stopped one on its next pass and reloads the driver under a hung one).
  - Installs the heal (`scripts/bkshading-wifi-heal.sh` + its two libs, mirroring the repo layout
    under `/usr/local/lib/bkshading/`, and `systemd/bkshading-wifi-heal.{service,timer}`), enables
    `systemd-networkd` + `wpa_supplicant@wlan0` + the timer, starts nothing — and only THEN moves the
    netplan WiFi YAML aside ONCE as `<file>.bak` (the `fstab.bak` pattern; it still holds the original
    passphrase, as the YAML did). A failure before the move leaves netplan's WiFi in charge at the
    next boot, never a board with no WiFi. Ethernet + usb0 YAMLs stay.
  - Refuses (exit 1, nothing changed — read before the rw remount) on: no netplan WiFi YAML AND no
    conf; a YAML that also configures other interfaces, a static address, an open/enterprise
    network or any key it would lose; two files defining wlan0; a wlan0 in `/run/netplan` or
    `/lib/netplan` that no `/etc/netplan` file of the same name shadows (netplan reads all three, the
    migration moves only `/etc` files); ANY `/run` or `/lib` file under the migrated file's own name
    (the shadowing is judged AFTER the move: once the `/etc` file is `.bak`, that file goes live —
    re-review finding, reproduced); a YAML present again while its `.bak` exists; a YAML next to an
    existing conf with a DIFFERENT WiFi (both named; the SAME WiFi — a re-run after an install that
    never got to the move — is finished). "Same" = `bkshading_sbc_wpa_conf_identity`: country +
    SSIDs + PSKs only, never the whole text, so a changed comment or lib constant is no difference;
    wpa_cli/ip/ping missing (the remediation names the remount for a read-only root). A re-run
    keeps an existing conf. A wired box skips all of it.
- **The heal** (`bkshading-wifi-heal.timer`, 20 s; `AccuracySec=1s` — the default 1 min would
  stretch the cadence): pings the DHCP default gateway read from `ip -4 route show default dev
  wlan0` (never a hard-coded address) with `-I wlan0`; a pass misses only when all 3 pings are lost.
  The pure `bkshading_sbc_wifi_heal_decide` table: not COMPLETED → none + count reset (the
  supplicant is working; each new association gets its full ~60 s for DHCP — the trade-off is
  written at the function; the STUCK rung below is the one exception to "not COMPLETED = wait");
  3 misses → `wpa_cli reassociate`; 6 → `systemctl restart
  wpa_supplicant@wlan0` + count reset. COMPLETED with no DHCP route counts as a miss (its own
  line, never "did not answer pings"). The count lives in `/run/bkshading-wifi-heal/misses`; a
  damaged count reads as 0 (decimal, never octal).
  - **The link reads are ONE probe lib** (`scripts/lib/bkshading-sbc-wifi-probe.sh`) the heal and
    `--check` share: the gateway (rc 2 = the read itself failed), the ping verdict (0 reply, 1 no
    reply, 2 no verdict — iputils exits 2 on setup errors, a missing tool reads 127), and the
    BSSID/signal/state snapshot with every `wpa_cli` call bounded by `timeout` (a wedged supplicant
    otherwise holds each call ~10 s and pushes a pass past `TimeoutStartSec`).
  - **A pass that cannot judge the link changes nothing.** A missing tool, an `ip` failure or a ping
    exit other than 0/1 keeps the count, names the tool and exits 1 (the unit shows failed). The
    first draft counted a missing `ping` as a miss: a healthy link was reassociated every ~60 s and
    its supplicant restarted every ~2 min (review finding, reproduced).
  - **The after-read waits for the NEW association.** `wpa_cli reassociate` returns at once, and
    while the supplicant scans it stays COMPLETED on the OLD BSSID, so an immediate read repeats
    "before" even when the roam succeeds. A COMPLETED read counts only after the state left
    COMPLETED or the BSSID moved; otherwise the heal waits the whole 15 s settle bound (a full
    2.4 + 5 GHz scan with passive DFS channels takes ~3-8 s, so no short fixed minimum is safe). The
    test stub keeps the old BSSID for N status reads (2 and 7) to prove it.
  - One journal line per action names the BSSID + signal before and after; the first miss, and the
    first answer after misses or an action (naming that action, from `/run/.../last-action`), are
    one line each; a supplicant that does not answer (`?`) is logged as such, never as "working".
    No reboot, no ifdown.
- **`--check` rows (wl* boards only, skipped on a wired box):** `wpa_supplicant@wlan0` and
  `systemd-networkd` enabled; the conf is mode 0600 and carries the exact bgscan line (the conf
  holds the PSK: read line by line, never printed); its `ctrl_interface`/`key_mgmt`/`ieee80211w`
  match the lib (`bkshading_sbc_wpa_conf_setting_lines` — a kept conf is never rewritten, so a later
  constant change such as the SAE ruling shows here as drift); netplan defines no wlan0 any more
  (read with the reader's `--names-only` mode, which prints file names only — `--check` never loads
  a passphrase); the heal
  (script, both libs, units, the restart drop-in) installed byte-identical to this checkout + its
  timer enabled; wpa_cli/ip/ping present; the gateway answers a ping — the FAIL names the BSSID,
  signal and wpa_state, and a missing tool or a no-verdict ping is named as such, never as a dead
  gateway.
- **`key_mgmt` = `WPA-PSK WPA-PSK-SHA256`, NO SAE (main ROZHODNUTÉ 5973115530).** SAE cannot
  authenticate from the hex PSK the conf carries; wpa_supplicant 2.10 prefers SAE whenever the
  driver supports it and fails with `SAE: No password available`. `newlevel.media` advertises
  `WPA2-PSK+SAE` on every BSSID (a WPA2/WPA3 transition network), so a listed SAE could keep the
  board off it. WPA2-PSK joins a transition AP fine. Do not re-add SAE without also storing the
  passphrase (`sae_password`), which the takeover deliberately never does.
- **The STUCK rung (live 3.10.2026):** after a run of forced reassociations the out-of-tree
  uwe5622 driver answered every connect with `Association request to the driver failed`, so
  wpa_state never reached COMPLETED and the reachability rungs (which wait while the supplicant is
  "working") never acted. A supplicant restart and a link down/up did NOT help; `modprobe -r
  sprdwl_ng && modprobe sprdwl_ng` did (COMPLETED in 6 s). The pure
  `bkshading_sbc_wifi_heal_stuck_decide` decides each pass (fresh-context review round, 3.10.2026):
  - **A supplicant that does not answer (`?`) is told apart by its unit.** The heal reads
    `systemctl is-active wpa_supplicant@wlan0` only then. `inactive`/`failed` = STOPPED: it is
    started on that pass, no driver reload, no stuck pass (the supervisor's live
    `systemctl stop` first read as hung and cost a reload). Any other word, an unreadable one
    included, = running but silent = HUNG = a stuck pass.
  - **A stuck pass** = not COMPLETED and either a driver-refused association in the supplicant
    journal since the last pass, or a hung supplicant. Plain scanning out of range is never stuck.
  - **Each journal line counts once.** The read follows `journalctl --cursor-file=/run/
    bkshading-wifi-heal/journal-cursor`; `--since -25s` only when there is no cursor yet (the old
    `--since` on every 20 s pass counted each refusal twice). A COMPLETED pass drops the cursor, so a
    later drop never reads an hour of a working stretch. A failed read counts 0 and drops the
    cursor; journalctl exits 1 on a cursor it cannot seek (live on dev1), and a kept bad cursor
    would blind the count forever. Right after the reload's `systemctl stop` the cursor moves to
    the end (`-n 1`): the old supplicant refuses until the stop returns, and none of that counts
    against the reloaded driver.
  - **After 3 stuck passes** (~60 s) the heal stops the supplicant and runs the pure
    `bkshading_sbc_wifi_heal_driver_plan`, then starts the supplicant:
    - `reload` (`modprobe -r` + load) while wlan0 is there;
    - `load` alone when wlan0 is gone;
    - no reload for no-modprobe / a built-in driver / an unknown module.
  - **The module name is remembered in `/run/bkshading-wifi-heal/driver-module`** whenever
    `/sys/class/net/wlan0/device/driver/module` resolves (every pass, bash `cd` + `pwd -P`). Without
    it, a reload whose load failed (wlan0 and its link gone) would read "a built-in driver" on every
    later pass until a reboot. A stopped supplicant with wlan0 gone gets the remembered module
    loaded before its start. Both names must pass `bkshading_sbc_wifi_heal_module_name_ok`.
  - **A TERM/INT/EXIT trap is armed only from the supplicant stop to its start.** A pass ended
    there loads the module, waits for wlan0 and starts the supplicant before it exits 143/130. The
    trap runs a no-op handler for further signals, never SIG_IGN: an ignored TERM is inherited, so
    `timeout` could no longer kill the children it starts.
  - **Every step is bounded** by named lib constants: wpa_cli/journalctl/is-active 3 s,
    systemctl/modprobe 20 s, the wlan0 wait 15 s, the after-read 23 s. The longest pass, a reload,
    is 133 s under `TimeoutStartSec=150`; the trap's 55 s fits `TimeoutStopSec=90`. A test computes
    both from the constants and pins the unit comment's figures.
  - **Live module facts, handheld-1 (read by the supervisor, 3.10.2026):**
    - `readlink -f /sys/class/net/wlan0/device/driver/module` = `/sys/module/sprdwl_ng`;
    - the driver is `/sys/bus/platform/drivers/unisoc_wifi`;
    - `sprdwl_ng` has refcount 0 with the supplicant stopped, so `modprobe -r` succeeds;
    - `uwe5622_bsp_sdio` is the in-use bus module, NOT what the link resolves to: never reload it;
    - the live reload via the heal logged `reload the WiFi driver sprdwl_ng ... result=ok`.
  - **Live checks:**
    - stopped: `systemctl stop wpa_supplicant@wlan0` → the next pass logs
      `start wpa_supplicant@wlan0.service on wlan0 (it was inactive; no driver reload)`;
    - hung: `systemctl kill -s STOP wpa_supplicant@wlan0` (the unit stays active, wpa_cli times
      out) → 3 stuck passes, then the reload (systemd's stop sends SIGCONT after SIGTERM, so the
      frozen process ends).
- **Do not force a BSSID with `wpa_cli bssid 0 <mac>` to test roaming unless you clear it with
  `bssid 0 00:00:00:00:00:00`** (`bssid 0 any` returns FAIL and leaves the lock). A locked network
  keeps the board on that one AP, and the heal cannot undo a lock it did not set.
- **Testing the heal:** the test runs the REAL script with PATH = a stub dir only, so a missing stub
  never reaches the machine's real `wpa_cli`:
  - python stubs for `wpa_cli`/`ip`/`ping`/`systemctl` answer from a JSON state that
    `reassociate`/`restart` update: a delayed roam, a ping exit code, a hanging `wpa_cli`;
  - `systemctl` models the supplicant unit: `stop` sets it inactive (no wpa_cli answer), `start`
    refuses while the fake wlan0 is missing (the unit Requires= the device), `is-active` answers;
  - the `journalctl` stub serves lines ONLY for the exact calls the heal must make (a `--since`
    read without a cursor, a cursor read, a `-n 1` cursor-to-end), and mixes ordinary supplicant
    lines into the output. It also fails a cursor it cannot seek like the real journalctl;
  - the `modprobe` stub moves `<fake sysfs>/wlan0` aside on `-r` and brings it back from a
    detached process `iface_delay_s` after a load (`modprobe_rc` / `modprobe_load_rc` fail it), so
    the wait for wlan0 is exercised;
  - `BKSHADING_WIFI_HEAL_SYSFS_NET` points the script at that fake `/sys/class/net`, where
    `wlan0/device/driver/module` links to a dir named after the module;
  - symlinks to `dirname`/`mkdir`/`mv`/`rm`/`sleep`/`timeout` complete the stub dir.

  The trap test lets the modprobe stub SIGTERM the heal's bash (its grandparent, through
  `timeout`, found in `/proc/<ppid>/stat`) right after `modprobe -r`.

  The `wpa_passphrase` stub runs the real binary where it exists (dev1) and emulates it
  exactly elsewhere (the CI runner has none); the PSK assertion uses `hashlib.pbkdf2_hmac`
  independently, so both paths are checked against the 802.11i definition. The provision harness
  points `BKSHADING_SBC_NETPLAN_OTHER_DIRS` at temp dirs, never the test machine's own netplan.
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
7. **WiFi roam + heal (design 5972548198)** — copy the current `scripts/` dir, rerun `--install`
   (it migrates the netplan WiFi, moves `30-wifis-dhcp.yaml` to `.bak`) and reboot. Then:
   - `ls /run/netplan/` has no `wpa-wlan0.conf`; `systemctl is-active wpa_supplicant@wlan0`;
     `systemctl cat wpa_supplicant@wlan0` shows the `bkshading-restart.conf` drop-in;
     `systemctl list-timers bkshading-wifi-heal.timer` shows a 20 s cadence;
   - `wpa_cli -i wlan0 status` = COMPLETED on the strong AP; `--check` is all OK (the gateway row
     names the BSSID + signal); `systemctl --failed` is empty;
   - **forced bad AP:** `wpa_cli -i wlan0 bssid 0 <far-bssid>` + `wpa_cli -i wlan0 reassociate` →
     the board sits on the far AP (about -73 dBm). Then clear the lock with
     `wpa_cli -i wlan0 bssid 0 00:00:00:00:00:00` (`bssid 0 any` FAILs and keeps it): within about
     30-60 s bgscan (30 s scans below -65 dBm) roams back to the strong AP —
     `journalctl -u wpa_supplicant@wlan0` shows the new `CTRL-EVENT-CONNECTED`, `wpa_cli status`
     the strong BSSID;
   - **the heal acts on an unreachable gateway:** drop only the pings to the gateway, leaving the
     association COMPLETED — `nft add table inet healtest; nft add chain inet healtest out '{ type
     filter hook output priority 0; }'; nft add rule inet healtest out ip daddr <gw> icmp type
     echo-request drop`. `journalctl -u bkshading-wifi-heal -f`: a miss line, about 60 s later the
     `wpa_cli reassociate` line with BSSID/signal before and after, about 60 s after that the
     `systemctl restart wpa_supplicant@wlan0` line. `nft delete table inet healtest` → the next
     pass logs `answers again` (or nothing after a restart reset the count) and `--check` is OK.
