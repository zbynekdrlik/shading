//! The relay announces itself to the venue's shading service (issue 808, owner ruling 28.10.2026:
//! "to sa malo pripajat na shading bridge a nie bridge na shading pc").
//!
//! A handheld SBC sits on venue WiFi with whatever DHCP address it got, so the service must never
//! need that address. With `--register-url http://strih.lan:8770/api/relays/register` the relay
//! POSTs `{"id": <relay id>, "port": <its HTTP port>}` there every [`REGISTER_INTERVAL`]; the
//! service keeps the address the request came FROM (`register:<id>` in its camera config). A
//! cambox relay keeps its fixed fleet address and runs without the flag.
//!
//! Plain HTTP/1.1 over a tokio `TcpStream` (the relay carries no HTTP client crate: the arm64
//! cross-build stays dependency-light). Every failure is logged once per change and retried on
//! the next tick; nothing here can stop the relay serving its API.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// How often the relay announces itself (the service forgets a relay after ~35 s of silence).
pub const REGISTER_INTERVAL: Duration = Duration::from_secs(10);

/// One announcement's whole budget: connect, write, read the status line.
pub const REGISTER_TIMEOUT: Duration = Duration::from_secs(5);

/// The parsed `http://host[:port]/path` the relay announces to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterTarget {
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// Parse a plain-`http` register URL. `https` and anything malformed are refused by name: the
/// announcement only ever goes to the venue LAN.
pub fn parse_register_url(url: &str) -> Result<RegisterTarget, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("register url must start with http:// (got {url:?})"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/api/relays/register"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>()
                .map_err(|_| format!("bad port in register url {url:?}"))?,
        ),
        None => (authority, 80),
    };
    if host.is_empty()
        || port == 0
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
    {
        return Err(format!("bad host in register url {url:?}"));
    }
    Ok(RegisterTarget {
        host: host.to_string(),
        port,
        path: path.to_string(),
    })
}

/// A relay id is 1-64 chars of `[A-Za-z0-9._-]` (the service refuses anything else).
pub fn valid_relay_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The exact HTTP/1.1 request bytes of one announcement.
pub fn register_request(target: &RegisterTarget, id: &str, relay_port: u16) -> String {
    let body = serde_json::json!({ "id": id, "port": relay_port }).to_string();
    format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\n\
         Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        path = target.path,
        host = target.host,
        port = target.port,
        len = body.len(),
    )
}

/// The status code of an HTTP/1.x response's first line, else `None`.
pub fn response_status(head: &str) -> Option<u16> {
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    parts.next().filter(|v| v.starts_with("HTTP/1."))?;
    parts.next()?.parse().ok()
}

/// One announcement: Ok(status) for any HTTP answer, Err(text) for a transport failure.
pub async fn announce_once(
    target: &RegisterTarget,
    id: &str,
    relay_port: u16,
) -> Result<u16, String> {
    let work = async {
        let mut stream = TcpStream::connect((target.host.as_str(), target.port))
            .await
            .map_err(|e| format!("connect {}:{}: {e}", target.host, target.port))?;
        stream
            .write_all(register_request(target, id, relay_port).as_bytes())
            .await
            .map_err(|e| format!("write: {e}"))?;
        let mut buf = vec![0u8; 512];
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|e| format!("read: {e}"))?;
        response_status(&String::from_utf8_lossy(&buf[..n]))
            .ok_or_else(|| "no HTTP status line in the answer".to_string())
    };
    tokio::time::timeout(REGISTER_TIMEOUT, work)
        .await
        .map_err(|_| format!("no answer within {} s", REGISTER_TIMEOUT.as_secs()))?
}

/// The announcement outcome a log line is written for: logged only when it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Accepted,
    Refused(u16),
    Failed(String),
}

/// Classify one [`announce_once`] result.
pub fn outcome(result: &Result<u16, String>) -> Outcome {
    match result {
        Ok(s) if (200..300).contains(s) => Outcome::Accepted,
        Ok(s) => Outcome::Refused(*s),
        Err(e) => Outcome::Failed(e.clone()),
    }
}

/// Announce forever, every [`REGISTER_INTERVAL`], logging each change of outcome once.
pub async fn run(target: RegisterTarget, id: String, relay_port: u16) {
    let mut last: Option<Outcome> = None;
    let mut ticker = tokio::time::interval(REGISTER_INTERVAL);
    loop {
        ticker.tick().await;
        let now = outcome(&announce_once(&target, &id, relay_port).await);
        if last.as_ref() != Some(&now) {
            match &now {
                Outcome::Accepted => tracing::info!(
                    id = %id, host = %target.host, port = target.port,
                    "registered with the shading service"
                ),
                Outcome::Refused(s) => tracing::warn!(
                    id = %id, host = %target.host, status = s,
                    "shading service refused the registration"
                ),
                Outcome::Failed(e) => tracing::warn!(
                    id = %id, host = %target.host, error = %e,
                    "cannot reach the shading service to register"
                ),
            }
            last = Some(now);
        }
    }
}
