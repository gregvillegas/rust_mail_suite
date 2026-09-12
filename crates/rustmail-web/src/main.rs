//! RustMail Suite web server binary.
//!
//! Phase 0 scaffolding: initializes tracing and logs a startup line, then
//! exits. The axum HTTP server, templates, and WebSocket push are added in
//! Phase 3 and beyond.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    tracing::info!(
        crate_version = rustmail_core::version(),
        "rustmail-web starting (Phase 0 scaffold — no HTTP server yet)"
    );
    Ok(())
}

/// Initializes the global tracing subscriber, honoring `RUST_LOG`.
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}
