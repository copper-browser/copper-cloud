//! The `copper-cloud` binary as a library: CLI, server bootstrap, doctor and admin commands.
//! Integration tests drive [`build_app`] directly.

pub mod admin;
pub mod admin_api;
pub mod cli;
pub mod doctor;
pub mod portal;
pub mod serve;

use axum::Router;
use copper_cloud_core::state::SharedState;

/// The full HTTP app: core (`/healthz`, `/v1` auth/pairing/devices/sync) + canvases behind
/// the instance gate, the admin API at `/admin/api` (cookie sessions, not gated) and the
/// embedded admin portal for every other path; auth rate limiters and request
/// tracing/metrics around everything.
pub fn build_app(state: SharedState) -> Router {
    let root = Router::new()
        .nest("/admin/api", admin_api::router())
        .merge(copper_cloud_canvas::landing_router())
        .fallback(portal::serve);
    copper_cloud_core::app_with(state, copper_cloud_canvas::router(), root)
}

/// Version string: crate version plus the git revision CI stamps in at build time.
pub fn version() -> String {
    match option_env!("COPPER_CLOUD_GIT_SHA") {
        Some(sha) if !sha.is_empty() => format!("{} ({sha})", env!("CARGO_PKG_VERSION")),
        _ => env!("CARGO_PKG_VERSION").to_owned(),
    }
}
