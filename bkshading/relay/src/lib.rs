//! `bkshading-relay` library surface — the cambox/SBC USB relay logic (issue 808).
//!
//! Split into a lib so the transport/HTTP logic is reachable from integration tests
//! (`tests/rest_camera_6.rs`) against a local fake camera REST server, no camera required. The
//! `bkshading-relay` binary (`src/main.rs`) is a thin wrapper over this.

pub mod http;
pub mod register;
pub mod transport;
