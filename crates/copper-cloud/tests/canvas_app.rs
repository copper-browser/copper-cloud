//! Canvas App at `/canvas/`: the vendored page, ungated, its own headers, never the portal.

mod common;

use common::*;

#[tokio::test]
async fn serves_canvas_app() {
    let s = start().await;
    let r = s.http.get(s.url("/canvas/")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let h = r.headers().clone();
    assert_eq!(h["content-type"], "text/html; charset=utf-8");
    assert_eq!(h["cache-control"], "no-cache");
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["referrer-policy"], "no-referrer");
    // Only framing is restricted by the server; the page's <meta> CSP does the rest.
    assert_eq!(h["content-security-policy"], "frame-ancestors 'none'");
    let body = r.text().await.unwrap();
    assert!(body.contains("id=\"web-root\""), "the Canvas App build");
    assert_eq!(body.len(), copper_cloud::canvas_app::PAGE.len());

    let no_redirect = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r = no_redirect.get(s.url("/canvas")).send().await.unwrap();
    assert_eq!(r.status(), 308);
    assert_eq!(r.headers()["location"], "/canvas/");
}
