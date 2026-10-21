//! Canvas App at `/canvas/`: the canvas in any browser, for people who are not on Copper
//! (`copper-browser/canvas-app`, its `dist/web/index.html` at the commit pinned in
//! `assets/CANVAS_APP_REF`, vendored as `assets/canvas-app.html` by
//! `packaging/sync-canvas-app.sh`). Served from the server's own origin so its REST calls need
//! no CORS (the server grants none, and the gate would refuse a preflight); it reaches the
//! linked Canvas Cloud through the same `/v1/canvases…` proxy Copper uses, and its room socket
//! carries the gate credential as `?instance=` (`copper_cloud_core::app::INSTANCE_QUERY`), since
//! a browser cannot set headers on a WebSocket. The page is one self-contained file with its own
//! CSP in a `<meta>` tag.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use copper_cloud_core::state::SharedState;

/// The vendored page.
pub static PAGE: &[u8] = include_bytes!("../assets/canvas-app.html");

/// `/canvas` → `/canvas/`, and the page at `/canvas/`.
pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/canvas", get(|| async { Redirect::permanent("/canvas/") }))
        .route("/canvas/", get(page))
}

async fn page() -> Response {
    let mut resp = (StatusCode::OK, PAGE).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    // Revalidate, so an upgraded server serves its new page at once.
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    // Only framing is restricted here; everything else is the page's own `<meta>` policy.
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    resp
}
