//! Accounts, devices and bearer sessions.
//!
//! * Passwords: Argon2id (m = 64 MiB, t = 3, p = 1), hashed on the blocking pool with bounded
//!   concurrency so a login burst cannot exhaust memory.
//! * Sessions: 32 random bytes (base64url) handed to the client once; the DB stores only
//!   SHA-256(token). Sliding 90-day expiry, touched at most once a minute.
//! * Devices: client-generated UUIDs, scoped per user.

use std::sync::OnceLock;

use argon2::password_hash::{PasswordHash, PasswordHasher as _, PasswordVerifier as _, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::extract::{FromRequestParts, Path, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::crypto::Crypto;
use crate::error::{is_unique_violation, ApiError, ApiResult};
use crate::extract::JsonBody;
use crate::ids;
use crate::state::SharedState;

/// Sliding session lifetime (days of inactivity before a session dies).
pub const SESSION_TTL_DAYS: i64 = 90;
pub const MIN_PASSWORD_CHARS: usize = 10;
pub const MAX_PASSWORD_BYTES: usize = 1024;
/// Request body cap for auth endpoints.
const AUTH_BODY_LIMIT: usize = 16 * 1024;

// ---------------------------------------------------------------------------------------------
// Extractor

/// The authenticated caller. Extract it in any handler on a `Router<SharedState>`:
/// reads `Authorization: Bearer <token>` (or `?token=` for WebSocket tooling), verifies
/// SHA-256(token) against `sessions`, touches `last_seen_at` (≤ once / 60 s) and records
/// `user_id` on the request span.
#[derive(Clone, Debug)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub email: String,
    pub device_id: Uuid,
    pub session_id: Uuid,
}

/// Cached per request so several extractors in one handler cost one query.
#[derive(Clone)]
pub(crate) struct AuthContext {
    pub user: AuthUser,
    pub data_key_wrapped: Vec<u8>,
}

impl FromRequestParts<SharedState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &SharedState,
    ) -> Result<Self, Self::Rejection> {
        Ok(authenticate(parts, state).await?.user)
    }
}

/// An authenticated caller plus their unwrapped data key (sync handlers).
pub(crate) struct KeyedUser {
    pub user: AuthUser,
    pub data_key: [u8; 32],
}

impl FromRequestParts<SharedState> for KeyedUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &SharedState,
    ) -> Result<Self, Self::Rejection> {
        let ctx = authenticate(parts, state).await?;
        let data_key = state.crypto.unwrap_key(&ctx.data_key_wrapped)?;
        Ok(Self {
            user: ctx.user,
            data_key,
        })
    }
}

fn bearer_token(parts: &Parts) -> Option<&str> {
    if let Some(value) = parts.headers.get(AUTHORIZATION) {
        let value = value.to_str().ok()?;
        let (scheme, token) = value.split_once(' ')?;
        return scheme
            .eq_ignore_ascii_case("bearer")
            .then_some(token.trim());
    }
    parts
        .uri
        .query()?
        .split('&')
        .find_map(|kv| kv.strip_prefix("token="))
}

