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
use bkshading_proto::wire::FpsSync;
use clap::Parser;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How often the live-push pump snapshots the aggregate and publishes it to `/ws` clients
/// (issue 808 WS milestone). Matches the panel's former HTTP poll cadence (2 s) — shading is
/// about colour/exposure, not motion, so a couple of seconds is plenty.
const LIVE_PUSH_INTERVAL_MS: u64 = 2000;

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

    let config = Arc::new(config);
    let agg = Arc::new(Aggregator::new(VERSION)?);

    // Start one preview worker per camera that has an NDI preview source (M2). Returns the
    // shared store the `preview.jpg` HTTP handler reads. Cameras without a preview name get
    // no worker (params-only blocks).
    let previews = bkshading::preview::start_all(&config);
    let preview_workers = config
        .cameras
        .iter()
        .filter(|c| c.ndi_preview.is_some())
        .count();

    // Live-push pump (issue 808 WS milestone): ONE background task snapshots the aggregate on a
    // fixed cadence and publishes it on a `watch` channel. Every `/ws` client fans out from this
    // single snapshot, so the relays are polled once per interval no matter how many panels are
    // open — the single-source-of-truth push the owner asked for. Seed the channel with an
    // initial snapshot so a client connecting before the first tick still gets current state.
    let initial = Arc::new(agg.snapshot(&config).await);
    let (live_tx, live_rx) = tokio::sync::watch::channel(initial);
    {
        let agg = agg.clone();
        let config = config.clone();
        tokio::spawn(async move {
            let mut ticker =
                tokio::time::interval(std::time::Duration::from_millis(LIVE_PUSH_INTERVAL_MS));
            // issue 809: per-camera fps-sync alert state, so a mismatch / grab-config desync is
            // logged ONCE on transition (not every ~2 s pump cycle).
            let mut fps_alert_state: std::collections::HashMap<String, (FpsSync, bool)> =
                std::collections::HashMap::new();
            ticker.tick().await; // consume the immediate first tick (channel is already seeded)
            loop {
                ticker.tick().await;
                let snapshot = Arc::new(agg.snapshot(&config).await);
                // issue 809: telemetry — surface a camera fps mismatch or a config-vs-capture
                // grab desync in the log (with the cross-reference to capture_rate_health), on
                // transition only.
                for line in bkshading::monitor::fps_alert_transitions(
                    &mut fps_alert_state,
                    &snapshot.cameras,
                ) {
                    tracing::warn!("{line}");
                }
                // The AppState keeps a receiver alive, so `send` never fails for "no receivers".
                let _ = live_tx.send(snapshot);
            }
        });
    }

    let state = AppState {
        agg,
        config,
        previews,
        live: live_rx,
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
