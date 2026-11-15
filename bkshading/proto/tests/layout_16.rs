//! shading#16: the venue's panel layout (block order + hidden blocks), its pure rules.

use bkshading_proto::layout::{
    apply_put, camera_number, effective_order, hidden_count, number_order, same_layout,
    shown_order, validate, LayoutDoc, LayoutPut, LayoutRefusal,
};
use bkshading_proto::wire::ServerMsg;

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

#[test]
fn the_camera_number_is_the_ids_trailing_digits() {
    assert_eq!(camera_number("cam7"), Some(7));
    assert_eq!(camera_number("cam10"), Some(10));
    assert_eq!(camera_number("7"), Some(7));
    assert_eq!(camera_number("spare"), None);
    assert_eq!(camera_number(""), None);
    assert_eq!(camera_number("cam1x"), None);
}

#[test]
fn number_order_sorts_by_number_numberless_last_ties_keep_config_order() {
    let config = ids(&["cam10", "spare", "cam7", "cam1", "b", "a"]);
    assert_eq!(
        number_order(&config),
        ids(&["cam1", "cam7", "cam10", "spare", "b", "a"])
    );
}

#[test]
fn a_fresh_install_is_number_order_13() {
    // strih-pp's config order: its own 1 4 6 7, then the visiting 2.
    let cams = ids(&["cam1", "cam4", "cam6", "cam7", "cam2"]);
    let fresh = LayoutDoc::default();
    assert_eq!(fresh.rev, 0);
    assert_eq!(
        effective_order(&fresh, &cams),
        ids(&["cam1", "cam2", "cam4", "cam6", "cam7"])
    );
    assert_eq!(
        shown_order(&fresh, &cams),
        ids(&["cam1", "cam2", "cam4", "cam6", "cam7"])
    );
}

#[test]
fn the_stored_order_wins_over_numbers() {
    let cams = ids(&["cam2", "cam3", "cam5"]);
    let d = doc(3, &["cam5", "cam2", "cam3"], &[]);
    assert_eq!(effective_order(&d, &cams), ids(&["cam5", "cam2", "cam3"]));
}

#[test]
fn a_camera_new_to_the_config_lands_after_the_last_lower_numbered_one() {
    // Stored 7 3 5 1; cam4 is new: the last placed id with a lower number is cam1 (index 3),
    // so cam4 goes after it, at the end; cam6 is new too: after cam1 as well, cam4 already there.
    let d = doc(2, &["cam7", "cam3", "cam5", "cam1"], &[]);
    let cams = ids(&["cam1", "cam3", "cam5", "cam7", "cam4", "cam6"]);
    assert_eq!(
        effective_order(&d, &cams),
        ids(&["cam7", "cam3", "cam5", "cam1", "cam4", "cam6"])
    );
    // Stored 3 5 7; cam1 is new and no placed id has a lower number: it goes first.
    let d = doc(1, &["cam3", "cam5", "cam7"], &[]);
    assert_eq!(
        effective_order(&d, &ids(&["cam1", "cam3", "cam5", "cam7"])),
        ids(&["cam1", "cam3", "cam5", "cam7"])
    );
    // Stored 1 2 5; cam3 lands right after cam2 (the last placed id below 3), before cam5.
    let d = doc(1, &["cam1", "cam2", "cam5"], &[]);
    assert_eq!(
        effective_order(&d, &ids(&["cam1", "cam2", "cam3", "cam5"])),
        ids(&["cam1", "cam2", "cam3", "cam5"])
    );
}

#[test]
fn an_id_without_a_number_lands_at_the_end() {
    let d = doc(1, &["cam2", "cam1"], &[]);
    assert_eq!(
        effective_order(&d, &ids(&["spare", "cam1", "cam2"])),
        ids(&["cam2", "cam1", "spare"])
    );
}

#[test]
fn ids_the_config_no_longer_lists_are_kept_in_place_but_not_shown_1431() {
    // A joint event saved cam2 (visiting) between cam1 and cam4; at a normal event cam2 is not
    // listed. The document keeps its place; the panel skips it.
    let d = doc(4, &["cam1", "cam2", "cam4"], &["cam2"]);
    let cams = ids(&["cam1", "cam4"]);
    assert_eq!(effective_order(&d, &cams), ids(&["cam1", "cam2", "cam4"]));
    assert_eq!(shown_order(&d, &cams), ids(&["cam1", "cam4"]));
    assert_eq!(
        hidden_count(&d, &cams),
        0,
        "an unknown hidden id is not counted"
    );
}

