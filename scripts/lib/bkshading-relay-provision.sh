#!/usr/bin/env bash
# scripts/lib/bkshading-relay-provision.sh -- the ONE bkshading cambox RELAY provisioning lib (issue 808).
# airuleset:script-ok source-only lib -- set -euo pipefail would leak into the sourcing shell (ci-testing-gotchas)
#
# WHY (25.9.2026): the M.2 re-provisioning of cam1-4 left NO relay on them -- no unit, no binary, no
# gphoto2 -- because only the separate scripts/bkshading-provision-relay.sh installed it and
# setup-device.sh never called it. The install body used to live inside that script; it now lives
# HERE, sourced by BOTH scripts/bkshading-provision-relay.sh (the standalone CLI) and
# scripts/setup-device.sh (every fresh / re-provisioned cambox) -- one source of truth, never a copy.
# scripts/verify-device.sh (ao) grades the result with the pure verdict below.
#
# Contents:
#   - impure install functions (gphoto2, env, unit, binary, enable-state) -- ENABLE-ONLY: they
#     daemon-reload + enable OR disable, NEVER start/restart (.claude/rules/provisioning-scripts.md);
#     each returns non-zero on failure (callers use `if !` so a failure is recorded, never swallowed);
#   - pure decisions: the rig-mode enable-state, the relay binary source plan, the (ao) verdict, and
#     the read-only remote gather snippet verify-device.sh runs over ssh.
#
# Source-only: function definitions + three sibling-lib sources (pure constants + the run resolver). No side effects.
# Overridable targets (Tier-0 tests point them at a temp root -- no root/apt/systemd needed):
#   BKSHADING_RELAY_UNIT_DEST, BKSHADING_RELAY_ENV_FILE, BKSHADING_RELAY_BIN,
#   BKSHADING_RELAY_DROPIN_DIR, BKSHADING_RELAY_GPHOTO2, BKSHADING_RELAY_SYSTEMCTL
_BKRP_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bkshading-relay-runtime.sh
. "$_BKRP_HERE/bkshading-relay-runtime.sh"
# shellcheck source=scripts/lib/ci-run-resolve.sh
. "$_BKRP_HERE/ci-run-resolve.sh"  # ci_run_latest_success -- the `latest` relay-binary plan
# shellcheck source=scripts/lib/bkshading-deploy-runtime.sh
. "$_BKRP_HERE/bkshading-deploy-runtime.sh"  # the relay CI artifact + the binary name inside it

# --- targets (resolved per call, so an env override set after sourcing still applies) -------------
_bkrp_init() {
  BKRP_UNIT_NAME="$(bkshading_relay_unit_name)"
  BKRP_UNIT_SRC="$(cd "$_BKRP_HERE/../.." && pwd)/systemd/$BKRP_UNIT_NAME"
  BKRP_UNIT_DEST="${BKSHADING_RELAY_UNIT_DEST:-/etc/systemd/system/$BKRP_UNIT_NAME}"
  BKRP_ENV_FILE="${BKSHADING_RELAY_ENV_FILE:-$(bkshading_relay_env_path)}"
  BKRP_BIN="${BKSHADING_RELAY_BIN:-$(bkshading_relay_bin_path)}"
  BKRP_DROPIN_DIR="${BKSHADING_RELAY_DROPIN_DIR:-/etc/systemd/system/camera-box.service.d}"
  BKRP_GPHOTO2="${BKSHADING_RELAY_GPHOTO2:-gphoto2}"
  BKRP_SYSTEMCTL="${BKSHADING_RELAY_SYSTEMCTL:-systemctl}"
  BKRP_APT_PKG="$(bkshading_relay_apt_package)"
}

