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

/// How many pump cycles between reachability HEARTBEAT lines (issue 1309): 5 min / 2 s = 150. The
/// per-cycle `relay unreachable` WARN is gone (downgraded to `debug`); the pump instead logs ONE
/// line per reachability TRANSITION plus this periodic count, so a chronically-down relay stays
/// visible without the 365 KB/run spam.
const REACH_HEARTBEAT_CYCLES: u64 = 150;

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

    /// The panel layout file (shading#16: block order + hidden blocks, saved from the panel's
    /// ARRANGE mode). Default: `bkshading-layout.json` next to `--config`; without either the
    /// layout lives in memory only.
    #[arg(long, value_name = "FILE")]
    layout_file: Option<PathBuf>,

    /// Load this config file, print `ok: <N> cameras` and exit 0, or exit non-zero naming the
    /// file and the reason. Starts nothing and binds no port: camera-box runs it from the pinned
    /// release on its generated strih seed (shading#1, docs/CONTRACT.md).
    #[arg(long, value_name = "FILE", conflicts_with_all = ["config", "bind", "layout_file"])]
    check_config: Option<PathBuf>,
}

/// `--check-config`: load the file exactly as a service start would, then report.
fn check_config(path: &std::path::Path) -> Result<()> {
    let config = ServiceConfig::from_path(path)
        .map_err(|e| anyhow::anyhow!("config {} does not load: {e:#}", path.display()))?;
    println!("ok: {} cameras", config.cameras.len());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    if let Some(path) = &args.check_config {
        return check_config(path);
    }
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
    // issue 808: relays that announce themselves (a handheld SBC on venue WiFi).
    let registry = bkshading::registry::RelayRegistry::new();
    let agg = Arc::new(Aggregator::with_registry(VERSION, registry.clone())?);

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
    let initial = Arc::new(agg.snapshot(&config, &previews).await);
    let (live_tx, live_rx) = tokio::sync::watch::channel(initial);
    // issue 1337: share the publish handle so `set_params` can push an immediate confirmation over
    // the WS after a write (the pump also holds a clone for its ~2 s cadence snapshots).
    let live_tx = Arc::new(live_tx);
    // shading#25: the PGM/PVW tally from the strih tally service, only with a `[tally] url`.
    match config.tally.poll_url() {
        Some(url) => {
            tokio::spawn(bkshading::tally::run(
                url.to_string(),
                bkshading::tally::TALLY_POLL_INTERVAL,
                agg.clone(),
                live_tx.clone(),
            ));
        }
        None => tracing::info!("tally: off (no [tally] url in the config)"),
    }
    {
        let agg = agg.clone();
        let config = config.clone();
        let live_tx = live_tx.clone();
        let previews = previews.clone();
        tokio::spawn(async move {
            let mut ticker =
                tokio::time::interval(std::time::Duration::from_millis(LIVE_PUSH_INTERVAL_MS));
            // issue 809: per-camera fps-sync alert state, so a mismatch / grab-config desync is
            // logged ONCE on transition (not every ~2 s pump cycle).
            let mut fps_alert_state: std::collections::HashMap<String, (FpsSync, bool)> =
                std::collections::HashMap::new();
            // issue 1309: per-camera reachability state, so the pump logs ONE line per reachability
            // transition + a periodic heartbeat (replacing the per-poll `relay unreachable` spam).
            let mut reach_state: std::collections::HashMap<String, bool> =
                std::collections::HashMap::new();
            // issue 808: per-camera preview liveness, so a feed that stops (or comes back) is
            // logged ONCE per transition.
            let mut preview_state: std::collections::HashMap<String, bool> =
                std::collections::HashMap::new();
            let mut cycle: u64 = 0;
            ticker.tick().await; // consume the immediate first tick (channel is already seeded)
            loop {
                ticker.tick().await;
                cycle = cycle.wrapping_add(1);
                let (fresh, polled_from) = agg.snapshot_timed(&config, &previews).await;
                let snapshot = Arc::new(fresh);
                // issue 809: telemetry — surface a camera fps mismatch or a config-vs-capture
                // grab desync in the log (with the cross-reference to capture_rate_health), on
                // transition only.
                for line in bkshading::monitor::fps_alert_transitions(
                    &mut fps_alert_state,
                    &snapshot.cameras,
                ) {
                    tracing::warn!("{line}");
                }
                // issue 1309: relay reachability — one info line per transition, plus a heartbeat.
                for line in
                    bkshading::monitor::reach_transitions(&mut reach_state, &snapshot.cameras)
                {
                    tracing::info!("{line}");
                }
                // issue 808: preview liveness — one info line per live/stale flip.
                for line in
                    bkshading::monitor::preview_transitions(&mut preview_state, &snapshot.cameras)
                {
                    tracing::info!("{line}");
                }
                if cycle.is_multiple_of(REACH_HEARTBEAT_CYCLES) {
                    tracing::info!(
                        "{}",
                        bkshading::monitor::reach_heartbeat_line(&snapshot.cameras)
                    );
                }
                // shading#27 + shading#25: merged with the view it replaces, the write stamps and
                // the tally, all read under the channel's lock (`send_modify` also notifies with
                // no receivers; the AppState keeps one alive anyway).
                agg.publish_snapshot(&live_tx, (*snapshot).clone(), polled_from);
            }
        });
    }

    // shading#16: the venue's panel layout, next to the config unless --layout-file says otherwise.
    let layout_path =
        bkshading::layout::layout_path(args.layout_file.as_deref(), args.config.as_deref());
    let layout = bkshading::layout::LayoutStore::open(layout_path);

    let state = AppState {
        agg,
        config,
        previews,
        live: live_rx,
        live_tx,
        registry,
        layout,
    };

    tracing::info!(
        version = VERSION,
        %addr,
        cameras = state.config.cameras.len(),
        preview_workers,
        layout_file = %state
            .layout
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(memory)".into()),
        "bkshading service starting"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    // ConnectInfo: the relay registration keeps the announcing peer's address (issue 808).
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("bkshading service shutting down");
}
