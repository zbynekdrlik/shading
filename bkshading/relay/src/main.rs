//! `bkshading-relay` — the cambox/SBC camera relay (issue 808, shading#6).
//!
//! Runs ON the box the camera's USB-C cable is plugged into (a cambox such as cam1, or a
//! mini SBC on a handheld cage). Drives the local Blackmagic camera through the official
//! Blackmagic REST API over the camera's own USB CDC-NCM network link (shading#6; it used the
//! `gphoto2` CLI over PTP before) and exposes its shading over a small HTTP API that the
//! `bkshading` aggregation service polls. Pure Rust, no C dependency, so it cross-builds for the
//! arm64 SBC.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use bkshading_relay::transport::{
    parse_camera_addr, parse_capture_fps_env, parse_min_read_interval_env, CameraLocator,
    CameraSession, DEFAULT_MIN_READ_INTERVAL_MS,
};
use bkshading_relay::{http, register};
use clap::Parser;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[command(
    name = "bkshading-relay",
    version,
    about = "Cambox/SBC USB relay for Blackmagic shading (issue 808)"
)]
struct Args {
    /// Address:port to bind the relay HTTP API on.
    #[arg(long, default_value = "0.0.0.0:8771")]
    bind: String,

    /// The camera's REST address, `host[:port]` (shading#6). Unset: the relay finds the camera
    /// itself — the DHCP server on its `cdc_ncm` USB network link (systemd-networkd's lease).
    /// Also read from `BKSHADING_CAMERA`.
    #[arg(long, env = "BKSHADING_CAMERA")]
    camera: Option<String>,

    /// Announce this relay to the venue shading service at this URL (issue 808), e.g.
    /// `http://strih.lan:8770/api/relays/register`. A handheld SBC on venue WiFi uses it so the
    /// service never needs the SBC's DHCP address; a cambox relay runs without it. Also read from
    /// `BKSHADING_REGISTER_URL`.
    #[arg(long, env = "BKSHADING_REGISTER_URL")]
    register_url: Option<String>,

    /// The id announced with `--register-url` (default: this box's hostname, e.g. handheld-1).
    #[arg(long, env = "BKSHADING_RELAY_ID")]
    relay_id: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let addr: SocketAddr = args.bind.parse()?;

    // issue 809: report the box's actual capture-mode fps to the service. The relay runs on the
    // cambox, so it reads the SAME CAMERA_BOX_CAPTURE_FPS the appliance uses to request its grab
    // rate — no appliance change. Unset -> None -> the service falls back to the static config.
    let capture_fps = parse_capture_fps_env(std::env::var("CAMERA_BOX_CAPTURE_FPS").ok());

    // issue 1229: the read floor keeps the relay quiet on the camera's USB link: `/api/state` is
    // served from a cache between real reads (the default is bus-friendly; env only tunes it).
    let min_read_interval_ms =
        parse_min_read_interval_env(std::env::var("BKSHADING_RELAY_MIN_READ_INTERVAL_MS").ok())
            .unwrap_or(DEFAULT_MIN_READ_INTERVAL_MS);

    // shading#6: a fixed camera address when given, else discovery on the cdc_ncm link.
    let locator = match args.camera.as_deref().filter(|c| !c.trim().is_empty()) {
        Some(raw) => CameraLocator::Fixed(parse_camera_addr(raw).map_err(anyhow::Error::msg)?),
        None => CameraLocator::discover(),
    };
    tracing::info!(?locator, "camera locator");
    let session = Arc::new(
        CameraSession::new(locator, VERSION)
            .with_capture_fps(capture_fps)
            .with_min_read_interval_ms(min_read_interval_ms),
    );

    tracing::info!(
        version = VERSION,
        %addr,
        ?capture_fps,
        min_read_interval_ms,
        "bkshading-relay starting"
    );
    // issue 1309: log the startup camera-detect result so the journal records, at boot, whether a
    // camera answers — the relay logs every REST write + online/offline transition from here on.
    {
        let s = session.clone();
        let detected = tokio::task::spawn_blocking(move || s.detect())
            .await
            .unwrap_or(None);
        tracing::info!(camera = ?detected, "startup camera detect");
    }
    // issue 808 (owner 28.10.2026): a handheld announces itself to the shading service instead of
    // the service dialling a fixed SBC address. A bad flag fails the start by name.
    if let Some(url) = args
        .register_url
        .as_deref()
        .filter(|u| !u.trim().is_empty())
    {
        let target = register::parse_register_url(url.trim()).map_err(anyhow::Error::msg)?;
        let id = match args.relay_id.filter(|i| !i.trim().is_empty()) {
            Some(id) => id.trim().to_string(),
            None => std::fs::read_to_string("/etc/hostname")
                .map(|h| h.trim().to_string())
                .unwrap_or_default(),
        };
        if !register::valid_relay_id(&id) {
            anyhow::bail!("invalid relay id {id:?} for --register-url (set --relay-id)");
        }
        tracing::info!(id = %id, host = %target.host, port = target.port, "announcing to the shading service");
        tokio::spawn(register::run(target, id, addr.port()));
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, http::router(session))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("bkshading-relay shutting down");
}