/// Tokens are 32 bytes base64url → exactly 43 URL-safe chars; reject anything else without
/// touching the database.
fn plausible_token(t: &str) -> bool {
    t.len() == 43
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

const AUTH_SQL: &str = "
WITH s AS (
    SELECT s.id, s.user_id, s.device_id, u.email, u.data_key_wrapped
    FROM sessions s
    JOIN users u ON u.id = s.user_id
    WHERE s.token_sha256 = $1 AND s.expires_at > now() AND NOT u.disabled
), touch AS (
    UPDATE sessions
    SET last_seen_at = now(), expires_at = now() + make_interval(days => $2)
    WHERE id IN (SELECT id FROM s) AND last_seen_at < now() - interval '60 seconds'
    RETURNING user_id, device_id
), touch_device AS (
    UPDATE devices d SET last_seen_at = now()
    FROM touch t
    WHERE d.user_id = t.user_id AND d.id = t.device_id
)
SELECT id, user_id, device_id, email, data_key_wrapped FROM s";

pub(crate) async fn authenticate(parts: &mut Parts, state: &SharedState) -> ApiResult<AuthContext> {
    if let Some(ctx) = parts.extensions.get::<AuthContext>() {
        return Ok(ctx.clone());
    }
    let token = bearer_token(parts).ok_or(ApiError::Unauthorized("session"))?;
    if !plausible_token(token) {
        return Err(ApiError::Unauthorized("session"));
    }
    let digest = ids::sha256(token.as_bytes());
    let row: Option<(Uuid, Uuid, Uuid, String, Vec<u8>)> = sqlx::query_as(AUTH_SQL)
        .bind(&digest[..])
        .bind(i32::try_from(SESSION_TTL_DAYS).unwrap_or(90))
        .fetch_optional(&state.db)
        .await?;
    let (session_id, user_id, device_id, email, data_key_wrapped) =
        row.ok_or(ApiError::Unauthorized("session"))?;
    tracing::Span::current().record("user_id", tracing::field::display(user_id));
    let ctx = AuthContext {
        user: AuthUser {
            user_id,
            email,
            device_id,
            session_id,
        },
        data_key_wrapped,
    };
    parts.extensions.insert(ctx.clone());
    Ok(ctx)
}

/// Email of a user (any state), `None` if no such user.
pub async fn user_email(db: &sqlx::PgPool, user_id: Uuid) -> Result<Option<String>, ApiError> {
    Ok(sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(db)
        .await?)
}

/// Case-insensitive lookup of a user id by email.
pub async fn user_id_by_email(db: &sqlx::PgPool, email: &str) -> Result<Option<Uuid>, ApiError> {
    Ok(
        sqlx::query_scalar("SELECT id FROM users WHERE lower(email) = lower($1)")
            .bind(email.trim())
            .fetch_optional(db)
            .await?,
    )
}

// ---------------------------------------------------------------------------------------------
// Password hashing

fn argon2() -> Argon2<'static> {
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(64 * 1024, 3, 1, None).expect("valid argon2 params"),
    )
}

/// Bounds concurrent Argon2 computations (each holds 64 MiB).
fn hash_slots() -> &'static Semaphore {
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| {
        let cpus = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
        Semaphore::new(cpus.clamp(2, 8))
    })
}

/// Argon2id PHC string for `password` (runs on the blocking pool).
pub async fn hash_password(password: String) -> ApiResult<String> {
    let _permit = hash_slots().acquire().await.map_err(ApiError::internal)?;
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut rand::rngs::OsRng);
        argon2()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| ApiError::internal(format!("argon2: {e}")))
    })
    .await?
}

/// Constant-work verify. `hash = None` (unknown user) still burns one Argon2 computation so
/// response timing does not reveal which emails exist.
pub async fn verify_password(password: String, hash: Option<String>) -> ApiResult<bool> {
    let _permit = hash_slots().acquire().await.map_err(ApiError::internal)?;
    tokio::task::spawn_blocking(move || {
        static DUMMY: OnceLock<String> = OnceLock::new();
        let real = hash.is_some();
        let hash = hash.unwrap_or_else(|| {
            DUMMY
                .get_or_init(|| {
                    let salt = SaltString::generate(&mut rand::rngs::OsRng);
                    argon2()
                        .hash_password(b"copper-cloud-dummy-password", &salt)
                        .map(|h| h.to_string())
                        .unwrap_or_default()
                })
                .clone()
        });
        let Ok(parsed) = PasswordHash::new(&hash) else {
            return Ok(false);
        };
        let ok = argon2()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok();
        Ok(ok && real)
    })
    .await?
}

// ---------------------------------------------------------------------------------------------
// Validation

/// Trimmed, plausibly-shaped email (`local@domain.tld`-ish, ≤ 254 bytes, no whitespace).
pub fn normalize_email(raw: &str) -> ApiResult<String> {
    let email = raw.trim();
    let valid = email.len() <= 254
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !domain.contains('@')
        });
    if valid {
        Ok(email.to_owned())
    } else {
        Err(ApiError::bad_request("invalid email address"))
    }
}

pub fn validate_password(pw: &str) -> ApiResult<()> {
    if pw.chars().count() < MIN_PASSWORD_CHARS {
        return Err(ApiError::bad_request(format!(
            "password must be at least {MIN_PASSWORD_CHARS} characters"
        )));
    }
    if pw.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::bad_request("password is too long"));
    }
    Ok(())
}

pub fn clean_name(raw: Option<&str>, max_chars: usize) -> Option<String> {
    let s: String = raw?
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect();
    (!s.is_empty()).then_some(s)
}

// ---------------------------------------------------------------------------------------------
// Wire types

