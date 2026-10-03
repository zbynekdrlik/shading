#!/usr/bin/env bash
# scripts/bkshading-provision-sbc.sh — provision + verify the bkshading RELAY on a mini SBC/handheld.
# Extended header below `set -euo pipefail` (kept early for pre-write-script-check.sh).
set -euo pipefail

# ---------------------------------------------------------------------------------------------
# WHY: the LAST bkshading milestone (issue 808) — the handheld branch of the owner architecture
# (comment 5356048130 path 2, "cieľový stav"; Design v3 comment 5664682477, 14.9.2026): a camera
# plugs USB into a separately powered zero-class arm64 SBC with WiFi (the board is device-agnostic —
# a Raspberry Pi Zero 2 W, a Radxa ZERO 3W, or an Orange Pi Zero 2W — powered from the camera cage's
# V-mount 5 V USB splitter) which runs the SAME `bkshading-relay` component the camboxes run — a
# "mini-cambox without video". The strih aggregation service already understands this transport
# (`Transport::SbcRelay`, the `handheld-1` record in bkshading.example.toml, a params-only block
# with no NDI preview), but nothing PROVISIONS the relay on a bare SBC. This script does.
#
# It is the SBC counterpart of scripts/bkshading-provision-relay.sh, with two deliberate
# differences (see the design comment on issue 808):
#   (1) it REUSES systemd/bkshading-relay.service UNCHANGED — the SBC runs the same relay; and
#   (2) it writes NO CAMERA_BOX_CAPTURE_FPS env: an SBC has no camera-box appliance (no
#       camera-box.service.d drop-ins to derive from) and a handheld has no grab-rate comparison
#       (its config carries no grab_fps). The unit's `EnvironmentFile=-` makes the absent file
#       graceful — the relay reports capture_fps=None and the service uses its static config,
#       never a wrong value.
#
# READ-ONLY ROOT (issue 808 slice B, owner ruling 5948648089: "the same as the camboxes"): the
# handheld is unplugged after every ~3 h use, and an abrupt power-off mid-write can corrupt the ext4
# root on its microSD. So --install also writes the read-only fstab from the ONE shared canon
# scripts/lib/ro-root.sh (the cambox root line + its five tmpfs mounts; the board's own other mounts
# kept; the original saved once to fstab.bak), makes journald volatile (the journal in RAM -- a
# single-partition SBC has no journal partition), and masks armbian-ramlog (it syncs a RAM /var/log
# back onto the root), systemd-networkd-persistent-storage and fake-hwclock-save + its timer (both
# fail on a read-only root). It all takes effect at the next reboot. An update later remounts rw, writes,
# and remounts ro: --install does that itself on a root that is already read-only, and
# scripts/bkshading-deploy-relay.sh does it when it reads a read-only root on the target.
#
# WIFI ROAM + HEAL (issue 808, design 5972548198): netplan 1.1 has no `bgscan` key, so its generated
# supplicant never roams to a stronger AP, and nothing checks that a COMPLETED link carries traffic
# (handheld-1 sat on a dead far AP after a reboot). On a board with a wl* radio, --install takes
# wlan0 over from netplan:
#   - writes /etc/wpa_supplicant/wpa_supplicant-wlan0.conf (0600) with ctrl_interface in /run, the
#     country, key_mgmt + PMF, `bgscan` (named constants in scripts/lib/bkshading-sbc-runtime.sh)
#     and the PSK as the 64-hex wpa_passphrase value. The SSID + passphrase are MIGRATED from the
#     board's own netplan WiFi YAML (read by scripts/bkshading_sbc_netplan_wifi.py); the passphrase
#     goes to wpa_passphrase on stdin, never on an argv, never printed, never into a log;
#   - moves that netplan YAML aside once as <file>.bak (ethernet + usb0 YAMLs stay) and writes a
#     systemd-networkd DHCP file for wlan0 with the settings netplan generated;
#   - installs the heal (scripts/bkshading-wifi-heal.sh + this lib, to /usr/local/lib/bkshading/,
#     and systemd/bkshading-wifi-heal.{service,timer}) and enables wpa_supplicant@wlan0 + the timer.
# A re-run keeps an existing wpa_supplicant-wlan0.conf. A board with neither a netplan WiFi YAML nor
# that conf is refused (exit 1, nothing changed). A wired box (no wl* radio) skips all of it.
#
# Idempotent (re-run just re-verifies), fail-loud (a gap exits non-zero with the exact remediation),
# ENABLE-ONLY (daemon-reload + enable, NEVER start/restart — defer to reboot, per
# .claude/rules/provisioning-scripts.md; the relay's live verify against the camera is the
# supervisor's post-reboot rig step).
#
# The relay BINARY must be the aarch64 build (the `bkshading-relay-linux-arm64` CI artifact,
# deployed via `scripts/bkshading-deploy-relay.sh --arch arm64`). --check verifies the deployed
# binary is actually AArch64 (an ELF e_machine read) so a mis-deployed amd64 binary is caught here,
# not at reboot with an opaque `Exec format error`.
#
# Usage:  scripts/bkshading-provision-sbc.sh [--check|--install]
#   --check    (default) verify gphoto2 + unit + enabled + binary present + binary is aarch64 +
#              the WiFi link is up + wpa_supplicant@wlan0 enabled + the bgscan line in its conf +
#              the heal installed and its timer enabled + the DHCP gateway answers a ping (the WiFi
#              rows are SKIPPED on a wired box with no wl* interface, e.g. a cambox) + the root
#              filesystem is read-only; 0 if all OK, 1 + remediation.
#   --install  install gphoto2 (if missing), install + enable the (reused) relay unit, take the
#              WiFi over from netplan + install the heal (a wl* board), write the read-only fstab +
#              volatile journald + mask the root writers; enable-only, effective at the next
#              reboot. Refuses (exit 2) on a cambox: setup-device.sh owns a cambox's root.
#
# Exit codes: 0 = OK; 1 = not fully provisioned + remediation printed (or the root / the WiFi config
# could not be read); 2 = bad argument / run on a cambox.
#
# Overridable targets (for Tier-0 tests to a temp root — no root/apt/systemd needed):
#   BKSHADING_SBC_UNIT_DEST, BKSHADING_SBC_BIN, BKSHADING_SBC_GPHOTO2, BKSHADING_SBC_SYSTEMCTL,
#   BKSHADING_SBC_NET_SYSFS (the /sys/class/net root the WiFi-link --check reads; default
#   /sys/class/net), BKSHADING_SBC_FSTAB (default /etc/fstab), BKSHADING_SBC_JOURNALD_DIR
#   (default /etc/systemd/journald.conf.d), BKSHADING_SBC_FINDMNT (default findmnt),
#   BKSHADING_SBC_MOUNT (default mount), BKSHADING_SBC_PROC_MOUNTS (default /proc/mounts),
#   BKSHADING_SBC_CAMBOX_MARKER (default /usr/local/bin/camera-box -- present = a cambox).
#   WiFi: BKSHADING_SBC_NETPLAN_DIR (/etc/netplan), BKSHADING_SBC_WPA_CONF_DIR (/etc/wpa_supplicant),
#   BKSHADING_SBC_NETWORKD_DIR (/etc/systemd/network), BKSHADING_SBC_HEAL_DIR
#   (/usr/local/lib/bkshading), BKSHADING_SBC_PYTHON (python3), BKSHADING_SBC_WPA_PASSPHRASE
#   (wpa_passphrase), BKSHADING_SBC_WPA_CLI (wpa_cli), BKSHADING_SBC_IP (ip), BKSHADING_SBC_PING
#   (ping). The heal units go beside the relay unit (the directory of BKSHADING_SBC_UNIT_DEST).
# ---------------------------------------------------------------------------------------------

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
# shellcheck source=scripts/lib/bkshading-relay-runtime.sh
. "$HERE/lib/bkshading-relay-runtime.sh" # unit name / bin path / gphoto2 pkg — REUSED (one source of truth)
# shellcheck source=scripts/lib/bkshading-sbc-runtime.sh
. "$HERE/lib/bkshading-sbc-runtime.sh" # SBC-specific: ELF-arch check, cross target, no-env decision
# shellcheck source=scripts/lib/ro-root.sh
. "$HERE/lib/ro-root.sh" # the ONE read-only-root canon, shared with setup-device.sh STEP 18 (issue 808)

