//! `rustmail-admin` — admin console server for RustMail Suite.
//!
//! Phase 0 provides only a buildable entrypoint that initializes structured
//! logging and reports the version. Domain/user/quota/audit management is
//! introduced in Phase 7.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    tracing::info!(
        service = "rustmail-admin",
        version = rustmail_core::version(),
        "starting (Phase 0 skeleton — no HTTP server yet)"
    );

    Ok(())
}
