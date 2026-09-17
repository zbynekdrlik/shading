---
paths:
  - "bkshading/**"
  - "scripts/bkshading-*"
  - "scripts/lib/bkshading-*"
  - "systemd/bkshading-*"
---

# bkshading — remote camera shading control (issue 808)

A multiplatform Rust **service** (`bkshading`, runs on the strih PC — Windows first, Linux later)
that aggregates per-camera **relays** into ONE operator web panel; and a cambox/SBC **relay**
(`bkshading-relay`) that drives a Blackmagic camera over USB-PTP via the `gphoto2` CLI. Shared
crate `bkshading-proto` holds the wire types + the byte-verified PTP mapping (ported 1:1 from the
dev2 MVP `pybridge/mapping.py`). Owner architecture: issue 808 comments 5355836067 / 5356048130 /
5356062847 (2026-08-20).

## Non-negotiables (owner decisions)
- **Transports are USB-relay / SBC-relay / ethernet-REST ONLY — NEVER Bluetooth.** The BLE path
  from the dev2 MVP is dead; do not reintroduce it.
- Camera list is config-driven (`bkshading/service/bkshading.example.toml`): a camera is a record
  (id, transport, address, optional `ndi_preview`). **A camera with no `ndi_preview` renders a
  params-only block** (no preview). M2 delivers the LIVE preview (JPEG over HTTP; see M2 section).
- Relay transport = shell out to the `gphoto2` CLI behind the `CameraTransport`-style trait (NOT a
  `libgphoto2` FFI binding — the trait keeps FFI as a future 2nd impl). Rationale: no build-time C
  dep → clean ARM cross-build for the zero-class arm64 SBC handheld relay.

## How the crates sit in the workspace WITHOUT disturbing the appliance
The repo root `Cargo.toml` is a **single package** (the camera-box appliance). The bkshading crates
are added as SEPARATE workspace members:
- Root `Cargo.toml` gained `[workspace] members = ["bkshading/proto","bkshading/relay","bkshading/service"]`
  + `resolver = "2"` (pins the SAME feature resolution the root edition-2021 package used standalone).
- **The appliance build is untouched because every existing CI job runs cargo at the ROOT WITHOUT
  `--workspace`** — so `cargo test`/`clippy`/`nextest`/`llvm-cov`/`mutants` still select ONLY the
  root package. `cargo fmt --all` DOES cover the new members (so they must be fmt-clean).
- The members get their OWN CI jobs: `bkshading` (Linux clippy/test/build) + `bkshading-windows`
  (windows-latest `cargo check` — the service ships to the strih PC, Windows first). Both are in the
  `notify-on-failure` needs list. These jobs are the members' FIRST real compile (Tier-0, below).
- Deps stay OUT of the appliance tree: axum/tokio/reqwest(rustls, no openssl)/toml live only in the
  member manifests, so the appliance's minimal deps + probe feature-gating are never touched.

## Crate versions — ONE workspace source of truth (issue 1154)
All four crates (appliance + the 3 members) inherit ONE version from root
`[workspace.package] version = "X"` via `version.workspace = true`; NO crate hard-codes its own.
A single edit of that ONE line bumps every crate's `CARGO_PKG_VERSION` (relay/service read it as
`const VERSION`, which feeds the panel DOM / `/api/version` / `RelayState.version` — the
version-on-dashboard surface). Before #1154 the members hard-coded their version and silently
drifted after each root-only bump (root `.518` vs members `.516` live).
- The three `^version = "X"` readers (`camera-box-version-gate.sh:169` incl. its origin/main pin,
  `recording-e2e.sh:903` sed, `rig-status.py` `_read_version`) STILL work UNTOUCHED: each anchors
  on the FIRST column-0 `version = "X"` line, now the `[workspace.package]` one (same value), which
  sits before all dependency lines. `version.workspace = true` never matches that anchor (no quote
  after `=`), and the new comment lines start with `#`.
- **Bump discipline is now: edit the single `[workspace.package].version` line at the root.**
- GOTCHA when bumping via sed: a blanket `sed 's/^version = "OLD"$/.../'` will ALSO rewrite the
  `[workspace.package]` literal (it is the ONLY column-0 `version = "X"` line now). Target that
  one line specifically (e.g. `sed '/^\[workspace.package\]$/{n;s/.../}'`).
- The `"1.7.0-dev.516"` literals in `service/tests/service.rs` + `relay/tests/relay.rs` are INERT
  test inputs (a version string passed INTO `CameraSession::new(...)`/a fixture and echoed back),
  decoupled from `env!` — they do NOT track the crate version and do NOT break CI.
- Invariant test: `tests/python/test_bkshading_versions_1154.py` (tomllib structural — runs in the
  `python-tests` CI job, no toolchain; a skip-if-no-cargo `cargo metadata` check proves resolved
  value-level uniformity where cargo exists).

## Tier-0 verification of a NEW crate (no cargo build locally, issue 557)
CI is the first compile. The local net that CAUGHT real issues here:
1. `cargo fmt --all -- --check` — parses every member, proving the Rust is brace/syntax-balanced
   (a fmt-clean run is your only local "it compiles-shaped" signal).
2. **Standalone `rustc --edition 2021 --test`** for a pure, `std`-only module (e.g.
   `proto/src/mapping.rs`): paste the module body inside `mod m { … }` with `//!`→`//` (an inner
   doc-comment breaks `include!`/module-wrap), add inline `#[test]`s, `rustc --test` + run the
   binary. This genuinely RUNS the fragile PTP math without cargo.
3. Python static tests under `tests/python/test_bkshading_*.py` (stdlib only: `os`/`re`/`tomllib`) —
   validate the web-UI structure + config schema. They are picked up automatically by the existing
   `python-tests` CI job (pytest discovers `test_*` in `tests/python`).
   NOTE: a python static webui test CANNOT catch a CSS/JS runtime bug — the M1 review found an
   author `.block-preview{display:flex}` beating the UA `[hidden]` rule (params-only block still
   showed its preview); a real Playwright E2E against a running service is M2 (Tier-0 can't run it).
4. Type errors are the residual risk fmt can't catch — hand-audit the axum 0.7 (`:id` routes, not
   `{id}`), reqwest-rustls, serde `rename_all`, clap-derive, `spawn_blocking` Send-bounds surfaces,
   and clippy `-D warnings` traps (a never-read struct field fails `dead_code`; and since Rust
   1.98, `chunks_exact(N)` with a CONSTANT N is a clippy-deny lint — use index math or
   `slice::as_chunks::<N>().0`; the main crate was fixed in dev `052da4c5d`;
   `Option::map_or(<bool literal>, |x| …)` trips `unnecessary_map_or` (clippy `style`, stable
   1.84) — use `is_none_or`/`is_some_and` instead, issue 1157; a two-comparison bound check
   `x > a && x <= b` (e.g. a sanity-cap on an env value) trips `manual_range_contains` (clippy
   `style`) — use `(a+1..=b).contains(&x)` / `(a..=b).contains(&x)`, issue 1229). These clippy traps bite HARDEST
   in the `#[cfg(feature = "ndi")]` code (`ndi_source.rs`): it compiles ONLY under
   `--features ndi` on CI, so a `-D warnings` lint there is invisible to `cargo fmt` and to every
   Tier-0 local check — CI is the first (and only) place it surfaces. Hand-audit feature-gated
   code against this list before pushing.