UNIT_NAME="$(bkshading_relay_unit_name)"
UNIT_SRC="$REPO/systemd/$UNIT_NAME"
UNIT_DEST="${BKSHADING_SBC_UNIT_DEST:-/etc/systemd/system/$UNIT_NAME}"
RELAY_BIN="${BKSHADING_SBC_BIN:-$(bkshading_relay_bin_path)}"
GPHOTO2="${BKSHADING_SBC_GPHOTO2:-gphoto2}"
SYSTEMCTL="${BKSHADING_SBC_SYSTEMCTL:-systemctl}"
APT_PKG="$(bkshading_relay_apt_package)"
FSTAB="${BKSHADING_SBC_FSTAB:-/etc/fstab}"
JOURNALD_DIR="${BKSHADING_SBC_JOURNALD_DIR:-/etc/systemd/journald.conf.d}"
FINDMNT="${BKSHADING_SBC_FINDMNT:-findmnt}"
MOUNT="${BKSHADING_SBC_MOUNT:-mount}"
PROC_MOUNTS="${BKSHADING_SBC_PROC_MOUNTS:-/proc/mounts}"
CAMBOX_MARKER="${BKSHADING_SBC_CAMBOX_MARKER:-/usr/local/bin/camera-box}"
NET_SYSFS="${BKSHADING_SBC_NET_SYSFS:-/sys/class/net}"
UNIT_DIR="$(dirname "$UNIT_DEST")"
NETPLAN_DIR="${BKSHADING_SBC_NETPLAN_DIR:-/etc/netplan}"
WPA_CONF_DIR="${BKSHADING_SBC_WPA_CONF_DIR:-/etc/wpa_supplicant}"
NETWORKD_DIR="${BKSHADING_SBC_NETWORKD_DIR:-/etc/systemd/network}"
HEAL_DIR="${BKSHADING_SBC_HEAL_DIR:-$(bkshading_sbc_wifi_heal_install_dir)}"
PYTHON="${BKSHADING_SBC_PYTHON:-python3}"
WPA_PASSPHRASE="${BKSHADING_SBC_WPA_PASSPHRASE:-wpa_passphrase}"
WPA_CLI="${BKSHADING_SBC_WPA_CLI:-wpa_cli}"
IP="${BKSHADING_SBC_IP:-ip}"
PING="${BKSHADING_SBC_PING:-ping}"
WPA_CONF="$WPA_CONF_DIR/$(bkshading_sbc_wpa_conf_name)"
WIFI_IFACE="$(bkshading_sbc_wifi_iface)"