#[derive(Debug, Deserialize, Default)]
pub struct DeviceInput {
    pub id: Option<Uuid>,
    pub name: Option<String>,
}

#[derive(Deserialize)]
pub struct SignupRequest {
    pub email: String,
    pub password: String,
    pub display_name: Option<String>,
    #[serde(default)]
    pub device: Option<DeviceInput>,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub device: Option<DeviceInput>,
}

#[derive(Deserialize, Default)]
pub struct LogoutRequest {
    /// Revoke every session of this user, not just the current one.
    #[serde(default)]
    pub all: bool,
}

#[derive(Deserialize)]
pub struct PasswordRequest {
    pub old: String,
    pub new: String,
}

#[derive(Deserialize)]
pub struct DeviceRename {
    pub name: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct UserView {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct DeviceView {
    pub id: Uuid,
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen_at: OffsetDateTime,
}

#[derive(Serialize)]
pub struct SessionResponse {
    pub token: String,
    pub user: UserView,
    pub device: DeviceView,
}

#[derive(Serialize)]
struct DeviceListItem {
    #[serde(flatten)]
    device: DeviceView,
    current: bool,
}

// ---------------------------------------------------------------------------------------------
// Handlers

pub(crate) fn routes() -> Router<SharedState> {
    Router::new()
        .route("/auth/signup", post(signup))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
        .route("/auth/password", post(change_password))
        .route("/devices", get(list_devices))
        .route("/devices/{id}", patch(rename_device).delete(delete_device))
}

/// Effective signup policy: the admin CLI override (`server_settings`) wins over the config.
pub async fn signup_allowed(state: &SharedState) -> ApiResult<bool> {
    let (has_users, override_): (bool, Option<serde_json::Value>) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM users), \
                (SELECT value FROM server_settings WHERE key = 'allow_signup')",
    )
    .fetch_one(&state.db)
    .await?;
    if !has_users {
        return Ok(true);
    }
    Ok(override_
        .and_then(|v| v.as_bool())
        .unwrap_or(state.cfg.allow_signup))
}

async fn upsert_device(
    tx: &mut sqlx::PgConnection,
    user_id: Uuid,
    input: Option<DeviceInput>,
) -> ApiResult<DeviceView> {
    let input = input.unwrap_or_default();
    let id = input.id.unwrap_or_else(ids::uuid_v7);
    let name = clean_name(input.name.as_deref(), 200);
    Ok(sqlx::query_as::<_, DeviceView>(
        "INSERT INTO devices (user_id, id, name) VALUES ($1, $2, COALESCE($3, 'Copper'))
         ON CONFLICT (user_id, id) DO UPDATE
           SET name = COALESCE($3, devices.name), last_seen_at = now()
         RETURNING id, name, created_at, last_seen_at",
    )
    .bind(user_id)
    .bind(id)
    .bind(name)
    .fetch_one(tx)
    .await?)
}

async fn create_session(
    tx: &mut sqlx::PgConnection,
    user_id: Uuid,
    device_id: Uuid,
) -> ApiResult<String> {
    let token = ids::random_token();
    let digest = ids::sha256(token.as_bytes());
    sqlx::query(
        "INSERT INTO sessions (id, user_id, device_id, token_sha256, expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(days => $5))",
    )
    .bind(ids::uuid_v7())
    .bind(user_id)
    .bind(device_id)
    .bind(&digest[..])
    .bind(i32::try_from(SESSION_TTL_DAYS).unwrap_or(90))
    .execute(tx)
    .await?;
    Ok(token)
}

async fn signup(
    State(state): State<SharedState>,
    JsonBody(req): JsonBody<SignupRequest, AUTH_BODY_LIMIT>,
) -> ApiResult<Json<SessionResponse>> {
    let email = normalize_email(&req.email)?;
    validate_password(&req.password)?;
    let display_name = clean_name(req.display_name.as_deref(), 200)
        .unwrap_or_else(|| default_display_name(&email));
    if !signup_allowed(&state).await? {
        return Err(ApiError::Forbidden);
    }
    if user_id_by_email(&state.db, &email).await?.is_some() {
        return Err(email_taken());
    }
    let password_hash = hash_password(req.password).await?;

    let mut tx = state.db.begin().await?;
    let user = insert_user(
        &mut tx,
        &state.crypto,
        &email,
        &display_name,
        &password_hash,
    )
    .await?;
    let device = upsert_device(&mut tx, user.id, req.device).await?;
    let token = create_session(&mut tx, user.id, device.id).await?;
    tx.commit().await?;
    tracing::Span::current().record("user_id", tracing::field::display(user.id));
    tracing::info!(user_id = %user.id, device_id = %device.id, "signup");
    Ok(Json(SessionResponse {
        token,
        user,
        device,
    }))
}

