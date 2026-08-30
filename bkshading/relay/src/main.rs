//! `bkshading-relay` — the cambox/SBC USB relay (issue 808, M1).
//!
//! Runs ON the box the camera's USB cable is plugged into (a cambox such as cam1, or a
//! mini SBC on a handheld cage). Drives the local Blackmagic camera over USB-PTP via the
//! `gphoto2` CLI and exposes its shading over a small HTTP API that the `bkshading`
//! aggregation service polls. Multiplatform / ARM-friendly by design (pure `std` process
//! spawning — no `libgphoto2` link).
//!
//! M1 note: `gphoto2` is a RUNTIME dependency and is not yet installed on cam1 (verified
//! read-only). The relay is not run against the live cam1 during the ongoing E2E; its
//! logic is fully unit-tested with a fake runner (`tests/relay.rs`).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use bkshading_relay::http;
use bkshading_relay::transport::{
    parse_capture_fps_env, parse_min_read_interval_env, CameraSession, Gphoto2Cli,
    DEFAULT_MIN_READ_INTERVAL_MS,
};
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

    /// Path to the `gphoto2` binary.
    #[arg(long, default_value = "gphoto2")]
    gphoto2: String,
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

    // issue 1229: the read-throttle floor makes the relay bus-friendly on a production cambox —
    // `/api/state` is served from a cache between real gphoto2 reads so the shared USB bus sees at
    // most one PTP session per floor (the default is bus-friendly; env only tunes it, never off).
    let min_read_interval_ms =
        parse_min_read_interval_env(std::env::var("BKSHADING_RELAY_MIN_READ_INTERVAL_MS").ok())
            .unwrap_or(DEFAULT_MIN_READ_INTERVAL_MS);

    let session = Arc::new(
        CameraSession::new(
            Box::new(Gphoto2Cli {
                binary: args.gphoto2,
            }),
            VERSION,
        )
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
