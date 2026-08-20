---
paths:
  - "bkshading/**"
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
   `slice::as_chunks::<N>().0`; the main crate was fixed in dev `052da4c5d`).

## M1 done / M2+ deferred
Done: the 3 crates + workspace/CI wiring, the 4+4 responsive web panel skeleton (version in the DOM,
version-on-dashboard), config-driven camera list, relay read+write logic unit-tested with a fake
runner; ONE workspace-inherited crate version across all four crates (#1154). M2 DONE: live NDI preview (below). Deferred (M2+): WS push of the aggregate;
cloudflare password-protected remote (NOT tailscale — owner decision); SBC/handheld image; installing
`gphoto2` on the camboxes (a RUNTIME dep — NOT present on cam1 yet) + provisioning hooks; automating
the E2E camera pre-run shutter checklist.


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
