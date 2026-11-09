//! `bkshading-proto` — the shared, IO-free core of the bkshading shading-control
//! system (issue 808).
//!
//! Depended on by BOTH the `bkshading` aggregation service and the `bkshading-relay`, so
//! the wire protocol and the Blackmagic camera mapping have exactly ONE source
//! of truth (the owner explicitly flagged the MVP's Python-vs-Kotlin duplicate-truth
//! drift). Nothing here does process IO or networking:
//!   - [`mapping`] — pure camera-value conversions (aperture grid, shutter angle, frame rate).
//!   - [`rest`] — the Blackmagic camera REST API paths and JSON shapes (shading#6).
//!   - [`read`] — pure assembly of live state from the REST answers + pure write planning.
//!   - [`wire`] — the serde JSON types exchanged between web panel, service, and relay.

pub mod mapping;
pub mod read;
pub mod rest;
pub mod wire;

pub use read::{
    not_applied_keys, params_and_caps, plan_writes, FpsNotSettable, RestWrite, WriteBasis,
};
pub use rest::RestRead;
pub use wire::{
    resolve_grab, Aggregate, CameraCaps, CameraView, FpsSync, GrabResolution, RegistrationView,
    RelayState, ServerMsg, SetRequest, ShadingParams, Transport,
};

/// The bkshading protocol version — bumped when the wire types change incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;