MODE="${1:---check}"
case "$MODE" in
  --check | --install) ;;
  -h | --help)
    grep -E '^# ' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  *)
    echo "unknown argument: $MODE (use --check or --install)" >&2
    exit 2
    ;;
esac

install_gphoto2() {
  if command -v "$GPHOTO2" >/dev/null 2>&1; then
    echo "  gphoto2 already present: $(command -v "$GPHOTO2")"
    return 0
  fi
  echo "  installing $APT_PKG (the relay's USB-PTP runtime) via apt ..."
  apt-get update -qq
  apt-get install -y -qq "$APT_PKG"
}

# The root filesystem's mount options (`findmnt`, else /proc/mounts — the setup-device.sh
# ensure_root_writable fallback); empty when neither answers.
read_root_opts() {
  "$FINDMNT" -n -o OPTIONS / 2>/dev/null || awk '$2=="/"{print $4; exit}' "$PROC_MOUNTS" 2>/dev/null || true
}

# ROOT_WAS_RO: set when --install found the root already read-only and remounted it rw for its own
# writes (a re-run after the read-only reboot); restore_root_mode then puts it back to ro.
ROOT_WAS_RO=0
restore_root_mode() {
  [ "$ROOT_WAS_RO" = 1 ] || return 0
  ROOT_WAS_RO=0
  # An apt call during the install can D-Bus-activate PackageKit, whose open database blocks the
  # ro remount with EBUSY (the setup-device.sh restore_root_mode incident) -- stop the writers first.
  "$SYSTEMCTL" stop packagekit unattended-upgrades 2>/dev/null || true
  if "$MOUNT" -o remount,ro /; then
    echo "  root back to read-only"
  else
    echo "ERROR: 'mount -o remount,ro /' failed -- the root stays read-WRITE until the next reboot" >&2
    echo "       (the read-only fstab pins it ro at boot); stop the writer and remount by hand." >&2
    return 1
  fi
}

# Write the read-only fstab (the shared canon), the volatile journald drop-in, and mask the units
# that would write logs onto the root. Takes effect at the next reboot.
install_ro_root() {  # $1 = root UUID, $2 = root fstype
  local uuid="$1" fstype="$2" text f unit
  if [ ! -f "$FSTAB.bak" ]; then
    cp "$FSTAB" "$FSTAB.bak"
    echo "  backed up the original fstab to $FSTAB.bak"
  else
    echo "  $FSTAB.bak already exists -- keeping the original backup (re-run)"
  fi
  text="$(ro_root_fstab_text "$uuid" "$fstype" "$(cat "$FSTAB.bak")")"
  printf '%s\n' "$text" > "$FSTAB.tmp.$$"
  mv -f "$FSTAB.tmp.$$" "$FSTAB"
  echo "  wrote the read-only fstab: / ro + tmpfs $(ro_root_tmpfs_paths | tr '\n' ' ')(scripts/lib/ro-root.sh)"

  mkdir -p "$JOURNALD_DIR"
  for f in $(bkshading_sbc_stale_journald_dropins); do
    if [ -e "$JOURNALD_DIR/$f" ]; then
      rm -f "$JOURNALD_DIR/$f"
      echo "  removed the debug journald drop-in $JOURNALD_DIR/$f"
    fi
  done
  bkshading_sbc_journald_dropin_content > "$JOURNALD_DIR/$(bkshading_sbc_journald_dropin_name)"
  echo "  journald: Storage=volatile ($JOURNALD_DIR/$(bkshading_sbc_journald_dropin_name))"

  for unit in $(bkshading_sbc_masked_units); do
    "$SYSTEMCTL" mask "$unit"
    echo "  masked $unit (it would write onto the read-only root)"
  done
}

