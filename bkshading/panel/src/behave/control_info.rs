//! Why a camera cannot be controlled, and what the operator should switch on (shading#14): the
//! block's warning badge and the popup, in plain Slovak. The wire carries only the status
//! (`CameraView.control`); the texts live here only (ported verbatim from the JS panel).

use bkshading_proto::wire::{ControlStatus, Transport};

/// One status's texts. `cause` may hold `{version}` (the relay's version).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub badge: &'static str,
    pub title: &'static str,
    pub cause: &'static str,
    pub step: &'static str,
}

/// Every status that shows a warning (all but `Ok`).
pub const ALL_NOT_OK: [ControlStatus; 6] = [
    ControlStatus::RelayUnreachable,
    ControlStatus::RelayTooOld,
    ControlStatus::NoCameraLink,
    ControlStatus::CameraNotAnswering,
    ControlStatus::RestOff,
    ControlStatus::CameraOffline,
];

/// How the rare "remote control not allowed" case is switched on (owner 27.11.2026, shading#14):
/// said the owner's way, never "Web media manager" (nobody understands it).
const PERMISSION_STEP: &str = "Povolenie riadenia sa zapína raz, cez notebook pripojený USB-C káblom do kamery (program Blackmagic Camera Setup). Býva to pri novej kamere; povedz to technikovi.";
const NOT_ANSWERING_STEP: &str = "Počkaj chvíľu, kým sa kamera zapne. Ak to trvá dlhšie ako minútu, vypni a zapni kameru. Ak nepomôže ani to, kamera asi nemá povolené riadenie: zapína sa raz, cez notebook pripojený USB-C káblom do kamery.";

const ORANGE_OFFLINE: Info = Info {
    badge: "⚠ Orange nie je online",
    title: "Orange pri kamere nie je online",
    cause: "Panel sa nespojí s Orange (malý počítač na kamere, cez ktorý panel kameru ovláda). Orange je vypnutý, nie je v sieti, alebo na ňom nebeží relay.",
    step: "Zapni Orange pri kamere a skontroluj, či je v sieti. Ak je zapnutý, reštartuj ho.",
};
const ORANGE_NO_CAMERA: Info = Info {
    badge: "⚠ Orange nemá USB-C do kamery",
    title: "Orange nevidí kameru",
    cause: "Orange beží, ale kameru nevidí: USB-C kábel medzi Orange a kamerou nie je zapojený, alebo je kamera vypnutá.",
    step: "Zapni kameru a zapoj USB-C kábel medzi Orange a kamerou (aj znova).",
};
const CAMBOX_OFFLINE: Info = Info {
    badge: "⚠ Cambox nie je online",
    title: "Cambox pri kamere nie je online",
    cause: "Panel sa nespojí s camboxom (počítač pri kamere, cez ktorý panel kameru ovláda). Cambox je vypnutý, nie je v sieti, alebo na ňom nebeží relay.",
    step: "Skontroluj, či je cambox zapnutý a pripojený do siete. Ak je zapnutý, reštartuj ho.",
};
const CAMBOX_NO_CAMERA: Info = Info {
    badge: "⚠ Kamera nie je pripojená do camboxu",
    title: "Cambox nevidí kameru",
    cause: "Cambox beží, ale kameru nevidí: USB-C kábel medzi kamerou a camboxom nie je zapojený, alebo je kamera vypnutá.",
    step: "Zapni kameru a zapoj USB-C kábel medzi kamerou a camboxom (aj znova).",
};

