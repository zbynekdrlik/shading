//! Whether the panel can control a camera, and why not (shading#14).
//!
//! Pure: the service fills [`crate::wire::CameraView::control`] from it, and the tests pin each
//! case without a relay. The panel owns the Slovak text for each [`ControlStatus`].

use crate::wire::{ControlStatus, OfflineReason, RelayState};

/// The first relay version that drives the camera over the Blackmagic REST API (shading#6,
/// v0.2.0). An older relay (gphoto2/PTP) that reports the camera offline is "too old".
pub const MIN_REST_RELAY_VERSION: (u64, u64, u64) = (0, 2, 0);

/// `"0.2.5"`, `"0.1.2"`, `"1.7.0-dev.e2e"` -> `(major, minor, patch)`; the pre-release or build
/// suffix after the patch number is ignored. `None` for anything else.
pub fn parse_relay_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let rest = parts.next()?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let patch = digits.parse().ok()?;
    Some((major, minor, patch))
}

/// The relay is older than [`MIN_REST_RELAY_VERSION`]. An unreadable version is not judged old
/// (never a false "update the relay").
pub fn relay_too_old(version: &str) -> bool {
    parse_relay_version(version).is_some_and(|v| v < MIN_REST_RELAY_VERSION)
}

/// The block's control status from what the service got from the relay this cycle: `None` = the
/// relay did not answer.
pub fn control_status(state: Option<&RelayState>) -> ControlStatus {
    let Some(st) = state else {
        return ControlStatus::RelayUnreachable;
    };
    if st.online {
        return ControlStatus::Ok;
    }
    if relay_too_old(&st.version) {
        return ControlStatus::RelayTooOld;
    }
    match st.offline_reason {
        Some(OfflineReason::NoCameraLink) => ControlStatus::NoCameraLink,
        Some(OfflineReason::CameraNotAnswering) => ControlStatus::CameraNotAnswering,
        Some(OfflineReason::RestOff) => ControlStatus::RestOff,
        Some(OfflineReason::Unknown) | None => ControlStatus::CameraOffline,
    }
}