# --- The WiFi takeover + heal (issue 808, design 5972548198) ---
# plan_wifi READS and decides before anything changes (a refusal leaves the box untouched);
# apply_wifi writes inside the rw window; enable_wifi enables (never starts) the units.
#   WIFI_PLAN: skip (no wl* radio), keep (the existing conf stays), migrate (write it from netplan)
#   WIFI_YAML: the netplan WiFi YAML to move aside ("" = none present)
#   WIFI_CONF_TEXT: the conf to write on migrate -- it holds the PSK, so it is never printed.
WIFI_PLAN=skip
WIFI_YAML=""
WIFI_CONF_TEXT=""
WIFI_SUMMARY=""

wifi_refuse() {
  echo "ERROR: $1 -- nothing changed" >&2
  shift
  local line
  for line in "$@"; do echo "       $line" >&2; done
  exit 1
}

plan_wifi() {
  local state rc=3 i country="" pass psk out
  local -a fields=() ssids=() passes=() pairs=()
  state="$(bkshading_sbc_wifi_link_state "$NET_SYSFS" 'wl*')"
  if [ "$state" = none ]; then
    WIFI_PLAN=skip
    return 0
  fi
  if [ ! -e "$NET_SYSFS/$WIFI_IFACE" ]; then
    wifi_refuse "a wireless radio is present ($(bkshading_sbc_first_wifi_iface "$NET_SYSFS" 'wl*')) but no $WIFI_IFACE" \
      "the WiFi takeover owns $WIFI_IFACE only (every candidate board names its radio $WIFI_IFACE)"
  fi
  if compgen -G "$NETPLAN_DIR/*.yaml" >/dev/null; then
    # NUL-separated tag/value pairs from the one YAML reader, its exit code appended last. The
    # passphrase travels only through this pipe into bash variables -- never an argv, never a file.
    mapfile -d '' -t fields < <("$PYTHON" "$HERE/bkshading_sbc_netplan_wifi.py" "$NETPLAN_DIR" "$WIFI_IFACE"; printf 'rc\0%s\0' "$?")
    for ((i = 0; i + 1 < ${#fields[@]}; i += 2)); do
      case "${fields[i]}" in
        file) WIFI_YAML="${fields[i + 1]}" ;;
        country) country="${fields[i + 1]}" ;;
        ssid) ssids+=("${fields[i + 1]}") ;;
        pass) passes+=("${fields[i + 1]}") ;;
        rc) rc="${fields[i + 1]}" ;;
      esac
    done
    fields=()
    case "$rc" in
      0 | 3) ;;
      2) wifi_refuse "the netplan WiFi config under $NETPLAN_DIR cannot be migrated (reason above)" \
        "fix that YAML by hand (or write $WPA_CONF yourself), then re-run --install" ;;
      *) wifi_refuse "could not read the netplan YAML under $NETPLAN_DIR ($PYTHON exited $rc)" \
        "the reader needs python3 + PyYAML, which the netplan.io package depends on" ;;
    esac
  fi
  if [ "$rc" = 0 ] && { [ "${#ssids[@]}" -eq 0 ] || [ "${#ssids[@]}" -ne "${#passes[@]}" ]; }; then
    wifi_refuse "the netplan WiFi reader returned an incomplete access-point list for $WIFI_IFACE"
  fi
  if [ -n "$WIFI_YAML" ] && [ -e "$WIFI_YAML.bak" ]; then
    wifi_refuse "$WIFI_YAML defines $WIFI_IFACE again while $WIFI_YAML.bak already holds the original" \
      "netplan would run a second supplicant on $WIFI_IFACE; remove or merge one of the two by hand"
  fi
  if [ -e "$WPA_CONF" ]; then
    WIFI_PLAN=keep
    return 0
  fi
  if [ -z "$WIFI_YAML" ]; then
    wifi_refuse "$WIFI_IFACE has no WiFi config to migrate: no netplan YAML under $NETPLAN_DIR defines wifis.$WIFI_IFACE and $WPA_CONF does not exist" \
      "join the WiFi through netplan first (the Armbian preset $NETPLAN_DIR/30-wifis-dhcp.yaml:" \
      "wifis: $WIFI_IFACE: access-points: <SSID>: password: ...), or restore a moved-aside <file>.yaml.bak," \
      "then re-run --install"
  fi
  for i in "${!ssids[@]}"; do
    pass="${passes[i]}"
    if [[ "$pass" =~ ^[0-9a-fA-F]{64}$ ]]; then
      psk="${pass,,}" # netplan accepts a ready 64-hex PSK; wpa_passphrase would refuse its length
    else
      command -v "$WPA_PASSPHRASE" >/dev/null 2>&1 \
        || wifi_refuse "$WPA_PASSPHRASE not found" "install the wpasupplicant package, then re-run --install"
      # The passphrase on STDIN (wpa_passphrase reads it there when no second argument is given);
      # its output (which echoes the passphrase in a #psk= comment) stays in this variable.
      out="$(printf '%s\n' "$pass" | "$WPA_PASSPHRASE" "${ssids[i]}" 2>/dev/null || true)"
      psk="$(bkshading_sbc_psk_hex_from_wpa_passphrase "$out")"
      out=""
      [ -n "$psk" ] || wifi_refuse "wpa_passphrase derived no PSK for SSID '${ssids[i]}'" \
        "a WPA passphrase is 8-63 printable characters; fix it in $WIFI_YAML"
    fi
    pairs+=("${ssids[i]}" "$psk")
  done
  WIFI_CONF_TEXT="$(bkshading_sbc_wpa_conf_text "$country" "${pairs[@]}")"
  pairs=()
  passes=()
  pass=""
  psk=""
  WIFI_PLAN=migrate
  WIFI_SUMMARY="SSID ${ssids[*]}, country ${country:-<none in the YAML>}"
}

