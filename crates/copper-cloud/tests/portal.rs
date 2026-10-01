//! Portal hosting (spec §7.3): every non-API path serves the embedded Next.js export (or a
//! "not built" placeholder), with SPA fallback, cache headers and security headers.
//!
//! Debug builds read `portal/out` at run time, so these assertions hold whether or not the
//! portal has been built; file-specific checks run only when the files exist.

mod common;

use common::*;
use serde_json::Value;

const PORTAL_OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../portal/out");

fn built() -> bool {
    std::path::Path::new(PORTAL_OUT).join("index.html").exists()
}

fn assert_security_headers(r: &reqwest::Response, what: &str) {
    let h = r.headers();
    let csp = h["content-security-policy"].to_str().unwrap();
    assert!(
        csp.starts_with("default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'"),
        "{what}: {csp}"
    );
    assert!(
        csp.ends_with("connect-src 'self'; frame-ancestors 'none'"),
        "{what}: {csp}"
    );
    assert!(
        !csp.contains("'unsafe-inline'; connect"),
        "no inline scripts: {csp}"
    );
    assert_eq!(h["x-content-type-options"], "nosniff", "{what}");
    assert_eq!(h["referrer-policy"], "same-origin", "{what}");
    assert_eq!(h["x-frame-options"], "DENY", "{what}");
}

#[tokio::test]
async fn root_and_spa_fallback() {
    let s = start().await;
    let root = s.http.get(s.url("/")).send().await.unwrap();
    assert_eq!(root.status(), 200);
    assert_security_headers(&root, "/");
    assert_eq!(root.headers()["cache-control"], "no-store");
    let root_type = root.headers()["content-type"].to_str().unwrap().to_owned();
    let root_body = root.text().await.unwrap();
    if built() {
        assert!(root_type.starts_with("text/html"), "{root_type}");
    } else {
        assert!(root_type.starts_with("text/plain"), "{root_type}");
        assert!(root_body.contains("not built"), "{root_body}");
        assert!(root_body.contains("/admin/api/overview"), "{root_body}");
    }

    // Unknown client-side routes fall back to index.html (no gate header needed).
    for path in ["/some/deep/client/route", "/admin", "/people?x=1"] {
        let r = s.http.get(s.url(path)).send().await.unwrap();
        assert_eq!(r.status(), 200, "{path}");
        assert_security_headers(&r, path);
        assert_eq!(r.headers()["cache-control"], "no-store", "{path}");
        if !built() {
            assert_eq!(r.text().await.unwrap(), root_body, "{path}");
        }
    }

    // HEAD works, other methods do not.
    let r = s.http.head(s.url("/")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let r = s.http.post(s.url("/")).send().await.unwrap();
    assert_eq!(r.status(), 405);
    assert_eq!(r.headers()["allow"], "GET, HEAD");
}

#[tokio::test]
async fn api_paths_are_never_portal() {
    let s = start().await;
    let r = s.http.get(s.url("/healthz")).send().await.unwrap();
    assert_eq!(r.text().await.unwrap(), "ok");
    for path in ["/v1x", "/metrics", "/healthz/x", "/admin/apix"] {
        let r = s.http.get(s.url(path)).send().await.unwrap();
        assert_eq!(r.status(), 404, "{path}");
        assert_eq!(
            r.json::<Value>().await.unwrap()["error"],
            "not_found",
            "{path}"
        );
    }
    // /v1 stays gated; /admin/api has its own auth.
    let r = s.http.get(s.url("/v1/nope")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = s
        .http
        .get(s.url("/admin/api/overview"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "admin_session");
    // A missing hashed asset is a 404, not index.html posing as JavaScript.
    let r = s
        .http
        .get(s.url("/_next/static/chunks/does-not-exist.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    assert_eq!(r.headers()["x-content-type-options"], "nosniff");
    // Traversal attempts never leave the asset root.
    for path in [
        "/%2e%2e/Cargo.toml",
        "/..%2fCargo.toml",
        "/_next/%2e%2e/%2e%2e/Cargo.toml",
    ] {
        let r = s.http.get(s.url(path)).send().await.unwrap();
        assert!(
            !r.text().await.unwrap().contains("[workspace]"),
            "{path} escaped the asset root"
        );
    }
}

fn first_file(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::path);
    for e in entries {
        let p = e.path();
        if p.is_file() {
            return Some(p);
        }
        if let Some(f) = first_file(&p) {
            return Some(f);
        }
    }
    None
}

#[tokio::test]
async fn built_assets_cache_headers() {
    let s = start().await;
    let out = std::path::Path::new(PORTAL_OUT);
    // Hashed Next assets are immutable.
    if let Some(file) = first_file(&out.join("_next/static")) {
        let rel = file
            .strip_prefix(out)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let r = s.http.get(s.url(&format!("/{rel}"))).send().await.unwrap();
        assert_eq!(r.status(), 200, "{rel}");
        assert_eq!(
            r.headers()["cache-control"],
            "public, max-age=31536000, immutable",
            "{rel}"
        );
        assert_security_headers(&r, &rel);
        let etag = r.headers()["etag"].to_str().unwrap().to_owned();
        let r = s
            .http
            .get(s.url(&format!("/{rel}")))
            .header("If-None-Match", &etag)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 304);
    }
    // Inline scripts in the exported HTML are allowed by hash only.
    if built() {
        let html = std::fs::read(out.join("index.html")).unwrap();
        let r = s.http.get(s.url("/")).send().await.unwrap();
        let csp = r.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .to_owned();
        for h in copper_cloud::portal::inline_script_hashes(&html) {
            assert!(csp.contains(&format!("'sha256-{h}'")), "{h} in {csp}");
        }
        assert_eq!(r.bytes().await.unwrap().as_ref(), html.as_slice());
    }
    let r = s.http.get(s.url("/")).send().await.unwrap();
    assert_eq!(r.status(), 200);
}