#[test]
fn hidden_cameras_are_left_out_and_counted() {
    let d = doc(1, &["cam1", "cam2", "cam3"], &["cam2"]);
    let cams = ids(&["cam1", "cam2", "cam3"]);
    assert_eq!(shown_order(&d, &cams), ids(&["cam1", "cam3"]));
    assert_eq!(hidden_count(&d, &cams), 1);
}

#[test]
fn a_repeated_id_in_a_stored_order_keeps_its_first_place() {
    let d = doc(1, &["cam2", "cam1", "cam2"], &[]);
    assert_eq!(
        effective_order(&d, &ids(&["cam1", "cam2"])),
        ids(&["cam2", "cam1"])
    );
}

#[test]
fn validate_refuses_duplicates_empty_ids_and_a_hidden_id_outside_the_order() {
    assert_eq!(validate(&ids(&["cam1", "cam2"]), &ids(&["cam2"])), Ok(()));
    assert_eq!(validate(&[], &[]), Ok(()));
    let bad = Err(LayoutRefusal::Invalid);
    assert_eq!(validate(&ids(&["cam1", "cam1"]), &[]), bad);
    assert_eq!(
        validate(&ids(&["cam1", "cam2"]), &ids(&["cam2", "cam2"])),
        bad
    );
    assert_eq!(validate(&ids(&["cam1"]), &ids(&["cam9"])), bad);
    assert_eq!(validate(&ids(&["cam1", " "]), &[]), bad);
}

#[test]
fn a_save_on_the_current_revision_makes_the_next_one() {
    let current = doc(5, &["cam1", "cam2"], &[]);
    let put = LayoutPut {
        rev: 5,
        order: ids(&["cam2", "cam1"]),
        hidden: ids(&["cam1"]),
    };
    assert_eq!(
        apply_put(&current, &put),
        Ok(doc(6, &["cam2", "cam1"], &["cam1"]))
    );
}

#[test]
fn a_save_on_an_older_revision_is_stale_even_when_invalid() {
    let current = doc(5, &["cam1"], &[]);
    let late = LayoutPut {
        rev: 4,
        order: ids(&["cam1", "cam1"]),
        hidden: vec![],
    };
    assert_eq!(apply_put(&current, &late), Err(LayoutRefusal::Stale));
    let invalid = LayoutPut { rev: 5, ..late };
    assert_eq!(apply_put(&current, &invalid), Err(LayoutRefusal::Invalid));
}

#[test]
fn refusal_words_round_trip() {
    for r in [LayoutRefusal::Stale, LayoutRefusal::Invalid] {
        assert_eq!(LayoutRefusal::from_text(r.text()), Some(r));
    }
    assert_eq!(LayoutRefusal::Stale.text(), "stale");
    assert_eq!(LayoutRefusal::Invalid.text(), "invalid");
    assert_eq!(LayoutRefusal::from_text("other"), None);
}

#[test]
fn same_layout_compares_order_and_the_hidden_set() {
    let d = doc(1, &["cam1", "cam2", "cam3"], &["cam3", "cam1"]);
    assert!(same_layout(
        &ids(&["cam1", "cam2", "cam3"]),
        &ids(&["cam1", "cam3"]),
        &d
    ));
    assert!(!same_layout(
        &ids(&["cam2", "cam1", "cam3"]),
        &ids(&["cam1", "cam3"]),
        &d
    ));
    assert!(!same_layout(
        &ids(&["cam1", "cam2", "cam3"]),
        &ids(&["cam1"]),
        &d
    ));
}

#[test]
fn the_layout_push_is_a_flat_tagged_message() {
    let msg = ServerMsg::Layout(doc(3, &["cam2", "cam1"], &["cam1"]));
    let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
    assert_eq!(
        v,
        serde_json::json!({"type": "layout", "rev": 3, "order": ["cam2", "cam1"], "hidden": ["cam1"]})
    );
    let back: ServerMsg = serde_json::from_value(v).unwrap();
    assert_eq!(back, msg);
}

#[test]
fn a_document_file_without_lists_reads_as_empty_lists() {
    let d: LayoutDoc = serde_json::from_str(r#"{"rev": 2}"#).unwrap();
    assert_eq!(d, doc(2, &[], &[]));
}