apply_wifi() {
  local f
  if [ "$WIFI_PLAN" = skip ]; then
    echo "  no wireless interface (a wired box) -- WiFi takeover + heal skipped"
    return 0
  fi
  if [ "$WIFI_PLAN" = migrate ]; then
    mkdir -p "$WPA_CONF_DIR"
    (
      umask 077
      printf '%s\n' "$WIFI_CONF_TEXT" >"$WPA_CONF.tmp.$$"
    )
    chmod 0600 "$WPA_CONF.tmp.$$"
    mv -f "$WPA_CONF.tmp.$$" "$WPA_CONF"
    WIFI_CONF_TEXT=""
    echo "  wrote $WPA_CONF (0600; $WIFI_SUMMARY; $(bkshading_sbc_bgscan_line); the wpa_passphrase-derived PSK)"
  else
    echo "  $WPA_CONF already exists -- kept (a re-run never rewrites it)"
  fi
  if [ -n "$WIFI_YAML" ]; then
    mv "$WIFI_YAML" "$WIFI_YAML.bak"
    echo "  moved the netplan WiFi YAML aside: $WIFI_YAML -> $WIFI_YAML.bak (netplan runs no supplicant on $WIFI_IFACE now)"
  fi
  mkdir -p "$NETWORKD_DIR"
  bkshading_sbc_networkd_wifi_content >"$NETWORKD_DIR/$(bkshading_sbc_networkd_wifi_name)"
  echo "  wrote $NETWORKD_DIR/$(bkshading_sbc_networkd_wifi_name) (DHCP on $WIFI_IFACE, as netplan generated it)"

  mkdir -p "$HEAL_DIR/lib" "$UNIT_DIR"
  install -m 0755 "$HERE/$(bkshading_sbc_wifi_heal_script_name)" "$HEAL_DIR/$(bkshading_sbc_wifi_heal_script_name)"
  install -m 0644 "$HERE/lib/bkshading-sbc-runtime.sh" "$HEAL_DIR/lib/bkshading-sbc-runtime.sh"
  for f in $(bkshading_sbc_wifi_heal_units); do
    install -m 0644 "$REPO/systemd/$f" "$UNIT_DIR/$f"
  done
  echo "  installed the WiFi heal: $HEAL_DIR/$(bkshading_sbc_wifi_heal_script_name) + $(bkshading_sbc_wifi_heal_units | tr '\n' ' ')"
}

enable_wifi() {
  [ "$WIFI_PLAN" != skip ] || return 0
  "$SYSTEMCTL" enable "$(bkshading_sbc_wpa_unit)"
  "$SYSTEMCTL" enable "$(bkshading_sbc_wifi_heal_timer)"
  echo "  enabled $(bkshading_sbc_wpa_unit) + $(bkshading_sbc_wifi_heal_timer) (NOT started -- reboot to take effect)"
}

