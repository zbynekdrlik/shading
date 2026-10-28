//! Self-registering relays (issue 808, owner ruling 28.10.2026: "to sa malo pripajat na shading
//! bridge a nie bridge na shading pc").
//!
//! A handheld SBC sits on venue WiFi and gets whatever DHCP address the venue hands out, so the
//! service must never need its IP. The SBC announces itself instead: `POST /api/relays/register`
//! `{"id": "<relay id>", "port": <relay port>}` every few seconds, and the service keeps the
//! address the announcement came FROM. A camera whose config `address` is `register:<relay id>`
//! is reached through that announced address while it is fresh ([`REGISTRATION_TTL`]); a stale
//! or missing registration reads as an unreachable relay (the panel greys the block out), never
//! a guessed address.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bkshading_proto::RegistrationView;

/// The config `address` prefix that names a self-registering relay by its id.
pub const REGISTER_PREFIX: &str = "register:";

/// A registration older than this no longer resolves (the relay announces every ~10 s, so three
/// missed announcements in a row).
pub const REGISTRATION_TTL: Duration = Duration::from_secs(35);

/// What a relay sends to announce itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Registration {
    pub id: String,
    pub port: u16,
}

/// The relay id a config address names (`register:<id>`), or `None` for a plain `host:port`.
pub fn registered_id(address: &str) -> Option<&str> {
    address
        .strip_prefix(REGISTER_PREFIX)
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

/// A relay id is 1-64 chars of `[A-Za-z0-9._-]` (a hostname-like name): anything else is refused,
/// so an announcement can never inject text into a log line or an address.
pub fn valid_relay_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The `host:port` an announcement from `peer` resolves to (an IPv6 peer is bracketed).
pub fn announced_address(peer: IpAddr, port: u16) -> String {
    match peer {
        IpAddr::V4(v4) => format!("{v4}:{port}"),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => format!("{v4}:{port}"),
            None => format!("[{v6}]:{port}"),
        },
    }
}

/// One relay's registration: where its last announcement came from, when, and since when it has
/// announced without a gap longer than [`REGISTRATION_TTL`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    address: String,
    last: Instant,
    since: Instant,
}

/// The live registrations, shared between the HTTP handler and the aggregator.
#[derive(Debug, Clone, Default)]
pub struct RelayRegistry {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

impl RelayRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an announcement at `now`. Returns the previous address when it changed (the caller
    /// logs a move once), `None` for a new or unchanged relay. An announcement after a gap longer
    /// than [`REGISTRATION_TTL`] starts a new announcing streak.
    pub fn register(&self, id: &str, address: String, now: Instant) -> Option<String> {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let previous = map.get(id).cloned();
        let since = match &previous {
            Some(e) if now.saturating_duration_since(e.last) <= REGISTRATION_TTL => e.since,
            _ => now,
        };
        map.insert(
            id.to_string(),
            Entry {
                address: address.clone(),
                last: now,
                since,
            },
        );
        previous.and_then(|e| (e.address != address).then_some(e.address))
    }

    /// The fresh announced address of relay `id` at `now`, else `None`.
    pub fn lookup(&self, id: &str, now: Instant) -> Option<String> {
        self.status(id, now).filter(|s| s.fresh).map(|s| s.address)
    }

    /// The panel's view of relay `id` at `now`, stale or fresh; `None` if it never announced.
    pub fn status(&self, id: &str, now: Instant) -> Option<RegistrationView> {
        let map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        map.get(id).map(|e| {
            let age = now.saturating_duration_since(e.last);
            RegistrationView {
                relay_id: id.to_string(),
                address: e.address.clone(),
                age_s: age.as_secs(),
                announcing_s: e.last.saturating_duration_since(e.since).as_secs(),
                fresh: age <= REGISTRATION_TTL,
            }
        })
    }

    /// The registration view for a camera configured with `address`: `None` for a fixed
    /// `host:port`, or a `register:<id>` relay that never announced.
    pub fn view_for(&self, address: &str, now: Instant) -> Option<RegistrationView> {
        registered_id(address).and_then(|id| self.status(id, now))
    }

    /// The address to reach a camera configured with `address`: a plain `host:port` as is, a
    /// `register:<id>` through its fresh registration, else `None` (unreachable).
    pub fn resolve(&self, address: &str, now: Instant) -> Option<String> {
        match registered_id(address) {
            Some(id) => self.lookup(id, now),
            None => Some(address.to_string()),
        }
    }
}
