//! `bkshading` — the shading-control aggregation service (issue 808, M1).
//!
//! Runs on the strih PC (Windows first, Linux after the frame-loss P0). Aggregates the
//! shading control of every configured camera (each reached through a `bkshading-relay` over
//! the LAN) into ONE operator web panel served at e.g. `strih.lan` — the 4+4 block layout
//! the owner specified (top: camera preview [placeholder in M1], bottom: shading parameters).
//! Multiplatform by design (pure axum/tokio/reqwest — no platform-specific deps).
//!
//! M1 scope: the service, its web panel skeleton, the config-driven camera list, and relay
//! aggregation. NDI preview (presenter tech) and cloudflare remote are M2+.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use bkshading::aggregator::Aggregator;
use bkshading::config::ServiceConfig;
use bkshading::http::{router, AppState};
use clap::Parser;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[command(
    name = "bkshading",
    version,
    about = "Camera shading-control aggregation service (issue 808)"
)]
struct Args {
    /// Path to the camera-list TOML config. If omitted, starts with an empty camera list.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Override the bind address from the config (host:port).
    #[arg(long)]
    bind: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let mut config = match &args.config {
        Some(path) => ServiceConfig::from_path(path)?,
        None => {
            tracing::warn!("no --config given; starting with an empty camera list");
            ServiceConfig::empty()
        }
    };
    if let Some(bind) = args.bind {
        config.bind = bind;
    }

    let addr: SocketAddr = config.bind.parse()?;

    // Start one preview worker per camera that has an NDI preview source (M2). Returns the
    // shared store the `preview.jpg` HTTP handler reads. Cameras without a preview name get
    // no worker (params-only blocks).
    let previews = bkshading::preview::start_all(&config);
    let preview_workers = config
        .cameras
        .iter()
        .filter(|c| c.ndi_preview.is_some())
        .count();

    let state = AppState {
        agg: Arc::new(Aggregator::new(VERSION)?),
        config: Arc::new(config),
        previews,
    };

    tracing::info!(
        version = VERSION,
        %addr,
        cameras = state.config.cameras.len(),
        preview_workers,
        "bkshading service starting"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("bkshading service shutting down");
}