/// Insert a user with a fresh wrapped data key. `email` must already be normalized and
/// `password_hash` produced by [`hash_password`]. 409 if the email is taken.
pub async fn insert_user(
    conn: &mut sqlx::PgConnection,
    crypto: &Crypto,
    email: &str,
    display_name: &str,
    password_hash: &str,
) -> ApiResult<UserView> {
    let wrapped = crypto.wrap_key(&Crypto::new_key());
    sqlx::query_as::<_, UserView>(
        "INSERT INTO users (id, email, display_name, password_hash, data_key_wrapped)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, email, display_name, created_at",
    )
    .bind(ids::uuid_v7())
    .bind(email)
    .bind(display_name)
    .bind(password_hash)
    .bind(&wrapped)
    .fetch_one(conn)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            email_taken()
        } else {
            e.into()
        }
    })
}

/// Default display name for an email: its local part.
pub fn default_display_name(email: &str) -> String {
    email.split('@').next().unwrap_or_default().to_owned()
}

fn email_taken() -> ApiError {
    ApiError::Conflict(json!({ "message": "an account with this email already exists" }))
}

async fn login(
    State(state): State<SharedState>,
    JsonBody(req): JsonBody<LoginRequest, AUTH_BODY_LIMIT>,
) -> ApiResult<Json<SessionResponse>> {
    if req.password.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::Unauthorized("credentials"));
    }
    let row: Option<(Uuid, String, bool)> = sqlx::query_as(
        "SELECT id, password_hash, disabled FROM users WHERE lower(email) = lower($1)",
    )
    .bind(req.email.trim())
    .fetch_optional(&state.db)
    .await?;
    let ok = verify_password(req.password, row.as_ref().map(|r| r.1.clone())).await?;
    let Some((user_id, _, disabled)) = row.filter(|_| ok) else {
        tracing::info!("login failed");
        return Err(ApiError::Unauthorized("credentials"));
    };
    if disabled {
        return Err(ApiError::Unauthorized("account_disabled"));
    }
    let mut tx = state.db.begin().await?;
    let user = sqlx::query_as::<_, UserView>(
        "SELECT id, email, display_name, created_at FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;
    let device = upsert_device(&mut tx, user_id, req.device).await?;
    let token = create_session(&mut tx, user_id, device.id).await?;
    tx.commit().await?;
    tracing::Span::current().record("user_id", tracing::field::display(user_id));
    tracing::info!(user_id = %user_id, device_id = %device.id, "login");
    Ok(Json(SessionResponse {
        token,
        user,
        device,
    }))
}

async fn logout(
    State(state): State<SharedState>,
    user: AuthUser,
    body: axum::body::Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    let all = if body.iter().all(u8::is_ascii_whitespace) {
        false
    } else {
        serde_json::from_slice::<LogoutRequest>(&body)
            .map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))?
            .all
    };
    let revoked = if all {
        sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(user.user_id)
            .execute(&state.db)
            .await?
    } else {
        sqlx::query("DELETE FROM sessions WHERE id = $1 AND user_id = $2")
            .bind(user.session_id)
            .bind(user.user_id)
            .execute(&state.db)
            .await?
    }
    .rows_affected();
    Ok(Json(json!({ "ok": true, "revoked": revoked })))
}

async fn me(
    State(state): State<SharedState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let u = sqlx::query_as::<_, UserView>(
        "SELECT id, email, display_name, created_at FROM users WHERE id = $1",
    )
    .bind(user.user_id)
    .fetch_one(&state.db)
    .await?;
    let device = sqlx::query_as::<_, DeviceView>(
        "SELECT id, name, created_at, last_seen_at FROM devices WHERE user_id = $1 AND id = $2",
    )
    .bind(user.user_id)
    .bind(user.device_id)
    .fetch_one(&state.db)
    .await?;
    let history_seq: i64 =
        sqlx::query_scalar("SELECT COALESCE(max(seq), 0) FROM history WHERE user_id = $1")
            .bind(user.user_id)
            .fetch_one(&state.db)
            .await?;
    Ok(Json(json!({
        "user": u,
        "device": device,
        "sync_cursor": { "history_seq": history_seq },
    })))
}

