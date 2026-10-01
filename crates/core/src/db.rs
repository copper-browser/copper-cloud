//! Postgres pool + embedded migrations.
//!
//! Migrations live in `crates/copper-cloud/migrations` (core owns `0001`–`0099`, the canvas
//! crate owns `0100`+) and are embedded here so every crate and test shares one migrator.

use std::str::FromStr as _;
use std::time::Duration;

use anyhow::Context as _;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions as _, PgPool};

use crate::config::Config;

/// All schema migrations (core + canvas), embedded at compile time.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../copper-cloud/migrations");

/// Open the connection pool (lazily connects; verifies with one round trip).
pub async fn connect(cfg: &Config) -> anyhow::Result<PgPool> {
    let pool = pool_options(cfg)
        .connect_with(connect_options(&cfg.database_url)?)
        .await
        .with_context(|| {
            format!(
                "connecting to {}",
                crate::config::redact_database_url(&cfg.database_url)
            )
        })?;
    Ok(pool)
}

/// Pool that connects on first use (for commands that should not fail fast).
pub fn connect_lazy(cfg: &Config) -> anyhow::Result<PgPool> {
    Ok(pool_options(cfg).connect_lazy_with(connect_options(&cfg.database_url)?))
}

fn pool_options(cfg: &Config) -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(cfg.db_max_connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Some(Duration::from_secs(600)))
        .max_lifetime(Some(Duration::from_secs(1800)))
}

fn connect_options(url: &str) -> anyhow::Result<PgConnectOptions> {
    Ok(PgConnectOptions::from_str(url)
        .context("parsing database_url")?
        .application_name("copper-cloud")
        // Never log statement text with bound values at info.
        .log_statements(log::LevelFilter::Debug)
        .log_slow_statements(log::LevelFilter::Warn, Duration::from_millis(500)))
}

/// Apply all pending migrations.
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    MIGRATOR.run(pool).await.context("running migrations")?;
    Ok(())
}

/// Migration status: (applied versions, embedded versions not yet applied).
pub async fn pending_migrations(pool: &PgPool) -> anyhow::Result<(usize, Vec<String>)> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = '_sqlx_migrations')",
    )
    .fetch_one(pool)
    .await?;
    let applied: Vec<i64> = if exists {
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(pool)
            .await?
    } else {
        Vec::new()
    };
    let pending = MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration() && !applied.contains(&m.version))
        .map(|m| format!("{:04}_{}", m.version, m.description.replace(' ', "_")))
        .collect();
    Ok((applied.len(), pending))
}

/// Delete expired sessions; returns rows removed.
pub async fn purge_expired_sessions(pool: &PgPool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM sessions WHERE expires_at < now()")
        .execute(pool)
        .await?
        .rows_affected())
}

/// Delete expired admin sessions and pairing codes that expired (or were used) more than a
/// day ago; returns rows removed.
pub async fn purge_expired_credentials(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let admins = sqlx::query("DELETE FROM admin_sessions WHERE expires_at < now()")
        .execute(pool)
        .await?
        .rows_affected();
    let codes = sqlx::query(
        "DELETE FROM pairing_codes
         WHERE expires_at < now() - interval '1 day' OR used_at < now() - interval '1 day'",
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(admins + codes)
}
