//! RustMail Suite web server binary.
//!
//! Serves the web UI (sign-in, dashboard, administration), the `/auth/*` JSON
//! API, `/static` and `/healthz`. The webmail client arrives in Phase 6.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use rustmail_core::auth::{AuthConfig, AuthService};
use rustmail_core::quarantine::{QuarantineConfig, QuarantineService};
use rustmail_web::auth::{AuthState, DeploymentSecurity, RateLimits};
use rustmail_web::config::WebConfig;
use rustmail_web::AppState;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<()> {
    // A missing .env file is normal (production); ignore that case.
    let _ = dotenvy::dotenv();
    init_tracing();

    let web = WebConfig::from_env()?;
    let auth_config = AuthConfig::from_env().context("invalid authentication settings")?;
    if !web.cookie_secure {
        tracing::warn!("COOKIE_SECURE=false: cookies are sent over plain HTTP. Use only for local development.");
    }
    if web.trust_proxy_headers {
        tracing::info!("trusting X-Forwarded-For from a reverse proxy for client IPs");
    }

    let pool = rustmail_core::db::connect(&web.database_url, 10)
        .await
        .context("could not connect to the database (is it running and migrated?)")?;
    let auth = AuthService::postgres(pool.clone(), auth_config);

    // Housekeeping: drop expired sessions hourly.
    let purger = auth.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3600));
        loop {
            ticker.tick().await;
            match purger.purge_expired_sessions().await {
                Ok(0) => {}
                Ok(n) => tracing::info!(removed = n, "purged expired sessions"),
                Err(error) => tracing::warn!(%error, "could not purge expired sessions"),
            }
        }
    });

    let auth_state = AuthState::new(
        auth,
        DeploymentSecurity {
            cookie_secure: web.cookie_secure,
            trust_proxy_headers: web.trust_proxy_headers,
        },
    )?;
    let state = AppState::new(auth_state, pool, web.static_dir.clone(), web.asset_source)
        .with_context(|| {
            format!(
                "static assets are not usable in {}",
                web.static_dir.display()
            )
        })?;
    // The quarantine pages need to know where Amavis keeps its files.
    let state = match QuarantineConfig::from_env() {
        Ok(config) => {
            tracing::info!(dir = %config.dir.display(), "quarantine pages enabled");
            let service = QuarantineService::new(state.pool.clone(), config);
            state.with_quarantine(service)
        }
        Err(_) => {
            tracing::info!("QUARANTINE_DIR is not set; the quarantine pages stay disabled");
            state
        }
    };
    let limits = RateLimits {
        login: web.login_rate,
        refresh: web.refresh_rate,
    };
    let (app, janitor) = rustmail_web::build_app(state, limits)?;
    janitor.spawn(Duration::from_secs(60));

    let listener = TcpListener::bind(web.bind_addr)
        .await
        .with_context(|| format!("could not bind {}", web.bind_addr))?;
    tracing::info!(version = rustmail_core::version(), addr = %web.bind_addr, "rustmail-web listening");

    // Connect info gives handlers (and the rate limiter) the peer address.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server error")?;
    tracing::info!("shut down cleanly");
    Ok(())
}

/// Resolves on Ctrl-C or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

/// Initializes the global tracing subscriber, honoring `RUST_LOG`.
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}
