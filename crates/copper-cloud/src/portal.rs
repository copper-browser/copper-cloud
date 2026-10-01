//! The admin portal (`portal/out`, a Next.js static export) embedded with `rust-embed` and
//! served from `/` — every path the API does not own.
//!
//! * Resolution: exact file → `<path>.html` → `<path>/index.html` → SPA fallback
//!   `index.html`. Misses under `/_next/` are a 404 (never HTML posing as a script).
//! * Caching: `/_next/static/*` is content-hashed → `public, max-age=31536000, immutable`;
//!   HTML is `no-store`; anything else `no-cache` with an `ETag`.
//! * Security headers on every portal response: CSP (`script-src 'self'` plus the SHA-256 of
//!   each inline `<script>` in that HTML file — Next's bootstrap scripts), `nosniff`,
//!   `Referrer-Policy: same-origin`, `X-Frame-Options: DENY`.
//! * When the portal was not built (`portal/out/index.html` absent at build time — debug
//!   builds read the folder at run time), the fallback is a one-paragraph text page and the
//!   API keeps working.

use std::borrow::Cow;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use copper_cloud_core::error::ApiError;
use copper_cloud_core::ids;

#[derive(rust_embed::RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../portal/out"]
#[exclude = ".gitkeep"]
#[allow_missing = true]
struct Assets;

/// Prefixes the server owns; never answered with portal content.
pub const RESERVED_PREFIXES: [&str; 4] = ["/v1", "/admin/api", "/healthz", "/metrics"];

/// The portal CSP with no inline-script hashes (exactly the documented policy).
pub const CSP: &str = "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'";
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// Text served at `/` when the binary was built without the portal.
pub const NOT_BUILT: &str = "copper-cloud: the admin portal was not built into this binary \
(portal/out/index.html was missing when it was compiled). The server and the admin API are \
running: see /admin/api/overview (sign in with POST /admin/api/login) or docs/admin-api.md.\n";

/// Whether the embedded portal has an `index.html`.
pub fn portal_built() -> bool {
    Assets::get("index.html").is_some()
}

/// Fallback handler for every route not matched by the API.
pub async fn serve(method: Method, uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path();
    if RESERVED_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return ApiError::NotFound.into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        let mut resp = plain(StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n");
        resp.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return resp;
    }
    let mut resp = match lookup(path) {
        Lookup::File(name, file) => file_response(&name, &file, &headers),
        Lookup::NotBuilt => plain(StatusCode::OK, NOT_BUILT),
        Lookup::Missing => plain(StatusCode::NOT_FOUND, "not found\n"),
    };
    if method == Method::HEAD {
        *resp.body_mut() = Body::empty();
    }
    resp
}

enum Lookup {
    File(String, rust_embed::EmbeddedFile),
    NotBuilt,
    Missing,
}

fn lookup(path: &str) -> Lookup {
    let Some(rel) = clean_path(path) else {
        return Lookup::Missing;
    };
    for name in candidates(&rel) {
        if let Some(file) = Assets::get(&name) {
            return Lookup::File(name, file);
        }
    }
    if rel.starts_with("_next/") {
        return Lookup::Missing;
    }
    match Assets::get("index.html") {
        Some(file) => Lookup::File("index.html".into(), file),
        None => Lookup::NotBuilt,
    }
}

/// Percent-decoded path without the leading `/`; `None` for anything that could escape the
/// asset root or is not valid UTF-8.
fn clean_path(path: &str) -> Option<String> {
    let decoded = percent_decode(path)?;
    let rel = decoded.trim_start_matches('/');
    let bad = rel.contains('\\')
        || rel.contains('\0')
        || rel.contains("//")
        || rel.split('/').any(|seg| seg == ".." || seg == ".");
    (!bad).then(|| rel.to_owned())
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Files to try for a relative path, in order.
fn candidates(rel: &str) -> Vec<String> {
    if rel.is_empty() {
        return vec!["index.html".into()];
    }
    if let Some(dir) = rel.strip_suffix('/') {
        return vec![format!("{dir}/index.html"), format!("{dir}.html")];
    }
    vec![
        rel.to_owned(),
        format!("{rel}.html"),
        format!("{rel}/index.html"),
    ]
}

fn is_html(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
}

fn cache_control(name: &str) -> &'static str {
    if name.starts_with("_next/static/") {
        IMMUTABLE
    } else if is_html(name) {
        "no-store"
    } else {
        "no-cache"
    }
}

fn file_response(name: &str, file: &rust_embed::EmbeddedFile, req: &HeaderMap) -> Response {
    let html = is_html(name);
    let etag = format!("\"{}\"", hex::encode(&file.metadata.sha256_hash()[..16]));
    let mut resp = if !html
        && req
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"))
    {
        let mut r = Response::new(Body::empty());
        *r.status_mut() = StatusCode::NOT_MODIFIED;
        r
    } else {
        let data: Cow<'static, [u8]> = file.data.clone();
        Response::new(Body::from(data.into_owned()))
    };
    let mime = file.metadata.mimetype();
    let content_type = if mime.starts_with("text/") || mime == "application/javascript" {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_owned()
    };
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&content_type) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control(name)),
    );
    if !html {
        if let Ok(v) = HeaderValue::from_str(&etag) {
            h.insert(header::ETAG, v);
        }
    }
    let csp = if html {
        csp_for_html(&file.data)
    } else {
        csp_with_hashes(&[])
    };
    security_headers(h, &csp);
    resp
}

