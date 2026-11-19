//! Why a write was not applied, in Slovak (shading#17). The relay names the reason per key
//! (`notAppliedReason`) and on a failed write (`x-bkshading-reason`); the wire carries the enum,
//! the texts live only here (as shading#14's `control_info`).

use bkshading_proto::wire::NotAppliedReason;

use super::steppers::{RowView, NOT_APPLIED_TITLE};

/// The Slovak text of `reason`; without one (a relay older than 0.2.14, or an unknown word) the
/// plain "not applied" text.
pub fn reason_text(reason: Option<NotAppliedReason>) -> &'static str {
    match reason {
        Some(NotAppliedReason::LensEnd) => "Clona je na konci rozsahu objektívu",
        Some(NotAppliedReason::NotControllable) => "Objektív nedovoľuje ovládať clonu z kamery",
        Some(NotAppliedReason::IrisNotFollowing) => {
            "Clona nereaguje: kamera ju má v automatike, alebo je objektív manuálny"
        }
        Some(NotAppliedReason::OutOfRange) => "Kamera hodnotu odmietla: je mimo rozsahu objektívu",
        Some(NotAppliedReason::CameraKeptValue) => {
            "Kamera zápis prijala, ale ponechala si pôvodnú hodnotu"
        }
        Some(NotAppliedReason::NotInChoices) => "Kamera túto hodnotu neponúka",
        Some(NotAppliedReason::CameraRefused) => "Kamera zápis odmietla",
        Some(NotAppliedReason::CameraNotAnswering) => "Kamera neodpovedá",
        Some(NotAppliedReason::CameraOffline) => "Kamera je offline, nič sa nezapísalo",
        Some(NotAppliedReason::RelayUnreachable) => "Relé kamery je nedostupné",
        Some(NotAppliedReason::Unknown) | None => NOT_APPLIED_TITLE,
    }
}

/// The reason a failed write's `x-bkshading-reason` header names, if any.
pub fn header_reason(header: Option<&str>) -> Option<NotAppliedReason> {
    header
        .filter(|h| !h.trim().is_empty())
        .map(NotAppliedReason::from_wire)
}

/// The note to show when a redraw starts a refusal flash (shading#17): the first row whose red
/// flash just started on a value the camera did not apply. A flash that only timed out, or one
/// already shown, gives none.
pub fn new_refusal_note(old: &[RowView], new: &[RowView]) -> Option<&'static str> {
    new.iter()
        .filter(|r| r.flash && r.not_applied)
        .find(|r| {
            !old.iter()
                .any(|o| o.row == r.row && o.flash && o.not_applied)
        })
        .map(|r| r.refusal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_has_its_own_slovak_text_17() {
        let texts: Vec<&str> = NotAppliedReason::KNOWN
            .iter()
            .map(|r| reason_text(Some(*r)))
            .collect();
        for (i, t) in texts.iter().enumerate() {
            assert!(!t.is_empty());
            assert_ne!(*t, NOT_APPLIED_TITLE, "{:?}", NotAppliedReason::KNOWN[i]);
            assert_eq!(texts.iter().filter(|x| *x == t).count(), 1, "{t}");
        }
        assert_eq!(reason_text(None), NOT_APPLIED_TITLE);
        assert_eq!(
            reason_text(Some(NotAppliedReason::Unknown)),
            NOT_APPLIED_TITLE
        );
    }

    #[test]
    fn the_header_names_the_reason_17() {
        assert_eq!(
            header_reason(Some("camera-refused")),
            Some(NotAppliedReason::CameraRefused)
        );
        assert_eq!(
            header_reason(Some("from-the-future")),
            Some(NotAppliedReason::Unknown)
        );
        assert_eq!(header_reason(Some("")), None);
        assert_eq!(header_reason(None), None);
    }
}