do_install() {
  local opts mode uuid fstype
  echo "[bkshading-provision-sbc] --install (enable-only; takes effect on next reboot)"
  # A cambox's read-only root is setup-device.sh STEP 18's (EFI + journal-partition lines this
  # script does not write); never rewrite it from here.
  if [ -e "$CAMBOX_MARKER" ]; then
    echo "ERROR: $CAMBOX_MARKER exists -- this is a cambox, not a handheld SBC. Its root and relay are" >&2
    echo "       setup-device.sh's (scripts/bkshading-provision-relay.sh for the relay alone)." >&2
    exit 2
  fi
  # Read everything the read-only fstab needs BEFORE changing anything: an unreadable root refuses
  # with the box untouched.
  opts="$(read_root_opts)"
  mode="$(ro_root_mount_mode "$opts")"
  if [ "$mode" = unknown ]; then
    echo "ERROR: could not read the root mount options (findmnt -n -o OPTIONS / = '${opts}') -- nothing changed" >&2
    exit 1
  fi
  uuid="$("$FINDMNT" -n -o UUID / 2>/dev/null || true)"
  fstype="$("$FINDMNT" -n -o FSTYPE / 2>/dev/null || true)"
  if [ -z "$uuid" ] || [ -z "$fstype" ]; then
    echo "ERROR: could not read the root UUID/fstype (findmnt: UUID='${uuid}' FSTYPE='${fstype}') -- nothing changed" >&2
    exit 1
  fi
  # The WiFi takeover is read and decided here too, so a board with no WiFi config to migrate is
  # refused before the root is remounted or any file is written.
  plan_wifi
  if [ "$mode" = ro ]; then
    echo "  root is read-only (a re-run after the read-only reboot) -- remounting rw for the install"
    "$MOUNT" -o remount,rw / || { echo "ERROR: 'mount -o remount,rw /' failed -- nothing changed" >&2; exit 1; }
    ROOT_WAS_RO=1
    trap 'restore_root_mode || true' EXIT
  fi

  install_gphoto2

  # An SBC writes NO CAMERA_BOX_CAPTURE_FPS env (no appliance to derive from; a handheld has no grab
  # comparison). The predicate is the single source of truth; if it ever flips, this branch is where
  # the env write would go — until then we deliberately do NOT create /etc/bkshading/relay.env.
  if [ "$(bkshading_sbc_writes_capture_fps_env)" = "yes" ]; then
    echo "  WARNING: SBC capture-fps env is enabled but this script has no derive path" >&2
  else
    echo "  no capture-fps env on an SBC (handheld has no grab comparison; unit degrades gracefully)"
  fi

  mkdir -p "$(dirname "$UNIT_DEST")"
  install -m 0644 "$UNIT_SRC" "$UNIT_DEST"
  echo "  installed $UNIT_DEST (reused relay unit)"

  apply_wifi

  # ENABLE-ONLY: never start/restart the relay here (provisioning-scripts.md) — reboot / the
  # post-reboot verify brings it live.
  "$SYSTEMCTL" daemon-reload
  "$SYSTEMCTL" enable "$UNIT_NAME"
  echo "  enabled $UNIT_NAME (NOT started -- reboot to take effect)"
  enable_wifi

  if [ ! -x "$RELAY_BIN" ]; then
    echo "  WARNING: relay binary not present/executable at $RELAY_BIN -- deploy the aarch64" >&2
    echo "           bkshading-relay there (scripts/bkshading-deploy-relay.sh --arch arm64" >&2
    echo "           --host <sbc>) before reboot." >&2
  elif [ "$(bkshading_sbc_arch_ok "$(bkshading_sbc_elf_arch_of_file "$RELAY_BIN")")" != "yes" ]; then
    echo "  WARNING: relay binary at $RELAY_BIN is not aarch64 (found: $(bkshading_sbc_elf_arch_of_file "$RELAY_BIN")) --" >&2
    echo "           deploy the arm64 build (bkshading-relay-linux-arm64), not the amd64 one." >&2
  fi

  install_ro_root "$uuid" "$fstype"
  restore_root_mode || exit 1
  echo "install done. Reboot the board to bring the relay up on a read-only root, then verify with:"
  echo "  scripts/bkshading-provision-sbc.sh --check"
}

