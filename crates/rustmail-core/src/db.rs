//! Database connection and migration helpers.

use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::postgres::PgPoolOptions;

use crate::error::Result;

/// The connection pool type used throughout the suite. Re-exported so binary
/// crates do not need their own `sqlx` dependency.
pub use sqlx::PgPool;

/// Migrations embedded into the binary at compile time from `/migrations`.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Opens a connection pool.
///
/// The URL contains credentials, so it is never logged here.
///
/// # Errors
/// Returns an error if the database cannot be reached within 5 seconds.
pub async fn connect(database_url: &str, max_connections: u32) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await?;
    Ok(pool)
}

/// Applies all pending migrations.
///
/// # Errors
/// Returns an error if a migration fails or a previously applied migration
/// was modified.
pub async fn migrate(pool: &PgPool) -> Result<()> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

/// Round-trips a trivial query and returns how long it took. Used by health
/// checks and the admin overview.
///
/// # Errors
/// Returns an error if the database cannot be reached.
pub async fn ping(pool: &PgPool) -> Result<Duration> {
    let started = std::time::Instant::now();
    sqlx::query("SELECT 1").execute(pool).await?;
    Ok(started.elapsed())
}
