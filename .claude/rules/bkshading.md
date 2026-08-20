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
  params-only block** (no preview). NDI preview itself is M2 (presenter tech) — M1 is a placeholder.
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
   and clippy `-D warnings` traps (a never-read struct field fails `dead_code`).

## M1 done / M2+ deferred
Done: the 3 crates + workspace/CI wiring, the 4+4 responsive web panel skeleton (version in the DOM,
version-on-dashboard), config-driven camera list, relay read+write logic unit-tested with a fake
runner. Deferred (M2+): NDI low-quality preview via presenter tech; WS push of the aggregate;
cloudflare password-protected remote (NOT tailscale — owner decision); SBC/handheld image; installing
`gphoto2` on the camboxes (a RUNTIME dep — NOT present on cam1 yet) + provisioning hooks; automating
the E2E camera pre-run shutter checklist; unifying crate versions with the bump discipline (#1154).
