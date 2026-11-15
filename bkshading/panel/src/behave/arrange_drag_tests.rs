use super::*;

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn doc(rev: u64, order: &[&str], hidden: &[&str]) -> LayoutDoc {
    LayoutDoc {
        rev,
        order: ids(order),
        hidden: ids(hidden),
    }
}

fn cams() -> Vec<String> {
    ids(&["cam1", "cam2", "cam3"])
}

#[test]
fn two_taps_without_a_change_leave() {
    let mut a = Arrange::default();
    let d = doc(0, &[], &[]);
    assert_eq!(a.tap(&d, &cams()), Some(Tap::Start));
    assert!(a.is_on());
    assert_eq!(a.tap(&d, &cams()), Some(Tap::Leave));
    assert_eq!(a, Arrange::Off);
}

#[test]
fn an_edit_is_sent_once_on_the_starting_revision() {
    let mut a = Arrange::default();
    let d = doc(4, &["cam1", "cam2", "cam3"], &[]);
    a.tap(&d, &cams());
    let e = a.edit_mut().unwrap();
    assert!(e.dropped("cam3", "cam1", DropKind::Before));
    e.toggle_hidden("cam2");
    let Some(Tap::Send(put)) = a.tap(&d, &cams()) else {
        panic!("a changed edit is sent");
    };
    assert_eq!(put.rev, 4);
    assert_eq!(put.order, ids(&["cam3", "cam1", "cam2"]));
    assert_eq!(put.hidden, ids(&["cam2"]));
    assert!(matches!(a, Arrange::Saving { .. }));
    assert_eq!(
        a.tap(&d, &cams()),
        None,
        "no second save while one is on its way"
    );
    assert!(a.edit_mut().is_none(), "nothing changes while saving");
}

#[test]
fn a_taken_save_ends_with_the_saved_layout() {
    let mut a = Arrange::default();
    let d = doc(4, &["cam1", "cam2"], &[]);
    a.tap(&d, &cams());
    a.edit_mut().unwrap().toggle_hidden("cam1");
    a.tap(&d, &cams());
    let (note, saved) = a.answered(Ok(5)).unwrap();
    assert_eq!(
        note,
        Note {
            text: SAVED_NOTE,
            tone: Tone::Saved
        }
    );
    let saved = saved.unwrap();
    assert_eq!(saved.rev, 5);
    assert_eq!(saved.hidden, ids(&["cam1"]));
    assert_eq!(a, Arrange::Off);
    assert_eq!(a.answered(Ok(6)), None, "an answer with no save on its way");
}

#[test]
fn a_refused_save_ends_with_its_note() {
    for (fail, text) in [
        (SaveFail::Refused(LayoutRefusal::Stale), STALE_NOTE),
        (SaveFail::Refused(LayoutRefusal::Invalid), INVALID_NOTE),
        (SaveFail::Offline, OFFLINE_NOTE),
        (SaveFail::Other, NOT_SAVED_NOTE),
    ] {
        let mut a = Arrange::default();
        let d = doc(1, &["cam1", "cam2"], &[]);
        a.tap(&d, &cams());
        a.edit_mut().unwrap().toggle_hidden("cam2");
        a.tap(&d, &cams());
        assert_eq!(
            a.answered(Err(fail)),
            Some((
                Note {
                    text,
                    tone: Tone::Refused
                },
                None
            ))
        );
        assert_eq!(a, Arrange::Off);
    }
}

#[test]
fn another_devices_save_ends_the_mode_unsaved() {
    let mut a = Arrange::default();
    a.tap(&doc(2, &[], &[]), &cams());
    assert_eq!(
        a.doc_arrived(2),
        None,
        "the same revision again changes nothing"
    );
    assert!(a.is_on());
    assert_eq!(
        a.doc_arrived(3),
        Some(Note {
            text: STALE_NOTE,
            tone: Tone::Refused
        })
    );
    assert_eq!(a, Arrange::Off);
    assert_eq!(a.doc_arrived(9), None);
}

