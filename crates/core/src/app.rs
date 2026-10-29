//! Router composition, the instance gate and the auth rate limiters.

use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{OriginalUri, Request, State};
use axum::middleware::{from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use serde_json::json;
use subtle::ConstantTimeEq as _;

use crate::access::{AccessMode, Gate, GateIdentity};
use crate::error::ApiError;
use crate::state::SharedState;

/// Header every gated `/v1` request must carry.
pub const INSTANCE_HEADER: &str = "x-copper-instance";

/// A `/v1` route reachable without the gate header: the pairing code is the credential
/// (rate limited by the auth limiter). The SSO routes ([`crate::sso::is_sso_path`]) and
/// [`crate::agents::AGENT_PAIR_PATH`] are ungated too.
pub const PAIR_PATH: &str = "/v1/auth/pair";

/// Query parameter that may carry the gate credential on a canvas room socket
/// (`/v1/canvases/{id}/ws`) instead of [`INSTANCE_HEADER`]: a browser cannot set headers on a
/// WebSocket (Canvas App, `/canvas/`). Nowhere else; request logs never record query strings.
pub const INSTANCE_QUERY: &str = "instance";

/// The admin login route (rate limited with its own per-IP bucket).
pub const ADMIN_LOGIN_PATH: &str = "/admin/api/login";

/// Optional capabilities advertised in `GET /v1/info` `features`, so clients can show or hide
/// controls on older servers. Only ever grows.
pub const FEATURES: &[&str] = &[
    // `DELETE /v1/sync/history` (0.8.0).
    "history_delete",
    // `GET /v1/auth/sso` + `POST /v1/auth/sso/{provider_id}` (0.9.0).
    "sso",
    // `POST /v1/agents/pair` + device-bound pairing codes (0.9.0).
    "agent_pairing",
    // `GET /v1/addons` + `GET /v1/addons/{id}/bundle/{sha256}` (0.10.0).
    "addons",
    // Canvases are served by a linked Canvas Cloud (0.11.0; see `canvas` in `/v1/info`).
    "canvas_link",
    // `GET /v1/sync/docs/{domain}/revisions` (+ `/{version}`, `/{version}/restore`) and the
    // optional `X-Copper-Build` header on doc writes (0.12.0).
    "doc_revisions",
];

/// Core `/v1` routes (auth, pairing, devices, sync, intelligence, info). Paths are relative to `/v1`.
pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/info", get(info))
        .merge(crate::auth::routes())
        .merge(crate::pairing::routes())
        .merge(crate::sso::routes())
        .merge(crate::agents::routes())
        .merge(crate::sync::routes())
        .merge(crate::intelligence::routes())
        .merge(crate::addons::routes())
}

/// The complete HTTP app for the canvas/core test suites: `/healthz` + `/v1/*` (core routes
/// merged with `extra_v1`), JSON 404 everywhere else. See [`app_with`].
pub fn app(state: SharedState, extra_v1: Router<SharedState>) -> Router {
    app_with(state, extra_v1, Router::new().fallback(not_found))
}