# bkshading_relay_provision_derive_capture_fps -> the effective capture fps from the box's
# camera-box.service.d drop-ins (or the appliance default). Pure logic lives in the runtime lib.
bkshading_relay_provision_derive_capture_fps() {
  _bkrp_init
  local text="" f raw
  if [ -d "$BKRP_DROPIN_DIR" ]; then
    for f in "$BKRP_DROPIN_DIR"/*.conf; do
      [ -f "$f" ] || continue
      text="$text$(cat "$f")"$'\n'
    done
  fi
  raw="$(bkshading_relay_capture_fps_from_dropins "$text")"
  bkshading_relay_effective_capture_fps "$raw"
}

# bkshading_relay_provision_install_gphoto2 -> installs the relay's USB-PTP runtime when missing.
bkshading_relay_provision_install_gphoto2() {
  _bkrp_init
  if command -v "$BKRP_GPHOTO2" >/dev/null 2>&1; then
    echo "  gphoto2 already present: $(command -v "$BKRP_GPHOTO2")"
    return 0
  fi
  echo "  installing $BKRP_APT_PKG (the relay's USB-PTP runtime) via apt ..."
  apt-get update -qq || return 1
  apt-get install -y -qq "$BKRP_APT_PKG" || return 1
  command -v "$BKRP_GPHOTO2" >/dev/null 2>&1 || { echo "  gphoto2 still missing after the apt install" >&2; return 1; }
}

# bkshading_relay_provision_install_binary SRC -> installs the CI relay binary at the relay bin path
# via a NEW inode (`install -m 0755`, safe over a running binary -- never an in-place truncate).
bkshading_relay_provision_install_binary() {
  _bkrp_init
  local src="${1:-}"
  [ -n "$src" ] && [ -s "$src" ] || { echo "  relay binary source '${src:-<none>}' missing or empty" >&2; return 1; }
  mkdir -p "$(dirname "$BKRP_BIN")" || return 1
  install -m 0755 "$src" "$BKRP_BIN" || return 1
  echo "  installed relay binary $BKRP_BIN"
}

# bkshading_relay_provision_install [enabled|disabled] [BINARY_SRC]
#   gphoto2 + the derived env + the unit (+ the binary when BINARY_SRC is given), then daemon-reload
#   and `enable` OR `disable` per the requested state, read back with a LITERAL is-enabled compare.
#   NEVER starts/restarts the relay (enable-only; the relay comes up at boot / rig-mode.sh event).
bkshading_relay_provision_install() {
  local want="${1:-enabled}" bin_src="${2:-}" fps got
  _bkrp_init
  case "$want" in enabled | disabled) ;; *) echo "  unknown enable-state '$want' (want enabled|disabled)" >&2; return 2 ;; esac
  bkshading_relay_provision_install_gphoto2 || return 1

  fps="$(bkshading_relay_provision_derive_capture_fps)"
  mkdir -p "$(dirname "$BKRP_ENV_FILE")" || return 1
  bkshading_relay_env_file_content "$fps" >"$BKRP_ENV_FILE" || return 1
  chmod 0644 "$BKRP_ENV_FILE" || return 1
  echo "  wrote $BKRP_ENV_FILE (CAMERA_BOX_CAPTURE_FPS=$fps, derived from $BKRP_DROPIN_DIR)"

  mkdir -p "$(dirname "$BKRP_UNIT_DEST")" || return 1
  install -m 0644 "$BKRP_UNIT_SRC" "$BKRP_UNIT_DEST" || return 1
  echo "  installed $BKRP_UNIT_DEST"

  if [ -n "$bin_src" ]; then
    bkshading_relay_provision_install_binary "$bin_src" || return 1
  fi

  # ENABLE-ONLY: never start/restart the relay here (provisioning-scripts.md).
  "$BKRP_SYSTEMCTL" daemon-reload || return 1
  if [ "$want" = enabled ]; then
    "$BKRP_SYSTEMCTL" enable "$BKRP_UNIT_NAME" || return 1
  else
    "$BKRP_SYSTEMCTL" disable "$BKRP_UNIT_NAME" || return 1
  fi
  got="$("$BKRP_SYSTEMCTL" is-enabled "$BKRP_UNIT_NAME" 2>/dev/null || true)"
  if [ "$got" != "$want" ]; then
    echo "  $BKRP_UNIT_NAME is-enabled='${got:-<none>}' after the $want step (want $want)" >&2
    return 1
  fi
  echo "  $BKRP_UNIT_NAME $want (NOT started -- it comes up at boot when enabled / via rig-mode.sh event)"

  if [ ! -x "$BKRP_BIN" ]; then
    echo "  WARNING: relay binary not present/executable at $BKRP_BIN -- deploy the CI-built" >&2
    echo "           bkshading-relay there (scripts/bkshading-deploy-relay.sh) before reboot." >&2
  fi
  return 0
}

# --- pure decisions -----------------------------------------------------------------------------

# bkshading_relay_expected_enable_state DEVICE MODE SOURCE_BOX PAINTER_BOX -> enabled|disabled|unknown
#   The relay's steady-state enable-state for DEVICE under the rig MODE (test|event|anything else =
#   unknown). The TEST roster is the SAME two boxes rig-mode.sh stops+disables (issue 1311, the
#   #1309 passive rule -- a shading camera shares the boot stick's USB hub): the source box + the
#   painter box (cam2). TEST -> roster disabled, every other box enabled; EVENT -> all enabled; an
#   unknown mode -> the roster is `unknown` (the caller fails loud), every other box enabled.
#   An EMPTY source box outside EVENT means the roster itself is unknown, so EVERY box is `unknown`
#   (setup-device then installs disabled + records it, verify fails) -- ONE table for both scripts.
#   Case-insensitive box names (setup-device.sh passes CAM1, camera-set.sh says cam1).
bkshading_relay_expected_enable_state() {
  local dev src painter mode="${2:-}"
  dev="$(printf '%s' "${1:-}" | tr '[:upper:]' '[:lower:]')"
  src="$(printf '%s' "${3:-}" | tr '[:upper:]' '[:lower:]')"
  painter="$(printf '%s' "${4:-}" | tr '[:upper:]' '[:lower:]')"
  mode="$(printf '%s' "$mode" | tr '[:upper:]' '[:lower:]')"
  if [ "$mode" = event ]; then printf '%s\n' enabled; return 0; fi
  if [ -z "$src" ]; then printf '%s\n' unknown; return 0; fi
  local roster=0
  if [ -n "$dev" ] && { [ "$dev" = "$src" ] || [ "$dev" = "$painter" ]; }; then roster=1; fi
  case "$mode" in
    test) if [ "$roster" = 1 ]; then printf '%s\n' disabled; else printf '%s\n' enabled; fi ;;
    *) if [ "$roster" = 1 ]; then printf '%s\n' unknown; else printf '%s\n' enabled; fi ;;
  esac
}

# bkshading_relay_roster_painter_box -> the painter box that is always in the TEST relay roster
# (cam2 -- the SAME box rig-mode.sh passes as `cam2=$PAINTER_IP` and cam2_is_painter_box pins).
bkshading_relay_roster_painter_box() { printf '%s\n' cam2; }

# bkshading_relay_provision_binary_plan RELAY_ARG RUN_ID GH_AVAILABLE(yes|no)
#   -> local:<path> | url:<url> | run:<id> | latest | none
#   Where setup-device.sh takes the relay binary from, in order: an explicit --relay-binary that is an
#   existing file; an http(s) URL; the SAME ci.yml run camera-box came from (RUN_ID); the newest
#   successful run carrying the artifact (gh available); else none (the caller records the gap --
#   a gh-less box is handed a dev1-staged binary, the --probe-binary idiom). A --relay-binary that is
#   neither an existing file nor a URL is `none` (a typo must not silently fall through to a download).
bkshading_relay_provision_binary_plan() {
  local arg="${1:-}" run="${2:-}" gh="${3:-no}"
  if [ -n "$arg" ]; then
    case "$arg" in
      http://* | https://*) printf 'url:%s\n' "$arg" ;;
      *) if [ -f "$arg" ]; then printf 'local:%s\n' "$arg"; else printf '%s\n' none; fi ;;
    esac
    return 0
  fi
  if [ "$gh" = yes ]; then
    case "$run" in
      '' | *[!0-9]*) printf '%s\n' latest ;;
      *) printf 'run:%s\n' "$run" ;;
    esac
    return 0
  fi
  printf '%s\n' none
}

# bkshading_relay_provision_fetch_binary PLAN DEST_DIR REPO BRANCH
#   Carry out a bkshading_relay_provision_binary_plan: stdout = the local path of the relay binary,
#   rc 0; or stdout = ONE line saying why there is none, rc 1 (the caller records it -- it never
#   aborts the provisioner). Progress goes to stderr. gh / curl are overridable for Tier-0 tests
#   (BKSHADING_RELAY_GH, BKSHADING_RELAY_CURL); the `latest` plan uses the ONE shared resolver.
bkshading_relay_provision_fetch_binary() {
  local plan="${1:-none}" dir="${2:-}" repo="${3:-}" branch="${4:-}" run art bin
  local gh="${BKSHADING_RELAY_GH:-gh}" curl="${BKSHADING_RELAY_CURL:-curl}"
  art="$(bkshading_deploy_artifact_name)"
  bin="$(bkshading_deploy_relay_artifact_bin)"
  [ -n "$dir" ] && mkdir -p "$dir" || { echo "no download dir for the relay binary"; return 1; }
  case "$plan" in
    local:*)
      echo "  relay binary: local ${plan#local:}" >&2
      printf '%s\n' "${plan#local:}"
      ;;
    url:*)
      echo "  relay binary: downloading ${plan#url:}" >&2
      if "$curl" -fsSL "${plan#url:}" -o "$dir/$bin" && [ -s "$dir/$bin" ]; then
        printf '%s\n' "$dir/$bin"
      else
        echo "download of the relay binary from ${plan#url:} failed"
        return 1
      fi
      ;;
    run:* | latest)
      run="${plan#run:}"
      if [ "$plan" = latest ]; then
        run="$(CI_RUN_RESOLVE_GH="$gh" ci_run_latest_success "$repo" "$branch" ci.yml "$art")" || run=""
      fi
      if [ -z "$run" ]; then
        echo "no successful ci.yml run on '$branch' carries $art"
        return 1
      fi
      local gh_err=""
      if gh_err="$("$gh" run download "$run" --repo "$repo" -n "$art" --dir "$dir" 2>&1 >/dev/null)" && [ -s "$dir/$bin" ]; then
        echo "  relay binary: $art from ci.yml run $run" >&2
        printf '%s\n' "$dir/$bin"
      else
        echo "gh run download of $art from ci.yml run $run failed (${gh_err##*$'\n'})"
        return 1
      fi
      ;;
    *)
      echo "no relay binary source -- this box has no gh/GH_TOKEN and no usable --relay-binary. STAGE IT FROM dev1: gh run download <ci.yml run> -n $art --dir /tmp && scp /tmp/$bin root@<box>:/tmp/ , then re-run with --relay-binary /tmp/$bin"
      return 1
      ;;
  esac
}

# bkshading_relay_provision_gather_remote_snippet -> REMOTE sh text (read-only) that prints the five
# KEY=value lines bkshading_relay_provision_verdict grades. Embed as the WHOLE ssh command.
bkshading_relay_provision_gather_remote_snippet() {
  local bin unit env
  bin="$(bkshading_relay_bin_path)"
  unit="/etc/systemd/system/$(bkshading_relay_unit_name)"
  env="$(bkshading_relay_env_path)"
  cat <<SNIP
printf 'BIN_X=%s\n' "\$(test -x $bin && echo yes || echo no)"
printf 'UNIT_SHA=%s\n' "\$(sha256sum $unit 2>/dev/null | awk '{print \$1}')"
printf 'ENV_FPS=%s\n' "\$(grep -oE '^CAMERA_BOX_CAPTURE_FPS=[0-9]+\$' $env 2>/dev/null | tail -n 1 | cut -d= -f2)"
printf 'GPHOTO2=%s\n' "\$(command -v gphoto2 >/dev/null 2>&1 && echo yes || echo no)"
printf 'ENABLED=%s\n' "\$(systemctl is-enabled $(bkshading_relay_unit_name) 2>/dev/null)"
SNIP
}

# _bkrp_block_field BLOCK KEY -> the LAST `KEY=value` value in BLOCK (empty when absent). Pure.
_bkrp_block_field() { printf '%s\n' "${1:-}" | sed -n "s/^$2=//p" | tail -n 1; }

# bkshading_relay_provision_verdict BLOCK EXPECTED_ENABLE_STATE -> `ok` | one `FAIL: <reason>` per line
#   BLOCK = the gather snippet's output. The installed unit must BYTE-match the repo unit (sha256).
#   EXPECTED = bkshading_relay_expected_enable_state's answer; `unknown` FAILs (the rig mode could not
#   be read for a roster box -- never a silent pass, test-strictness).
bkshading_relay_provision_verdict() {
  local block="${1:-}" want="${2:-}" out="" v want_sha
  _bkrp_init
  if [ -z "$block" ]; then
    printf '%s\n' "FAIL: empty relay state read -- cannot certify the bkshading relay is provisioned"
    return 0
  fi
  [ "$(_bkrp_block_field "$block" BIN_X)" = yes ] || out="${out}FAIL: relay binary missing/not executable at $(bkshading_relay_bin_path) -- run scripts/bkshading-deploy-relay.sh"$'\n'
  want_sha="$(sha256sum "$BKRP_UNIT_SRC" 2>/dev/null | awk '{print $1}')"
  v="$(_bkrp_block_field "$block" UNIT_SHA)"
  if [ -z "$v" ]; then
    out="${out}FAIL: relay unit $BKRP_UNIT_NAME not installed -- re-run setup-device.sh (or bkshading-provision-relay.sh --install)"$'\n'
  elif [ -z "$want_sha" ] || [ "$v" != "$want_sha" ]; then
    out="${out}FAIL: installed relay unit differs from the repo systemd/$BKRP_UNIT_NAME -- re-run setup-device.sh"$'\n'
  fi
  v="$(_bkrp_block_field "$block" ENV_FPS)"
  case "$v" in '' | *[!0-9]*) out="${out}FAIL: relay env $(bkshading_relay_env_path) missing or has no CAMERA_BOX_CAPTURE_FPS=<int>"$'\n' ;; esac
  [ "$(_bkrp_block_field "$block" GPHOTO2)" = yes ] || out="${out}FAIL: gphoto2 not installed (the relay's USB-PTP runtime)"$'\n'
  v="$(_bkrp_block_field "$block" ENABLED)"
  case "$want" in
    enabled | disabled)
      [ "$v" = "$want" ] || out="${out}FAIL: relay unit is-enabled='${v:-<none>}' but the rig mode wants it $want (TEST = the source box + cam2 disabled, issue 1311)"$'\n' ;;
    *)
      out="${out}FAIL: rig mode unreadable for this relay-roster box -- cannot grade the enable-state (set CAMERA_BOX_RIG_MODE=test|event)"$'\n' ;;
  esac
  if [ -z "$out" ]; then printf '%s\n' ok; else printf '%s' "$out"; fi
}
