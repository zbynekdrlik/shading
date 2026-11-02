//! Issue 808 (owner ruling 28.10.2026): the relay's self-registration, pure parts + one real
//! announcement over a socket.

use bkshading_relay::register::{
    announce_once, outcome, parse_register_url, register_request, response_status, valid_relay_id,
    Outcome, RegisterTarget,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn a_register_url_is_plain_http_to_a_lan_host_808() {
    assert_eq!(
        parse_register_url("http://strih.lan:8770/api/relays/register").unwrap(),
        RegisterTarget {
            host: "strih.lan".into(),
            port: 8770,
            path: "/api/relays/register".into()
        }
    );
    // No path: the service's register path; no port: 80.
    assert_eq!(
        parse_register_url("http://10.77.8.202:8770").unwrap().path,
        "/api/relays/register"
    );
    assert_eq!(parse_register_url("http://strih.lan/x").unwrap().port, 80);
    for bad in [
        "https://strih.lan:8770/",
        "strih.lan:8770",
        "http://:8770/",
        "http://strih.lan:0/",
        "http://strih.lan:99999/",
        "http://str ih.lan:8770/",
        "http://user@strih.lan:8770/",
    ] {
        assert!(parse_register_url(bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn the_announcement_is_one_json_post_with_its_length_808() {
    let t = parse_register_url("http://strih.lan:8770/api/relays/register").unwrap();
    let req = register_request(&t, "handheld-1", 8771);
    let (head, body) = req.split_once("\r\n\r\n").unwrap();
    assert!(head.starts_with("POST /api/relays/register HTTP/1.1\r\n"));
    assert!(head.contains("\r\nHost: strih.lan:8770"));
    assert!(head.contains("\r\nContent-Type: application/json"));
    assert!(head.contains(&format!("\r\nContent-Length: {}", body.len())));
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v, serde_json::json!({"id": "handheld-1", "port": 8771}));
}

#[test]
fn status_lines_and_ids_808() {
    assert_eq!(response_status("HTTP/1.1 200 OK\r\n"), Some(200));
    assert_eq!(response_status("HTTP/1.0 400 Bad Request"), Some(400));
    assert_eq!(response_status("SSH-2.0-OpenSSH"), None);
    assert_eq!(response_status(""), None);
    assert!(valid_relay_id("handheld-1"));
    assert!(!valid_relay_id("handheld 1"));
    assert!(!valid_relay_id(""));
    assert_eq!(outcome(&Ok(200)), Outcome::Accepted);
    assert_eq!(outcome(&Ok(400)), Outcome::Refused(400));
    assert_eq!(outcome(&Err("x".into())), Outcome::Failed("x".into()));
}

#[tokio::test]
async fn one_announcement_reaches_a_listener_and_reads_its_status_808() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = sock.read(&mut buf).await.unwrap();
        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        String::from_utf8_lossy(&buf[..n]).into_owned()
    });
    let t = parse_register_url(&format!("http://127.0.0.1:{port}/api/relays/register")).unwrap();
    assert_eq!(announce_once(&t, "handheld-1", 8771).await, Ok(200));
    let got = server.await.unwrap();
    assert!(got.ends_with(r#"{"id":"handheld-1","port":8771}"#), "{got}");
}

#[tokio::test]
async fn no_service_is_a_named_failure_never_a_hang_808() {
    // A port nothing listens on: bind, read the port, drop the listener.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let t = parse_register_url(&format!("http://127.0.0.1:{port}/")).unwrap();
    let r = announce_once(&t, "handheld-1", 8771).await;
    assert!(matches!(&r, Err(e) if e.contains("connect")), "{r:?}");
}
