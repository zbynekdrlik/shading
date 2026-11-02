//! The shading-write single-flight FIFO queue (`SetQueue`) + the shared `summarize_set_request`
//! log helper. issue 1309 established the single-flight gate (never a second parallel gphoto2);
//! issue 1337 changed the pending policy from latest-wins COALESCE to FIFO, so a burst of rapid
//! clicks becomes N in-order camera moves (the persistent write-burst shell makes that fast).
//! Pure — no camera, no I/O.

use bkshading_proto::wire::{summarize_set_request, SetQueue, SetRequest, SubmitAction};

fn req_iso(iso: i64) -> SetRequest {
    SetRequest {
        iso: Some(iso),
        ..Default::default()
    }
}

#[test]
fn set_queue_single_flight_runs_first_queues_rest_fifo() {
    let mut q = SetQueue::default();
    // First SET runs immediately.
    match q.submit(req_iso(100)) {
        SubmitAction::RunNow(r) => assert_eq!(r.iso, Some(100)),
        other => panic!("expected RunNow, got {other:?}"),
    }
    // While in flight, more are QUEUED (never a second parallel run) — FIFO, not coalesced.
    assert_eq!(q.submit(req_iso(200)), SubmitAction::Queued { total: 1 });
    assert_eq!(q.submit(req_iso(300)), SubmitAction::Queued { total: 2 });
    // Worker finishes the first: drains the FRONT of the FIFO (200 before 300 — in ORDER, so 20
    // rapid clicks become 20 in-order moves, not one latest-wins collapse).
    let drained = q.finish().expect("a queued SET is pending");
    assert_eq!(drained.iso, Some(200), "FIFO front, not latest-wins");
    let drained = q.finish().expect("a second queued SET is pending");
    assert_eq!(drained.iso, Some(300));
    // Worker finishes the last drained one: nothing left -> queue idle again.
    assert!(q.finish().is_none());
    assert_eq!(q.queued_total(), 2);
    // Idle again: the next SET runs immediately.
    assert!(matches!(q.submit(req_iso(400)), SubmitAction::RunNow(_)));
}

#[test]
fn set_queue_abort_clears_in_flight_and_whole_fifo() {
    let mut q = SetQueue::default();
    assert!(matches!(q.submit(req_iso(1)), SubmitAction::RunNow(_)));
    assert_eq!(q.submit(req_iso(2)), SubmitAction::Queued { total: 1 });
    assert_eq!(q.submit(req_iso(3)), SubmitAction::Queued { total: 2 });
    // A write error drops both in-flight AND the WHOLE queued FIFO, returning every dropped
    // follow-up (in order) so their loss is reconstructible from the log.
    let dropped = q.abort();
    let dropped_isos: Vec<i64> = dropped.iter().filter_map(|r| r.iso).collect();
    assert_eq!(
        dropped_isos,
        vec![2, 3],
        "abort returns the whole FIFO in order"
    );
    assert!(q.abort().is_empty()); // idempotent — a second abort has nothing to drop
    assert!(q.finish().is_none()); // nothing pending
    assert!(matches!(q.submit(req_iso(4)), SubmitAction::RunNow(_))); // fresh start
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
