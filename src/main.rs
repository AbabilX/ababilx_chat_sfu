use std::sync::Arc;

use ferrite_sfu::{config, report, sfu, signal};
use tokio::sync::mpsc;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `ferrite-sfu --health` probes a running instance and exits 0 or 1. The
    // container healthcheck uses it so the image needs no shell, no curl and no
    // wget — and so it keeps working on a smaller base image.
    if std::env::args().nth(1).as_deref() == Some("--health") {
        return health_probe().await;
    }

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("ferrite_sfu=info,str0m=warn")),
        )
        .init();

    // str0m needs a crypto backend chosen for the whole process before any Rtc
    // is built. Which backends are available is a Cargo feature.
    str0m::crypto::from_feature_flags().install_process_default();

    let config = Arc::new(config::Config::from_env()?);
    let (report_tx, report_rx) = mpsc::unbounded_channel();
    match config.webhook_url.clone() {
        Some(url) => {
            tracing::info!(%url, "reporting room lifecycle");
            tokio::spawn(report::run(report_rx, url, config.webhook_secret.clone()));
        }
        None => {
            // Standalone mode: nothing to report to, so the receiver is drained
            // rather than left to grow unboundedly.
            tracing::warn!("SFU_WEBHOOK_URL unset - room events will not be reported");
            tokio::spawn(async move {
                let mut rx = report_rx;
                while rx.recv().await.is_some() {}
            });
        }
    }

    let (engine, media_addr) = sfu::spawn(config.clone(), report_tx.clone())?;

    // Every room this process was carrying died with the previous process:
    // WebRTC state lives only in memory. Saying so on boot is what stops a
    // host application leaving participants marked as being in a call that no
    // longer exists.
    let _ = report_tx.send(report::ReportEvent::new(report::SERVER_STARTED, ""));
    tracing::info!(
        udp = %media_addr,
        http = %config.http_bind,
        ice_lite = config.ice_lite,
        max_room_peers = config.max_room_peers,
        "{} {} ready",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
    );

    let state = signal::AppState { engine: engine.clone(), config: config.clone() };
    let listener = tokio::net::TcpListener::bind(config.http_bind).await?;
    axum::serve(listener, signal::router(state))
        .with_graceful_shutdown(shutdown())
        .await?;

    engine.send(sfu::Command::Shutdown);
    Ok(())
}

async fn health_probe() -> Result<(), Box<dyn std::error::Error>> {
    let bind = std::env::var("SFU_BIND_HTTP").unwrap_or_else(|_| "0.0.0.0:7898".to_string());
    let port = bind.rsplit(':').next().unwrap_or("7898");
    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("healthz returned {}", response.status()).into())
    }
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
