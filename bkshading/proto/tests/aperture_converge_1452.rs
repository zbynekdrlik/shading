//! Aperture write convergence (issue 1452): the pure decisions the relay runs after an aperture
//! write. Live 19.10.2026: cam6 (BMPCC 6K, fw 8.1) moves its iris on a 0x5007 write but lands ONE
//! listed step past the request (f/4.0 -> request f/4.5 -> lands f/4.8); cam7 (fw 8.6) does not move
//! at all. The relay reads 0x5007 back inside the write burst, corrects an overshoot by ONE listed
//! step at most, and reports a write that moved the iris as applied (only an unmoved write keeps the
//! issue-1343 `notApplied` flag).

use bkshading_proto::aperture::{
    aperture_correction, aperture_moved, aperture_settle, fnumber_label_index,
    fnumber_raw_from_block, landed_aperture, not_applied_after_aperture_motion,
};
use bkshading_proto::mapping::fnumber_to_av;
use bkshading_proto::read::{current_fnumber_x100, RawConfigs};

/// cam6's f-number list around the evidence (ascending), as the relay holds it.
fn labels() -> Vec<String> {
    ["f/4", "f/4.5", "f/4.8", "f/5.2", "f/5.6"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

// --- aperture_correction: one listed step back after an overshoot -----------------------------

#[test]
fn overshoot_up_steps_one_back_toward_the_request_1452() {
    // cam6: before f/4 (idx 0), request f/4.5 (idx 1), landed f/4.8 (idx 2) -> correct to idx 1.
    assert_eq!(aperture_correction(1, Some(0), Some(2), 5), Some(1));
    // Two steps past (landed idx 3): still ONE step back (idx 2), never a jump or a loop.
    assert_eq!(aperture_correction(1, Some(0), Some(3), 5), Some(2));
}

#[test]
fn overshoot_down_steps_one_back_toward_the_request_1452() {
    // before f/5.2 (idx 3), request f/4.8 (idx 2), landed f/4.5 (idx 1) -> correct up to idx 2.
    assert_eq!(aperture_correction(2, Some(3), Some(1), 5), Some(2));
}

#[test]
fn landed_on_the_request_needs_no_correction_1452() {
    assert_eq!(aperture_correction(1, Some(0), Some(1), 5), None);
    assert_eq!(aperture_correction(2, Some(3), Some(2), 5), None);
}

#[test]
fn no_move_is_never_corrected_1452() {
    // cam7: the iris stayed where it was -> no correcting write (the report flags it instead).
    assert_eq!(aperture_correction(1, Some(0), Some(0), 5), None);
    assert_eq!(aperture_correction(2, Some(3), Some(3), 5), None);
}

#[test]
fn undershoot_is_never_corrected_1452() {
    // Moved toward the request but stopped short (a lens stop): not an overshoot, no correction.
    assert_eq!(aperture_correction(3, Some(0), Some(2), 5), None);
    assert_eq!(aperture_correction(1, Some(4), Some(2), 5), None);
}

#[test]
fn landed_outside_the_list_or_at_a_list_end_is_never_corrected_1452() {
    // cam5: a value outside the list (the lens's own wide-open f/4.0) has no index.
    assert_eq!(aperture_correction(0, Some(1), None, 5), None);
    // An unknown starting point gives no direction to judge an overshoot by.
    assert_eq!(aperture_correction(1, None, Some(2), 5), None);
    // Landed on the last / first listed value: the lens's own stop, never stepped back from.
    assert_eq!(aperture_correction(3, Some(1), Some(4), 5), None);
    assert_eq!(aperture_correction(1, Some(3), Some(0), 5), None);
}

#[test]
fn out_of_range_inputs_are_never_corrected_1452() {
    assert_eq!(aperture_correction(1, Some(0), Some(2), 0), None);
    assert_eq!(aperture_correction(0, Some(0), Some(0), 1), None);
    assert_eq!(aperture_correction(5, Some(0), Some(3), 5), None);
    assert_eq!(aperture_correction(1, Some(7), Some(3), 5), None);
    assert_eq!(aperture_correction(1, Some(0), Some(9), 5), None);
}

#[test]
fn a_correction_is_one_step_and_its_own_result_is_never_corrected_again_1452() {
    // The bound: whatever the correction lands on, a second decision from the corrected position
    // toward the same request never produces a further move away from it in the same direction.
    for n in 3..8usize {
        for req in 0..n {
            for before in 0..n {
                for landed in 0..n {
                    if let Some(fix) = aperture_correction(req, Some(before), Some(landed), n) {
                        assert!(fix < n, "the correction stays inside the list");
                        assert_eq!(
                            fix.abs_diff(landed),
                            1,
                            "exactly one listed step (req {req}, before {before}, landed {landed})"
                        );
                        assert!(
                            fix.abs_diff(req) < landed.abs_diff(req),
                            "the step goes back toward the request"
                        );
                    }
                }
            }
        }
    }
}

// --- aperture_settle: the 0x5007 read-back waits for the iris to stop -------------------------

#[test]
fn settle_waits_until_two_reads_agree_away_from_the_start_1452() {
    // Nothing read yet -> read again.
    assert_eq!(aperture_settle(Some(400), &[], 5), None);
    // One read that moved: not yet stable.
    assert_eq!(aperture_settle(Some(400), &[480], 5), None);
    // Still moving (two different reads).
    assert_eq!(aperture_settle(Some(400), &[450, 480], 5), None);
    // Two equal reads away from the start: settled there.
    assert_eq!(aperture_settle(Some(400), &[480, 480], 5), Some(480));
}

#[test]
fn settle_keeps_reading_while_the_iris_has_not_moved_then_ends_at_the_bound_1452() {
    // Two reads at the start value are not "settled" (the iris may not have started yet)...
    assert_eq!(aperture_settle(Some(400), &[400, 400], 5), None);
    // ...but the bound ends it: the last read is the answer (cam7: did not move).
    assert_eq!(
        aperture_settle(Some(400), &[400, 400, 400, 400, 400], 5),
        Some(400)
    );
    // The bound also ends a still-moving read.
    assert_eq!(aperture_settle(Some(400), &[450, 480, 520], 3), Some(520));
}

#[test]
fn settle_with_an_unknown_start_needs_two_equal_reads_1452() {
    assert_eq!(aperture_settle(None, &[480], 5), None);
    assert_eq!(aperture_settle(None, &[480, 480], 5), Some(480));
}

// --- aperture_moved + the report ---------------------------------------------------------------

#[test]
fn moved_compares_the_raw_values_1452() {
    assert_eq!(aperture_moved(Some(400), &[480]), Some(true));
    // cam5: a request for f/4.8 lands the lens's wide-open f/4.0, outside the list: still moved.
    assert_eq!(aperture_moved(Some(520), &[400]), Some(true));
    assert_eq!(aperture_moved(Some(400), &[400]), Some(false));
    assert_eq!(aperture_moved(None, &[400]), None);
    assert_eq!(aperture_moved(Some(400), &[]), None);
}

#[test]
fn moved_counts_every_landing_not_only_the_last_1452() {
    // The write landed elsewhere (f/4.5) and the correcting write took the iris back onto its
    // starting value (f/5.2): it moved twice, it was never ignored.
    assert_eq!(aperture_moved(Some(520), &[450, 520]), Some(true));
    // Overshoot then corrected: moved.
    assert_eq!(aperture_moved(Some(400), &[480, 450]), Some(true));
    // Every landing at the start: did not move.
    assert_eq!(aperture_moved(Some(400), &[400, 400]), Some(false));
}

#[test]
fn a_write_that_moved_the_iris_is_not_reported_as_ignored_1452() {
    let flagged = vec!["apertureNorm".to_string(), "iso".to_string()];
    assert_eq!(
        not_applied_after_aperture_motion(flagged, Some(true)),
        vec!["iso".to_string()],
        "a moved aperture lands elsewhere = applied; other keys keep their flag"
    );
}

#[test]
fn a_write_that_did_not_move_or_was_not_measured_keeps_its_flag_1452() {
    let flagged = vec!["apertureNorm".to_string()];
    assert_eq!(
        not_applied_after_aperture_motion(flagged.clone(), Some(false)),
        flagged,
        "did not move = ignored, flagged as today"
    );
    assert_eq!(
        not_applied_after_aperture_motion(flagged.clone(), None),
        flagged,
        "not measured = flagged as today"
    );
}

// --- the raw value <-> the f-number list ------------------------------------------------------

#[test]
fn label_index_maps_the_raw_value_onto_the_list_1452() {
    let l = labels();
    assert_eq!(fnumber_label_index(&l, 400), Some(0));
    assert_eq!(fnumber_label_index(&l, 450), Some(1));
    assert_eq!(fnumber_label_index(&l, 480), Some(2));
    assert_eq!(fnumber_label_index(&l, 560), Some(4));
    // Not a listed value (cam5's wide-open f/3.5 on this list) -> no index.
    assert_eq!(fnumber_label_index(&l, 350), None);
    assert_eq!(fnumber_label_index(&[], 400), None);
}

#[test]
fn raw_from_the_5007_block_1452() {
    let block = "Label: F-Number\nReadonly: 0\nType: MENU\nCurrent: 480\nChoice: 0 400\nEND";
    assert_eq!(fnumber_raw_from_block(block), Some(480));
    assert_eq!(
        fnumber_raw_from_block("Label: F-Number\nCurrent: 0\nEND"),
        None
    );
    assert_eq!(
        fnumber_raw_from_block("Label: F-Number\nCurrent: (null)\nEND"),
        None
    );
    assert_eq!(fnumber_raw_from_block(""), None);
}

#[test]
fn landed_aperture_is_the_real_value_on_the_panel_scale_1452() {
    let l = labels();
    let (av, norm) = landed_aperture(480, &l);
    assert!((av.unwrap() - fnumber_to_av(4.8).unwrap()).abs() < 1e-9);
    assert!((norm.unwrap() - 0.5).abs() < 1e-9, "f/4.8 = idx 2 of 5");
    // Off the list: the real f-number, the nearest listed position for the slider.
    let (av, norm) = landed_aperture(350, &l);
    assert!((av.unwrap() - fnumber_to_av(3.5).unwrap()).abs() < 1e-9);
    assert_eq!(norm, Some(0.0));
}

#[test]
fn current_fnumber_x100_prefers_the_radio_then_the_5007_block_1452() {
    let raw = RawConfigs {
        fnumber: "Current: f/4.5\nChoice: 0 f/4\nChoice: 1 f/4.5\nEND".into(),
        fnumber_raw: "Current: 480\nEND".into(),
        ..Default::default()
    };
    assert_eq!(
        current_fnumber_x100(&raw),
        Some(450),
        "the RADIO Current wins when it names a value"
    );
    let off_grid = RawConfigs {
        fnumber: "Current: (null)\nChoice: 0 f/4.8\nEND".into(),
        fnumber_raw: "Current: 400\nEND".into(),
        ..Default::default()
    };
    assert_eq!(current_fnumber_x100(&off_grid), Some(400));
    assert_eq!(current_fnumber_x100(&RawConfigs::default()), None);
}
