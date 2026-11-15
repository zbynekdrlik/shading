//! Why a camera cannot be controlled, and what the operator should switch on (shading#14): the
//! block's warning badge and the popup, in plain Slovak. The wire carries only the status
//! (`CameraView.control`); the texts live here only (ported verbatim from the JS panel).

use bkshading_proto::wire::ControlStatus;

/// One status's texts. `cause` may hold `{version}` (the relay's version).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub badge: &'static str,
    pub title: &'static str,
    pub cause: &'static str,
    pub step: &'static str,
}

const WMM_STEP: &str = "Zapni na kamere Web media manager: v menu kamery Setup, alebo v programe Blackmagic Camera Setup v časti Network access. Panel potom kameru nájde sám do pár sekúnd.";
const NOT_ANSWERING_STEP: &str = "Počkaj chvíľu, kým sa kamera zapne. Ak to trvá dlhšie ako minútu: Zapni na kamere Web media manager: v menu kamery Setup, alebo v programe Blackmagic Camera Setup v časti Network access. Panel potom kameru nájde sám do pár sekúnd.";

const CAMERA_OFFLINE: Info = Info {
    badge: "⚠ Kamera offline — nedá sa ovládať",
    title: "Kamera sa nedá ovládať",
    cause: "Relay beží, ale kameru nevidí. Tento relay presnú príčinu nehlási.",
    step: "Skontroluj, či je kamera zapnutá, či je zapojený USB-C kábel ku boxu a či má kamera zapnutý Web media manager.",
};

/// The texts of `status`; `None` for a camera that can be controlled.
pub fn info(status: ControlStatus) -> Option<Info> {
    Some(match status {
        ControlStatus::Ok => return None,
        ControlStatus::RelayUnreachable => Info {
            badge: "⚠ Relay nedostupný — kamera sa nedá ovládať",
            title: "Panel sa nespojí s relay pri kamere",
            cause: "Relay (malý box alebo počítač pri kamere, cez ktorý panel ovláda kameru) neodpovedá. Box je vypnutý, odpojený zo siete, alebo na ňom nebeží relay.",
            step: "Skontroluj, či je box pri kamere zapnutý a pripojený do siete (kábel alebo WiFi). Ak je zapnutý, reštartuj ho.",
        },
        ControlStatus::RelayTooOld => Info {
            badge: "⚠ Starý relay — kamera sa nedá ovládať",
            title: "Relay pri kamere je príliš starý",
            cause: "Relay na tomto boxe má verziu {version}. Táto verzia ešte nevie ovládať kameru cez jej rozhranie (REST); treba verziu 0.2.0 alebo novšiu.",
            step: "Na tento box treba nasadiť nový relay. Povedz to technikovi (nasadzuje ho camera-box).",
        },
        ControlStatus::NoCameraLink => Info {
            badge: "⚠ Kamera nie je pripojená cez USB",
            title: "Relay nevidí kameru na USB",
            cause: "Relay beží, ale kamera nie je pripojená USB-C káblom k boxu, alebo je vypnutá.",
            step: "Zapni kameru a skontroluj USB-C kábel medzi kamerou a boxom (zapoj ho znova).",
        },
        ControlStatus::CameraNotAnswering => Info {
            badge: "⚠ Kamera neodpovedá",
            title: "Kamera je pripojená, ale neodpovedá",
            cause: "Relay vidí kameru na USB-C, ale kamera neodpovedá na ovládanie. Kamera sa ešte zapína, alebo má vypnutý Web media manager.",
            step: NOT_ANSWERING_STEP,
        },
        ControlStatus::RestOff => Info {
            badge: "⚠ Zapni Web media manager na kamere",
            title: "Na kamere je vypnutý Web media manager",
            cause: "Kamera je pripojená a odpovedá, ale ovládanie cez sieť (Web media manager) má vypnuté.",
            step: WMM_STEP,
        },
        ControlStatus::CameraOffline => CAMERA_OFFLINE,
    })
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
        assert!(info(ControlStatus::CameraNotAnswering)
            .unwrap()
            .step
            .contains("Zapni na kamere Web media manager"));
    }

    #[test]
    fn the_old_relay_cause_names_its_version() {
        let i = info(ControlStatus::RelayTooOld).unwrap();
        assert!(cause_text(&i, Some("0.1.2")).contains("verziu 0.1.2"));
        assert!(cause_text(&i, None).contains("verziu ?"));
    }
}
