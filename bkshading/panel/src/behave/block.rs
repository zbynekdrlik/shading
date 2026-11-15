//! A camera block's lines outside the steppers (ported from the JS panel's `updateBlock`): the
//! online line, the handheld registration line (issue 808) and the fps-vs-grab line with its
//! explicit align button (issues 809, 1402).

use bkshading_proto::layout::camera_number;
use bkshading_proto::wire::{CameraView, FpsSync, RegistrationView};

/// `cam7` -> `7`; an id without a number shows the id.
pub fn number_text(id: &str) -> String {
    camera_number(id).map_or_else(|| id.to_string(), |n| n.to_string())
}

/// Whether the camera answers and can be shaded.
pub fn is_online(cam: &CameraView) -> bool {
    cam.reachable && cam.state.as_ref().is_some_and(|s| s.online)
}

/// The block's online line: `relay offline`, `online` or `kamera offline`.
pub fn online_text(cam: &CameraView) -> &'static str {
    if !cam.reachable {
        "relay offline"
    } else if is_online(cam) {
        "online"
    } else {
        "kamera offline"
    }
}

/// "4 s", "3 min", "2 h 5 min".
pub fn format_duration(s: u64) -> String {
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else {
        let (h, m) = (s / 3600, (s % 3600) / 60);
        if m > 0 {
            format!("{h} h {m} min")
        } else {
            format!("{h} h")
        }
    }
}

/// The registration line of a self-registering handheld (issue 808).
pub fn registration_text(reg: &RegistrationView) -> String {
    if reg.fresh {
        format!(
            "{} · {} · ohlásený pred {} · beží {}",
            reg.relay_id,
            reg.address,
            format_duration(reg.age_s),
            format_duration(reg.announcing_s)
        )
    } else {
        format!(
            "{} sa neohlásil {} (naposledy {})",
            reg.relay_id,
            format_duration(reg.age_s),
            reg.address
        )
    }
}

/// The fps-vs-grab line (issue 809): shown only with a grab rate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpsSyncView {
    pub grab: String,
    /// The static grab_fps config disagrees with the box's live capture rate.
    pub desync: Option<String>,
    /// The camera runs another rate than the grab.
    pub warn: Option<String>,
    /// The explicit align button's text, offered only when the camera would take the write
    /// (`fpsAlignSettable`, issue 1402) and there is a mismatch.
    pub align: Option<(String, i64)>,
}

/// The fps-vs-grab line of `cam`, or none without a grab rate.
pub fn fps_sync(cam: &CameraView) -> Option<FpsSyncView> {
    let grab = cam.grab_fps?;
    let cam_fps = cam
        .state
        .as_ref()
        .and_then(|s| s.params.fps100)
        .map(|f| f as f64 / 100.0);
    let mismatch = cam.fps_sync == FpsSync::Mismatch;
    Some(FpsSyncView {
        grab: format!("grab {grab}"),
        desync: cam
            .grab_fps_desync
            .then(|| format!("⚠ config ≠ box capture ({grab})")),
        warn: mismatch.then(|| {
            format!(
                "⚠ kamera {} ≠ grab {grab}",
                cam_fps.map_or("?".into(), |f| format!("{f:.2}"))
            )
        }),
        align: (mismatch && is_online(cam) && cam.fps_align_settable)
            .then(|| (format!("Zosúladiť s grab ({grab})"), grab)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behave::steppers::tests::cam;

    #[test]
    fn numbers_and_online_lines() {
        assert_eq!(number_text("cam7"), "7");
        assert_eq!(number_text("spare"), "spare");
        let mut c = cam();
        assert_eq!(online_text(&c), "online");
        c.state.as_mut().unwrap().online = false;
        assert_eq!(online_text(&c), "kamera offline");
        c.reachable = false;
        assert_eq!(online_text(&c), "relay offline");
    }

    #[test]
    fn durations_and_registration() {
        assert_eq!(format_duration(4), "4 s");
        assert_eq!(format_duration(185), "3 min");
        assert_eq!(format_duration(7500), "2 h 5 min");
        assert_eq!(format_duration(7200), "2 h");
        let mut reg = RegistrationView {
            relay_id: "handheld-1".into(),
            address: "10.0.0.9:8771".into(),
            age_s: 4,
            announcing_s: 65,
            fresh: true,
        };
        assert_eq!(
            registration_text(&reg),
            "handheld-1 · 10.0.0.9:8771 · ohlásený pred 4 s · beží 1 min"
        );
        reg.fresh = false;
        reg.age_s = 120;
        assert_eq!(
            registration_text(&reg),
            "handheld-1 sa neohlásil 2 min (naposledy 10.0.0.9:8771)"
        );
    }

    #[test]
    fn the_align_button_needs_a_mismatch_and_a_settable_grab() {
        let mut c = cam();
        assert_eq!(fps_sync(&c), None, "no grab, no line");
        c.grab_fps = Some(60);
        c.fps_sync = FpsSync::Synced;
        let v = fps_sync(&c).unwrap();
        assert_eq!(v.grab, "grab 60");
        assert_eq!((v.warn, v.align), (None, None));
        c.state.as_mut().unwrap().params.fps100 = Some(5000);
        c.fps_sync = FpsSync::Mismatch;
        let v = fps_sync(&c).unwrap();
        assert_eq!(v.warn.as_deref(), Some("⚠ kamera 50.00 ≠ grab 60"));
        assert_eq!(v.align, None, "6000 is not a choice");
        c.fps_align_settable = true;
        assert_eq!(
            fps_sync(&c).unwrap().align,
            Some(("Zosúladiť s grab (60)".to_string(), 60))
        );
        c.grab_fps_desync = true;
        assert_eq!(
            fps_sync(&c).unwrap().desync.as_deref(),
            Some("⚠ config ≠ box capture (60)")
        );
    }
}
