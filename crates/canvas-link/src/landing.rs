//! `GET /join/{token}` — the public landing page for canvas share links, moved unchanged from
//! the old canvas crate. It is merged at the application root (outside `/v1`, so no instance
//! key is needed) and deliberately touches neither the database nor Canvas Cloud: it only
//! checks the token's shape and hands it to Copper as
//! `copper://canvas/join/{token}?cloud={host}`.

use axum::extract::Path;
use axum::http::{header, HeaderMap, HeaderValue, Uri};
use axum::response::{Html, IntoResponse, Response};
use copper_cloud_core::ids;

fn token_digest(token: &str) -> Option<[u8; 32]> {
    ids::is_token_shape(token).then(|| ids::sha256(token.as_bytes()))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn percent_encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Accept only a host name/IP and an optional numeric port. In particular, reject user-info,
/// paths and control characters before putting the value in a custom URL scheme.
fn validated_host(headers: &HeaderMap, uri: &Uri) -> Option<String> {
    // HTTP/1.1 carries the target in a literal `Host` header; HTTP/2 (our
    // only transport once TLS is on, since rustls negotiates h2 by ALPN)
    // forbids that header and sends `:authority` instead, which hyper/h2
    // surface as the request URI's authority, not a header — so a host
    // taken from headers alone is always absent on this listener.
    let from_header = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let from_authority = uri.authority().map(|a| a.as_str().to_owned());
    let raw = from_header.or(from_authority)?;
    let raw = raw.as_str();
    if raw.is_empty()
        || raw.trim() != raw
        || raw.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    if raw.contains(['@', '/', '?', '#', '%']) {
        return None;
    }
    let host = if let Some(rest) = raw.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = &rest[..end];
        let suffix = &rest[end + 1..];
        if suffix.is_empty() {
            host
        } else {
            let port = suffix.strip_prefix(':').filter(|p| !p.is_empty())?;
            if port.parse::<u16>().is_err() {
                return None;
            }
            host
        }
    } else {
        if raw.matches(':').count() > 1 {
            return None;
        }
        match raw.rsplit_once(':') {
            Some((host, port)) if !port.is_empty() => {
                if port.parse::<u16>().is_err() {
                    return None;
                }
                host
            }
            Some((_, _)) => return None,
            None => raw,
        }
    };
    if host.is_empty()
        || host.parse::<std::net::IpAddr>().is_err() && url::Host::parse(host).is_err()
    {
        return None;
    }
    Some(raw.to_owned())
}

/// `GET /join/:token` deliberately does not consult the database. It is outside `/v1`, so it is
/// reachable before a client has an instance key and can hand the opaque token to Copper.
pub async fn join_landing(Path(token): Path<String>, uri: Uri, headers: HeaderMap) -> Response {
    let href = token_digest(&token).and_then(|_| {
        validated_host(&headers, &uri).map(|host| {
            format!(
                "copper://canvas/join/{}?cloud={}",
                percent_encode_component(&token),
                host
            )
        })
    });
    let html = match href {
        Some(href) => format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Open canvas in Copper</title><style>body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#f7f7f5;color:#171717;font:16px system-ui,sans-serif}}main{{max-width:28rem;padding:2rem;text-align:center}}a{{display:inline-block;padding:.75rem 1.1rem;border-radius:.5rem;background:#171717;color:#fff;text-decoration:none}}a:focus-visible{{outline:3px solid #6d5dfc;outline-offset:3px}}</style></head><body><main><h1>Canvas share link</h1><p>Open this canvas in Copper to continue.</p><a href=\"{}\">Open this canvas in Copper</a></main></body></html>",
            escape_html(&href)
        ),
        None => "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Open canvas in Copper</title><style>body{margin:0;min-height:100vh;display:grid;place-items:center;background:#f7f7f5;color:#171717;font:16px system-ui,sans-serif}main{max-width:28rem;padding:2rem;text-align:center}</style></head><body><main><h1>Canvas share link</h1><p>Open this link in Copper to continue.</p></main></body></html>".to_owned(),
    };
    let mut response = Html(html).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_falls_back_to_authority_when_no_host_header() {
        // HTTP/2 requests never carry a literal `Host` header (RFC 9113 §8.3.1
        // forbids it); hyper/h2 surface the `:authority` pseudo-header as the
        // request URI's authority instead. The join landing page is only ever
        // served over h2 once TLS is on, so this path must work from the URI
        // alone, not just from headers.
        let empty = HeaderMap::new();
        let uri: Uri = "https://127.0.0.1:8445/join/abc".parse().unwrap();
        assert_eq!(
            validated_host(&empty, &uri).as_deref(),
            Some("127.0.0.1:8445")
        );

        // A literal Host header (HTTP/1.1) still wins when present.
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("example.com"));
        assert_eq!(
            validated_host(&headers, &uri).as_deref(),
            Some("example.com")
        );

        // Neither a header nor a URI authority: no host to offer.
        let relative: Uri = "/join/abc".parse().unwrap();
        assert_eq!(validated_host(&empty, &relative), None);
    }

    async fn landing(token: &str, host: Option<&str>) -> (axum::http::HeaderMap, String) {
        let mut headers = HeaderMap::new();
        if let Some(h) = host {
            headers.insert(header::HOST, HeaderValue::from_str(h).unwrap());
        }
        let uri: Uri = format!("/join/{token}").parse().unwrap();
        let resp = join_landing(Path(token.to_owned()), uri, headers).await;
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let headers = resp.headers().clone();
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (headers, String::from_utf8(body.to_vec()).unwrap())
    }

    // Moved from the old canvas crate's `tests/rest.rs` share-link test.
    #[tokio::test]
    async fn landing_page_links_to_copper() {
        let token = ids::random_token();
        let (headers, html) = landing(&token, Some("cloud.example:443")).await;
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(headers["referrer-policy"], "no-referrer");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(
            headers["content-security-policy"],
            "default-src 'none'; style-src 'unsafe-inline'"
        );
        assert!(html.contains("copper://canvas/join/"));
        assert!(html.contains("cloud=cloud.example:443"));

        let (_, html) = landing(&token, Some("[::1]")).await;
        assert!(html.contains("cloud=[::1]"));

        let (_, html) = landing(&token, Some("<bad>")).await;
        assert!(!html.contains("copper://"));
        assert!(!html.contains("<bad>"));

        // Not a token: no link either.
        let (_, html) = landing("not-a-token", Some("cloud.example")).await;
        assert!(!html.contains("copper://"));
    }
}