# --check rows for the WiFi takeover + heal (issue 808). $1 = the WiFi link state of row (5).
# Prints one OK/FAIL row each; returns 1 when any row FAILs.
check_wifi_takeover() {
  local fail=0 unit en want line found=0 f gw status poll bssid rssi wstate
  local -a stale=()
  if [ "${1:-}" = none ]; then
    echo "OK: no wireless interface -- the wpa_supplicant, bgscan, heal and gateway rows are skipped"
    return 0
  fi
  # wpa_supplicant@wlan0 owns the WiFi (not netplan's generated supplicant).
  unit="$(bkshading_sbc_wpa_unit)"
  en="$("$SYSTEMCTL" is-enabled "$unit" 2>/dev/null || true)"
  if [ "$en" = enabled ]; then
    echo "OK: $unit enabled"
  else
    echo "FAIL: $unit not enabled (is-enabled=${en:-<none>}) -- re-run --install" >&2
    fail=1
  fi
  # its conf carries the background scan (read line by line: the file holds the PSK, never printed).
  want="$(bkshading_sbc_bgscan_line)"
  if [ -r "$WPA_CONF" ]; then
    while IFS= read -r line || [ -n "$line" ]; do
      line="${line#"${line%%[![:space:]]*}"}"
      if [ "$line" = "$want" ]; then found=1; fi
    done <"$WPA_CONF"
    if [ "$found" = 1 ]; then
      echo "OK: $WPA_CONF carries $want"
    else
      echo "FAIL: $WPA_CONF has no $want line -- the supplicant never roams to a stronger AP;" >&2
      echo "      add that line to its network block (or move the conf aside, restore the netplan WiFi YAML from its .bak, and re-run --install)" >&2
      fail=1
    fi
  else
    echo "FAIL: $WPA_CONF missing or unreadable -- re-run --install" >&2
    fail=1
  fi
  # the heal is installed (byte-identical to this checkout) and its timer enabled.
  for f in $(bkshading_sbc_wifi_heal_units); do
    cmp -s "$REPO/systemd/$f" "$UNIT_DIR/$f" || stale+=("$UNIT_DIR/$f")
  done
  cmp -s "$HERE/$(bkshading_sbc_wifi_heal_script_name)" "$HEAL_DIR/$(bkshading_sbc_wifi_heal_script_name)" \
    || stale+=("$HEAL_DIR/$(bkshading_sbc_wifi_heal_script_name)")
  cmp -s "$HERE/lib/bkshading-sbc-runtime.sh" "$HEAL_DIR/lib/bkshading-sbc-runtime.sh" \
    || stale+=("$HEAL_DIR/lib/bkshading-sbc-runtime.sh")
  en="$("$SYSTEMCTL" is-enabled "$(bkshading_sbc_wifi_heal_timer)" 2>/dev/null || true)"
  if [ "${#stale[@]}" -eq 0 ] && [ "$en" = enabled ]; then
    echo "OK: WiFi heal installed + $(bkshading_sbc_wifi_heal_timer) enabled"
  else
    [ "${#stale[@]}" -eq 0 ] || echo "FAIL: WiFi heal missing or differs from this checkout: ${stale[*]} -- re-run --install" >&2
    [ "$en" = enabled ] || echo "FAIL: $(bkshading_sbc_wifi_heal_timer) not enabled (is-enabled=${en:-<none>}) -- re-run --install" >&2
    fail=1
  fi
  # the DHCP default gateway answers over the WiFi (a COMPLETED link on a dead AP reads "up" above).
  gw="$(bkshading_sbc_default_gw_from_route "$("$IP" -4 route show default dev "$WIFI_IFACE" 2>/dev/null || true)")"
  status="$("$WPA_CLI" -p "$(bkshading_sbc_wpa_ctrl_dir)" -i "$WIFI_IFACE" status 2>/dev/null || true)"
  poll="$("$WPA_CLI" -p "$(bkshading_sbc_wpa_ctrl_dir)" -i "$WIFI_IFACE" signal_poll 2>/dev/null || true)"
  bssid="$(bkshading_sbc_wpa_field "$status" bssid)"
  wstate="$(bkshading_sbc_wpa_field "$status" wpa_state)"
  rssi="$(bkshading_sbc_wpa_field "$poll" RSSI)"
  if [ -z "$gw" ]; then
    echo "FAIL: no DHCP default gateway on $WIFI_IFACE (bssid=${bssid:-?} signal=${rssi:-?} dBm wpa_state=${wstate:-?})" >&2
    fail=1
  elif "$PING" -n -q -c "$(bkshading_sbc_wifi_heal_ping_count)" -W "$(bkshading_sbc_wifi_heal_ping_timeout_s)" \
    -I "$WIFI_IFACE" "$gw" >/dev/null 2>&1; then
    echo "OK: gateway $gw answers a ping on $WIFI_IFACE (bssid=${bssid:-?} signal=${rssi:-?} dBm)"
  else
    echo "FAIL: gateway $gw does not answer a ping on $WIFI_IFACE (bssid=${bssid:-?} signal=${rssi:-?} dBm wpa_state=${wstate:-?})" >&2
    echo "      the heal timer reassociates after $(bkshading_sbc_wifi_heal_miss_limit) misses; to force it now: wpa_cli -i $WIFI_IFACE reassociate" >&2
    fail=1
  fi
  return "$fail"
}