async fn change_password(
    State(state): State<SharedState>,
    user: AuthUser,
    JsonBody(req): JsonBody<PasswordRequest, AUTH_BODY_LIMIT>,
) -> ApiResult<Json<serde_json::Value>> {
    validate_password(&req.new)?;
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
        .bind(user.user_id)
        .fetch_one(&state.db)
        .await?;
    if req.old.len() > MAX_PASSWORD_BYTES || !verify_password(req.old, Some(hash)).await? {
        return Err(ApiError::Unauthorized("credentials"));
    }
    let new_hash = hash_password(req.new).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE users SET password_hash = $2, updated_at = now() WHERE id = $1")
        .bind(user.user_id)
        .bind(&new_hash)
        .execute(&mut *tx)
        .await?;
    // Every other session must sign in again with the new password.
    let revoked = sqlx::query("DELETE FROM sessions WHERE user_id = $1 AND id <> $2")
        .bind(user.user_id)
        .bind(user.session_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    tracing::info!(user_id = %user.user_id, revoked, "password changed");
    Ok(Json(json!({ "ok": true, "revoked_sessions": revoked })))
}

async fn list_devices(
    State(state): State<SharedState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<serde_json::Value>>> {
    let devices = sqlx::query_as::<_, DeviceView>(
        "SELECT id, name, created_at, last_seen_at FROM devices
         WHERE user_id = $1 ORDER BY last_seen_at DESC",
    )
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await?;
    let out = devices
        .into_iter()
        .map(|d| {
            let current = d.id == user.device_id;
            serde_json::to_value(DeviceListItem { device: d, current })
                .unwrap_or(serde_json::Value::Null)
        })
        .collect();
    Ok(Json(out))
}

async fn rename_device(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    JsonBody(req): JsonBody<DeviceRename, AUTH_BODY_LIMIT>,
) -> ApiResult<Json<DeviceView>> {
    let name = clean_name(Some(&req.name), 200)
        .ok_or_else(|| ApiError::bad_request("name must not be empty"))?;
    let device = sqlx::query_as::<_, DeviceView>(
        "UPDATE devices SET name = $3 WHERE user_id = $1 AND id = $2
         RETURNING id, name, created_at, last_seen_at",
    )
    .bind(user.user_id)
    .bind(id)
    .bind(name)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok(Json(device))
}

/// Remove a device: revokes its sessions (FK cascade) and deletes its `tabs:<id>` doc.
async fn delete_device(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let mut tx = state.db.begin().await?;
    let deleted = sqlx::query("DELETE FROM devices WHERE user_id = $1 AND id = $2")
        .bind(user.user_id)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    let domain = crate::sync::tabs_domain(id);
    sqlx::query("DELETE FROM sync_docs WHERE user_id = $1 AND domain = $2")
        .bind(user.user_id)
        .bind(&domain)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_validation() {
        assert_eq!(normalize_email("  A@B.co ").unwrap(), "A@B.co");
        for bad in [
            "", "a", "a@b", "@b.co", "a@.co", "a@b.co.", "a b@c.de", "a@b@c.de",
        ] {
            assert!(normalize_email(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn password_rules() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("0123456789").is_ok());
        assert!(validate_password("ññññññññññ").is_ok());
        assert!(validate_password(&"x".repeat(MAX_PASSWORD_BYTES + 1)).is_err());
    }

    #[test]
    fn token_plausibility() {
        assert!(plausible_token(&ids::random_token()));
        assert!(!plausible_token("abc"));
        assert!(!plausible_token(&"a".repeat(42).add_char('!')));
    }

    trait AddChar {
        fn add_char(self, c: char) -> String;
    }
    impl AddChar for String {
        fn add_char(mut self, c: char) -> String {
            self.push(c);
            self
        }
    }

    #[tokio::test]
    async fn hash_and_verify() {
        let h = hash_password("correct horse battery".into()).await.unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=65536,t=3,p=1$"));
        assert!(
            verify_password("correct horse battery".into(), Some(h.clone()))
                .await
                .unwrap()
        );
        assert!(!verify_password("wrong horse battery".into(), Some(h))
            .await
            .unwrap());
        assert!(!verify_password("anything at all".into(), None)
            .await
            .unwrap());
    }
}