/// The complete HTTP app: `/healthz`, `/v1/*` (core routes merged with `extra_v1`, e.g. the
/// canvas router; unknown `/v1` paths are a JSON 404) and `root` (top-level routes and/or a
/// fallback, e.g. the admin API and the portal), wrapped in the instance gate (which only
/// guards `/v1`), the auth rate limiters and request tracing/metrics.
pub fn app_with(
    state: SharedState,
    extra_v1: Router<SharedState>,
    root: Router<SharedState>,
) -> Router {
    let limiter = Arc::new(RateLimits {
        auth: AuthRateLimiter::new(state.cfg.limits.auth_per_minute, state.cfg.trust_proxy),
        admin: AuthRateLimiter::new(state.cfg.limits.auth_per_minute, state.cfg.trust_proxy),
    });
    Router::new()
        .route("/healthz", get(healthz))
        .nest("/v1", router().merge(extra_v1).fallback(not_found))
        .merge(root)
        // Innermost first: rate limit → instance gate → tracing/metrics (outermost).
        .layer(from_fn_with_state(limiter, auth_rate_limit))
        .layer(from_fn_with_state(state.clone(), instance_gate))
        .layer(from_fn_with_state(state.clone(), crate::observe::track))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

/// JSON `404 {"error":"not_found"}`.
pub async fn not_found() -> ApiError {
    ApiError::NotFound
}

async fn info(
    State(state): State<SharedState>,
    Gate(gate): Gate,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mode = crate::access::access_mode(&state).await?;
    // Whether *this caller* may sign up: an access key implies permission (in any mode);
    // the shared instance key follows `allow_signup` (open mode only).
    let signup = match gate {
        Some(GateIdentity::AccessKey { .. }) => true,
        _ if mode == AccessMode::Directory => false,
        _ => crate::auth::signup_allowed(&state).await?,
    };
    Ok(Json(json!({
        "name": "copper-cloud",
        "version": env!("CARGO_PKG_VERSION"),
        "signup": signup,
        "access_mode": mode,
        "features": FEATURES,
        "limits": {
            "max_blob_bytes": state.cfg.limits.max_blob_bytes,
            "max_history_batch": state.cfg.limits.max_history_batch,
            "max_history_entry_bytes": state.cfg.limits.max_history_entry_bytes,
        },
    })))
}

/// Whether `path` is behind the instance gate: everything under `/v1` except [`PAIR_PATH`],
/// the SSO routes and [`crate::agents::AGENT_PAIR_PATH`] (each carries its own credential or
/// none at all). `/healthz`, `/admin/api/*` (own cookie auth) and the portal are not.
pub fn is_gated(path: &str) -> bool {
    (path == "/v1" || path.starts_with("/v1/"))
        && path != PAIR_PATH
        && path != crate::agents::AGENT_PAIR_PATH
        && !crate::sso::is_sso_path(path)
}

/// Whether `path` is a canvas room socket, `/v1/canvases/{id}/ws`.
pub fn is_canvas_socket(path: &str) -> bool {
    path.strip_prefix("/v1/canvases/")
        .and_then(|rest| rest.strip_suffix("/ws"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

/// The [`INSTANCE_QUERY`] value of a query string, percent-decoded.
fn query_credential(query: Option<&str>) -> Option<String> {
    form_urlencoded::parse(query?.as_bytes())
        .find(|(k, _)| k == INSTANCE_QUERY)
        .map(|(_, v)| v.into_owned())
}

/// Middleware: every gated path (see [`is_gated`]) must present a valid credential in
/// `X-Copper-Instance` — the shared instance key (only while `access_mode = open`, compared
/// in constant time) or a valid access key — else 401 `{"error":"instance_key"}`. A canvas
/// room socket without the header may carry it as `?instance=` instead ([`INSTANCE_QUERY`]).
/// The accepted credential is attached as a [`GateIdentity`] extension.
pub async fn instance_gate(
    State(state): State<SharedState>,
    mut req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path();
    if !is_gated(path) {
        return next.run(req).await;
    }
    let presented: Vec<u8> = match req.headers().get(INSTANCE_HEADER) {
        Some(value) => value.as_bytes().to_vec(),
        None if is_canvas_socket(path) => query_credential(req.uri().query())
            .unwrap_or_default()
            .into_bytes(),
        None => Vec::new(),
    };
    match gate_identity(&state, &presented).await {
        Ok(Some(identity)) => {
            req.extensions_mut().insert(identity);
            next.run(req).await
        }
        Ok(None) => ApiError::Unauthorized("instance_key").into_response(),
        Err(err) => err.into_response(),
    }
}

async fn gate_identity(
    state: &SharedState,
    presented: &[u8],
) -> Result<Option<GateIdentity>, ApiError> {
    if presented.is_empty() {
        return Ok(None);
    }
    let mode = crate::access::access_mode(state).await?;
    if mode == AccessMode::Open
        && instance_key_matches(presented, state.cfg.instance_key.as_bytes())
    {
        return Ok(Some(GateIdentity::Instance));
    }
    match std::str::from_utf8(presented) {
        Ok(key) if crate::access::plausible_access_key(key) => {
            crate::access::lookup_access_key(&state.db, key).await
        }
        _ => Ok(None),
    }
}

/// Constant-time equality over SHA-256 digests (so neither content nor length leaks).
pub fn instance_key_matches(presented: &[u8], expected: &[u8]) -> bool {
    if presented.is_empty() {
        return false;
    }
    let a = crate::ids::sha256(presented);
    let b = crate::ids::sha256(expected);
    a.ct_eq(&b).into()
}

// ---------------------------------------------------------------------------------------------
// Rate limiting

/// Keyed GCRA limiter for `/v1/auth/*`: `per_minute` requests per client IP (IPv6 keyed by
/// /64 so one host cannot rotate through its prefix).
pub struct AuthRateLimiter {
    limiter: DefaultKeyedRateLimiter<IpAddr>,
    trust_proxy: bool,
    calls: AtomicU64,
}

impl AuthRateLimiter {
    pub fn new(per_minute: u32, trust_proxy: bool) -> Self {
        let n = std::num::NonZeroU32::new(per_minute.max(1)).expect("non-zero");
        Self {
            limiter: RateLimiter::keyed(Quota::per_minute(n)),
            trust_proxy,
            calls: AtomicU64::new(0),
        }
    }

    /// `true` if the request may proceed.
    pub fn check(&self, ip: IpAddr) -> bool {
        if self.calls.fetch_add(1, Ordering::Relaxed) % 4096 == 4095 {
            self.limiter.retain_recent();
            self.limiter.shrink_to_fit();
        }
        self.limiter.check_key(&rate_key(ip)).is_ok()
    }
}

fn rate_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                IpAddr::V4(v4)
            } else {
                let s = v6.segments();
                IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
        }
    }
}

/// The two per-IP buckets: `/v1/auth/*` (Copper sign-in, pairing) and admin login.
pub struct RateLimits {
    pub auth: AuthRateLimiter,
    pub admin: AuthRateLimiter,
}

async fn auth_rate_limit(
    State(limits): State<Arc<RateLimits>>,
    req: Request,
    next: Next,
) -> Response {
    let path = req
        .extensions()
        .get::<OriginalUri>()
        .map_or_else(|| req.uri().path(), |u| u.0.path());
    // `GET /v1/auth/me` and pairing-code management (`/v1/auth/pairing[/{id}]`) are
    // bearer-authenticated (no credential oracle: tokens are 256-bit; minting is capped per
    // user) and clients call them for status; everything else under /v1/auth counts —
    // including the ungated `POST /v1/auth/pair`, whose code is the credential, and the
    // ungated SSO sign-in (`POST /v1/auth/sso/{provider_id}`). The public provider list
    // (`GET /v1/auth/sso`) is a status read like `/me`.
    let get = req.method() == axum::http::Method::GET;
    let exempt = (get && (path == "/v1/auth/me" || path == crate::sso::SSO_PATH))
        || path == "/v1/auth/pairing"
        || path.starts_with("/v1/auth/pairing/");
    let limiter = if path == ADMIN_LOGIN_PATH {
        &limits.admin
    } else if path.starts_with("/v1/auth/") && !exempt {
        &limits.auth
    } else {
        return next.run(req).await;
    };
    let ip = crate::observe::client_ip(&req, limiter.trust_proxy)
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    if limiter.check(ip) {
        next.run(req).await
    } else {
        metrics::counter!("auth_rate_limited_total").increment(1);
        tracing::warn!(%ip, "auth rate limit exceeded");
        ApiError::RateLimited.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_compare() {
        assert!(instance_key_matches(b"abc", b"abc"));
        assert!(!instance_key_matches(b"abd", b"abc"));
        assert!(!instance_key_matches(b"", b""));
        assert!(!instance_key_matches(b"abcd", b"abc"));
    }

    #[test]
    fn canvas_sockets() {
        assert!(is_canvas_socket("/v1/canvases/0192aa/ws"));
        assert!(is_canvas_socket("/v1/canvases/personal/ws"));
        for p in [
            "/v1/canvases/ws",
            "/v1/canvases//ws",
            "/v1/canvases/a/b/ws",
            "/v1/canvases/a/state",
            "/v1/canvases",
            "/v1/sync/ws",
        ] {
            assert!(!is_canvas_socket(p), "{p}");
        }
    }

    #[test]
    fn instance_from_query() {
        assert_eq!(
            query_credential(Some("token=t&instance=ck_a%2Bb")).as_deref(),
            Some("ck_a+b")
        );
        assert_eq!(query_credential(Some("token=t")), None);
        assert_eq!(query_credential(None), None);
    }

    #[test]
    fn gated_paths() {
        for p in [
            "/v1",
            "/v1/",
            "/v1/info",
            "/v1/auth/login",
            "/v1/auth/pairing",
            "/v1/auth/sso/google/x",
            "/v1/agents",
            "/v1/x",
        ] {
            assert!(is_gated(p), "{p}");
        }
        for p in [
            "/healthz",
            "/v1/auth/pair",
            "/v1/auth/sso",
            "/v1/auth/sso/google",
            "/v1/agents/pair",
            "/admin/api/login",
            "/",
            "/keys",
            "/v1x",
            "/_next/static/a.js",
        ] {
            assert!(!is_gated(p), "{p}");
        }
    }

    #[test]
    fn limiter_counts_per_ip_and_v6_prefix() {
        let l = AuthRateLimiter::new(3, false);
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(l.check(a) && l.check(a) && l.check(a));
        assert!(!l.check(a));
        assert!(l.check(b));
        let v6a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let v6b: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        assert!(l.check(v6a) && l.check(v6b) && l.check(v6a));
        assert!(!l.check(v6b), "same /64 shares a bucket");
    }
}
