//! `rustmail-cli` — operator tooling for RustMail Suite.
//!
//! Phase 0 provides only a buildable entrypoint that initializes structured
//! logging and reports the version. Subcommands (seed admin, run migrations,
//! provisioning, maintenance) are introduced starting in Phase 1.

use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    tracing::info!(
        tool = "rustmail-cli",
        version = rustmail_core::version(),
        "ready (Phase 0 skeleton — no subcommands yet)"
    );

    Ok(())
}
