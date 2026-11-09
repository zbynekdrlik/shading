//! The relay's camera transport since shading#6: Blackmagic's REST API
//! (`http://<camera>/control/api/v1/...`) over the camera's own USB-C CDC-NCM link.
//!
//! Plain HTTP/1.1 over a blocking `std::net::TcpStream`, the same no-client-crate approach as the
//! relay's register path (`register.rs`), so the arm64 cross-build stays dependency-light. The
//! camera transport runs on blocking threads (`spawn_blocking`), hence std, not tokio. Every
//! request is `Connection: close` and bounded by one deadline (connect + write + read), so a camera
//! that stops answering can never hold a read cycle or a write. Bodies may come with a
//! `Content-Length` or `Transfer-Encoding: chunked`, or end at the close.
//!
//! Endpoints and JSON shapes are `bkshading_proto::rest`; the pure assembly is
//! `bkshading_proto::read`.

use std::fmt;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use bkshading_proto::read::RestWrite;
use bkshading_proto::rest::{self, Product, RestRead};
use serde::de::DeserializeOwned;

/// The camera's HTTP port (it also serves HTTPS on 443; the relay carries no TLS).
pub const CAMERA_HTTP_PORT: u16 = 80;

/// One request's whole budget: connect, send, read the answer. A camera on its own USB link
/// answers in milliseconds; a read cycle is 13 requests and the service polls `/api/state` with a
/// 1.5 s timeout, so a camera that stops answering must fail fast.
pub const REST_TIMEOUT: Duration = Duration::from_millis(1000);

/// The largest answer the relay reads (the biggest, `supportedFormats`, is a few KB).
pub const MAX_RESPONSE_BYTES: usize = 1 << 20;

/// Where the camera's REST API answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraAddr {
    pub host: String,
    pub port: u16,
}

impl fmt::Display for CameraAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

/// Parses `host`, `host:port` or `http://host[:port][/]` (the `--camera` flag). Only plain
/// `http`; the host is an IPv4 address or a DNS name.
pub fn parse_camera_addr(raw: &str) -> Result<CameraAddr, String> {
    let t = raw.trim();
    let rest = t.strip_prefix("http://").unwrap_or(t).trim_end_matches('/');
    if rest.contains("://") || rest.contains('/') {
        return Err(format!(
            "camera address must be host[:port] or http://host[:port] (got {raw:?})"
        ));
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>()
                .map_err(|_| format!("bad port in camera address {raw:?}"))?,
        ),
        None => (rest, CAMERA_HTTP_PORT),
    };
    if host.is_empty()
        || port == 0
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
    {
        return Err(format!("bad host in camera address {raw:?}"));
    }
    Ok(CameraAddr {
        host: host.to_string(),
        port,
    })
}

/// One HTTP answer: status and body text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// Why a REST call did not give a usable value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestError {
    /// No HTTP answer at all: connect refused, timeout, a reset, a malformed response. The camera
    /// is gone or not a camera — a read cycle stops at the first one.
    Transport(String),
    /// The camera answered with a non-2xx status (not implemented, refused, out of range).
    Status { status: u16, body: String },
    /// A 2xx answer whose body is not the JSON this endpoint documents.
    Decode(String),
}

impl fmt::Display for RestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RestError::Transport(e) => write!(f, "no answer: {e}"),
            RestError::Status { status, body } => {
                write!(f, "status {status}: {}", body_tail(body))
            }
            RestError::Decode(e) => write!(f, "undecodable answer: {e}"),
        }
    }
}

impl std::error::Error for RestError {}

/// The first 200 characters of a body, on one line (for logs and error messages).
pub fn body_tail(body: &str) -> String {
    let one: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 200 {
        format!("{}…", one.chars().take(200).collect::<String>())
    } else {
        one
    }
}

/// The exact request bytes of one call.
pub fn build_request(method: &str, addr: &CameraAddr, path: &str, body: Option<&str>) -> String {
    let mut req = format!(
        "{method} {prefix}{path} HTTP/1.1\r\nHost: {addr}\r\nAccept: application/json\r\n\
         Connection: close\r\n",
        prefix = rest::API_PREFIX,
    );
    match body {
        Some(b) => req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}",
            b.len()
        )),
        None => req.push_str("Content-Length: 0\r\n\r\n"),
    }
    req
}

