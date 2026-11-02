//! Unit tests for the issue-808 WebSocket state-push envelope (`ServerMsg`). The service
//! pushes the whole aggregate to every connected panel over `/ws`; the envelope must be the
//! internally-tagged `{"type":"state", ...aggregate...}` shape the verified dev2 MVP web UI
//! already speaks (`{"type":"state",...}`), so the wire stays compatible and the browser can
//! reuse its existing `render(agg)` on the flattened payload. Pure serde — no IO, no server.

use bkshading_proto::wire::{Aggregate, ServerMsg};

#[test]
fn state_envelope_is_internally_tagged_state() {
    // An empty-camera aggregate keeps this test independent of the CameraView/RelayState
    // struct shape (which other issues extend) — it exercises ONLY the envelope tagging.
    let msg = ServerMsg::State(Aggregate {
        version: "1.7.0-dev.530".into(),
        cameras: vec![],
    });
    let json = serde_json::to_string(&msg).unwrap();
    // Internally tagged: the discriminator is a `type` field alongside the flattened aggregate
    // fields — NOT a nested object — so the browser reads `msg.version`/`msg.cameras` directly.
    assert!(json.contains("\"type\":\"state\""), "tagged state: {json}");
    assert!(
        json.contains("\"version\":\"1.7.0-dev.530\""),
        "flattened version: {json}"
    );
    assert!(json.contains("\"cameras\":[]"), "flattened cameras: {json}");
    // The aggregate must NOT be nested under a wrapper key (that would break the flattened
    // dev2-MVP shape the panel expects).
    assert!(
        !json.contains("\"state\":{") && !json.contains("\"aggregate\":"),
        "aggregate is flattened, not nested: {json}"
    );
}

#[test]
fn state_envelope_round_trips() {
    let msg = ServerMsg::State(Aggregate {
        version: "v".into(),
        cameras: vec![],
    });
    let json = serde_json::to_string(&msg).unwrap();
    let back: ServerMsg = serde_json::from_str(&json).unwrap();
    assert_eq!(back, msg);
}
