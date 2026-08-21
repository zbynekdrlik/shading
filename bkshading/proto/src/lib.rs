//! `bkshading-proto` — the shared, IO-free core of the bkshading shading-control
//! system (issue 808).
//!
//! Depended on by BOTH the `bkshading` aggregation service and the `bkshading-relay`, so
//! the wire protocol and the byte-verified Blackmagic PTP mapping have exactly ONE source
//! of truth (the owner explicitly flagged the MVP's Python-vs-Kotlin duplicate-truth
//! drift). Nothing here does process IO or networking:
//!   - [`mapping`] — pure PTP<->wire conversions ported byte-for-byte from the dev2 MVP
//!     `pybridge/mapping.py`.
//!   - [`read`] — pure assembly of live state from raw gphoto2 text + pure write planning.
//!   - [`wire`] — the serde JSON types exchanged between web panel, service, and relay.

pub mod mapping;
pub mod read;
pub mod wire;

pub use read::{params_and_caps, plan_writes, RawConfigs};
pub use wire::{
    resolve_grab, Aggregate, CameraCaps, CameraView, FpsSync, GrabResolution, RelayState,
    ServerMsg, SetRequest, ShadingParams, Transport,
};

/// The bkshading protocol version — bumped when the wire types change incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;