## M1 done / M2+ deferred
Done: the 3 crates + workspace/CI wiring, the 4+4 responsive web panel skeleton (version in the DOM,
version-on-dashboard), config-driven camera list, relay read+write logic unit-tested with a fake
runner; ONE workspace-inherited crate version across all four crates (#1154). M2 DONE: live NDI preview (below). SBC/handheld provisioning DONE (below). ALSO DONE since: WS push of the aggregate
(`/ws` watch pump); cloudflare password-protected remote (NOT tailscale — owner decision); relay
provisioning incl. `gphoto2` runtime + the CAMERA_BOX_CAPTURE_FPS env (LIVE on cam2 since
2026-08-22, camera not yet cabled); CI artifacts + `bkshading-deploy-relay.sh`; the reconnect-safe
process-shared NDI runtime (below). Still deferred: automating the E2E camera pre-run shutter
checklist (meaningful once the camera is physically cabled to the relay box).


## M2 — live camera preview (issue 808, `bkshading/service/src/preview/**`)
Owner architecture: the cambox publishes ONE NDI stream (strih OBS + this service both consume it);
the service subscribes to the NDI **low-bandwidth** variant, decimates to ~3 fps, JPEG-encodes, and
serves the latest frame at `GET /api/cameras/<id>/preview.jpg`; the web UI reloads an `<img>` a few
times a second. Structure: a `PreviewSource` trait behind which the **default stub** (test pattern,
CI-safe, no libndi) and a `#[cfg(feature="ndi")]` real receiver live; pure CI-tested stages
(`frame`/`pattern`/`decimate`/`encode`/`convert`/`store`) + runtime glue (`source`/`worker` — one OS
thread per camera, NOT tokio, since NDI capture is a blocking FFI call). Feature `ndi` is OFF by
default + UNVERIFIED against a live source (follow-up #1157: verify + provision libndi on the strih
service). Delivery is JPEG-over-HTTP, NOT presenter's gstreamer→webrtcsink (WHEP) — WebRTC is too
heavy + CI-unverifiable for a ~3 fps shading preview; only the minimal "NDI recv → per-frame" idea
was reused.
- **The appliance ALREADY has the NDI recv pattern at `src/ndi.rs`** (`NdiReceiver::connect` +
  `capture_frame`, `recv_create_v3` with a `bandwidth` field, `recv_capture_v3`,
  `recv_free_video_v2`). The real preview receiver mirrors it VERBATIM (safest for an untestable
  path), only swapping bandwidth `HIGHEST`(100) → `LOWEST`(0). Do NOT depend on the appliance root
  crate from a member (pulls the whole heavy appliance tree) — copy the minimal recv FFI instead.
- **jpeg-encoder 0.7**: `Encoder::new(w, quality) -> Encoder` (NOT a Result); `encode(self, &[u8],
  width: u16, height: u16, ColorType::Rgb) -> Result` (consumes self). Pure Rust, default `std`
  only (no simd/C) → cross-compiles to Windows/ARM.
- **FFI init E0505 trap**: inline `*lib.get::<Fn>(...)?` calls INSIDE a struct literal keep their
  Symbol temporaries alive to the END of the statement, so moving the `Library` into the last field
  (`_library: lib`) fails with E0505 (borrowed). Deref-copy each fn pointer into its OWN `let` FIRST
  (each `?` temp ends at its statement — fn pointers are Copy), THEN build the struct moving `lib`
  last. (`src/ndi.rs` dodges it by binding Symbols to `let`s; the inline form does not.)
- **FFI `#[repr(C)]` dead_code**: a private field never read (e.g. `p_url_address`, most of the
  recv video-frame struct) trips `dead_code` under `-D warnings` — annotate the struct
  `#[allow(dead_code)]` (the appliance made its fields `pub` instead, which is also exempt).
- The feature-gated path gets its OWN CI step: `cargo clippy -p bkshading --features ndi
  --all-targets -- -D warnings` (libloading is a RUNTIME load, so it compiles without libndi).
- Decimation runs on a MONOTONIC `Instant` (not wall clock — immune to an NTP backward step); the
  store's `updated_ms` stays wall clock for diagnostics.

### M2 follow-up — cross-platform libndi discovery + provisioning (issue 1157)
The M2 receiver copied `src/ndi.rs`'s `NdiLib::load()` VERBATIM, which is Linux-only (`libndi.so*`
+ `/usr/lib/ndi` etc.). But the SERVICE ships to the strih PC (Windows first), where the NDI
runtime is `Processing.NDI.Lib.x64.dll` at `C:\Program Files\NDI\NDI 6 Tools\Runtime\` (documented
in-repo at `scripts/bundle-state-server.py::DEFAULT_NDI_RUNTIME_DLL`) — so `--features ndi` could
never load on its own ship target.
- **Discovery is now a PURE, default-feature module** `bkshading/service/src/preview/ndi_paths.rs`
  (Tier-0 unit-tested WITHOUT libndi, mirroring the `convert.rs` split): `NdiOs {Linux,Windows,Macos}`
  as an INPUT (not `cfg!`) so every OS's candidate set is tested on the Linux runner. The
  `NdiLib::load_uncached()` loader (reached only via the process-shared `NdiLib::shared()`)
  now consumes `ndi_search_candidates(current_ndi_os(), |k| std::env::var(k).ok())` (env dirs →
  per-OS well-known dirs → bare-name dynamic-linker fallback) instead of a hard-coded Linux list.
  Tests live in `service/tests/preview.rs` (run in the default-feature `bkshading` CI test).
- **CI compiles + verifies the feature on BOTH ship targets.** Since issue 1157 made `ndi` the
  DEFAULT service build (below), the PLAIN default-feature steps carry the real-ndi coverage: the
  `bkshading` job's `cargo clippy/test -p bkshading` compile+run the ndi path on Linux, and
  `bkshading-windows`'s `cargo check -p bkshading` compiles it on the strih Windows target. The two
  previously-explicit `--features ndi` clippy/test steps were REPURPOSED to `--no-default-features`
  so the stub (libndi-free) path stays proven and can't bit-rot; the RELEASE/deploy builds keep
  `--features ndi` written explicitly (it now equals the default, kept for intent/deploy-shape
  clarity + to satisfy `test_bkshading_deploy_relay_808.py`). `test_bkshading_ndi_default_1157.py`
  pins the default-includes-ndi decision + the `--no-default-features` CI coverage (tomllib + yaml,
  no cargo).
- **Provisioning/verify:** `scripts/bkshading-provision-ndi.sh` (+ source-only pure helper
  `scripts/lib/bkshading-ndi-runtime.sh`) — idempotent, fail-loud, enable-only. Linux `--check`
  verifies discovery / `--install` delegates to `vendor/distroav/CI/libndi-get.sh`; Windows reports
  the documented DLL path. `tests/python/test_bkshading_ndi_provision_1157.py` cross-checks the shell
  dirs/names + the Windows DLL AGAINST `ndi_paths.rs` so the two lists cannot drift.
- **STILL the rig-verify half (supervisor, live):** run the strih service `--features ndi` against a
  live cambox NDI source + confirm the 4+4 preview updates, and confirm the 1 remaining M2 SDK
  deferral (full FourCC coverage of the real low-bandwidth stream). The refcounting one —
  per-source init/destroy across a reconnect — is RESOLVED IN CODE (issue 808, 2026-08-23):
  `preview/shared_runtime.rs` is a pure default-feature keep-alive slot (`SharedRuntime<T>`,
  const-init static; load-once for the process lifetime, failed load never cached) and
  `ndi_source.rs` acquires the runtime ONLY through `NdiLib::shared()` — a per-connect load
  would let one camera's reconnect run the process-GLOBAL `NDIlib_destroy()` under every other
  live receiver (the worker drops its source before every backoff). Deliberately keep-alive, NOT
  a destroy-on-last-drop Weak pool: with a single preview camera the worker's drop-before-backoff
  would otherwise cycle full SDK destroy→init every ~2 s while the feed is down. Structural pins +
  behavior tests live in `service/tests/preview.rs` (default features).
  The 3rd — the color-format meaning vs the installed header — is
  RESOLVED (#808 SBC lane): value 0 is `BGRX_BGRA` per `Processing.NDI.Recv.h`, so the misnamed
  constant was renamed `COLOR_FORMAT_UYVY_BGRA` → `COLOR_FORMAT_BGRX_BGRA` (behaviour unchanged; the
  same harmless mislabel still stands in the main display path `src/ndi.rs`, a separate subsystem).
  Task 4 (make `--features ndi` the default build, or keep opt-in) is RESOLVED — owner decision
  2026-08-24 (issue 1157 comment 5393834171, možnosť 1; features-default-on rule): **`ndi` is now
  the DEFAULT bkshading service build** (`bkshading/service/Cargo.toml` `[features] default =
  ["ndi"]`). The appliance crate is byte-untouched (separate workspace member; appliance CI runs
  cargo at the root without `--workspace`/`-p bkshading`). libloading stays an optional dep
  activated by the default feature — a RUNTIME dynamic load, so the default build still compiles on
  CI with no libndi. **Graceful degrade on a libndi-less host:** with ndi default,
  `source::build_default_source` always builds the real `NdiPreviewSource` (never a stub fallback);
  a missing runtime bails from `ndi_source::load_uncached()` with a platform-neutral message
  (`"NDI runtime not found (install the NDI SDK / NDI Tools, or set NDI_RUNTIME_DIR_V6); ..."`),
  and `worker::run_forever` logs it as a `tracing::warn!` (cam+source+error) then backs off and
  retries forever — fail-loud, non-crashing, no silent stub. **libndi provisioning on the strih
  service host + the live end-to-end verify against a cambox NDI source remain the supervisor's rig
  steps.**


## Camera fps ↔ box grab-mode sync (issue 809)
The camera FRAME-RATE get/set ALREADY EXISTS from M1 — do NOT add a new message pair. It flows
through the general shading path: `ShadingParams.fps100` (project fps d007 x100) + `sensor_fps100`
(d006 readback) on the GET side, and `SetRequest.fps` → `read::plan_writes` → gphoto2 `d007` on the
SET side; the relay reads d006/d007 in `read_state`, `RelayState.fps_supported` reports whether d007
is exposed. #809 added only the grab-mode SYNC LAYER on top of that (duplicating the get/set would
break the one-source-of-truth the owner flagged in the MVP):
- proto: `FpsSync {Unknown,Synced,Mismatch}` + pure `FpsSync::classify(camera_fps100, grab_fps)`
  (kebab-case wire: `"unknown"/"synced"/"mismatch"`); `CameraView` gains `grab_fps` + `fps_sync`.
- service: `CameraConfig.grab_fps: Option<i64>` (per-camera box grab mode, `60` for cam1); the
  aggregator computes `fps_sync` in the pure `camera_view` (CI-tested in `service/tests/service.rs`).
- web panel: shows the grab fps, a mismatch warning, and an EXPLICIT per-camera "align to grab"
  button that issues the existing `SetRequest.fps`. NEVER an auto-write — a camera-side format
  change can interrupt recording (owner constraint); the button lives only in the click handler,
  never in `updateBlock` (which runs every poll). Test `test_app_js_align_button_...` pins that.
- The sync compares PROJECT fps (d007), NOT sensor fps (d006): d007 is exactly what the align write
  changes and what the camera's HDMI output follows; `sensor_fps100` stays an off-speed diagnostic.
- `grab_fps` is a plain integer for now (the rig is integer-genlock 60). A fractional NTSC grab
  (59.94/29.97) would classify Mismatch against an integer and needs a new representation — deferred
  scope, not a bug. Deriving grab from the box's live capture_fps (vs a static config field) is the
  follow-up.


## Relay focus-distance exposure + the honest focus/exposure-MODE constraint (issue 1238)
The relay's `/api/state` now reports the camera's **manual focus DISTANCE** as
`ShadingParams.focus_distance: Option<i64>` (camelCase `focusDistance`), read from gphoto2
`d003` (`FOCUS_DISTANCE_KEY` in `transport.rs`), parsed by the existing pure `current_i64` in
`read.rs::params_and_caps`. It rides the SAME issue-1229 coalesced/min-interval-floored read
cycle as the seven shading keys (one extra `--get-config` per throttled read — never a
per-request read, never a second cadence) and is read **best-effort** (`get_config(...).
unwrap_or_default()`): unlike the core exposure trio (iso/f-number/d002 use `?`), a missing
`d003` degrades to `None` and must NOT suppress the essential shading state. READ-ONLY by
design — never in `SetRequest`/`plan_writes` (a focus write during a take is unsafe). Wire
compat: `#[serde(default)]` (missing → `None`) + no `deny_unknown_fields` (an older reader
ignores the new key), so relay/service/panel interoperate across versions with no other edit.

