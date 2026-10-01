//! Router composition, the instance-key gate and the auth rate limiter.

use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{OriginalUri, Request, State};
use axum::http::HeaderValue;
use axum::middleware::{from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use serde_json::json;
use subtle::ConstantTimeEq as _;

use crate::error::ApiError;
use crate::state::SharedState;

/// Header every `/v1` request must carry.
pub const INSTANCE_HEADER: &str = "x-copper-instance";

/// Core `/v1` routes (auth, devices, sync, info). Paths are relative to `/v1`.
pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/info", get(info))
        .merge(crate::auth::routes())
        .merge(crate::sync::routes())
}

/// The complete HTTP app: `/healthz` + `/v1/*` (core routes merged with `extra_v1`, e.g. the
/// canvas router), the instance-key gate, the `/v1/auth/*` rate limiter and request
/// tracing/metrics.
pub fn app(state: SharedState, extra_v1: Router<SharedState>) -> Router {
    let limiter = Arc::new(AuthRateLimiter::new(
        state.cfg.limits.auth_per_minute,
        state.cfg.trust_proxy,
    ));
    Router::new()
        .route("/healthz", get(healthz))
        .nest("/v1", router().merge(extra_v1))
        .fallback(not_found)
        // Innermost first: rate limit → instance gate → tracing/metrics (outermost).
        .layer(from_fn_with_state(limiter, auth_rate_limit))
        .layer(from_fn_with_state(state.clone(), instance_gate))
        .layer(from_fn_with_state(state.clone(), crate::observe::track))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn not_found() -> ApiError {
    ApiError::NotFound
}

async fn info(State(state): State<SharedState>) -> Result<Json<serde_json::Value>, ApiError> {
    let signup = crate::auth::signup_allowed(&state).await?;
    Ok(Json(json!({
        "name": "copper-cloud",
        "version": env!("CARGO_PKG_VERSION"),
        "signup": signup,
        "limits": {
            "max_blob_bytes": state.cfg.limits.max_blob_bytes,
            "max_history_batch": state.cfg.limits.max_history_batch,
            "max_history_entry_bytes": state.cfg.limits.max_history_entry_bytes,
        },
    })))
}

/// Middleware: everything except `/healthz` must present `X-Copper-Instance: <instance_key>`
/// (compared in constant time) or gets 401 `{"error":"instance_key"}`.
pub async fn instance_gate(State(state): State<SharedState>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/healthz" {
        return next.run(req).await;
    }
    let presented = req
        .headers()
        .get(INSTANCE_HEADER)
        .map(HeaderValue::as_bytes)
        .unwrap_or_default();
    if instance_key_matches(presented, state.cfg.instance_key.as_bytes()) {
        next.run(req).await
    } else {
        ApiError::Unauthorized("instance_key").into_response()
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

async fn auth_rate_limit(
    State(limiter): State<Arc<AuthRateLimiter>>,
    req: Request,
    next: Next,
) -> Response {
    let path = req
        .extensions()
        .get::<OriginalUri>()
        .map_or_else(|| req.uri().path(), |u| u.0.path());
    // `GET /v1/auth/me` is bearer-authenticated and read-only (no credential oracle: tokens
    // are 256-bit), and clients call it for status; everything else under /v1/auth counts.
    if !path.starts_with("/v1/auth/")
        || (path == "/v1/auth/me" && req.method() == axum::http::Method::GET)
    {
        return next.run(req).await;
    }
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
