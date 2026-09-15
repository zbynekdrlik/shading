//! issue 1309: the shading-write coalescing policy (`SetQueue` / `coalesce_set_requests`) + the
//! shared `summarize_set_request` log helper. Pure — no camera, no I/O.

use bkshading_proto::wire::{
    coalesce_set_requests, summarize_set_request, SetQueue, SetRequest, SubmitAction,
};

fn req_iso(iso: i64) -> SetRequest {
    SetRequest {
        iso: Some(iso),
        ..Default::default()
    }
}

#[test]
fn coalesce_is_latest_wins_per_param() {
    let prev = SetRequest {
        iso: Some(400),
        aperture_norm: Some(0.2),
        ..Default::default()
    };
    let new = SetRequest {
        iso: Some(800),       // overrides prev
        kelvin: Some(6500),   // adds
        ..Default::default()  // aperture_norm None -> keeps prev's 0.2
    };
    let merged = coalesce_set_requests(Some(prev), new);
    assert_eq!(merged.iso, Some(800)); // newer wins
    assert_eq!(merged.aperture_norm, Some(0.2)); // preserved from pending
    assert_eq!(merged.kelvin, Some(6500)); // added
}

#[test]
fn coalesce_none_prev_is_identity() {
    let new = req_iso(200);
    assert_eq!(coalesce_set_requests(None, new.clone()), new);
}

#[test]
fn set_queue_single_flight_runs_first_coalesces_rest() {
    let mut q = SetQueue::default();
    // First SET runs immediately.
    match q.submit(req_iso(100)) {
        SubmitAction::RunNow(r) => assert_eq!(r.iso, Some(100)),
        other => panic!("expected RunNow, got {other:?}"),
    }
    // While in flight, two more coalesce (never a second parallel run).
    assert_eq!(q.submit(req_iso(200)), SubmitAction::Coalesced { total: 1 });
    assert_eq!(q.submit(req_iso(300)), SubmitAction::Coalesced { total: 2 });
    // Worker finishes the first: drains the LATEST coalesced value (300 beat 200).
    let drained = q.finish().expect("a coalesced SET is pending");
    assert_eq!(drained.iso, Some(300));
    // Worker finishes the drained one: nothing left -> queue idle again.
    assert!(q.finish().is_none());
    assert_eq!(q.coalesced_total(), 2);
    // Idle again: the next SET runs immediately.
    assert!(matches!(q.submit(req_iso(400)), SubmitAction::RunNow(_)));
}

#[test]
fn set_queue_abort_clears_in_flight_and_pending() {
    let mut q = SetQueue::default();
    assert!(matches!(q.submit(req_iso(1)), SubmitAction::RunNow(_)));
    assert_eq!(q.submit(req_iso(2)), SubmitAction::Coalesced { total: 1 });
    q.abort(); // a write error drops both in-flight AND the queued follow-up
    assert!(q.finish().is_none()); // nothing pending
    assert!(matches!(q.submit(req_iso(3)), SubmitAction::RunNow(_))); // fresh start
}

#[test]
fn summarize_lists_only_set_params() {
    let req = SetRequest {
        iso: Some(800),
        fps: Some(30),
        auto_wb: Some(true),
        ..Default::default()
    };
    let s = summarize_set_request(&req);
    assert!(s.contains("iso=800"));
    assert!(s.contains("fps=30"));
    assert!(s.contains("auto_wb=true"));
    assert!(!s.contains("kelvin"));
    assert_eq!(summarize_set_request(&SetRequest::default()), "(none)");
}
