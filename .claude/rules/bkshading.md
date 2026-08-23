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
   1.84) — use `is_none_or`/`is_some_and` instead, issue 1157). These clippy traps bite HARDEST
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
  as an INPUT (not `cfg!`) so every OS's candidate set is tested on the Linux runner. `NdiLib::load()`
  now consumes `ndi_search_candidates(current_ndi_os(), |k| std::env::var(k).ok())` (env dirs →
  per-OS well-known dirs → bare-name dynamic-linker fallback) instead of a hard-coded Linux list.
  Tests live in `service/tests/preview.rs` (run in the default-feature `bkshading` CI test).
- **CI compiles + verifies the feature on BOTH ship targets:** the `bkshading` job gained
  `cargo test -p bkshading --features ndi` + a `--features ndi` bins build; `bkshading-windows`
  gained `cargo check -p bkshading --features ndi` (the strih deploy ships this exact binary — the
  M2 lane only ever clippy-compiled the feature for Linux).
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
  Task 4
  (make `--features ndi` the default strih build, or keep opt-in) is the owner's call AFTER that
  live verify — left opt-in for now.


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
