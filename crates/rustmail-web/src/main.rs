//! `rustmail-web` — webmail and public web UI server for RustMail Suite.
//!
//! Phase 0 provides only a buildable entrypoint that initializes structured
//! logging and reports the version. The axum server, askama templates, and
//! WebSocket push are introduced starting in Phase 3.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    tracing::info!(
        service = "rustmail-web",
        version = rustmail_core::version(),
        "starting (Phase 0 skeleton — no HTTP server yet)"
    );

    Ok(())
}
