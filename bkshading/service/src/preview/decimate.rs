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
        match self.last_emit_ms {
            Some(prev) if now_ms.saturating_sub(prev) < self.min_interval_ms => false,
            _ => {
                self.last_emit_ms = Some(now_ms);
                true
            }
        }
    }

    pub fn min_interval_ms(&self) -> u64 {
        self.min_interval_ms
    }
}
