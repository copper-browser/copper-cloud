//! The canvas API reverse proxy (CONTRACT §7.2).
//!
//! * Caller: a signed-in Copper user ([`AuthUser`]); the instance gate already ran.
//! * Forwarded to `{canvas cloud}/v1{same path}{same query minus token=}` with the method, the
//!   body (64 KiB, 4 MiB for `/ops`), `Content-Type`, `Accept` and `If-*`. Nothing else of the
//!   client's headers is forwarded (no `Authorization`, `Cookie`, `X-Copper-Instance`,
//!   hop-by-hop); added: `Authorization: Bearer <app key>`, `X-Canvas-User: <user id>`,
//!   `X-Forwarded-For`.
//! * Response: status, body (streamed), `Content-Type`, `Cache-Control` (+ `ETag`,
//!   `Last-Modified`, `Location`, `Retry-After`) verbatim — except **401**, which Copper
//!   treats as "signed out": `401 unknown_user` provisions the user and retries once; any
//!   other 401 (or a second `unknown_user`) becomes `502 canvas_upstream`.
//! * Canvas Cloud unreachable / timed out → `502 canvas_upstream`; unlinked → `503
//!   canvas_unavailable`.

use std::net::IpAddr;

use axum::body::{Body, Bytes};
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts as _, OriginalUri, Request, State};
use axum::http::{header, HeaderMap, HeaderName, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use copper_cloud_core::auth::AuthUser;
use copper_cloud_core::error::ApiError;
use copper_cloud_core::extract::read_body;
use copper_cloud_core::state::SharedState;
use uuid::Uuid;

use crate::upstream::{
    error_fields, UpstreamError, CANVAS_USER_HEADER, LONG_REQUEST_TIMEOUT, REQUEST_TIMEOUT,
};
use crate::{upstream_failed, Active, CanvasLink, MAX_BODY_BYTES, MAX_OPS_BODY_BYTES};

/// Request headers forwarded to Canvas Cloud.
const FORWARD_REQUEST: &[HeaderName] = &[
    header::CONTENT_TYPE,
    header::ACCEPT,
    header::IF_MATCH,
    header::IF_NONE_MATCH,
    header::IF_MODIFIED_SINCE,
    header::IF_UNMODIFIED_SINCE,
    header::IF_RANGE,
];

/// Response headers passed back to Copper.
const FORWARD_RESPONSE: &[HeaderName] = &[
    header::CONTENT_TYPE,
    header::CACHE_CONTROL,
    header::ETAG,
    header::LAST_MODIFIED,
    header::LOCATION,
    header::RETRY_AFTER,
    header::CONTENT_DISPOSITION,
];

/// Largest 401 body we read to look for `unknown_user`.
const MAX_ERROR_BODY: usize = 16 * 1024;

pub(crate) async fn proxy(
    State(state): State<SharedState>,
    Extension(link): Extension<CanvasLink>,
    req: Request,
) -> Response {
    match handle(&state, &link, req).await {
        Ok(resp) | Err(resp) => resp,
    }
}

/// What the client asked for, ready to forward.
pub(crate) struct Forward {
    pub user_id: Uuid,
    pub client_ip: Option<IpAddr>,
    /// `/v1/...?query` (token removed).
    pub path_and_query: String,
}

// `Err` is the finished error response (same type as `Ok`).
#[allow(clippy::result_large_err)]
async fn handle(
    state: &SharedState,
    link: &CanvasLink,
    req: Request,
) -> Result<Response, Response> {
    let client_ip = copper_cloud_core::observe::client_ip(&req, state.cfg.trust_proxy);
    let (mut parts, body) = req.into_parts();
    let user = AuthUser::from_request_parts(&mut parts, state)
        .await
        .map_err(IntoResponse::into_response)?;
    let Some(active) = link.enabled().await.map_err(IntoResponse::into_response)? else {
        return Err(crate::unavailable());
    };
    let uri = parts
        .extensions
        .get::<OriginalUri>()
        .map_or_else(|| parts.uri.clone(), |o| o.0.clone());
    let path = v1_path(&uri);
    let fwd = Forward {
        user_id: user.user_id,
        client_ip,
        path_and_query: match forwarded_query(uri.query()) {
            Some(q) => format!("{path}?{q}"),
            None => path.clone(),
        },
    };

    if is_ws_path(&path) {
        if let Ok(ws) = WebSocketUpgrade::from_request_parts(&mut parts, state).await {
            return Ok(crate::ws::proxy(link, &active, &fwd, ws).await);
        }
    }

    let ops = path.ends_with("/ops");
    let limit = if ops {
        MAX_OPS_BODY_BYTES
    } else {
        MAX_BODY_BYTES
    };
    check_declared_length(&parts.headers, limit).map_err(IntoResponse::into_response)?;
    let bytes = read_body(body, limit)
        .await
        .map_err(IntoResponse::into_response)?;
    let timeout = if ops {
        LONG_REQUEST_TIMEOUT
    } else {
        REQUEST_TIMEOUT
    };

    let mut provisioned = false;
    loop {
        let resp = send(
            &active,
            &parts.method,
            &parts.headers,
            &fwd,
            &bytes,
            timeout,
        )
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "canvas upstream request failed");
            upstream_failed(e.to_string())
        })?;
        if resp.status() != StatusCode::UNAUTHORIZED {
            return Ok(pass_through(resp));
        }
        let body = limited_bytes(resp).await;
        let (code, message) = error_fields(&body);
        if code.as_deref() == Some("unknown_user") && !provisioned {
            provisioned = true;
            jit_provision(link, &active, fwd.user_id).await?;
            continue;
        }
        return Err(rejected(code.as_deref(), message.as_deref(), fwd.user_id));
    }
}

