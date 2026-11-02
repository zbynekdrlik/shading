//! Cross-platform NDI runtime discovery for the bkshading preview receiver (issue 1157).
//!
//! PURE + compiled on DEFAULT features (NOT behind `feature = "ndi"`), so the ordered
//! candidate-path DECISION is unit-tested on CI without libndi — the bkshading Tier-0 doctrine
//! (pure logic lives un-gated + testable; only the `unsafe` FFI glue in `ndi_source.rs` is
//! feature-gated + CI-only). The feature-gated `NdiLib::load_uncached()` (reached only through
//! the process-shared `NdiLib::shared()` slot) consumes [`ndi_search_candidates`].
//!
//! WHY this exists (issue 1157): `ndi_source.rs` copied the appliance `src/ndi.rs` `load()`
//! verbatim, and that is deliberately Linux-only (the camboxes are Ubuntu). The bkshading
//! SERVICE ships to the strih PC — Windows first — where the NDI runtime is
//! `Processing.NDI.Lib.x64.dll` under `C:\Program Files\NDI\NDI 6 Tools\Runtime\` (documented
//! in-repo at `scripts/bundle-state-server.py::DEFAULT_NDI_RUNTIME_DLL`). The Linux-only search
//! never tries that, so the real preview could never load on its own ship target.

use std::path::PathBuf;

/// Target OS families whose NDI runtime layout differs. Taken as an INPUT (never read from
/// `cfg!` inside the pure logic) so every OS's candidate set is unit-testable on one CI runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NdiOs {
    Linux,
    Windows,
    Macos,
}

/// Env-var names, highest priority first, whose values are NDI runtime DIRECTORIES. The NDI
/// redistributable sets `NDI_RUNTIME_DIR_V6`; the appliance systemd unit sets it too
/// (`scripts/setup-device.sh`). Same list the appliance `src/ndi.rs` reads.
pub const NDI_ENV_DIRS: &[&str] = &[
    "NDI_RUNTIME_DIR_V6",
    "NDI_RUNTIME_DIR_V5",
    "NDI_RUNTIME_DIR",
];

/// Library file names to try for `os`, most-preferred first.
pub fn ndi_lib_names(os: NdiOs) -> &'static [&'static str] {
    match os {
        // Mirrors the appliance `src/ndi.rs` (grounded).
        NdiOs::Linux => &["libndi.so.6", "libndi.so.5", "libndi.so"],
        // Windows: the NDI Tools / redist DLL (x64 grounded from bundle-state-server.py; x86 the
        // standard 32-bit sibling name).
        NdiOs::Windows => &["Processing.NDI.Lib.x64.dll", "Processing.NDI.Lib.x86.dll"],
        // macOS (not a current ship target — bare-name/dyld fallback only, no invented dir).
        NdiOs::Macos => &["libndi.dylib"],
    }
}

/// Well-known install directories to try for `os`, IN ADDITION to the [`NDI_ENV_DIRS`] values.
/// Only repo-grounded / conventional locations — never an invented path.
pub fn ndi_wellknown_dirs(os: NdiOs) -> &'static [&'static str] {
    match os {
        // Grounded: the appliance's `/usr/lib/ndi` (setup-device.sh) + the two other dirs the
        // appliance `src/ndi.rs` already probes.
        NdiOs::Linux => &["/usr/lib/ndi", "/usr/local/lib/ndi", "/opt/ndi/lib"],
        // Grounded: the strih NDI Tools runtime dir (bundle-state-server.py / drift-guard.md).
        // The redist install is covered by NDI_RUNTIME_DIR_V6 (env) + the PATH fallback.
        NdiOs::Windows => &[r"C:\Program Files\NDI\NDI 6 Tools\Runtime"],
        // No hardcoded macOS dir (env + dyld fallback only).
        NdiOs::Macos => &[],
    }
}

/// The current build target's OS family.
pub fn current_ndi_os() -> NdiOs {
    #[cfg(target_os = "windows")]
    {
        NdiOs::Windows
    }
    #[cfg(target_os = "macos")]
    {
        NdiOs::Macos
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        NdiOs::Linux
    }
}

/// Ordered candidate library paths for `os`. `env` resolves an env-var name to its value
/// (`|k| std::env::var(k).ok()` at runtime; a fixed map in tests).
///
/// Order (first match wins in the `NdiLib::load_uncached` loader): each [`NDI_ENV_DIRS`] dir
/// (priority order) ×
/// each name, then each well-known dir × each name, then each bare name as the dynamic-linker
/// fallback (LD_LIBRARY_PATH on Linux, PATH on Windows, dyld search on macOS).
pub fn ndi_search_candidates<F>(os: NdiOs, env: F) -> Vec<PathBuf>
where
    F: Fn(&str) -> Option<String>,
{
    let names = ndi_lib_names(os);
    let mut out = Vec::new();
    for &var in NDI_ENV_DIRS {
        if let Some(dir) = env(var) {
            for &name in names {
                out.push(PathBuf::from(&dir).join(name));
            }
        }
    }
    for &dir in ndi_wellknown_dirs(os) {
        for &name in names {
            out.push(PathBuf::from(dir).join(name));
        }
    }
    for &name in names {
        out.push(PathBuf::from(name));
    }
    out
}