do_check() {
  local rc=0 en arch
  # (1) relay binary present -- FIRST, so an unprovisioned SBC fails deterministically before we
  #     touch gphoto2 / systemctl.
  if [ -x "$RELAY_BIN" ]; then
    echo "OK: relay binary $RELAY_BIN"
    # (1b) and it must be the aarch64 build -- a mis-deployed amd64 binary would die at reboot with
    #      an opaque "Exec format error"; catch it here.
    arch="$(bkshading_sbc_elf_arch_of_file "$RELAY_BIN")"
    if [ "$(bkshading_sbc_arch_ok "$arch")" = "yes" ]; then
      echo "OK: relay binary is aarch64"
    else
      echo "FAIL: relay binary $RELAY_BIN is not aarch64 (found: $arch) -- deploy the arm64 build" >&2
      rc=1
    fi
  else
    echo "FAIL: relay binary missing/non-executable at $RELAY_BIN" >&2
    rc=1
  fi
  # (2) systemd unit installed AND byte-matches the repo unit (the SAME reused relay unit).
  if [ -f "$UNIT_DEST" ]; then
    if cmp -s "$UNIT_SRC" "$UNIT_DEST"; then
      echo "OK: unit installed $UNIT_DEST"
    else
      echo "FAIL: installed unit $UNIT_DEST differs from repo $UNIT_SRC (re-run --install)" >&2
      rc=1
    fi
  else
    echo "FAIL: unit not installed at $UNIT_DEST" >&2
    rc=1
  fi
  # (3) gphoto2 runtime present.
  if command -v "$GPHOTO2" >/dev/null 2>&1; then
    echo "OK: gphoto2 ($(command -v "$GPHOTO2"))"
  else
    echo "FAIL: gphoto2 not installed (the relay's USB-PTP transport)" >&2
    rc=1
  fi
  # (4) unit enabled (reboot-survival; live runtime state is the post-reboot rig verify).
  en="$("$SYSTEMCTL" is-enabled "$UNIT_NAME" 2>/dev/null || true)"
  if [ "$en" = "enabled" ]; then
    echo "OK: unit enabled"
  else
    echo "FAIL: unit not enabled (is-enabled=${en:-<none>})" >&2
    rc=1
  fi
  # (5) WiFi link -- the handheld SBC is wireless and the whole topology depends on it. A WIRED box
  #     (the cambox class, which runs the SAME reused relay unit) has no wl* interface and this check
  #     is SKIPPED, never FAILed. Band-agnostic: a 2.4 GHz-only board is fine, we only require a link.
  local net_root wifi ssid _if wlif
  net_root="${BKSHADING_SBC_NET_SYSFS:-/sys/class/net}"
  wifi="$(bkshading_sbc_wifi_link_state "$net_root" 'wl*')"
  case "$wifi" in
    none)
      echo "OK: no wireless interface (wired box, e.g. a cambox) -- WiFi link check skipped"
      ;;
    up)
      ssid=""
      if command -v iw >/dev/null 2>&1; then
        # best-effort SSID for the OK line; never gates. Try each wl* iface until one reports one.
        for _if in "$net_root"/wl*; do
          [ -e "$_if" ] || continue
          ssid="$(bkshading_sbc_wifi_ssid_from_iw "$(iw dev "$(basename "$_if")" link 2>/dev/null || true)")"
          [ -n "$ssid" ] && break
        done
      fi
      if [ -n "$ssid" ]; then
        echo "OK: WiFi link up (SSID: $ssid)"
      else
        echo "OK: WiFi link up"
      fi
      ;;
    *)
      wlif="$(bkshading_sbc_first_wifi_iface "$net_root" 'wl*')"
      echo "FAIL: WiFi link is down (a wl* interface is present but has no up link / carrier) -- join the rig WiFi, e.g.:" >&2
      echo "        nmcli device wifi connect '<RIG_SSID>' password '<PSK>' ifname ${wlif:-wlan0}" >&2
      echo "        (a 2.4 GHz-only board needs a 2.4 GHz SSID on site; band is not asserted here)" >&2
      rc=1
      ;;
  esac
  # (5b) the WiFi is wpa_supplicant's own, roams (bgscan) and heals (issue 808) -- wl* boards only.
  check_wifi_takeover "$wifi" || rc=1

  # (6) the root filesystem is read-only (issue 808 slice B) -- the same first-token reading as
  #     the cambox root_mount_is_readonly. An rw root means the read-only fstab is not live yet.
  local opts mode
  opts="$(read_root_opts)"
  mode="$(ro_root_mount_mode "$opts")"
  case "$mode" in
    ro) echo "OK: root filesystem is read-only (${opts%%,*})" ;;
    rw)
      echo "FAIL: root filesystem is read-WRITE (${opts}) -- reboot after --install (the read-only fstab takes effect at boot)" >&2
      rc=1
      ;;
    *)
      echo "FAIL: could not read the root mount options (findmnt -n -o OPTIONS / = '${opts}')" >&2
      rc=1
      ;;
  esac

  if [ "$rc" -ne 0 ]; then
    cat >&2 <<MSG
bkshading relay NOT fully provisioned on this SBC. Fix:
  scripts/bkshading-provision-sbc.sh --install   # gphoto2 + relay unit + WiFi roam/heal + read-only root; enable (defer to reboot)
Then deploy the aarch64 bkshading-relay binary to $RELAY_BIN and reboot:
  scripts/bkshading-deploy-relay.sh --arch arm64 --host <sbc-ip>
MSG
  else
    echo "OK: bkshading relay fully provisioned on this SBC (live after reboot / already running)."
  fi
  return "$rc"
}

case "$MODE" in
  --install) do_install ;;
  --check) do_check ;;
esac
