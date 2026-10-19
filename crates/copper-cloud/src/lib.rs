//! The `copper-cloud` binary as a library: CLI, server bootstrap, doctor and admin commands.
//! Integration tests drive [`build_app`] directly.

pub mod admin;
pub mod admin_api;
pub mod cli;
pub mod doctor;
pub mod portal;
pub mod serve;
pub mod start;

use axum::{Extension, Router};
use copper_cloud_canvas_link::CanvasLink;
use copper_cloud_core::state::SharedState;

/// The full HTTP app: core (`/healthz`, `/v1` auth/pairing/devices/sync) + the canvas API
/// proxied to the linked Canvas Cloud behind the instance gate, the admin API at
/// `/admin/api` (cookie sessions, not gated), the `/join/{token}` share-link landing page, the
/// SSO onboarding page at `/start` and the embedded admin portal for every other path; auth
/// rate limiters and request tracing/metrics around everything. Starts the canvas link's
/// background tasks (event bridge, user reconcile, outbox) when called inside a tokio runtime.
pub fn build_app(state: SharedState) -> Router {
    build_app_with_link(state).0
}

/// [`build_app`], also returning the canvas link handle (shutdown closes its proxied sockets).
pub fn build_app_with_link(state: SharedState) -> (Router, CanvasLink) {
    let link = CanvasLink::new(state.clone());
    let root = Router::new()
        .nest("/admin/api", admin_api::router())
        .merge(CanvasLink::landing_router())
        .route("/start", axum::routing::get(start::start))
        .fallback(portal::serve);
    let app = copper_cloud_core::app_with(state, link.router(), root)
        .layer(axum::middleware::from_fn_with_state(
            link.clone(),
            copper_cloud_canvas_link::info_layer,
        ))
        .layer(Extension(link.clone()));
    link.start();
    (app, link)
}

/// Version string: crate version plus the git revision CI stamps in at build time.
pub fn version() -> String {
    match option_env!("COPPER_CLOUD_GIT_SHA") {
        Some(sha) if !sha.is_empty() => format!("{} ({sha})", env!("CARGO_PKG_VERSION")),
        _ => env!("CARGO_PKG_VERSION").to_owned(),
    }
}
