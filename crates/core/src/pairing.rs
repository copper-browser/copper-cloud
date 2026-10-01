//! Single-use pairing codes: a signed-in Copper mints a code that links **and** signs in
//! another Copper in one step.
//!
//! * `POST /v1/auth/pairing {device_name?}` 🔒 → `{id, code, link, expires_at, device_name}`.
//!   `code` = `cp_` + 24 random bytes base64url; the DB stores only SHA-256(code). Valid
//!   [`PAIRING_TTL_MINUTES`] minutes.
//! * `GET /v1/auth/pairing` 🔒 → my active (unused, unexpired) codes.
//! * `DELETE /v1/auth/pairing/{id}` 🔒 → revoke an unused code.
//! * `POST /v1/auth/pair {code, device:{id,name}}` — **no gate header** (the code is the
//!   credential; rate limited with `/v1/auth`) → `{token, user, device, gate_key}`. The code
//!   is consumed atomically (`UPDATE … WHERE used_at IS NULL … RETURNING`). `gate_key` is what
//!   the new Copper sends in `X-Copper-Instance` from now on: the instance key in `open`
//!   mode, a freshly minted access key (label `"<device name> via pairing"`, email = the
//!   user's) in `directory` mode.

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::access::{self, AccessMode, NewAccessKey};
use crate::auth::{self, AuthUser, DeviceInput, SessionResponse, UserView};
use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::extract::{JsonBody, OptionalJsonBody};
use crate::ids;
use crate::link::LinkCode;
use crate::state::SharedState;

/// Prefix of every pairing code.
pub const PAIRING_PREFIX: &str = "cp_";
/// Lifetime of a pairing code.
pub const PAIRING_TTL_MINUTES: i32 = 10;
/// Most unused codes one user may hold at a time.
pub const MAX_ACTIVE_CODES: i64 = 20;
const BODY_LIMIT: usize = 16 * 1024;

/// A fresh pairing code: `cp_` + 32 base64url chars (24 random bytes).
pub fn new_pairing_code() -> String {
    format!(
        "{PAIRING_PREFIX}{}",
        ids::b64url(&ids::random_bytes::<24>())
    )
}

