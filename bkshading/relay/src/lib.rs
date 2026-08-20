//! `bkshading-relay` library surface — the cambox/SBC USB relay logic (issue 808).
//!
//! Split into a lib so the transport/HTTP logic is reachable from integration tests
//! (`tests/relay.rs`) with a fake gphoto2 runner, no camera required. The `bkshading-relay`
//! binary (`src/main.rs`) is a thin wrapper over this.

pub mod http;
pub mod transport;
