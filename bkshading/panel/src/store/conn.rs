//! The connection to the service, its pure decisions (issue 1343 + issue 808, ported from the JS
//! panel): the `/ws` push is the live channel, the HTTP poll of `/api/cameras` + `/api/layout` the
//! fallback while it is down. The panel counts as connected while the socket is open or the last
//! contact (a push or a poll) is younger than [`STALE_MS`]; otherwise the loud banner shows how long
//! ago the last contact was. The steppers stay enabled (a tap still fires a PUT): the banner is the
//! truth signal, not a lockout.

/// No contact for this long shows the banner.
pub const STALE_MS: f64 = 5000.0;
/// The fallback poll's period while the socket is down.
pub const POLL_MS: u32 = 2000;
/// The banner's counter ticks this often.
pub const TICK_MS: u32 = 500;
/// The first reconnect waits this long, doubling up to [`RECONNECT_MAX_MS`].
pub const RECONNECT_FIRST_MS: u32 = 1000;
pub const RECONNECT_MAX_MS: u32 = 15000;

/// Whether the panel has contact at `now`.
pub fn is_connected(ws_open: bool, last_contact: f64, now: f64) -> bool {
    ws_open || now - last_contact < STALE_MS
}

/// The banner's text, or none while connected.
pub fn banner_text(ws_open: bool, last_contact: f64, now: f64) -> Option<String> {
    if is_connected(ws_open, last_contact, now) {
        return None;
    }
    let age = ((now - last_contact) / 1000.0).floor().max(0.0) as u64;
    Some(format!(
        "⚠ Bez spojenia so službou · posledný kontakt pred {age} s · zobrazené hodnoty môžu byť staré"
    ))
}

/// The wait before reconnect attempt `attempt` (0 for the first).
pub fn reconnect_delay(attempt: u32) -> u32 {
    RECONNECT_FIRST_MS
        .saturating_mul(1u32 << attempt.min(8))
        .min(RECONNECT_MAX_MS)
}

/// The push socket's URL for a page served over `scheme` (`http:` / `https:`) from `host`. The
/// socket is the service's root `/ws`, wherever the panel itself is served.
pub fn ws_url(scheme: &str, host: &str) -> String {
    let ws = if scheme == "https:" { "wss" } else { "ws" };
    format!("{ws}://{host}/ws")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connected_while_open_or_recently_heard() {
        assert!(is_connected(true, 0.0, 1e9));
        assert!(is_connected(false, 1000.0, 5999.0));
        assert!(!is_connected(false, 1000.0, 6000.0));
    }

    #[test]
    fn the_banner_counts_seconds_since_the_last_contact() {
        assert_eq!(banner_text(true, 0.0, 1e6), None);
        assert_eq!(
            banner_text(false, 1000.0, 8500.0).unwrap(),
            "⚠ Bez spojenia so službou · posledný kontakt pred 7 s · zobrazené hodnoty môžu byť staré"
        );
    }

    #[test]
    fn reconnects_back_off_to_15_s() {
        assert_eq!(reconnect_delay(0), 1000);
        assert_eq!(reconnect_delay(1), 2000);
        assert_eq!(reconnect_delay(3), 8000);
        assert_eq!(reconnect_delay(4), 15000);
        assert_eq!(reconnect_delay(40), 15000);
    }

    #[test]
    fn the_socket_url_follows_the_page_scheme() {
        assert_eq!(ws_url("http:", "strih.lan:8770"), "ws://strih.lan:8770/ws");
        assert_eq!(
            ws_url("https:", "shading.newlevel.media"),
            "wss://shading.newlevel.media/ws"
        );
    }
}