/// Shape check before touching the database.
pub fn plausible_pairing_code(s: &str) -> bool {
    s.strip_prefix(PAIRING_PREFIX).is_some_and(|rest| {
        rest.len() == 32
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

/// The instance's link code (fingerprint included when the certificate is readable). Falls
/// back to `public_url` without a fingerprint if the certificate cannot be read.
pub fn instance_link(cfg: &Config) -> LinkCode {
    crate::tls::link_code(cfg).unwrap_or_else(|err| {
        tracing::warn!(error = format!("{err:#}"), "link code without fingerprint");
        LinkCode {
            host: cfg.public_host().to_owned(),
            port: cfg.public_port(),
            instance_key: cfg.instance_key.clone(),
            fingerprint: None,
        }
    })
}

pub(crate) fn routes() -> Router<SharedState> {
    Router::new()
        .route("/auth/pairing", get(list_codes).post(create_code))
        .route("/auth/pairing/{id}", delete(revoke_code))
        .route("/auth/pair", post(pair))
}

#[derive(Deserialize, Default)]
struct CreateRequest {
    device_name: Option<String>,
}

#[derive(Serialize)]
struct CreatedCode {
    id: Uuid,
    code: String,
    link: String,
    device_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

#[derive(Serialize, sqlx::FromRow)]
struct CodeView {
    id: Uuid,
    device_name: Option<String>,
    created_by_device: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

async fn create_code(
    State(state): State<SharedState>,
    user: AuthUser,
    OptionalJsonBody(req): OptionalJsonBody<CreateRequest, BODY_LIMIT>,
) -> ApiResult<Json<CreatedCode>> {
    let device_name = auth::clean_name(req.device_name.as_deref(), 200);
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pairing_codes
         WHERE user_id = $1 AND used_at IS NULL AND expires_at > now()",
    )
    .bind(user.user_id)
    .fetch_one(&state.db)
    .await?;
    if active >= MAX_ACTIVE_CODES {
        return Err(ApiError::bad_request(format!(
            "too many active pairing codes (max {MAX_ACTIVE_CODES}); revoke some first"
        )));
    }
    let code = new_pairing_code();
    let digest = ids::sha256(code.as_bytes());
    let id = ids::uuid_v7();
    let (created_at, expires_at): (OffsetDateTime, OffsetDateTime) = sqlx::query_as(
        "INSERT INTO pairing_codes
             (id, user_id, created_by_device, device_name, code_sha256, expires_at)
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(mins => $6))
         RETURNING created_at, expires_at",
    )
    .bind(id)
    .bind(user.user_id)
    .bind(user.device_id)
    .bind(&device_name)
    .bind(&digest[..])
    .bind(PAIRING_TTL_MINUTES)
    .fetch_one(&state.db)
    .await?;
    let link = instance_link(&state.cfg).pairing_link(&code);
    tracing::info!(user_id = %user.user_id, pairing_id = %id, "pairing code created");
    Ok(Json(CreatedCode {
        id,
        code,
        link,
        device_name,
        created_at,
        expires_at,
    }))
}

async fn list_codes(
    State(state): State<SharedState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<CodeView>>> {
    let rows = sqlx::query_as::<_, CodeView>(
        "SELECT id, device_name, created_by_device, created_at, expires_at FROM pairing_codes
         WHERE user_id = $1 AND used_at IS NULL AND expires_at > now()
         ORDER BY created_at DESC, id DESC",
    )
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

async fn revoke_code(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let deleted =
        sqlx::query("DELETE FROM pairing_codes WHERE id = $1 AND user_id = $2 AND used_at IS NULL")
            .bind(id)
            .bind(user.user_id)
            .execute(&state.db)
            .await?
            .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct PairRequest {
    code: String,
    #[serde(default)]
    device: Option<DeviceInput>,
}

#[derive(Serialize)]
struct PairResponse {
    #[serde(flatten)]
    session: SessionResponse,
    gate_key: String,
}

fn bad_code() -> ApiError {
    ApiError::Unauthorized("pairing_code")
}

async fn pair(
    State(state): State<SharedState>,
    JsonBody(req): JsonBody<PairRequest, BODY_LIMIT>,
) -> ApiResult<Json<PairResponse>> {
    let code = req.code.trim();
    if !plausible_pairing_code(code) {
        return Err(bad_code());
    }
    let digest = ids::sha256(code.as_bytes());
    let mut device = req.device.unwrap_or_default();
    let device_id = device.id.unwrap_or_else(ids::uuid_v7);
    device.id = Some(device_id);

    let mut tx = state.db.begin().await?;
    // Single use: only one transaction can flip used_at from NULL.
    let claimed: Option<(Uuid, Uuid, Option<String>)> = sqlx::query_as(
        "UPDATE pairing_codes SET used_at = now(), used_by_device = $2
         WHERE code_sha256 = $1 AND used_at IS NULL AND expires_at > now()
         RETURNING id, user_id, device_name",
    )
    .bind(&digest[..])
    .bind(device_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((pairing_id, user_id, code_device_name)) = claimed else {
        tracing::info!("pairing failed: unknown, used or expired code");
        return Err(bad_code());
    };
    let user: Option<(UserView, bool)> =
        sqlx::query_as::<_, (Uuid, String, String, OffsetDateTime, bool)>(
            "SELECT id, email, display_name, created_at, disabled FROM users WHERE id = $1",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?
        .map(|(id, email, display_name, created_at, disabled)| {
            (
                UserView {
                    id,
                    email,
                    display_name,
                    created_at,
                },
                disabled,
            )
        });
    let Some((user, disabled)) = user else {
        return Err(bad_code());
    };
    if disabled {
        return Err(ApiError::Unauthorized("account_disabled"));
    }
    if auth::clean_name(device.name.as_deref(), 200).is_none() {
        device.name = code_device_name;
    }
    let device = auth::upsert_device(&mut tx, user.id, Some(device)).await?;
    let token = auth::create_session(&mut tx, user.id, device.id).await?;
    let gate_key = match access::access_mode(&state).await? {
        AccessMode::Open => state.cfg.instance_key.clone(),
        AccessMode::Directory => {
            let label = format!("{} via pairing", device.name);
            let (key_id, key) = access::mint_access_key(
                &mut tx,
                &NewAccessKey {
                    label: &label,
                    email: Some(&user.email),
                    created_by_user: Some(user.id),
                    ..NewAccessKey::default()
                },
            )
            .await?;
            tracing::info!(user_id = %user.id, access_key_id = %key_id, "access key minted by pairing");
            key
        }
    };
    tx.commit().await?;
    tracing::Span::current().record("user_id", tracing::field::display(user.id));
    tracing::info!(user_id = %user.id, device_id = %device.id, %pairing_id, "paired");
    Ok(Json(PairResponse {
        session: SessionResponse {
            token,
            user,
            device,
        },
        gate_key,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_code_format() {
        let c = new_pairing_code();
        assert!(c.starts_with("cp_"));
        assert_eq!(c.len(), 3 + 32);
        assert!(plausible_pairing_code(&c));
        assert_eq!(ids::b64url_decode(&c[3..]).unwrap().len(), 24);
        assert_ne!(c, new_pairing_code());
        for bad in ["", "cp_", "cp_short", &c[3..], &format!("ck_{}", &c[3..])] {
            assert!(!plausible_pairing_code(bad), "{bad}");
        }
        assert!(!plausible_pairing_code(&format!("{c}!")));
        let mut wrong = c.clone();
        wrong.replace_range(5..6, "!");
        assert!(!plausible_pairing_code(&wrong));
    }
}