/// Where the header block ends (the index just past `\r\n\r\n`).
fn header_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// Decodes a `Transfer-Encoding: chunked` body. `Ok(None)` = not complete yet (the terminating
/// zero-size chunk has not arrived).
pub fn decode_chunked(mut data: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let mut out = Vec::new();
    loop {
        let Some(line_end) = data.windows(2).position(|w| w == b"\r\n") else {
            return Ok(None);
        };
        let size_line = std::str::from_utf8(&data[..line_end])
            .map_err(|_| "chunk size line is not text".to_string())?;
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| format!("bad chunk size {size_hex:?}"))?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(Some(out));
        }
        if data.len() < size + 2 {
            return Ok(None);
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

/// Parses a raw HTTP/1.x response. `Ok(None)` = not complete yet; `eof` = the peer closed, so a
/// body without a length ends here.
pub fn parse_response(raw: &[u8], eof: bool) -> Result<Option<HttpResponse>, String> {
    let Some(end) = header_end(raw) else {
        return if eof {
            Err(format!(
                "connection closed before the headers ended ({} bytes)",
                raw.len()
            ))
        } else {
            Ok(None)
        };
    };
    let head = String::from_utf8_lossy(&raw[..end]);
    let status = {
        let line = head.lines().next().unwrap_or("");
        let mut parts = line.split_whitespace();
        let version = parts.next().unwrap_or("");
        if !version.starts_with("HTTP/1.") {
            return Err(format!("not an HTTP/1.x status line: {line:?}"));
        }
        parts
            .next()
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| format!("no status code in {line:?}"))?
    };
    let body = &raw[end..];
    let chunked = header_value(&head, "transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let bytes: Vec<u8> = if chunked {
        match decode_chunked(body)? {
            Some(b) => b,
            None if eof => return Err("connection closed inside a chunked body".into()),
            None => return Ok(None),
        }
    } else if let Some(len) = header_value(&head, "content-length") {
        let len: usize = len
            .parse()
            .map_err(|_| format!("bad Content-Length {len:?}"))?;
        if body.len() < len {
            return if eof {
                Err(format!(
                    "connection closed after {} of {len} body bytes",
                    body.len()
                ))
            } else {
                Ok(None)
            };
        }
        body[..len].to_vec()
    } else if status == 204 || status == 304 || (100..200).contains(&status) {
        Vec::new()
    } else if eof {
        body.to_vec()
    } else {
        return Ok(None);
    };
    Ok(Some(HttpResponse {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }))
}

/// The JSON body of a write (`None` = a bodiless PUT, `doAuto`).
pub fn write_body(w: &RestWrite) -> Option<String> {
    let v = match w {
        RestWrite::FrameRate(label) => serde_json::json!({ "frameRate": label }),
        RestWrite::ApertureStop(f) => serde_json::json!({ "apertureStop": f }),
        RestWrite::Iso(n) => serde_json::json!({ "iso": n }),
        RestWrite::ShutterSpeed(n) => serde_json::json!({ "shutterSpeed": n }),
        RestWrite::ShutterAngle(n) => serde_json::json!({ "shutterAngle": n }),
        RestWrite::WhiteBalance(k) => serde_json::json!({ "whiteBalance": k }),
        RestWrite::Tint(t) => serde_json::json!({ "whiteBalanceTint": t }),
        RestWrite::AutoWhiteBalance => return None,
    };
    Some(v.to_string())
}

/// A REST client for one camera address.
#[derive(Debug, Clone)]
pub struct RestClient {
    pub addr: CameraAddr,
    pub timeout: Duration,
}

impl RestClient {
    pub fn new(addr: CameraAddr, timeout: Duration) -> Self {
        RestClient { addr, timeout }
    }

    fn socket_addr(&self) -> Result<SocketAddr, String> {
        (self.addr.host.as_str(), self.addr.port)
            .to_socket_addrs()
            .map_err(|e| format!("resolve {}: {e}", self.addr))?
            .next()
            .ok_or_else(|| format!("resolve {}: no address", self.addr))
    }