/// Provision `user_id` after `unknown_user`; failure → `502`.
#[allow(clippy::result_large_err)]
pub(crate) async fn jit_provision(
    link: &CanvasLink,
    active: &Active,
    user_id: Uuid,
) -> Result<(), Response> {
    crate::provision::provision_user(link, active, user_id)
        .await
        .map_err(|e| {
            tracing::warn!(%user_id, error = %e, "just-in-time canvas user provisioning failed");
            upstream_failed(format!(
                "could not provision this user on Canvas Cloud ({e})"
            ))
        })
}

/// Map an upstream 401 (never forwarded: Copper would sign the user out) to `502`.
pub(crate) fn rejected(code: Option<&str>, message: Option<&str>, user_id: Uuid) -> Response {
    let detail = match (code, message) {
        (Some(c), Some(m)) if !m.is_empty() => format!("{c}: {m}"),
        (Some(c), _) => c.to_owned(),
        (None, Some(m)) => m.to_owned(),
        (None, None) => "401".to_owned(),
    };
    tracing::error!(
        %user_id,
        upstream_error = %detail,
        "Canvas Cloud answered 401 to the app; check the canvas link's app key"
    );
    upstream_failed(format!(
        "Canvas Cloud rejected this Copper Cloud's app key/user ({detail})"
    ))
}

async fn send(
    active: &Active,
    method: &Method,
    headers: &HeaderMap,
    fwd: &Forward,
    body: &Bytes,
    timeout: std::time::Duration,
) -> Result<reqwest::Response, UpstreamError> {
    let up = &active.upstream;
    let mut rb = up
        .client()
        .request(method.clone(), up.url(&fwd.path_and_query))
        .timeout(timeout)
        .header(header::AUTHORIZATION, up.auth_header())
        .header(CANVAS_USER_HEADER, fwd.user_id.to_string());
    if let Some(ip) = fwd.client_ip {
        rb = rb.header("x-forwarded-for", ip.to_string());
    }
    for name in FORWARD_REQUEST {
        for value in headers.get_all(name) {
            rb = rb.header(name, value);
        }
    }
    if !(body.is_empty() && matches!(*method, Method::GET | Method::HEAD)) {
        rb = rb.body(body.clone());
    }
    rb.send().await.map_err(|e| UpstreamError::from_reqwest(&e))
}

fn pass_through(resp: reqwest::Response) -> Response {
    let status = resp.status();
    let mut out = Response::builder().status(status);
    if let Some(h) = out.headers_mut() {
        for name in FORWARD_RESPONSE {
            for value in resp.headers().get_all(name) {
                h.append(name, value.clone());
            }
        }
    }
    let body = Body::from_stream(resp.bytes_stream());
    out.body(body)
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

async fn limited_bytes(mut resp: reqwest::Response) -> Vec<u8> {
    let mut out = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        if out.len() + chunk.len() > MAX_ERROR_BODY {
            break;
        }
        out.extend_from_slice(&chunk);
    }
    out
}

fn check_declared_length(headers: &HeaderMap, limit: usize) -> Result<(), ApiError> {
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    match declared {
        Some(n) if n > limit as u64 => Err(ApiError::PayloadTooLarge),
        _ => Ok(()),
    }
}

/// The request path as Canvas Cloud sees it: `/v1/...` (the router is nested under `/v1`).
fn v1_path(uri: &Uri) -> String {
    let path = uri.path();
    if path.starts_with("/v1/") {
        path.to_owned()
    } else {
        format!("/v1{path}")
    }
}

/// `/v1/canvases/{id}/ws`.
pub(crate) fn is_ws_path(path: &str) -> bool {
    path.strip_prefix("/v1/canvases/")
        .and_then(|rest| rest.strip_suffix("/ws"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

/// The query string minus every `token=` parameter (Copper's session token for WebSocket
/// tooling); `None` when nothing is left.
pub(crate) fn forwarded_query(query: Option<&str>) -> Option<String> {
    let kept: Vec<&str> = query?
        .split('&')
        .filter(|kv| !kv.is_empty() && *kv != "token" && !kv.starts_with("token="))
        .collect();
    (!kept.is_empty()).then(|| kept.join("&"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_drops_token() {
        assert_eq!(forwarded_query(None), None);
        assert_eq!(forwarded_query(Some("")), None);
        assert_eq!(forwarded_query(Some("token=abc")), None);
        assert_eq!(
            forwarded_query(Some("full=true&token=abc&ids=a,b")).as_deref(),
            Some("full=true&ids=a,b")
        );
        assert_eq!(
            forwarded_query(Some("tokens=1&x=%20")).as_deref(),
            Some("tokens=1&x=%20")
        );
        assert_eq!(forwarded_query(Some("token&q=1")).as_deref(), Some("q=1"));
    }

    #[test]
    fn ws_paths() {
        assert!(is_ws_path("/v1/canvases/personal/ws"));
        assert!(is_ws_path(
            "/v1/canvases/0190f0f0-0000-7000-8000-000000000000/ws"
        ));
        assert!(!is_ws_path("/v1/canvases/ws"));
        assert!(!is_ws_path("/v1/canvases//ws"));
        assert!(!is_ws_path("/v1/canvases/a/b/ws"));
        assert!(!is_ws_path("/v1/canvases/a/state"));
    }

    #[test]
    fn v1_prefix() {
        let u: Uri = "/v1/canvases?x=1".parse().unwrap();
        assert_eq!(v1_path(&u), "/v1/canvases");
        let u: Uri = "/canvases/a".parse().unwrap();
        assert_eq!(v1_path(&u), "/v1/canvases/a");
    }
}
