//! Process-shared, load-once runtime keeper (issue 808 — reconnect-safe NDI lifecycle).
//!
//! The NDI SDK's initialize/destroy pair is APPLICATION-lifetime, not per-connection: the
//! destroy is process-GLOBAL, so a per-source runtime means one camera's routine reconnect
//! (the worker drops its source before every backoff) tears the SDK down under every other
//! live receiver. This module holds the lifecycle DECISION as a pure, default-feature seam
//! (the `ndi_paths`/`convert` split canon): the first successful acquire loads and caches the
//! runtime for the WHOLE process lifetime; every later acquire returns the same shared handle;
//! a FAILED load is never cached (the caller's backoff retry loads again).
//!
//! The runtime is deliberately never released mid-flight — a resident service's preview
//! workers retry forever, so there is no idle state to release for, and a destroy-on-last-drop
//! pool would re-create the global destroy→init churn on every single-camera reconnect (the
//! worker holds zero sources during its backoff sleep).

use std::sync::{Arc, Mutex};

/// A process-wide, lazily-loaded, keep-alive slot for a shared runtime `T`.
///
/// `const`-constructible so it can live in a `static`.
pub struct SharedRuntime<T> {
    slot: Mutex<Option<Arc<T>>>,
}

impl<T> SharedRuntime<T> {
    /// An empty slot (nothing loaded yet).
    pub const fn new() -> Self {
        Self {
            slot: Mutex::new(None),
        }
    }

    /// Return the shared instance, loading it via `loader` exactly once on first success.
    ///
    /// - The first successful load is cached for the process lifetime; every later call
    ///   returns a clone of the SAME `Arc` (pointer-identical) without running `loader`
    ///   again — even after every caller has dropped its handle (keep-alive).
    /// - A failed load caches NOTHING: the error is returned and the next call retries.
    pub fn acquire<E>(&self, loader: impl FnOnce() -> Result<Arc<T>, E>) -> Result<Arc<T>, E> {
        // A poisoned lock (a loader panicked mid-acquire) must NOT permanently kill every
        // preview worker's next reconnect: the guarded data is valid by invariant (the slot
        // is only ever written AFTER a fully successful load), so recover the guard and let
        // this acquire retry — a one-off panic self-heals instead of poisoning forever.
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = slot.as_ref() {
            return Ok(Arc::clone(existing));
        }
        let loaded = loader()?;
        *slot = Some(Arc::clone(&loaded));
        Ok(loaded)
    }
}

impl<T> Default for SharedRuntime<T> {
    fn default() -> Self {
        Self::new()
    }
}
