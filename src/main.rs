//! seerr-mcp: serve the household's Seerr as three MCP tools, plus Seerr's ready webhook, on one
//! listener. Every secret is required; a missing one stops the process before it binds.

use std::sync::Arc;
use std::time::Duration;

use seerr_mcp::config::Config;
use seerr_mcp::http::{build_router, App};
use seerr_mcp::metrics::Metrics;
use seerr_mcp::notify::Notifier;
use seerr_mcp::seerr::Seerr;
use tracing_subscriber::EnvFilter;

/// Generous on purpose: a proxy that starts a sleeping Seerr on demand holds the first
/// request until it is up, and a 180 s proxy read timeout is common for that.
const SEERR_TIMEOUT: Duration = Duration::from_secs(190);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Answers before configuration, so a deploy can smoke-test a new image with no env: it proves only
    // that the binary loads.
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("seerr-mcp {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_lookup(|key| std::env::var(key).ok())?;
    let app = Arc::new(App {
        seerr: Seerr::new(&config.seerr_url, &config.seerr_api_key, SEERR_TIMEOUT),
        metrics: Metrics::new(),
        notifier: Notifier::new(
            &config.notify_secret,
            &config.hermes_webhook_base,
            &config.hermes_webhook_secret,
            config.identities.clone(),
        ),
        identities: config.identities,
    });

    let listener = tokio::net::TcpListener::bind(&config.addr).await?;
    tracing::info!(
        addr = %config.addr,
        people = app.identities.len(),
        "seerr-mcp serving /mcp, /notify, /metrics, /healthz"
    );
    axum::serve(listener, build_router(app))
        .with_graceful_shutdown(async {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler installs");
            tokio::select! {
                _ = term.recv() => {},
                _ = tokio::signal::ctrl_c() => {},
            }
        })
        .await?;
    Ok(())
}