#[test]
fn while_saving_a_push_waits_for_the_answer() {
    let mut a = Arrange::default();
    let d = doc(2, &["cam1", "cam2"], &[]);
    a.tap(&d, &cams());
    a.edit_mut().unwrap().toggle_hidden("cam1");
    a.tap(&d, &cams());
    assert_eq!(
        a.doc_arrived(3),
        None,
        "the save's own push can come before its answer"
    );
    assert!(matches!(a, Arrange::Saving { .. }));
    assert_eq!(a.answered(Ok(3)).unwrap().0.text, SAVED_NOTE);
}

#[test]
fn hiding_the_page_ends_the_mode() {
    let mut a = Arrange::default();
    assert!(!a.hidden_page());
    a.tap(&doc(0, &[], &[]), &cams());
    assert!(a.hidden_page());
    assert_eq!(a, Arrange::Off);
}

#[test]
fn blocks_close_up_outside_the_mode_and_show_ghosts_in_it() {
    let d = doc(1, &["cam3", "cam1", "cam2"], &["cam1"]);
    let mut a = Arrange::default();
    assert_eq!(
        a.blocks(&d, &cams()),
        vec![("cam3".to_string(), false), ("cam2".to_string(), false)]
    );
    a.tap(&d, &cams());
    assert_eq!(
        a.blocks(&d, &cams()),
        vec![
            ("cam3".to_string(), false),
            ("cam1".to_string(), true),
            ("cam2".to_string(), false)
        ]
    );
}

#[test]
fn a_camera_arriving_during_the_mode_is_drawn_at_its_number_slot() {
    let d = doc(1, &["cam3", "cam1"], &[]);
    let mut a = Arrange::default();
    a.tap(&d, &ids(&["cam1", "cam3"]));
    let ids_only: Vec<String> = a
        .blocks(&d, &ids(&["cam1", "cam3", "cam2"]))
        .into_iter()
        .map(|b| b.0)
        .collect();
    assert_eq!(ids_only, ids(&["cam3", "cam1", "cam2"]));
}

#[test]
fn the_drop_kind_follows_the_edges() {
    assert_eq!(drop_kind(0.1), DropKind::Before);
    assert_eq!(drop_kind(0.5), DropKind::Swap);
    assert_eq!(drop_kind(0.9), DropKind::After);
    assert_eq!(drop_kind(EDGE_SHARE), DropKind::Swap);
}

#[test]
fn the_nearest_centre_is_the_target() {
    let spot = |id: &str, left: f64| Spot {
        id: id.into(),
        left,
        top: 0.0,
        width: 200.0,
        height: 100.0,
    };
    let spots = vec![spot("cam2", 0.0), spot("cam3", 210.0)];
    let (s, k) = target_at(&spots, 300.0, 50.0).unwrap();
    assert_eq!((s.id.as_str(), k), ("cam3", DropKind::Swap));
    let (s, k) = target_at(&spots, 215.0, 50.0).unwrap();
    assert_eq!((s.id.as_str(), k), ("cam3", DropKind::Before));
    let (s, k) = target_at(&spots, 190.0, 50.0).unwrap();
    assert_eq!((s.id.as_str(), k), ("cam2", DropKind::After));
    assert!(target_at(&[], 0.0, 0.0).is_none());
}

#[test]
fn the_slop_decides_a_carry() {
    assert!(!drags(0.0, 0.0, 4.0, 4.0));
    assert!(drags(0.0, 0.0, 5.0, 5.0));
}

#[test]
fn a_note_timer_clears_only_its_own_note() {
    let mut t = NoteTimer::default();
    let first = t.show();
    let second = t.show();
    assert!(!t.expires(first));
    assert!(t.expires(second));
}

#[test]
fn each_tone_has_its_class() {
    assert_eq!(Tone::Plain.class(), "note");
    assert_eq!(Tone::Saved.class(), "note saved");
    assert_eq!(Tone::Refused.class(), "note refused");
}
