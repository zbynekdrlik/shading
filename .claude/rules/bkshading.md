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
  dep → clean ARM cross-build for the Pi Zero 2 W handheld relay.

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


## SBC / handheld provisioning (issue 808 — the last milestone)
A handheld camera runs the SAME `bkshading-relay` on a mini SBC (a Pi Zero 2 W): camera USB → Pi,
Pi on WiFi. The service already understands it (`Transport::SbcRelay`, `handheld-1` /
`transport="sbc-relay"` in `bkshading.example.toml`, a params-only block — no NDI preview). The box
side is `scripts/bkshading-provision-sbc.sh` (+ pure lib `scripts/lib/bkshading-sbc-runtime.sh`),
mirroring the relay/cloudflared provisioning canon but with two deliberate deltas + one gotcha:
- **The SBC REUSES `systemd/bkshading-relay.service` UNCHANGED** (owner: "the SAME relay component")
  and writes **NO `CAMERA_BOX_CAPTURE_FPS` env** — an SBC has no camera-box appliance to derive from
  and a handheld has no grab comparison, so the unit's `EnvironmentFile=-` degrades gracefully
  (relay → `capture_fps=None` → service static config). Do NOT reuse `bkshading-provision-relay.sh`
  (its whole job is deriving that env from `camera-box.service.d` drop-ins, which an SBC lacks).
- **Deploy uses `bkshading-deploy-relay.sh --arch arm64 --no-remount`.** `--arch arm64` fetches the
  `bkshading-relay-linux-arm64` artifact; `--no-remount` skips the read-only-root swap (a cambox
  appliance has a ro root; a **stock Raspberry Pi OS root is read-WRITE** — remounting it ro is
  wrong). The default (no flags) is still amd64 + ro-root remount (cambox), byte-unchanged.
- **CROSS-BUILD GOTCHA — only the RELAY cross-builds to aarch64 trivially; the SERVICE does NOT.**
  The relay is pure Rust (axum/tokio/serde/clap; **no reqwest/rustls/ring, no libndi** on the relay
  side), so the CI `bkshading` job cross-compiles it for `aarch64-unknown-linux-gnu` with just
  `rustup target add` + the `gcc-aarch64-linux-gnu` linker + `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER`.
  Target = aarch64 (NOT armhf): the Pi Zero 2 W is ARMv8 and Pi OS 64-bit is its default; a 32-bit
  `armv7-unknown-linux-gnueabihf` build is one extra matrix entry only if a legacy handheld needs it.
  **Do NOT naively add a service ARM cross-build** — the service pulls `ring`/`rustls` (reqwest) +
  the libndi FFI, which do NOT cross-link with a bare gcc linker; the service is Windows/amd64 only
  (it runs on the strih PC), so there is deliberately no service ARM artifact.
- **`--check` verifies the deployed relay binary is actually AArch64** (an ELF `e_machine` read via
  `od` — offset 18, 2 bytes LE, AArch64=183 / x86-64=62; pure helpers in `bkshading-sbc-runtime.sh`,
  Tier-0 testable with a 20-byte fake-ELF fixture) so a mis-deployed amd64 binary is caught here,
  not at reboot with an opaque `Exec format error`.
- The physical bring-up (flash Pi OS with `rpi-imager`, headless WiFi, then deploy + `--install` +
  reboot) is the owner's/supervisor's rig step. Transports stay USB-PTP (gphoto2/libusb) / USB-Eth
  REST — NEVER Bluetooth; a gphoto2 camera is a USB device, not a network link, so the netplan
  `enx*` CDC-NCM trap (#1155) does not touch the handheld.


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
  colliding with the grabber's isochronous stream on the shared xHCI bus. The owner explicitly kept
  1229 OPEN (`needs-owner-action`) pending a PHYSICAL step — moving the BMPCC's USB cable on cam1 to
  a USB2 port (PTP only needs 480 Mb/s, isolating it from the grabber's SuperSpeed bandwidth domain)
  — plus one more clean watch after that. **1228 unblocks only once 1229 actually closes** (or the
  owner explicitly says otherwise) — do NOT add `Restart=on-failure` just because the floor merged;
  re-check `gh issue view 1229` state/labels before touching the unit.
- **Complementary idle lever (owner's "poll len on-demand keď je panel otvorený" half, NOT done
  here):** the service could poll relays only while a WS/panel client is connected, for TRUE-zero
  idle. It lives in a different crate (service, ships to Windows/strih) with WS-lifecycle
  subtleties (stale-on-reconnect, immediate-refresh) → a separate focused PR + rig-verify. The
  floor already bounds the worst case (1 read/floor even with a panel open — the case that matters
  during live shading), so it is deferred, not dropped; file it with evidence if live-verify shows
  the residual idle burst still disturbs capture.


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
auto-recovered by `Restart=on-failure`, by design** — systemd suppresses the restart when a stop was
requested by the service manager itself (an administrative/clean stop), regardless of the
`Restart=` policy. So even once issue 1228 lands `Restart=on-failure` on
`systemd/bkshading-relay.service`, it will **only** protect against a genuine unexpected crash
(panic, segfault, OOM-kill) — it will NOT bring back a relay that was deliberately silenced as an
interim mitigation (correct behavior: an operator's deliberate stop should stay off until they
undo it). **Any interim "stop this on box X while we investigate" mitigation needs its OWN explicit
tracking** (a ticket comment naming which boxes were stopped + a reminder to restore them) — the
harness-managed pause (`bkshading-e2e-pause.sh`, above) only covers stops the E2E harness ITSELF
performs; it has no visibility into an ad-hoc manual stop done directly on the rig.

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
