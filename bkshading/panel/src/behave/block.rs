//! A camera block's lines outside the steppers (ported from the JS panel's `updateBlock`): the
//! online line, the handheld registration line (issue 808; its one-line short form, shading#24)
//! and the fps-vs-grab line with its explicit align button (issues 809, 1402).

use bkshading_proto::layout::camera_number;
use bkshading_proto::wire::{CameraView, FpsSync, RegistrationView, Tally};

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

/// The registration line's age, short and technical (shading#24): `2s`, `10m`, `1h08m`.
pub fn short_age(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        uptime_text(s)
    }
}

/// How long a relay has announced, always hours and minutes (shading#24): `0h52m`, `3h12m`.
pub fn uptime_text(s: u64) -> String {
    format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
}

/// The legend the registration line's tooltip carries after the full text.
pub const REGISTRATION_LEGEND: &str = "↻ = posledné ohlásenie relaya, ▲ = ako dlho relay beží";

/// The one-line registration of a handheld (shading#24, Marcel): `relay · address · ↻ 2s · ▲
/// 1h08m`; a relay that stopped announcing: `relay · address · ↻ 10m ⚠`. The full sentence
/// ([`registration_text`]) is the tooltip.
pub fn registration_short(reg: &RegistrationView) -> String {
    if reg.fresh {
        format!(
            "{} · {} · ↻ {} · ▲ {}",
            reg.relay_id,
            reg.address,
            short_age(reg.age_s),
            uptime_text(reg.announcing_s)
        )
    } else {
        format!(
            "{} · {} · ↻ {} ⚠",
            reg.relay_id,
            reg.address,
            short_age(reg.age_s)
        )
    }
}

/// The registration line's tooltip: the full sentence and the legend.
pub fn registration_title(reg: &RegistrationView) -> String {
    format!("{} ({REGISTRATION_LEGEND})", registration_text(reg))
}

/// The card's status line, always one line of text (shading#28): a registered relay's short form
/// ([`registration_short`]); else the address the service dials + `bez ohlásenia` (a relay that
/// never announced, or a fixed address); else `—`. An offline card keeps its line, so its controls
/// sit where an online card's do.
pub fn status_line(cam: &CameraView) -> String {
    if let Some(reg) = &cam.registration {
        return registration_short(reg);
    }
    match cam.address.as_deref().filter(|a| !a.is_empty()) {
        Some(addr) => format!("{addr} · bez ohlásenia"),
        None => "—".to_string(),
    }
}

/// The status line's tooltip: the registration's sentence, else the line itself.
pub fn status_title(cam: &CameraView) -> String {
    cam.registration
        .as_ref()
        .map_or_else(|| status_line(cam), registration_title)
}

/// The card's tally (shading#25, mockup theme-v4d): the class on the card (`pgm` = red border +
/// tint, `pvw` = green border; the 2 px border is always there, so nothing moves) and the badge
/// text in the head. `None` = no tally.
pub fn tally_badge(cam: &CameraView) -> Option<(&'static str, &'static str)> {
    match cam.tally? {
        Tally::Pgm => Some(("pgm", "PGM")),
        Tally::Pvw => Some(("pvw", "PVW")),
    }
}

/// Whether the card carries `tally` (the `.pgm` / `.pvw` card class).
pub fn has_tally(cam: &CameraView, tally: Tally) -> bool {
    cam.tally == Some(tally)
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
    fn the_registration_line_is_short_and_technical() {
        assert_eq!(short_age(2), "2s");
        assert_eq!(short_age(59), "59s");
        assert_eq!(short_age(600), "10m");
        assert_eq!(short_age(4080), "1h08m");
        assert_eq!(uptime_text(3120), "0h52m");
        assert_eq!(uptime_text(11520), "3h12m");
        let mut reg = RegistrationView {
            relay_id: "cam6".into(),
            address: "10.77.9.66:8771".into(),
            age_s: 2,
            announcing_s: 4080,
            fresh: true,
        };
        assert_eq!(
            registration_short(&reg),
            "cam6 · 10.77.9.66:8771 · ↻ 2s · ▲ 1h08m"
        );
        assert_eq!(
            registration_title(&reg),
            "cam6 · 10.77.9.66:8771 · ohlásený pred 2 s · beží 1 h 8 min \
             (↻ = posledné ohlásenie relaya, ▲ = ako dlho relay beží)"
        );
        reg.fresh = false;
        reg.age_s = 600;
        assert_eq!(registration_short(&reg), "cam6 · 10.77.9.66:8771 · ↻ 10m ⚠");
    }

    #[test]
    fn the_tally_badge_follows_the_views_tally_25() {
        let mut c = cam();
        assert_eq!(tally_badge(&c), None);
        assert!(!has_tally(&c, Tally::Pgm) && !has_tally(&c, Tally::Pvw));
        c.tally = Some(Tally::Pgm);
        assert_eq!(tally_badge(&c), Some(("pgm", "PGM")));
        assert!(has_tally(&c, Tally::Pgm) && !has_tally(&c, Tally::Pvw));
        c.tally = Some(Tally::Pvw);
        assert_eq!(tally_badge(&c), Some(("pvw", "PVW")));
        assert!(has_tally(&c, Tally::Pvw) && !has_tally(&c, Tally::Pgm));
    }

    #[test]
    fn the_status_line_always_has_one_line_of_text_28() {
        let mut c = cam();
        c.registration = Some(RegistrationView {
            relay_id: "cam6".into(),
            address: "10.77.9.66:8771".into(),
            age_s: 2,
            announcing_s: 4080,
            fresh: true,
        });
        assert_eq!(status_line(&c), "cam6 · 10.77.9.66:8771 · ↻ 2s · ▲ 1h08m");
        c.registration.as_mut().unwrap().fresh = false;
        c.registration.as_mut().unwrap().age_s = 600;
        assert_eq!(status_line(&c), "cam6 · 10.77.9.66:8771 · ↻ 10m ⚠");
        c.registration = None;
        c.address = Some("10.77.8.64:8771".into());
        assert_eq!(status_line(&c), "10.77.8.64:8771 · bez ohlásenia");
        c.address = None;
        assert_eq!(status_line(&c), "—");
        c.address = Some(String::new());
        assert_eq!(status_line(&c), "—", "an empty address is nothing known");
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
