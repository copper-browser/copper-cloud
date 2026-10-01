//! The `copper-cloud` binary as a library: CLI, server bootstrap, doctor and admin commands.
//! Integration tests drive [`build_app`] directly.

pub mod admin;
pub mod cli;
pub mod doctor;
pub mod serve;

use axum::Router;
use copper_cloud_core::state::SharedState;

/// The full HTTP app: core (`/healthz`, `/v1` auth/devices/sync) + canvases, behind the
/// instance-key gate, auth rate limiter and request tracing/metrics.
pub fn build_app(state: SharedState) -> Router {
    copper_cloud_core::app(state, copper_cloud_canvas::router())
}

/// Version string: crate version plus the git revision CI stamps in at build time.
pub fn version() -> String {
    match option_env!("COPPER_CLOUD_GIT_SHA") {
        Some(sha) if !sha.is_empty() => format!("{} ({sha})", env!("CARGO_PKG_VERSION")),
        _ => env!("CARGO_PKG_VERSION").to_owned(),
    }
}