    /// One HTTP exchange inside [`RestClient::timeout`]. `Err` only for a transport failure.
    pub fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<HttpResponse, String> {
        let deadline = Instant::now() + self.timeout;
        let left = |what: &str| {
            deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or_else(|| format!("{what}: no answer within {} ms", self.timeout.as_millis()))
        };
        let sa = self.socket_addr()?;
        let mut stream = TcpStream::connect_timeout(&sa, left("connect")?)
            .map_err(|e| format!("connect {}: {e}", self.addr))?;
        stream
            .set_write_timeout(Some(left("write")?))
            .map_err(|e| format!("set write timeout: {e}"))?;
        stream
            .write_all(build_request(method, &self.addr, path, body).as_bytes())
            .map_err(|e| format!("write: {e}"))?;
        let mut raw: Vec<u8> = Vec::with_capacity(4096);
        let mut buf = [0u8; 4096];
        loop {
            stream
                .set_read_timeout(Some(left("read")?))
                .map_err(|e| format!("set read timeout: {e}"))?;
            let n = match stream.read(&mut buf) {
                Ok(n) => n,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(format!(
                        "read: no complete answer within {} ms ({} bytes so far)",
                        self.timeout.as_millis(),
                        raw.len()
                    ))
                }
                Err(e) => return Err(format!("read: {e}")),
            };
            raw.extend_from_slice(&buf[..n]);
            if raw.len() > MAX_RESPONSE_BYTES {
                return Err(format!("answer larger than {MAX_RESPONSE_BYTES} bytes"));
            }
            if let Some(resp) = parse_response(&raw, n == 0)? {
                return Ok(resp);
            }
        }
    }

    /// `GET` an endpoint and decode its documented JSON.
    pub fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, RestError> {
        let started = Instant::now();
        let resp = self
            .request("GET", path, None)
            .map_err(RestError::Transport)?;
        tracing::debug!(
            camera = %self.addr, path, status = resp.status,
            duration_ms = started.elapsed().as_millis() as u64,
            body = %body_tail(&resp.body),
            "camera REST GET"
        );
        if !(200..300).contains(&resp.status) {
            return Err(RestError::Status {
                status: resp.status,
                body: resp.body,
            });
        }
        serde_json::from_str(&resp.body).map_err(|e| RestError::Decode(format!("{path}: {e}")))
    }

    /// `GET system/product` — the probe that tells a camera from anything else at an address.
    pub fn product(&self) -> Result<Product, RestError> {
        self.get_json(rest::PRODUCT)
    }

    /// One read cycle: every [`rest::READ_ENDPOINTS`] in order. A transport failure ends the cycle
    /// (`Err`: the camera is gone, the caller goes offline); an endpoint that answers an error
    /// status or an undecodable body is just missing this cycle (logged), never a failed read.
    pub fn read_all(&self) -> Result<RestRead, String> {
        fn one<T: DeserializeOwned>(
            c: &RestClient,
            path: &str,
            missing: &mut Vec<String>,
        ) -> Result<Option<T>, String> {
            match c.get_json::<T>(path) {
                Ok(v) => Ok(Some(v)),
                Err(RestError::Transport(e)) => Err(format!("GET {path}: {e}")),
                Err(e) => {
                    missing.push(format!("{path} ({e})"));
                    Ok(None)
                }
            }
        }
        let started = Instant::now();
        let mut missing: Vec<String> = Vec::new();
        let m = &mut missing;
        let read = RestRead {
            iso: one(self, rest::ISO, m)?,
            supported_isos: one(self, rest::SUPPORTED_ISOS, m)?,
            shutter: one(self, rest::SHUTTER, m)?,
            supported_shutters: one(self, rest::SUPPORTED_SHUTTERS, m)?,
            white_balance: one(self, rest::WHITE_BALANCE, m)?,
            white_balance_description: one(self, rest::WHITE_BALANCE_DESCRIPTION, m)?,
            tint: one(self, rest::TINT, m)?,
            iris: one(self, rest::IRIS, m)?,
            iris_description: one(self, rest::IRIS_DESCRIPTION, m)?,
            focus: one(self, rest::FOCUS, m)?,
            format: one(self, rest::FORMAT, m)?,
            supported_formats: one(self, rest::SUPPORTED_FORMATS, m)?,
        };
        tracing::info!(
            camera = %self.addr,
            endpoints = rest::READ_ENDPOINTS.len(),
            missing = missing.len(),
            duration_ms = started.elapsed().as_millis() as u64,
            "camera REST read cycle"
        );
        if !missing.is_empty() {
            tracing::debug!(camera = %self.addr, missing = ?missing, "camera REST endpoints without a value this cycle");
        }
        Ok(read)
    }

    /// One planned write as a `PUT`, logged at info with its body, status and duration.
    pub fn write(&self, w: &RestWrite) -> Result<(), RestError> {
        let path = w.path();
        let body = write_body(w);
        let started = Instant::now();
        let result = self.request("PUT", path, body.as_deref());
        let duration_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(resp) if (200..300).contains(&resp.status) => {
                tracing::info!(
                    camera = %self.addr, path, body = body.as_deref().unwrap_or(""),
                    status = resp.status, duration_ms, "camera REST PUT"
                );
                Ok(())
            }
            Ok(resp) => {
                tracing::warn!(
                    camera = %self.addr, path, body = body.as_deref().unwrap_or(""),
                    status = resp.status, duration_ms, answer = %body_tail(&resp.body),
                    "camera REST PUT refused"
                );
                Err(RestError::Status {
                    status: resp.status,
                    body: resp.body,
                })
            }
            Err(e) => {
                tracing::warn!(
                    camera = %self.addr, path, body = body.as_deref().unwrap_or(""),
                    duration_ms, error = %e, "camera REST PUT failed"
                );
                Err(RestError::Transport(e))
            }
        }
    }
}