- **The BMPCC PTP space exposes NO focus-MODE (AF/MF) selector and NO auto/manual
  exposure-MODE (program) selector — this is a hardware fact, not a gap in our code.** Verified
  against the authoritative TalOrg BMPCC-over-PTP control-point list
  (https://www.tal.org/tutorials/blackmagic-pocket-cinema-camera-usb-control-over-ptp) + the MVP
  `mapping.rs` "Verified PTP facts". The documented properties are `iso`, `f-number`, and
  `d001`(unknown RANGE 30–5000), `d002`(shutter angle), **`d003`(manual focus DISTANCE)**,
  `d004`(WB Kelvin), `d005`(tint), `d006`(sensor fps), `d007`(project fps),
  `d008`(unknown MENU 2/0), `d009`(unknown ro 0), `d00a`(unknown ro 0). The standard PTP
  `focusmode`(0x500A)/`expprogram`(0x500E) are absent. So `d003` distance is the ONLY honest
  focus signal — its presence confirms manual focus control is reachable, and a value that is
  STABLE across reads is a no-AF-hunt proxy; there is no honest way to report a focus/exposure
  MODE flag. **Cache caveat for the consumer (issue 1229):** two `/api/state` samples within the
  relay's `min_read_interval_ms` floor (default 10 s) return the SAME cached snapshot and
  `RelayState` has no read-timestamp/cycle id, so a stability-based no-hunt check MUST space its
  samples further apart than the floor (or add a `readAtMs`/cycle field to `RelayState` first) —
  a naive "changed between two quick polls?" check would always read "stable".
- **Do NOT fabricate a `focusMode`/`exposureMode` field.** An explicit absent field with this
  documented meaning beats a permanently-`null` field reading a key the BMPCC does not implement,
  and asserting `d008 = exposure mode` (or any undiscovered d-code) without the live camera is the
  fabrication the LOUD-UNKNOWN doctrine bans.
- **Rig-discovery follow-up (supervisor step, needs the live-cabled BMPCC):** `d001`/`d008`/
  `d009`/`d00a` are undiscovered and MIGHT hold a mode flag. To identify one: `gphoto2
  --get-config d001` (…d008/d009/d00a) while toggling the camera's Auto Exposure / focus menu and
  observing which value changes. If a mode d-code is found, add it exactly like `focus_distance`
  (a new `RawConfigs` field + `FOCUS_DISTANCE_KEY`-style const + a `ShadingParams` field, read
  best-effort). Until then, no mode field exists — by design.
- **Consumer wiring landed (issue 1238, follow-up lane).** The issue-1237 `[0/8]` preflight
  (`scripts/lib/bkshading-preflight.sh`) now reads `params.focusDistance` via
  `bkshading_preflight_state_focus_distance` and, when the relay reports a value this cycle,
  prints ONE new informational REPORT-ONLY line (`bkshading_preflight_focus_distance_message`,
  `d003=<value>`) — never phrased as satisfying the #220 "FOCUS: MANUAL" checklist item, since
  presence only confirms manual focus control is reachable (the stability-across-reads no-hunt
  proxy above still needs samples spaced beyond the issue-1229 read floor; the preflight's single
  `curl` per E2E run does not attempt that). Absent/null `focusDistance` prints nothing new — the
  behavior from before this ticket is preserved exactly. The honest
  `bkshading_preflight_focus_note_message` NOTE (FOCUS-MODE / auto-manual EXPOSURE-MODE are
  hardware-unexposed) is printed UNCONDITIONALLY either way — a present distance never makes the
  MODE knowable. Live BMPCC verification of the printed `d003=` value against the physical lens
  ring is a supervisor rig step (needs the camera cabled to a relay box), not a code-lane task.


## SBC / handheld provisioning (issue 808 — the last milestone; Design v3, owner 14.9.2026)
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
- **Deploy uses `bkshading-deploy-relay.sh --arch arm64 --no-remount`.** `--arch arm64` fetches the
  `bkshading-relay-linux-arm64` artifact; `--no-remount` skips the read-only-root swap (a cambox
  appliance has a ro root; a **stock arm64 SBC image — Raspberry Pi OS / Debian / Armbian — root is
  read-WRITE** — remounting it ro is wrong). The default (no flags) is still amd64 + ro-root remount
  (cambox), byte-unchanged.
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


## Service DEPLOY path onto strih (Windows) — issue 808 (repeatable, mirrors the relay canon)

The `bkshading-windows` CI job release-builds + uploads the deployable service as
`bkshading-windows-amd64` (`target/release/bkshading.exe`). The repeatable deploy of THAT onto strih
is `scripts/bkshading-deploy-service.sh` (dev1 orchestrator) + `scripts/bkshading-install-service.ps1`
(on-box installer) + the pure-invariant lib `scripts/lib/bkshading-deploy-service-runtime.sh` — the
ONE source of truth for the artifact name, exe, install dir (`C:\bkshading`), config filename, task
name (`bkshading-service`), port (`8770` == `config.rs` `default_bind`), and keep-alive cadence, so
CI / the .sh / the .ps1 cannot drift (`tests/python/test_bkshading_deploy_service_808.py` cross-checks).

- **Transport = the recordings-retention style:** `scp -O` the exe + config seed + installer to the
  box, then run the installer via `powershell -NoProfile -ExecutionPolicy Bypass -File` — NEVER a
  nested `powershell -Command` over ssh. DRY-RUN is the DEFAULT for BOTH the `.sh` and the `.ps1`;
  `--execute` / `-Execute` performs the mutating half. The `.sh` DRY-RUN touches nothing remote.
- **Keep-alive, NOT Task Scheduler RestartCount:** Task Scheduler has no real Restart=on-failure for
  a long-lived process, so the installer registers ONE task with two triggers (AtLogOn + a repetition
  every N min) whose action re-runs the DEPLOYED installer `-KeepAlive -Execute` — the idempotent
  check-and-relaunch idiom (obs-self-heal / avsync-keepalive, `.claude/rules/avsync-monitoring.md`).
  The `-KeepAlive` pass matches the running service by its EXACT `ExecutablePath` (never a bare
  process name — the avsync gotcha) and relaunches via `Start-Process ... --config <toml>` if absent.
- **Config is seeded ONLY IF absent** (`bkshading.example.toml` -> `bkshading.toml`) — a redeploy
  never clobbers an operator-tuned config. The service config carries NO credential (pure camera
  list + bind + `[preview]`), so nothing secret is ever written by the deploy. The `.ps1` is pure
  ASCII (scp'd → non-UTF-8 codepage on the box, `.claude/rules/recordings-retention.md`).
- **Two verify gotchas hardened by the #808 review (reusable for ANY Windows-service deploy here):**
  (1) the port check must confirm the LISTENER'S OWNER, not just that *something* listens — resolve
  `(Get-NetTCPConnection -LocalPort N -State Listen).OwningProcess` → that process's `ExecutablePath`
  and require it be the DEPLOYED exe; a bare "port N is Listening" false-greens when a stale/foreign
  instance (e.g. the manual `C:\stage-bkshading` from #1157) holds the port while the new exe fails to
  bind. Pair it with an install-time **by-NAME** stop (migration off the manual stage) while the
  steady-state keep-alive pass keeps the EXACT-path match. (2) byte-verify the scp'd exe:
  local `sha256sum` vs remote `certutil -hashfile <path> SHA256` (line 2 is the hash — strip
  whitespace, lowercase; empty side = mismatch), mirroring the relay sibling — a truncated scp is
  otherwise caught only by scp's exit code.
- **UNVERIFIED (supervisor rig step):** the LIVE `--execute` install against strih (scp +
  `Register-ScheduledTask` + `:8770` verify) + confirming the panel is up — done from a session with
  win-strih MCP / rig access, not an isolated worktree lane. This complements the deferred libndi
  provisioning + live NDI-preview verify already noted above.


## E2E harness must PAUSE the relay on the two measurement-critical camboxes (issue 808, live evidence)

The relay is a fleet-standby service — owner directive: it runs on EVERY cambox so any camera can
be shaded on demand — but its gphoto2 USB-PTP polling causally degrades the E2E harness's own
measurement quality on the two boxes it needs to trust most: the SOURCE camera (USB-bus
contention with the physical camera's Cam Link 4K capture device — cam1 measured 58.3-58.9 fps vs
a healthy 60.0, confirmed by stop/start isolation) and cam2/painter (a 3-core box already running
camera-box RT + the painter, where the extra CPU/jitter correlates with worse dual-QR window
quality — 2/2 clean relay-off vs 4/5 over-tolerance relay-on). Evidence: issue 808 comments
2026-08-29T09:59:31Z / 2026-08-29T15:54:47Z. If you ever see a mysteriously degraded/dropped-frame
E2E run and a camera happens to be physically cabled to a cambox at the time, check
`systemctl is-active bkshading-relay` on the SOURCE box and cam2 first — it is a known, already-
mitigated contention source, not a mystery regression in camera-box/genlock code.

- **The fix is `scripts/lib/bkshading-e2e-pause.sh`** (mirrors the sibling
  `bkshading-preflight.sh` split: pure remote-text builders + a fail-safe pure parser + two thin
  ssh orchestrators). `scripts/recording-e2e.sh` pauses (`systemctl stop`) the relay on the
  run-resolved `$CAM1_IP` (the SOURCE camera, whichever of cam1/cam3/cam4/cam5/cam6 was selected)
  and `$PAINTER_IP` (cam2) right after the existing `bkshading_preflight_report` call, recording
  each box's PRIOR active state; `cleanup()` restores it at the very end, but ONLY on a box where
  the pause step found it genuinely active beforehand — a box the operator deliberately silenced
  (e.g. via the interim manual `systemctl stop`) is never woken back up by a run.
- **Do not conflate this with the M3 preflight check** (`scripts/lib/bkshading-preflight.sh`,
  automated shutter-checklist WARNING) — that reads the camera's state; this pauses the relay
  process entirely, and both run back-to-back at `[0/8]`.
- **This is deliberately its OWN, dedicated ssh call — never spliced into the existing
  `CAMBOX_PARALLEL_*` device-restore group** in `cleanup()` (`cambox_parallel_retry_failed`'s own
  retry command is camera-box-specific and would be wrong to apply to a bkshading-relay-only
  restore). It runs LAST in `cleanup()`, after the `#684`-class FINAL camera-box.service verify,
  so this non-safety-critical restore never delays the safety-critical device-restore phase.

### The CANCELLED-run case — a persistent ON-BOX marker, not just the in-memory was-active var (issue 1278)

**The pause/restore pair above has a gap the runner's own kill semantics create: a CANCELLED (`gh
run cancel`) or otherwise KILLED run never reaches its restore call at all.** `_bksh_was_active`
(and the `$BKSH_PAUSE_*_WAS_ACTIVE` vars that carry it) live ONLY in the memory of the ONE
`recording-e2e.sh` process running that attempt — the runner kills the process tree SECONDS after
a cancel, while `cleanup()`'s own device-restore phase (which must run BEFORE the bkshading
restore, per the ordering rule above) takes MINUTES. Live incident (2026-09-02, run 33640143227):
attempt 2's `[0/8]` pause found the relay active on cam1+cam2 and stopped it, was cancelled with
no `[cleanup]` line ever printed; attempt 3's OWN pause then found the unit already `inactive`
(from attempt 2's stop) with no way to know that was OUR doing, read the fail-safe default
was-active=0, and — correctly per its OWN inputs — left it stopped. Net: both boxes sat silently
shading-dead for 76 minutes until a human noticed and ran `systemctl start` by hand.

- **The fix: a persistent marker file ON THE BOX**, `/run/bkshading-e2e-paused` (tmpfs — writable
  even on the cambox's read-only rootfs; vanishes on reboot, after which the `enabled` relay unit
  starts on its own anyway). ONE source of truth: `bkshading_e2e_pause_marker_path` (default
  `/run/bkshading-e2e-paused`, overridable via `BKSHADING_E2E_PAUSE_MARKER` for Tier-0 testing),
  consumed by BOTH `bkshading_e2e_pause_stop_cmds` and `bkshading_e2e_pause_restore_cmds`.
- **`bkshading_e2e_pause_stop_cmds` now treats EITHER `systemctl is-active` OR an already-existing
  marker file as was-active=1** — a marker still on disk means a PRIOR run paused the relay and
  never got to restore it, so THIS run must (and does) report was-active=1 and takes over the
  restore obligation. When was-active becomes 1, the marker is written BEFORE `systemctl stop` —
  so a run killed between the write and the stop still leaves the marker behind for the next run.
- **`bkshading_e2e_pause_restore_cmds`'s "1" branch clears the marker ONLY in the SUCCESS path**,
  after `systemctl is-active` has actually confirmed the relay came back up — the pre-existing
  WARNING/failure branch (unit never comes back active) deliberately LEAVES the marker in place,
  so a genuinely-failed restore is retried by a LATER run's pause step (or a future watchdog)
  instead of the marker being lost and the relay staying silently dead forever. The "0" (no-op)
  branch is untouched — an operator's deliberate manual stop, WITH NO UNRESOLVED E2E-CAUSED
  MARKER ON THE BOX (unit already inactive, no marker), is still never woken back up by a run,
  exactly the pre-existing #808 guarantee. **Precision caveat (review finding):** the marker
  itself can't distinguish "we still owe a restore" from "the operator separately silenced it in
  that same window" — if an earlier CANCELLED run's marker is still on disk when an operator
  independently `systemctl stop`s the relay by hand, the NEXT run's restore will re-activate it
  against that fresh manual intent (a narrow coincidence-window residual, not a regression: the
  pre-1278 code had no better answer here either — it just silently left the relay broken).
- **No `scripts/recording-e2e.sh` edit at all** — the whole fix lives in the sourced lib (the #675
  pattern); both existing anchored call sites (the `[0/8]` pause + both cleanup/temp-trap restore
  calls) stay byte-identical. Tests: `tests/harness_bkshading_e2e_pause_808.rs` — pure builder/
  parser assertions for the marker existence-check + write-before-stop ordering + marker-clearing-
  only-on-success, PLUS two full functional simulations (a fake `systemctl` + a fake unit-state
  file, driven through the REAL stop_cmds/restore_cmds/parse_state) reproducing the exact live
  incident sequence (pause→cancel→pause-again reads was-active=1 via the marker→restore succeeds→
  marker cleared) and its negative companion (no marker + already-inactive unit stays was-active=0,
  restore stays a no-op, no marker ever created).
- **Not done here (optional per the ticket, not required for the fix): a dev1 watchdog that
  notices a stale marker with no live E2E run holding the rig lease and restores it proactively.**
  The marker alone already closes the 76-minute silent-dead window down to "the NEXT E2E run" —
  a watchdog would shrink it further for the case where no E2E runs again soon, but is deliberately
  out of scope for this fix (the ticket names it optional).


## The relay lifecycle is EVENT-ONLY on the rig — `rig-mode.sh test` stops+disables it, `event` starts it (issue 1311)

**Two boot sticks died in 24h (cam1 13.9., cam2 14.9.) on exactly the two boxes with a shading
camera on the SAME USB3 root hub as the boot stick** (issue 1309/1311 Finding 1/2): a consumer
896 mA SanDisk + a 512 mA Cam Link grabber + a 144 mA+ BMPCC PTP camera share one mini-PC 5 V rail,
and every relay start/stop is a PTP-session power change on that hub. Owner ruling
(14.9.2026): „nie nemas nic co by moholo odpalit disk skusat" — pursue it PASSIVELY, and remove
the software provocations. During DEVELOPMENT the shading panel is not needed, so the relay has no
business running (and polling the shared bus) while measurements run.

- **`rig-mode.sh test` STOPS + DISABLES `bkshading-relay`; `rig-mode.sh event` ENABLES + STARTS it**
  — via `scripts/lib/bkshading-relay-mode.sh` (`bkshading_relay_mode_apply <test|event> <cam_pw>
  <label=ip>…`), an additive sourced-helper call in `do_test`/`do_event` (no anchored `rig-mode.sh`
  line edited, the additive pattern). The roster is DERIVED, not a literal list: the same two boxes
  the issue-808 E2E pause + `EVENT_ASSERT_TARGETS` use — the resolved source box
  (`${RIG_SOURCE_BOX}=$RIG_SOURCE_IP`) + `cam2=$PAINTER_IP`. The unit name is the ONE source of
  truth `bkshading_relay_unit_name` (from `bkshading-relay-runtime.sh`). Every systemctl line is
  `|| true`-tolerant and the ssh loop is best-effort per box (a bad/unreachable box never aborts
  the switch).
- **This makes the issue-808 E2E pause/restore a TRUE no-op in TEST mode:** with the relay already
  stopped+disabled, `bkshading_e2e_pause_stop_cmds` reads `systemctl is-active`=false AND (in steady
  state) no `/run/bkshading-e2e-paused` marker → `was-active=0` → `bkshading_e2e_pause_restore_cmds`
  takes its `true # leave it stopped` branch → toggles nothing. No change to
  `bkshading-e2e-pause.sh` was needed; verify that path STAYS a no-op if either lib changes.
- **It also removes the issue-1229 gphoto2 polling noise from every measurement** — the bus-friendly
  min-interval floor below still applies while the relay IS running (EVENT mode), but development
  runs no longer pay any relay bus traffic at all.
- **Distinct from the deploy/ops gate below** ("NEVER deploy OR restart a cambox relay during
  production"): that is a rig-busy gate on the DEPLOY TOOL; this is the relay's steady-state
  lifecycle on the TEST/EVENT switch. Both point the same way — the relay is a broadcast-time
  service, not a development-time one.


## Relay polling is BUS-FRIENDLY — a min-interval floor, never gphoto2-per-poll (issue 1229, P0)

**The relay MUST NOT shell out to `gphoto2` on every `GET /api/state`.** Root cause of the #1229
production freeze: `read_state()` used to do one `gphoto2 --auto-detect` + seven `--get-config` =
**8 fresh USB-PTP sessions (open/enumerate/close) per poll**, and the service pump
(`service/src/main.rs`, `LIVE_PUSH_INTERVAL_MS = 2000`) polls every relay's `/api/state` every 2 s
UNCONDITIONALLY. On cam1 the BMPCC (PTP) and the ezcap CAM LINK 4K grabber hang on the SAME
4-port xHCI SuperSpeed bus, so that per-poll PTP traffic disturbed the grabber's isochronous UVC
stream — capture 60→55 fps within 6 s of relay start, then the #663 capture-rate self-heal
USB-reset every 600 s cooldown = ~10 s frozen picture, ~6× live during production 30.8.

**The doctrine (the chosen fix — approach 1 of the ticket, and the owner's "kadencia ≥10 s idle"
half):** `CameraSession` serves `/api/state` from a `read_cache` gated by a **min-interval floor**
(`DEFAULT_MIN_READ_INTERVAL_MS = 10_000`, env `BKSHADING_RELAY_MIN_READ_INTERVAL_MS` TUNES it but
can never disable it — 0/negative/junk falls back to the default; features-default-on). Key points:
- The floor caps the READ RATE regardless of how hard the service polls: a poll within the floor of
  the last real read is served from cache with ZERO gphoto2 / ZERO USB traffic. So even with a
  panel open (service pumping every 2 s) the shared bus sees **at most one PTP session per floor**.
- **The cache `Mutex` is held ACROSS the blocking read on purpose** — a burst of concurrent
  `/api/state` requests coalesces to ONE real read (the others get the cache). Serializing gphoto2
  access to the single USB camera is itself correct: concurrent gphoto2 processes on one device
  would contend on the very bus this protects.
- **Writes (`apply`/`SetRequest`) stay per-invocation** (user-initiated + rare) and INVALIDATE the
  cache on success, so the next poll reflects the change instead of a stale cache for up to a floor.
- **Testability seam:** pure `read_is_fresh(read_at_ms, now_ms, floor_ms)` + a `MonoClock` trait
  (`InstantClock` prod, `FakeClock` in `tests/relay.rs`) let the floor be Tier-0 tested via the
  fake runner with an injected clock (count gphoto2 spawns under a burst / after floor-expiry /
  after a write) — no real sleeps, no camera. When cargo can't run locally (Tier-0 #557), verify
  the decision RED→GREEN with a throwaway local python replica of `read_is_fresh` + the
  burst/floor/invalidate simulation (a dev aid — nothing committed; CI is the first real compile).
- **REJECTED alternative — a persistent `gphoto2 --shell` session** (approach 2): it only cuts
  per-read RÉŽIU (re-enumeration), NOT the FREQUENCY of control traffic (the actual root), and
  brings its own failure class (shell wedge, camera-unplug holding a dead session, fragile
  stdin/stdout parsing needing detect+restart). The `Gphoto2Runner` trait seam keeps it as a
  possible future 2nd impl, but the floor solves the root far more simply/safely.
- **Cross-ref issue 1228 (relay `Restart=` lifecycle) — STILL BLOCKED even after this floor merged
  (status 2026-08-30):** this fix does NOT touch the systemd unit. The floor IS merged + live-verified
  on cam1 (17-30 min, 0× capture-rate self-heal, 0× USB reset), but issue 1229's OWN live-verify comment
  found a documented residual — occasional capture dips (54.5-58.5 fps, well below the 60.0 baseline
  but NOT enough to re-trip self-heal) still correlate with individual gphoto2 PTP transactions
  colliding with the grabber's isochronous stream on the shared xHCI bus. **SUPERSEDED 2026-09-03
  (owner ruling, webterm):** the PHYSICAL USB2 step is IMPOSSIBLE — the cam1 mini PC has a SINGLE
  xHCI controller (see the "Coalesce the read into ONE multi `--get-config` USB session" subsection
  below), so #1229 became a CODE lane and the residual is now attacked by coalescing the per-read
  gphoto2 sessions (9 → 3), not by re-cabling. **1228 unblocks only once 1229 actually closes** (or
  the owner explicitly says otherwise) — do NOT add `Restart=on-failure` just because a floor/coalesce
  merged; re-check `gh issue view 1229` state/labels before touching the unit.
- **Complementary idle lever (owner's "poll len on-demand keď je panel otvorený" half, NOT done
  here):** the service could poll relays only while a WS/panel client is connected, for TRUE-zero
  idle. It lives in a different crate (service, ships to Windows/strih) with WS-lifecycle
  subtleties (stale-on-reconnect, immediate-refresh) → a separate focused PR + rig-verify. The
  floor already bounds the worst case (1 read/floor even with a panel open — the case that matters
  during live shading), so it is deferred, not dropped; file it with evidence if live-verify shows
  the residual idle burst still disturbs capture.

### Coalesce the read into ONE multi `--get-config` USB session — the residual per-read footprint fix (issue 1229 CODE lane, 2026-09-03)

**Owner ruling 2026-09-03 (webterm): the PHYSICAL USB2 step is IMPOSSIBLE and the ticket is a CODE
lane.** The cam1 mini PC has a SINGLE xHCI controller (confirmed live `lsusb -t`: Bus 002, one
4-port SuperSpeed root hub, carrying the grabber's isochronous UVC stream AND the BMPCC PTP camera
AND a `uas` mass-storage SSD together), so moving the BMPCC cable to "another port" isolates
nothing — every USB device on the box shares the one controller. Disabling shading on cam1 is also
rejected (owner wants shading working WHILE grabbing). So the residual (documented above: freezes
GONE with the floor — 0× self-heal live — but ~5 % of 5 s windows still dip <58.5 fps, min ~51.8,
each dip correlating with a gphoto2 read) is closed by a CODE reduction of the per-read footprint,
NOT a physical or lifecycle change.

- **The fix: a batched `Gphoto2Runner::get_config_many(&[&str])` on top of the floor.** A real read
  used to fire NINE separate `gphoto2` processes (1 `--auto-detect` + 8 `--get-config`), each a
  full USB open/enumerate/close cycle — and re-enumeration (interface claim + PTP OpenSession) is
  the part that most disturbs the grabber's isochronous stream. `Gphoto2Cli::get_config_many` now
  reads the SEVEN core shading keys (`CORE_CONFIG_KEYS`) in ONE `gphoto2 --get-config k1
  --get-config k2 …` process (ONE USB session for all seven), splitting the combined stdout back
  into per-key blocks by `END`-line boundaries (`split_config_blocks`, positional map, fail-safe
  `None` on a block/key count mismatch → the read degrades to offline, never a mis-assigned block).
  `read_raw()` is now detect (1) + core batch (1) + best-effort `d003` kept SEPARATE (1) = **3 USB
  sessions per read, down from 9.** DEFAULT-ON (no toggle — features-default-on); the floor still
  caps the read RATE, this cuts the per-read enumeration COUNT.
- **`d003` stays its own call, NEVER folded into the batch** — a camera that does not answer `d003`
  would abort a batched core read (gphoto2 errors the whole invocation), wrongly degrading the
  essential shading state to offline. Best-effort `d003` alone can fail harmlessly (→ `None`).
- **Tier-0:** the batch/session count is proven by a `SessionCountingRunner` (one read ≤ 3 sessions;
  RED at 9 on the pre-fix path), `split_config_blocks`/`build_get_config_many_args` are pure and
  unit-tested + rustc-replicated, and a `CoalescingFakeRunner` (join stdout → split, mirroring the
  real `Gphoto2Cli`) proves a coalesced read yields byte-identical state to the per-key path.
- **What is NOT reduced (deferred, not dropped):** the `--auto-detect` presence probe stays a
  separate session (folding it needs caching the port + deriving presence from the batch — a bigger
  change, not this lane); and the residual "one 3-session read/floor can still occasionally clip the
  isochronous stream" is bounded far lower but not provably zero on a single shared controller — the
  final clean-watch A/B (relay-on steady vs off) is a supervisor live-verify step (blocked in this
  lane by an in-progress E2E, which itself pauses the relay). If that watch still shows dips, the
  next levers are the service-side on-demand poll (idle lever above) and inter-call pacing — both
  strictly after coalesce, which removes the most sessions for the least risk.


## A manual interim `systemctl stop bkshading-relay` is NEVER auto-restored — not even by `Restart=on-failure` (issue 1228 TERM-origin finding)

**Root cause of the 29.8.-30.8. cam1 incident (relay found dead a full day after it was stopped):**
NOT `bkshading-deploy-relay.sh`'s own stop→start (that always re-starts what it stops, and the
deploy at 06:22-06:33 UTC on 29.8 was 3+ hours before the observed TERM). The actual cause was a
**manual interim mitigation** — `systemctl stop bkshading-relay` run by hand on cam1 at
`2026-08-29T09:56:25Z` while investigating the SAME gphoto2/USB-bus contention issue 1229 later
fixed properly (issue 808 comment `2026-08-29T09:59:31Z`, 3 minutes after the TERM: *"relay STOP:
captured 59.8-60.0 fps... Mitigácia TERAZ: bkshading-relay na cam1 STOPNUTÝ"*). `systemctl stop`
sends `SIGTERM` to the main process — exactly the journal's `code=killed, signal=TERM`. The unit was
left `enabled` (comes back only on a REBOOT) and nobody manually restarted it, so it stayed dead
until the owner tried to use shading the next day.

**The lesson generalizes past this one incident: a DELIBERATE `systemctl stop` is never
auto-recovered by `Restart=on-failure`, by design — and neither is ANY OTHER SIGTERM, regardless of
who sent it.** Two independent systemd mechanisms both point the same way here: (1) systemd
suppresses `Restart=` entirely when a stop was requested by the service manager itself
(`systemctl stop`/`systemctl restart`, e.g. the deploy flow), and (2) separately, systemd treats
SIGHUP/SIGINT/**SIGTERM**/SIGPIPE as a CLEAN exit by default (same as exit code 0) for the purpose
of `Restart=on-failure`'s own "was this a failure?" check — so even a SIGTERM from a source OTHER
than the manager (a stray `kill -TERM`, some other tool) still would not trigger a restart under
`on-failure`. Issue 1228's `Restart=` code lane has now LANDED (`systemd/bkshading-relay.service`:
`Restart=always`/`RestartSec=3` → `Restart=on-failure`/`RestartSec=5`) — the precondition (issue
1229's USB2 topology question) was settled once the owner ruled the physical re-cabling step
impossible (single xHCI controller), so today's topology already IS the target one. As predicted
here, it **only** protects against a genuine unexpected crash — a non-zero exit, an operation
timeout, a watchdog failure, or termination by a signal OTHER than the four "clean" ones above
(SIGSEGV, SIGABRT, SIGKILL/OOM-kill) — it does NOT bring back a relay that was deliberately
silenced as an interim mitigation (correct behavior: an operator's deliberate stop should stay off
until they undo it), and it does NOT change the deploy stop→start flow either (both `always` and
`on-failure` already skip a manager-requested stop, and `on-failure` additionally treats SIGTERM as
clean regardless of origin). **Any interim "stop this on box X while we investigate" mitigation
still needs its OWN explicit tracking** (a ticket comment naming which boxes were stopped + a
reminder to restore them) — the harness-managed pause (`bkshading-e2e-pause.sh`, above) only covers
stops the E2E harness ITSELF performs; it has no visibility into an ad-hoc manual stop done directly
on the rig.

**Why this "SIGTERM is always clean" fact applies cleanly to `bkshading-relay` specifically —
verified against its own source, not just systemd theory (issue 1228 review-fix follow-up):**
`bkshading/relay/src/main.rs` only installs `tokio::signal::ctrl_c()` (SIGINT / Ctrl-C) for graceful
shutdown — it registers **no SIGTERM handler at all**. So a SIGTERM (from `systemctl stop`, the
deploy flow, or a stray external `kill -TERM`) is never intercepted; it kills the process via the
raw OS default, which is exactly the `code=killed, signal=TERM` shape the 29.8 incident's journal
showed, and exactly systemd's "clean exit" case (confirmed from this box's own local
`man systemd.service` "Table 1. Exit causes and the effect of the Restart= settings" — `on-failure`
has NO entry for the "Clean exit code or signal" row). **This does NOT contradict
`imag-obs-supervision.md`'s opposite-sounding claim** ("an external `pkill -TERM` … STILL triggers
`Restart=on-failure`") — that is about a DIFFERENT process (OBS) that DOES install its own signal
handling and exits via a controlled-but-non-zero path (or crashes) in response, which lands in the
table's "Unclean exit code" / "Unclean signal" rows instead. The generalizable rule: whether a raw
SIGTERM is "clean" (no restart) or "unclean" (restart) under `on-failure` depends on **whether the
specific binary catches the signal and how it exits in response** — never assume either way without
checking that binary's own signal handling, the way this was checked here.

## E2E `[0/8]` camera pre-run auto-check reads `/api/state` — shutter+iso+aperture, NOT focus/exposure-MODE (issue 808 shutter half + issue 1237 exposure half)

`scripts/lib/bkshading-preflight.sh` (wired at `recording-e2e.sh:650`, `bkshading_preflight_report
"$CAMERA_NAME" "$CAM1_IP"`, tested by `tests/harness_bkshading_preflight_808.rs`) automates the
`#220` CAMERA PRE-RUN checklist by reading the relay's `GET /api/state` — ONE `curl -fsS`, served
from the relay's issue-1229 read-floor cache (never a direct gphoto2 call). It is REPORT-ONLY
(always `return 0`, WARN never abort — owner M3 decision).

- **What is measurable from `/api/state`, and what is NOT.** `RelayState`/`ShadingParams`
  (`bkshading/proto/src/wire.rs`) + the relay read plan (`relay/src/transport.rs` reads only
  `iso, f-number, d002, d004/d005, d006/d007`) expose SHUTTER (`params.shutter`, a DENOMINATOR —
  500 == 1/500s, LARGER = faster), ISO/gain (`params.iso`), and APERTURE (`params.apertureAv`).
  There is **NO focus-mode field and NO auto/manual exposure-MODE field.** So the shutter check
  (issue 808) and the exposure-VALUES-readable check (iso+aperture, issue 1237) are real; manual
  FOCUS and auto/manual EXPOSURE MODE are genuinely unreadable → surfaced as a report-only
  `bkshading_preflight_focus_note_message` NOTE (LOUD-UNKNOWN, never a fabricated pass); issue 1238
  additionally wired an informational `bkshading_preflight_focus_distance_message` line for the one
  honest focus signal that IS readable (manual focus DISTANCE, d003) — see the "Relay
  focus-distance exposure" section above. **Do NOT let an OK line claim "exposure fixed /
  satisfied automatically" — presence of a value ≠ a fixed MODE; a BMPCC in auto still reports
  concrete iso/f-number.** (The exposure OK line was caught doing exactly this in review.)
- **Report-only python3-safety pattern (reuse for any python3-backed preflight lib).** The JSON
  extractors are python3 one-liners. Under the caller's `set -euo pipefail`, a bare
  `x="$(py_extractor "$raw")"` will ABORT the whole E2E if python3 is missing/crashes — the exact
  opposite of a report-only check. Guard it: a LOUD-BY-NAME `command -v python3 || { NOTE; return
  0; }` gate at the top of the orchestrator + `|| true` on every python3-backed substitution (a
  transient failure degrades to EMPTY → a report-only warn, never a crash). The extractors treat a
  JSON bool as ABSENT (`not isinstance(v, bool)` — python bool is an int subclass) and print EMPTY
  on a non-dict body / non-dict `params` / null (never a fabricated value).
- **Extending it is anchor-safe by construction.** New behavior goes into the LIB
  (`bkshading_preflight_report` + pure fns) — `recording-e2e.sh`'s one call line stays
  byte-identical, so the #675 anchor sweep is trivially clean. Keep the classifier's decision the
  single source of truth for a WARN's named parameter (pass the STATUS into the message, don't
  re-derive the missing set from the values in two places).

## NEVER deploy OR restart a cambox relay during production — the deploy tool is now rig-busy gated (2026-09-13 escalation, issue 1229)

**The 2026-09-13 escalation, past the capture-dip class this ticket started on:** deploying the
relay to cam1 (`scripts/bkshading-deploy-relay.sh`-style scp/mv-over-running + `systemctl restart
bkshading-relay`) DURING a live production recording did not merely dip the capture rate — it
fork-WEDGED the whole box. ssh, the `linux-cam1` MCP agent, and `gphoto2 --auto-detect` all
reset/timed out while the box still pinged and the already-running relay still answered HTTP;
recovery needed an owner power-cycle (no remote reboot path to a cambox). The gphoto2 PTP polling
sharing the single xHCI controller with the grabber is dangerous UNDER PRODUCTION LOAD beyond the
capture drop — it cascades into stuck D-state gphoto2 processes and fork-exhaustion of the box.

**The doctrine (both halves now enforced in the deploy tool):**

- **Every bkshading relay deploy/restart is gated on rig-not-busy.** `bkshading-deploy-relay.sh`
  runs a rig-busy PREFLIGHT before its first cambox ssh/scp, reusing the ONE shared guard
  `recording-e2e.sh` uses — `stray_session_check_assert HERE STRIH STREAM WHAT` (→ `obs_phase2.py
  rig-busy-check`; NEVER a duplicated per-box WS loop, see `rig-mutation-broadcast-guard.md`). It
  REFUSES (non-zero, naming what is streaming) when strih/stream are broadcasting, and fail-OPENs
  (WARN + proceed) ONLY when NO box is readable — never blocks on a transient read error.
  `--force-live` (logged loudly) is the SUPERVISOR-ONLY override for a genuine idle-rig-but-guard-
  unavailable case; a normal deploy never passes it. Env `STRIH_HOST`/`STREAM_HOST` (default
  10.77.9.202/.204) + `OBS_PASSWORD` mirror `rig-busy-gate.sh`; `BKSHADING_DEPLOY_OBS_PHASE2_DIR`
  is a Tier-0 test seam pointing the guard at a fake `obs_phase2.py`. The gate applies to EVERY
  deploy including a handheld SBC (`--arch arm64`), which shares no xHCI bus with the rig — this is
  a deliberate conservative default (a handheld camera may itself be in use during a broadcast);
  pass `--force-live` for a genuinely off-rig handheld deploy.
- **The RESTART that adopts a freshly-deployed binary is a SEPARATE, rig-idle-ONLY supervisor
  step — never part of the deploy.** The deploy stays ENABLE-ONLY (it never starts/restarts the
  unit; provisioning-scripts.md), and now uses an ETXTBSY-safe swap: scp lands on a staging path
  in the SAME directory (`<dest>.deploy.<pid>`) then atomic `mv -f` over the (possibly running)
  binary — scp directly onto a running executable fails `ETXTBSY` ("dest open: Failure"), and
  `rename(2)` swaps the inode while the running process keeps the old one. So the new bytes are on
  disk but NOT live until a deliberate restart, and that restart (adopting the new binary, or the
  interim manual pause/resume) is itself subject to the same rig-idle discipline — do it only when
  the rig is idle, never during a broadcast. (The relay `Restart=on-failure` lifecycle is issue
  1228, unchanged here — this deploy touches no systemd unit.)


## Panel +/- step buttons — aperture is a CHOICE step, K/tint are linear (issue 1304)

The operator panel's three sliders (clona/biely bod/tint) each gained a `-`/`+` step button pair
(`data-role="<role>-dec"/"-inc"`, `.step-btn` in a `.slider-row`), for precise stepping where a
drag is imprecise. The three step semantics DIFFER and are not interchangeable:
- **Aperture = ONE f-number CHOICE, not a fixed norm delta.** The panel needs the choice COUNT to
  compute the step, which the M1 `CameraCaps` did not carry. Added an ADDITIVE
  `CameraCaps.fnumber_choices: Vec<f64>` (camelCase `fNumberChoices`, `#[serde(default)]`), filled
  by `read.rs::params_and_caps` from the SAME `parse_choices(&raw.fnumber)` RADIO list via
  `parse_fnumber` — so the panel's `idx = round(norm*(n-1))`, `idx' = clamp(idx±1, 0, n-1)`,
  `apertureNorm = idx'/(n-1)` round-trips EXACTLY through the relay's `norm_to_choice_index`
  (round-half-up) over the identical list. **`Eq` was dropped from `CameraCaps`** — `Vec<f64>` has
  no total order; only `PartialEq` was ever used (the `RelayState` whole-state `assert_eq!` tests
  need only `PartialEq`), and nothing bounds `CameraCaps: Eq`. When the relay sends no choices
  (older relay → empty `Vec` via serde default), the panel DISABLES the aperture ± with a title —
  never a fabricated step.
- **Biely bod = ±100 K, tint = ±1**, clamped to the slider's own min/max; the button `disabled`s
  at a bound. `refreshStepDisabled(el)` (called each poll AND after each step) drives the disabled
  state; the choices are stored on the block dataset (`el.dataset.fnumberChoices`), the SAME
  pattern as issue 809's `grabFps`.
- **One tap = one PUT, NO auto-repeat on hold** — the issue 1229 USB-PTP shared-bus doctrine (one
  write = one gphoto2 session). The step handlers (`stepAperture`/`stepLinear`) are plain CLICK
  handlers with NO `setInterval`/`setTimeout` — pinned by `test_app_js_step_handlers_have_no_repeat_timer_1304`.
- **Server-truth preserved:** each tap reads the slider's CURRENT (last server) value, steps it,
  sets the slider locally so a quick second tap builds on it, and sends the ABSOLUTE value; labels
  re-sync from the next WS push. The existing `interacting` pointerdown/up guard + `guard()`
  wrapper are respected exactly like the slider `change` handlers.
- **Playwright E2E** (`bkshading/service/tests/e2e/`, wired into the `bkshading` CI job): the
  JUST-BUILT service runs against a stdlib stub relay (`bkshading/service/tests/stub_relay.py`,
  serving `/api/state` with `fNumberChoices` + recording PUT `/api/params` bodies). Playwright's
  `webServer` array manages BOTH the stub and the service; the spec clicks +/- on clona/K/tint and
  asserts the forwarded absolute PUT bodies + a clean console. Chromium only, one spec (small CI
  budget). `npm install` (no committed lockfile — Tier-0 has no network), then `npx playwright
  install --with-deps chromium`.


## Installable web app (PWA) — manifest + no-cache SW + embedded icons (issue 1305)

The panel is installable as a windowed web app (own icon in the Windows dock, no browser chrome):
- **Static assets in `bkshading/service/web/`:** `manifest.webmanifest` (name/short_name, `lang
  "sk"`, `start_url`/`scope` `/`, `display "standalone"`, dark `#14171c` bg/theme, icons 192+512
  with BOTH `purpose "any"` and `"maskable"` entries), `sw.js`, `icon-192.png`/`icon-512.png`,
  `favicon.svg`. `index.html` links `rel="manifest"`, `meta theme-color`, `rel="icon"` (svg+png),
  `apple-touch-icon`; `app.js` registers the SW guarded on `"serviceWorker" in navigator`
  (insecure-origin LAN page → API undefined → no-op, clean console; `.catch(()=>{})` swallows any
  error).
- **The SW does NO caching — server-truth.** `sw.js` is install/activate + a pure `fetch(event.
  request)` passthrough; it exists ONLY to meet the browser's PWA-install requirement. NEVER add
  the Cache Storage API (a cache would risk a stale UI/state) — pinned by the python + Rust tests
  asserting `caches` is absent.
- **Icons are generated deterministically by a committed stdlib script** `web/gen-icons.py`
  (zlib/struct PNG writer + the SVG, NO Pillow, no new dependency) — an aperture/lens glyph inside
  the central ~60% so the icons are maskable-safe. Re-run it after a palette/glyph change; output
  is byte-deterministic. The PNGs are committed (small: ~2 KB / ~5.5 KB) and EMBEDDED via
  `include_bytes!`/`include_str!` in `http.rs` (self-contained binary, same model as the HTML/JS).
- **Routes in `http.rs`:** `/manifest.webmanifest` (`application/manifest+json`), `/sw.js`
  (`text/javascript; charset=utf-8` + a `Service-Worker-Allowed: /` header so the SW controls the
  whole origin), `/icon-192.png` + `/icon-512.png` (`image/png`), `/favicon.svg` (`image/svg+xml`).
  Each is served via a pure `*_asset()` helper + a `*_CONTENT_TYPE` const (mirrors `rendered_index`),
  so the service route tests pin the payload + content-type WITHOUT standing up an HTTP server
  (Tier-0 — the codebase has no tower dev-dep).
- **Secure-context fact (for the owner):** Chrome/Edge offer a PWA install ONLY on HTTPS or
  `localhost`. So: (a) on the strih PC itself, `http://localhost:8770` gives the FULL install; (b)
  on another PC over plain-HTTP LAN (`http://strih.lan:8770`) the browser gives no install prompt —
  use Edge "Apps → Install this site as an app" / Chrome "Cast, save and share → Install page as
  app" to create a windowed shortcut (icon + name come from this change) — this stays a FALLBACK;
  (c) a full PWA from any LAN device = the **LAN HTTPS front** (below), the PRIMARY path. Do NOT
  add self-signed HTTPS to the service (an untrusted cert is still an insecure context).

### LAN HTTPS front — PRIMARY full-PWA path (issue 808, owner ruling 17.9.2026)

The owner ruled 17.9.2026: HTTPS **áno, ale cez lokálnu sieť, nie cez internet** — a trusted-cert
HTTPS origin, but traffic stays on the LAN, never through the internet. So the full PWA install
from any LAN PC/mobile now comes from a **dev1 nginx TLS reverse proxy on a PUBLIC DNS NAME that
resolves to dev1's PRIVATE LAN IP** — never the cloudflared tunnel.

- **Topology:** browser (strih/stream/mobile on the LAN) → DNS `shading.newlevel.media` =
  `10.77.9.200` (dev1 LAN, **DNS-only / not proxied**) → dev1 nginx `:443` (Let's Encrypt cert via
  DNS-01) → `http://strih.lan:8770` (the bkshading panel). Traffic never leaves the LAN; the
  internet is used **only** for the DNS lookup and the ACME cert renewal (the rig on an event has
  mobile data for those).
- **Reproduce it on dev1:** `scripts/dev1-shading-https-install.sh --install` (idempotent) and
  `--check` (report-only: packages, DNS answer, cert presence, nginx site enabled + `nginx -t`,
  `curl` 200 on the manifest). The committed nginx site is `scripts/nginx/shading.newlevel.media.conf`
  (== the lib's default render — a drift-guard test pins the equality). The pure decisions (site
  render, certbot argv, deploy hook, cloudflare.ini, --check verdict) live in
  `scripts/lib/shading-https.sh` and are Tier-0 tested by `tests/python/test_shading_https_install_808.py`.
  Flags: `--hostname` / `--upstream` / `--lan-ip` / `--email` (live defaults baked in), `--dry-run`
  (rehearse the DNS record step). The installer needs privilege for `/etc` writes; the owner/
  supervisor runs it on dev1 (never a lane worker).
- **Cloudflare pieces (dev1, NOT committed):** the DNS A record is created via the airuleset
  `cli_cloudflare_dns` API client (zone token `~/.secrets/cloudflare-newlevel`); the certbot DNS-01
  credentials live in `/etc/letsencrypt/cloudflare.ini` (root:600, token copied from that secret
  file — NEVER printed, NEVER committed); `certbot.timer` renews and the deploy hook
  `/etc/letsencrypt/renewal-hooks/deploy/nginx-reload.sh` reloads nginx.
- **GOTCHA — negative DNS cache:** never query the public name BEFORE the A record exists. A failed
  lookup poisons resolver negative caches for the zone SOA minimum (**1800 s**). `--install` creates
  the record FIRST; run `--check` (which queries) only after.
- **GOTCHA — h2 vs WebSocket:** the panel pushes live state over WS, and browsers speak WS over
  **HTTP/1.1** (the proxy uses `proxy_http_version 1.1` + `Upgrade`/`Connection` passthrough; the
  supervisor's live dev1 stand-up confirmed `/ws` → 101). Over HTTP/2 the `Upgrade` handshake is
  rejected 400 — so a raw `curl --http2` WS
  probe returns 400 while a real browser (which negotiates 1.1 for the WS) works. Test WS with an
  HTTP/1.1 client, not `curl` over h2. nginx 1.24 has NO `http2 on;` directive — the http2 flag rides
  on `listen 443 ssl http2;`.
- **Rejected alternative — cloudflared tunnel** (`scripts/bkshading-provision-cloudflared.sh`, kept
  for reference): the owner rejected any internet path 17.9.2026 (every request would leave the LAN
  and come back). The provisioning script + its systemd unit stay in the repo as the optional
  alternative, but the LAN HTTPS front above is the canonical path.


## Clona `f/—`: off-grid aperture from `gphoto2 --summary` (issue 1306)

The panel showed `f/—` for clona (dead slider) on BOTH online cameras. Root cause: libgphoto2's
`_get_FNumber` sets the `f-number` RADIO `Current:` ONLY when the raw value exactly matches an
enumerated choice; both lenses sit open BELOW the camera's first enumerated stop (cam1 f/4.0 vs enum
from f/4.5, cam2 f/2.0 vs enum from f/2.6), so `gphoto2 --get-config f-number` prints
`Current: (null)` → `parse_fnumber` fails → `aperture_av`/`aperture_norm` = `None`. The ONLY honest
current-aperture signal is the `gphoto2 --summary` line `F-Number(0x5007) … value: f/4 (400)`
(raw = PTP F-Number x100).

- **`--summary` folds into the EXISTING best-effort `d003` session — never a new USB session.**
  `Gphoto2Runner::get_focus_and_summary` runs `gphoto2 --get-config d003 --summary` (ONE process),
  and `split_focus_and_summary` splits the stdout at the FIRST `END` line (block = d003, remainder =
  summary). Per-read session count stays 3 (detect + core batch + this) — the issue 1229 doctrine.
  The default trait impl (test fakes) reads d003 alone with an empty summary; the real `Gphoto2Cli`
  overrides it. `RawConfigs` gained a `summary` field (empty → the pre-1306 RADIO-`Current:` path).
- **`params_and_caps`:** when `Current:` is missing/`(null)`, `aperture_av = fnumber_to_av(raw/100)`
  and `aperture_norm` = the NEAREST enumerated choice (`nearest_choice_norm`); an exact `Current:`
  still maps to its exact position. Pure parser `parse_summary_fnumber_raw` (fail-safe → `None`).
- **Junk-choice filter is in the ONE canonical list.** `parse_fnumber_labels` now also drops any
  choice below `MIN_VALID_FNUMBER` (0.5) — the gphoto2 placeholders `f/0`/`f/0.2` — so the grid used
  for read-norm, write (`plan_writes`), AND `CameraCaps.fNumberChoices` is consistent and a near-0
  step can never write `f/0`. Threshold 0.5 keeps a genuine `f/0.95` lens.
- **Consequence for the #1304 +/- step (off-grid state):** the recovered `aperture_norm` is 0, so
  `refreshStepDisabled` disables "−" (idx 0) while "+" moves to the first/next enumerated choice —
  the camera exposes no smaller f-number than its first enumerated stop over PTP. No panel change
  was needed; the existing bound-disable logic produces this by construction.
- **Tier-0:** the pure aperture selection + parsers were RED→GREEN-proven via a `rustc --test`
  replica (cam1 → `av=2·log2(4)`, norm 0, `[4.5,4.8,5.6]`; cam2 → `av=2·log2(2)`, norm 0,
  `[2.6,2.8,3.2]`; the clean-list `f/5.2` case still norm 2/3). CI runs the real proto+relay tests.


## Relay + service LOGGING is on by default, and gphoto2 shell-outs are BOUNDED (issue 1309, 15.9.2026)

The 2026-09-13/15 half-dead-cambox class lands right after bkshading-relay activity, and each time
it was undiagnosable: the relay emitted ZERO journal lines on a RUNNING unit (`journalctl -u
bkshading-relay -b` = 0 — its subscriber defaulted to info but nothing logged per command), and the
strih service logged NOTHING for a `PUT /api/params` while spamming `WARN relay unreachable` every
2 s (365 KB/run). The 15.9. escalation was "shading crashol po dvoch zdvihnutiach clony" — two
aperture SETs = two gphoto2 shell-outs. The fix makes the whole chain reconstructible + bounded:

- **The relay logs EVERY gphoto2 command at info through ONE centralised seam** `Gphoto2Cli::run`
  (`bkshading/relay/src/transport.rs`): kind (detect/read/set), param + value, the exact argv, rc,
  duration_ms, and on failure the stderr tail — one structured line per command. Startup logs the
  camera-detect result; `note_online_transition` logs one line per camera online/offline FLIP. The
  `Gphoto2Runner` TRAIT is deliberately untouched (the logging + timeout live only in the real
  `Gphoto2Cli`), so every fake-runner unit test stays green in shape. Reads stay cached (issue 1229),
  so info volume is ~3 lines per real read cycle (once per 10 s floor), never per-poll.
- **gphoto2 shell-outs are BOUNDED.** `run_with_timeout` gives every command a hard 8 s timeout
  (`GPHOTO2_TIMEOUT`) that KILLS + reaps the child (stdout/stderr drained on threads so a large
  output can't deadlock the wait) — a gphoto2 hung on a busy USB-PTP device can no longer hold the
  serialized camera lock forever. SET and READ already serialise through the one `read_cache` mutex
  (no parallel fork); on top of that a pure single-flight `SetQueue` (`bkshading-proto`) coalesces a
  burst of SETs latest-wins: the first runs, concurrent SETs fold into `pending`, the in-flight
  worker drains the coalesced latest — so "raising the aperture twice" NEVER forks a second gphoto2.
  `apply()` is unchanged (existing `apply(&req)==count` tests stay green); the HTTP handler calls the
  new `submit()`, which returns `Applied(n)` or `Coalesced` (`202 {"coalesced":true}`).
- **The service logs every `PUT /api/params`** (`forward_set`: id/params/status/latency_ms, error
  body on failure), and the per-poll `relay unreachable` WARN is DOWNGRADED to `debug`; reachability
  is now logged ONCE per transition (`monitor::reach_transitions`, mirroring `fps_alert_transitions`)
  + a 5-min heartbeat count in the pump — so a chronic-down relay is visible without the spam.
- **verify-device `(am)`** HARD-FAILs a box that RUNS the relay if the LIVE unit's `TasksMax` > 512
  (the #1309 blast-radius ceiling, already in the unit) OR an ACTIVE relay emits zero info journal
  lines this boot (an old binary predating info-by-default logging). Relay absent = `na`;
  disabled/inactive (the TEST-mode default per the lifecycle section above) skips the log assertion.
  Pure `relay_blast_radius_verdict`/`relay_tasksmax_within_ceiling`, inserted BEFORE `(q)`.
- **rig-dev-handover-check** gains a `shading` item (`rig_dev_handover_decision.py` + a read-only
  per-cambox ssh+curl probe in the orchestrator): SHADING-ON (enabled+active+online) = OK,
  SHADING-DEAD (enabled but crashed) = FORGOT (the crash class), SHADING-OFF (disabled = the dev
  default) / SHADING-NO-CAMERA / SHADING-UNREACHABLE = neutral → UNKNOWN. So "shading is off on
  cam1" is reported, never discovered, and an all-disabled dev fleet never false-forgots.

## Service redeploy on strih: the installer's start dies with the ssh session — relaunch via the keep-alive task (16.9.2026)

`scripts/bkshading-deploy-service.sh --execute` copies the exe, registers the `bkshading-service` keep-alive task and starts the service from the ssh session; Windows OpenSSH tears that process down at disconnect (the same job-object physics as OBS-over-ssh, issue 859), so 60 s later there is no process and no `:8770` listener while the script printed `OK … Listening`. Recovery = `schtasks /run /tn bkshading-service` over ssh (session-agnostic; the task launches the exe detached) → `keep-alive: bkshading service was not running - relaunched`; verify from dev1 with `curl :8770/api/version`. The relay on a cambox is unaffected (systemd unit). Followup for the script: start via the task, verify from a SECOND connection.


## Immediate control responsiveness — write-burst session + optimistic panel (issue 1337, 17.9.2026)

Owner complaint during live shading: every panel click had a ~1-3 s response and the confirmation
number lagged 2-3 s, and the clona (aperture) would not move from the first click. Root cause: the
relay ran a full `read_raw()` (3 gphoto2 spawns) BEFORE every write + one `--set-config` spawn per
param (~4 processes/click ≈ 1 s); the service `forward_set` shared the 1.5 s poll `reqwest` client
so a ~1 s write timed out mid-apply (no confirmation → the owner re-clicked); the panel dropped the
whole push while `interacting` and had no optimistic echo; and the aperture step read the off-grid
slider norm (0) instead of the real f-number. Fixed across THREE layers, each Tier-0 tested:

### Layer 1 — relay write-burst session (`bkshading/relay/src/burst.rs` + `transport.rs`)
- A persistent `gphoto2 --shell` child (`Gphoto2Shell`) is used ONLY inside a WRITE BURST: opened on
  the first set, kept warm across a burst of clicks, closed after `WRITE_SESSION_IDLE_MS` (5 s) idle.
  Inside a burst each write is planned from the f-number choices read ONCE at burst open (NO
  pre-write read), and applied through the shell — so a click moves the camera in ~150 ms.
- **It is a best-effort OPTIMISATION with a hard CLI FALLBACK.** Any shell error, or no completion
  within `WRITE_SESSION_WEDGE_MS` (3 s), KILLS the child and falls the write back to a per-invocation
  CLI `--set-config` (bounded by the existing 8 s `GPHOTO2_TIMEOUT`). So correctness NEVER depends on
  the shell — a broken/slow shell just always uses the CLI path (still pre-read-free). This is why
  re-introducing the shell here is safe even though issue 1229 REJECTED it for READS: the kill
  boundary + CLI fallback are the whole safety story, and the read path is untouched (still
  per-invocation CLI, floored — the issue-1229 quiet-bus doctrine resumes the moment the burst closes).
- **The `--shell` command grammar (`set-config key=value`) + prompt shape are UNVERIFIED against a
  live camera** — a Tier-0 lane has no gphoto2/camera. The fallback makes that acceptable; the
  supervisor's rig acceptance (below) confirms/tunes the shell speedup. The PURE pieces (the
  `burst_step` state machine, `burst_idle_expired`, the shell-line classifiers, `project_shading`,
  the FIFO `SetQueue`) ARE Tier-0 tested (rustc replicas + `tests/burst_1337.rs` incl. a fake
  `gphoto2 --shell` script).
- **Wedge watchdog:** the shell read is bounded by `WRITE_SESSION_WEDGE_MS` via a reader-thread +
  `recv_timeout` (the `wedge-watchdog-pattern.md` idea — a bounded kill boundary, never an unbounded
  block); the camera lock is never held across an unbounded wait.
- **`SetQueue` is FIFO, not latest-wins coalesce (issue 1309 → 1337):** 20 rapid clicks become 20
  in-order camera moves (the shell makes that fast), still single-flight (never a second parallel
  gphoto2). `read_state` serves cache while the shell owns the camera and does ONE authoritative read
  at burst idle-close; ALL camera access stays serialized (read-cache lock + the new burst lock, lock
  order read_cache→burst, `submit` never nests read_cache under burst → no deadlock).
- `PUT /api/params` now returns `{"applied":n,"state":{...}}` — the projected resulting state (pure
  `project_shading`) so the service can push an immediate confirmation.

### Layer 2 — service dedicated write client + immediate WS push (`aggregator.rs` + `http.rs`)
- `forward_set` uses a DEDICATED write client (1 s connect, 10 s total) — the 1.5 s poll client
  timed out mid-write. After a successful write the service pushes an IMMEDIATE per-camera
  confirmation over the WS (pure `aggregate_with_camera_update` — rebuilds ONLY the target camera's
  view, no fleet re-poll), instead of waiting up to 2 s for the pump tick. The publish handle is
  shared (`Arc<watch::Sender>`) between the pump and `set_params`.

### Layer 3 — optimistic panel + step-from-real-value (`web/app.js`)
- A click shows its new value immediately as `.pending` (accent + italic), reconciled by the next
  push. `render()` no longer drops the whole push while interacting (that hid every confirmation
  during a click sequence — the number lag); a dragged slider stays protected by the
  `document.activeElement` check in `updateBlock` (never a whole-push drop).
- **ISO + uzávierka share the aperture-style `−`/index-slider/`+` stepper (addendum #1337, owner:
  "selektory na iso ako samostatné tlačidlá je blbosť, daj to ako ostatné" + "uzávierka tiež").**
  The rows of per-value choice buttons (`renderButtonGroup`, `.btn-group`) are GONE — one control
  per parameter, no second way to set it. `ISO_STEPPER`/`SHUTTER_STEPPER` configs drive a shared
  `stepEnum(el, id, cfg, dir)`: it steps ONE enumerated choice from the camera's REAL current value
  (`dataset.isoVal`/`dataset.shutterVal`) via the SAME `stepChoice` (both `iso_choices` and
  `shutter_choices_for_fps` are ASCENDING `Vec<i64>`, so the ascending semantics apply directly),
  then PUTs the ABSOLUTE value (`{iso: v}` / `{shutter: v}`) — NOT a norm (that stays aperture-only).
  The slider is an INDEX (`0..n-1`) over `caps.isoChoices`/`caps.shutterChoices`; `updateBlock`
  positions it via `nearestIndex` (off-grid tolerant) and reconciles the optimistic `.pending` echo.
  Removing the button rebuild retired the write-only `interacting` flag (sliders were already
  `activeElement`-guarded). Pinned by `test_bkshading_webui.py` (steppers present, no `btn-group`,
  `stepEnum` uses `stepChoice`, absolute PUT) + the panel Playwright E2E (ISO 400→800, shutter
  50→60 off-grid onto the grid).
- Aperture stepping uses a JS `stepChoice` MIRROR of the proto `mapping::step_choice`, stepping from
  the camera's REAL current f-number (`dataset.apertureFnum` from `apertureAv`) — an off-grid lens
  (cam1 f/4.0 below the first stop 4.5; cam3 f/3.36) moves ONTO the grid on the first tap instead of
  the pre-fix idx-0 nearest-snap +1. `step_choice` is the pure Rust spec, JS-mirror-pinned by a node
  parity check + `test_bkshading_webui.py`.

### Standing rules that DID NOT change
- **One tap = one PUT, no auto-repeat on hold** (the issue-1229 USB-PTP shared-bus doctrine).
- **The relay is stopped+disabled in `rig-mode.sh test`, started in `event`** (issue 1311) — the
  burst only runs while the relay is live (EVENT mode), so development runs pay no relay bus traffic.
- **NEVER deploy OR restart a cambox relay during production** (the rig-busy-gated deploy + the
  rig-idle-only restart, above). Deploy ORDER for this change: the SERVICE lands on strih anytime
  (owner 17.9.: shading during a live show is allowed), the RELAY lands on the camboxes only OUTSIDE
  production and its restart is a separate rig-idle supervisor step.

### Rig acceptance (supervisor, relay half OUTSIDE production, EVENT mode)
Deploy service → strih (anytime) + relay → camboxes (outside production only), start the relay, then:
set `duration_ms` ≤ 150 ms per set in a burst (relay log); click → confirmed number ≤ 1 s; 20 rapid
clicks = 20 moves; cam1 grabber `Streaming: sent/captured` flat during a 60 s burst (issue-1229
metric); no `gphoto2` process 5 s after the burst (`pgrep gphoto2` empty). The `--shell` speedup
itself is UNVERIFIED in code lanes — if the shell path never completes it silently uses the CLI
(still pre-read-free, ~1 set/spawn), which the rig step confirms/tunes.