fn plain(status: StatusCode, body: &'static str) -> Response {
    let mut resp = (status, body).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    security_headers(h, &csp_with_hashes(&[]));
    resp
}

fn security_headers(h: &mut HeaderMap, csp: &str) {
    if let Ok(v) = HeaderValue::from_str(csp) {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
}

/// [`CSP`] with `'sha256-…'` sources added to `script-src` for each hash.
pub fn csp_with_hashes(hashes: &[String]) -> String {
    let mut script = String::from("script-src 'self'");
    for h in hashes {
        script.push_str(" 'sha256-");
        script.push_str(h);
        script.push('\'');
    }
    CSP.replacen("script-src 'self'", &script, 1)
}

fn csp_for_html(html: &[u8]) -> String {
    csp_with_hashes(&inline_script_hashes(html))
}

/// Base64 SHA-256 of the body of every inline (no `src`) non-empty `<script>` element.
pub fn inline_script_hashes(html: &[u8]) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(start) = find(&lower[at..], b"<script").map(|i| at + i) {
        let tag_end = match find(&lower[start..], b">") {
            Some(i) => start + i,
            None => break,
        };
        let attrs = &lower[start + b"<script".len()..tag_end];
        let body_start = tag_end + 1;
        let Some(body_len) = find(&lower[body_start..], b"</script") else {
            break;
        };
        let body = &html[body_start..body_start + body_len];
        if !has_src(attrs) && !body.is_empty() {
            let h = ids::b64_std(&ids::sha256(body));
            if !out.contains(&h) {
                out.push(h);
            }
        }
        at = body_start + body_len;
    }
    out
}

fn has_src(attrs: &[u8]) -> bool {
    attrs
        .split(|b| b.is_ascii_whitespace() || *b == b'/')
        .any(|tok| tok == b"src" || tok.starts_with(b"src="))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_order() {
        assert_eq!(candidates(""), ["index.html"]);
        assert_eq!(candidates("keys"), ["keys", "keys.html", "keys/index.html"]);
        assert_eq!(candidates("keys/"), ["keys/index.html", "keys.html"]);
        assert_eq!(candidates("_next/static/a.js")[0], "_next/static/a.js");
    }

    #[test]
    fn path_cleaning() {
        assert_eq!(clean_path("/").as_deref(), Some(""));
        assert_eq!(clean_path("/a/b.js").as_deref(), Some("a/b.js"));
        assert_eq!(
            clean_path("/_next/static/chunks/app/%28dash%29/page.js").as_deref(),
            Some("_next/static/chunks/app/(dash)/page.js")
        );
        for bad in [
            "/../etc/passwd",
            "/a/../../b",
            "/%2e%2e/x",
            "/a\\b",
            "/a//b",
            "/%zz",
            "/./a",
        ] {
            assert_eq!(clean_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn cache_policy() {
        assert_eq!(cache_control("_next/static/chunks/main-abc.js"), IMMUTABLE);
        assert_eq!(cache_control("index.html"), "no-store");
        assert_eq!(cache_control("keys.html"), "no-store");
        assert_eq!(cache_control("favicon.ico"), "no-cache");
    }

    #[test]
    fn script_hashes() {
        let html = br#"<html><head><script src="/_next/static/a.js" async=""></script>
<SCRIPT>self.__next_f=self.__next_f||[]</SCRIPT><script>self.__next_f.push([1,"x"])</script>
<script type="module" src=/b.js></script><script></script></head></html>"#;
        let hashes = inline_script_hashes(html);
        assert_eq!(
            hashes,
            [
                ids::b64_std(&ids::sha256(b"self.__next_f=self.__next_f||[]")),
                ids::b64_std(&ids::sha256(br#"self.__next_f.push([1,"x"])"#)),
            ]
        );
        let csp = csp_with_hashes(&hashes);
        assert!(csp.starts_with("default-src 'self'; img-src 'self' data:;"));
        assert_eq!(csp_with_hashes(&[]), CSP);
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(csp.contains(&format!("script-src 'self' 'sha256-{}'", hashes[0])));
        assert!(!csp.contains("script-src 'self' 'unsafe-inline'"));
        // Unterminated tags do not loop or panic.
        let none: [String; 0] = [];
        assert_eq!(inline_script_hashes(b"<script>never closed"), none);
        assert_eq!(inline_script_hashes(b"<script"), none);
    }

    #[test]
    fn reserved() {
        for p in ["/v1/info", "/v1", "/admin/api/x", "/healthz", "/metrics"] {
            assert!(RESERVED_PREFIXES.iter().any(|r| p.starts_with(r)), "{p}");
        }
        for p in ["/", "/keys", "/admin", "/_next/static/x.js", "/login"] {
            assert!(!RESERVED_PREFIXES.iter().any(|r| p.starts_with(r)), "{p}");
        }
    }
}
