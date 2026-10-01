//! `copper-cloud serve`: bootstrap, background tasks, graceful shutdown.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context as _;
use axum_server::Handle;
use copper_cloud_core::config::Config;
use copper_cloud_core::state::AppState;
use copper_cloud_core::{db, observe, shutdown, tls};

/// How long in-flight requests get after SIGTERM before connections are dropped.
const GRACE: Duration = Duration::from_secs(10);

pub async fn run(cfg: Config, migrate: bool) -> anyhow::Result<()> {
    observe::init_tracing(cfg.log_format, &cfg.log_level);
    tracing::info!(version = %crate::version(), config = ?cfg, "starting copper-cloud");

    let metrics = match cfg.metrics_bind {
        Some(_) => Some(observe::install_metrics()?),
        None => None,
    };

    let pool = connect_with_retry(&cfg).await?;
    if migrate {
        db::migrate(&pool).await?;
        tracing::info!("migrations up to date");
    } else {
        let (_, pending) = db::pending_migrations(&pool).await?;
        if !pending.is_empty() {
            tracing::warn!(?pending, "pending migrations (started with --no-migrate)");
        }
    }

    let state = AppState::new(pool.clone(), cfg);
    let cfg = state.cfg.clone();

    if let (Some(bind), Some(handle)) = (cfg.metrics_bind, metrics) {
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(err) = observe::serve_metrics(bind, handle, st).await {
                tracing::error!(error = format!("{err:#}"), "metrics listener failed");
            }
        });
    }

    if !crate::portal::portal_built() {
        tracing::warn!("admin portal not built into this binary; / serves a placeholder");
    }

    // Hourly cleanup of expired sessions, admin sessions and pairing codes.
    {
        let pool = pool.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(3600));
            loop {
                tick.tick().await;
                match db::purge_expired_sessions(&pool).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(removed = n, "purged expired sessions"),
                    Err(err) => tracing::warn!(error = %err, "session purge failed"),
                }
                match db::purge_expired_credentials(&pool).await {
                    Ok(0) => {}
                    Ok(n) => {
                        tracing::info!(
                            removed = n,
                            "purged expired admin sessions / pairing codes"
                        );
                    }
                    Err(err) => tracing::warn!(error = %err, "credential purge failed"),
                }
            }
        });
    }

    let handle: Handle<SocketAddr> = Handle::new();
    tokio::spawn(shutdown_on_signal(handle.clone()));

    let app = crate::build_app(state);
    let result = tls::serve(app, &cfg, handle).await;
    shutdown::trigger();
    pool.close().await;
    match &result {
        Ok(()) => tracing::info!("stopped"),
        Err(err) => tracing::error!(error = format!("{err:#}"), "server error"),
    }
    result
}

async fn connect_with_retry(cfg: &Config) -> anyhow::Result<sqlx::PgPool> {
    let mut delay = Duration::from_millis(500);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        match db::connect(cfg).await {
            Ok(pool) => return Ok(pool),
            Err(err) if tokio::time::Instant::now() < deadline => {
                tracing::warn!(
                    error = format!("{err:#}"),
                    "database not reachable yet; retrying"
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
            Err(err) => return Err(err).context("database unavailable"),
        }
    }
}

async fn shutdown_on_signal(handle: Handle<SocketAddr>) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = term => {},
    }
    tracing::info!(
        grace_secs = GRACE.as_secs(),
        "shutdown signal received; draining"
    );
    // End SSE streams and tell canvas peers we are going away (close 1001) so the drain
    // does not wait on long-lived connections.
    shutdown::trigger();
    copper_cloud_canvas::close_all_rooms();
    handle.graceful_shutdown(Some(GRACE));
}