/// The texts of `status` for a camera reached through `transport` (owner 27.11.2026,
/// shading#14: the warning names the device at the camera, the Orange of a handheld or the
/// cambox); `None` for a camera that can be controlled. `EthernetRest` (no box, future) reads
/// like a cambox.
pub fn info_for(status: ControlStatus, transport: Transport) -> Option<Info> {
    let orange = transport == Transport::SbcRelay;
    Some(match status {
        ControlStatus::Ok => return None,
        ControlStatus::RelayUnreachable if orange => ORANGE_OFFLINE,
        ControlStatus::RelayUnreachable => CAMBOX_OFFLINE,
        // A relay without a reason (CameraOffline) is in practice the missing USB-C link.
        ControlStatus::NoCameraLink | ControlStatus::CameraOffline if orange => ORANGE_NO_CAMERA,
        ControlStatus::NoCameraLink | ControlStatus::CameraOffline => CAMBOX_NO_CAMERA,
        ControlStatus::RelayTooOld => Info {
            badge: "⚠ Starý relay — kamera sa nedá ovládať",
            title: "Relay pri kamere je príliš starý",
            cause: "Relay pri kamere má verziu {version}. Táto verzia ešte nevie ovládať kameru cez jej rozhranie (REST); treba verziu 0.2.0 alebo novšiu.",
            step: "Na tento box treba nasadiť nový relay. Povedz to technikovi (nasadzuje ho camera-box).",
        },
        ControlStatus::CameraNotAnswering => Info {
            badge: "⚠ Kamera neodpovedá",
            title: "Kamera je pripojená, ale neodpovedá",
            cause: "Kamera je pripojená USB-C káblom, ale neodpovedá na ovládanie. Väčšinou sa ešte zapína.",
            step: NOT_ANSWERING_STEP,
        },
        ControlStatus::RestOff => Info {
            badge: "⚠ Kamera nemá povolené riadenie",
            title: "Kamera nemá povolené riadenie",
            cause: "Kamera je pripojená a odpovedá, ale nemá povolené riadenie na diaľku.",
            step: PERMISSION_STEP,
        },
    })
}

/// [`info_for`] for a cambox camera (the default transport).
pub fn info(status: ControlStatus) -> Option<Info> {
    info_for(status, Transport::CamboxRelay)
}

/// The popup's cause line with the relay's version filled in (`?` when unknown).
pub fn cause_text(info: &Info, relay_version: Option<&str>) -> String {
    info.cause
        .replace("{version}", relay_version.unwrap_or("?"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_controllable_camera_has_no_warning() {
        assert_eq!(info(ControlStatus::Ok), None);
    }

    #[test]
    fn every_other_status_has_a_badge_title_and_step() {
        for s in [
            ControlStatus::RelayUnreachable,
            ControlStatus::RelayTooOld,
            ControlStatus::NoCameraLink,
            ControlStatus::CameraNotAnswering,
            ControlStatus::RestOff,
            ControlStatus::CameraOffline,
        ] {
            let i = info(s).unwrap();
            assert!(i.badge.starts_with('⚠'), "{s:?}");
            assert!(!i.title.is_empty() && !i.step.is_empty());
        }
        // owner 27.11.2026 (shading#14): the rare permission case is told the owner's way.
        assert!(info(ControlStatus::CameraNotAnswering)
            .unwrap()
            .step
            .contains("notebook pripojený USB-C káblom do kamery"));
    }

    /// shading#14, owner 27.11.2026: the device at the camera is named (cambox / Orange), and
    /// no text says "Web media manager" (nobody understands it).
    #[test]
    fn the_warning_names_the_cambox_or_the_orange_never_web_media_manager() {
        let orange = |s| info_for(s, Transport::SbcRelay).unwrap();
        let cambox = |s| info_for(s, Transport::CamboxRelay).unwrap();
        assert_eq!(
            orange(ControlStatus::RelayUnreachable).badge,
            "⚠ Orange nie je online"
        );
        assert_eq!(
            orange(ControlStatus::NoCameraLink).badge,
            "⚠ Orange nemá USB-C do kamery"
        );
        assert_eq!(
            orange(ControlStatus::CameraOffline).badge,
            "⚠ Orange nemá USB-C do kamery"
        );
        assert_eq!(
            cambox(ControlStatus::RelayUnreachable).badge,
            "⚠ Cambox nie je online"
        );
        assert_eq!(
            cambox(ControlStatus::NoCameraLink).badge,
            "⚠ Kamera nie je pripojená do camboxu"
        );
        assert_eq!(
            cambox(ControlStatus::CameraOffline).badge,
            "⚠ Kamera nie je pripojená do camboxu"
        );
        assert!(cambox(ControlStatus::RestOff)
            .step
            .contains("notebook pripojený USB-C káblom do kamery"));
        for t in [
            Transport::SbcRelay,
            Transport::CamboxRelay,
            Transport::EthernetRest,
        ] {
            for s in ALL_NOT_OK {
                let i = info_for(s, t).unwrap();
                for text in [i.badge, i.title, i.cause, i.step] {
                    assert!(!text.contains("Web media manager"), "{s:?} {t:?}: {text}");
                }
            }
        }
    }

    #[test]
    fn the_old_relay_cause_names_its_version() {
        let i = info(ControlStatus::RelayTooOld).unwrap();
        assert!(cause_text(&i, Some("0.1.2")).contains("verziu 0.1.2"));
        assert!(cause_text(&i, None).contains("verziu ?"));
    }
}
