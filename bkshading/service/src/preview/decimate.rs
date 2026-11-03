//! Time-based frame decimation.
//!
//! An NDI source arrives at ~25–60 fps; a shading preview only needs ~2–5 fps (colour and
//! exposure, not motion). This thins the stream to a target rate. Pure: it takes the frame's
//! arrival timestamp in milliseconds — no clock of its own — so the whole decision is
//! deterministically unit-testable.

/// Drops frames so at most `target_fps` are emitted per second.
#[derive(Debug, Clone)]
pub struct Decimator {
    min_interval_ms: u64,
    last_emit_ms: Option<u64>,
}

impl Decimator {
    /// `target_fps` <= 0 is treated as 1 fps (a safe floor — never divide by zero, never
    /// emit every frame by accident).
    pub fn new(target_fps: f64) -> Self {
        let fps = if target_fps.is_finite() && target_fps > 0.0 {
            target_fps
        } else {
            1.0
        };
        // Round the interval; clamp to >= 1 ms so an absurd fps can never make it 0.
        let interval = (1000.0 / fps).round() as u64;
        Self {
            min_interval_ms: interval.max(1),
            last_emit_ms: None,
        }
    }

    /// Should a frame arriving at `now_ms` be emitted? The first frame always is; afterwards
    /// only once at least `min_interval_ms` has passed since the last emitted frame. Clocks
    /// are assumed monotonic; a backwards `now_ms` (`< last`) is treated as "not enough time"
    /// (dropped) rather than emitting a burst.
    pub fn should_emit(&mut self, now_ms: u64) -> bool {
        if !self.is_due(now_ms) {
            return false;
        }
        self.mark_emitted(now_ms);
        true
    }

    /// Record that the frame that arrived at `now_ms` was emitted. The worker calls it after an
    /// encode attempt, so a kept frame the source could not build does not spend the slot
    /// (shading#3).
    pub fn mark_emitted(&mut self, now_ms: u64) {
        self.last_emit_ms = Some(now_ms);
    }

    /// Forget the last emit: the next frame is due at once. For a new clock (a rebuilt source
    /// starts its own at 0), where an old stamp would block every frame until the new clock
    /// passed it (shading#3).
    pub fn reset(&mut self) {
        self.last_emit_ms = None;
    }

    /// Would a frame arriving at `now_ms` be emitted? Same rule as [`Self::should_emit`], but it
    /// spends nothing: the worker asks it when a frame arrives, and spends the slot with
    /// [`Self::mark_emitted`] once it tried to encode the frame (shading#3).
    pub fn is_due(&self, now_ms: u64) -> bool {
        match self.last_emit_ms {
            Some(prev) => now_ms.saturating_sub(prev) >= self.min_interval_ms,
            None => true,
        }
    }

    pub fn min_interval_ms(&self) -> u64 {
        self.min_interval_ms
    }
}
