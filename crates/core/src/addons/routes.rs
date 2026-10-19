//! Client API (device session auth, like `/v1/intelligence`):
//!
//! * `GET /v1/addons` — what the caller gets (`{revision, addons}`), `ETag: "<revision>"`,
//!   `If-None-Match` → 304.
//! * `GET /v1/addons/{id}/bundle/{sha256}` — the served bundle, only while that version is
//!   resolved for the caller (else 404); immutable.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use super::store;
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use crate::state::SharedState;

pub(crate) fn routes() -> Router<SharedState> {
    Router::new()
        .route("/addons", get(get_addons))
        .route("/addons/{id}/bundle/{sha256}", get(get_bundle))
}

/// Whether an `If-None-Match` header matches `revision`. Accepts the quoted `ETag`, a bare
/// revision (clients that store `revision` and send it back), weak validators, lists and `*`.
pub(crate) fn if_none_match(headers: &HeaderMap, revision: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|tag| {
            let tag = tag.strip_prefix("W/").unwrap_or(tag);
            tag == "*" || tag.trim_matches('"') == revision
        })
}

async fn get_addons(
    State(state): State<SharedState>,
    user: AuthUser,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let resolved = store::resolve(&state.db, &user.email).await?;
    let etag =
        HeaderValue::from_str(&format!("\"{}\"", resolved.revision)).map_err(ApiError::internal)?;
    let mut resp = if if_none_match(&headers, &resolved.revision) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        tracing::debug!(
            revision = %resolved.revision,
            addons = resolved.addons.len(),
            "addons listed"
        );
        Json(resolved).into_response()
    };
    let h = resp.headers_mut();
    h.insert(header::ETAG, etag);
    // Settings may hold credentials: no shared or disk caches (as /v1/intelligence).
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

async fn get_bundle(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((id, sha256)): Path<(String, String)>,
) -> ApiResult<Response> {
    if !super::manifest::valid_id(&id) || !is_sha256_hex(&sha256) {
        return Err(ApiError::NotFound);
    }
    let bytes = store::resolved_bundle(&state.db, &user.email, &id, &sha256).await?;
    let mut resp = bytes.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{sha256}\"")) {
        h.insert(header::ETAG, etag);
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn if_none_match_forms() {
        let rev = "r_0123456789abcdef";
        for v in [
            "\"r_0123456789abcdef\"",
            "r_0123456789abcdef",
            "W/\"r_0123456789abcdef\"",
            "\"r_x\", \"r_0123456789abcdef\"",
            "*",
        ] {
            let mut h = HeaderMap::new();
            h.insert(header::IF_NONE_MATCH, HeaderValue::from_str(v).unwrap());
            assert!(if_none_match(&h, rev), "{v}");
        }
        let mut h = HeaderMap::new();
        assert!(!if_none_match(&h, rev));
        h.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_static("\"r_other\""),
        );
        assert!(!if_none_match(&h, rev));
    }

    #[test]
    fn sha_shape() {
        assert!(is_sha256_hex(&"a1".repeat(32)));
        assert!(!is_sha256_hex(&"A1".repeat(32)));
        assert!(!is_sha256_hex("abc"));
    }
}
