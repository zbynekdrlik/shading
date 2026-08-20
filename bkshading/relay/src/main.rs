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
use bkshading_relay::transport::{CameraSession, Gphoto2Cli};
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

    let session = Arc::new(CameraSession::new(
        Box::new(Gphoto2Cli {
            binary: args.gphoto2,
        }),
        VERSION,
    ));

    tracing::info!(version = VERSION, %addr, "bkshading-relay starting");
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
