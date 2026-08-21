//! `bkshading` service library surface (issue 808) — config, relay aggregation, and the
//! web/JSON HTTP layer. Split into a lib so the pure config parsing and camera-view
//! assembly are reachable from integration tests (`tests/service.rs`).

pub mod aggregator;
pub mod config;
pub mod http;
pub mod monitor;
pub mod preview;
