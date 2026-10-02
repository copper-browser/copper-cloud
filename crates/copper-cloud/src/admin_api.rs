//! `/admin/api/*` — the admin portal's JSON API (contract: `docs/admin-api.md`).
//!
//! * Not behind the instance gate: admins have their own accounts (`admins`) and cookie
//!   sessions (`cc_admin`: `HttpOnly`, `Secure` unless `tls.mode = off`,
//!   `SameSite=Strict`, `Path=/admin`, fixed 7-day lifetime, only SHA-256(token) stored).
//! * CSRF: every non-GET/HEAD request must carry `X-Requested-With: copper-cloud-portal`
//!   (a custom header cannot be sent cross-site without CORS, which we never grant), on top
//!   of SameSite=Strict.
//! * `POST login` is rate limited per client IP (see `copper_cloud_core::app`).
//! * Every mutation writes an `admin_audit` row. Every list paginates (`?limit=&offset=`,
//!   default 100, max 500), newest first, as `{items, total, limit, offset}`.

use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, Method};
use axum::middleware::{from_fn, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use copper_cloud_core::access::{self, AccessMode, NewAccessKey};
use copper_cloud_core::auth;
use copper_cloud_core::config::TlsMode;
use copper_cloud_core::error::{is_unique_violation, ApiError, ApiResult};
use copper_cloud_core::extract::{JsonBody, QueryParams};
use copper_cloud_core::ids;
use copper_cloud_core::state::{AppState, SharedState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use time::OffsetDateTime;
use uuid::Uuid;

/// Session cookie name.
pub const COOKIE: &str = "cc_admin";
/// CSRF header (name lower-case) and its required value.
pub const CSRF_HEADER: &str = "x-requested-with";
pub const CSRF_VALUE: &str = "copper-cloud-portal";
/// Admin session lifetime (absolute, from login).
pub const SESSION_DAYS: i32 = 7;
pub const DEFAULT_LIMIT: i64 = 100;
pub const MAX_LIMIT: i64 = 500;
const BODY_LIMIT: usize = 16 * 1024;

/// Every admin route (mount under `/admin/api`).
pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/me", get(me))
        .route("/password", post(change_password))
        .route("/overview", get(overview))
        .route("/settings", get(get_settings).patch(patch_settings))
        .route("/access-keys", get(list_keys).post(create_key))
        .route("/access-keys/{id}", delete(revoke_key))
        .route("/users", get(list_users))
        .route("/users/{id}", patch(update_user).delete(delete_user))
        .route("/users/{id}/reset-password", post(reset_user_password))
        .route("/devices", get(list_devices))
        .route("/devices/{id}", delete(delete_device))
        .route("/canvases", get(list_canvases))
        .route("/canvases/{id}", delete(delete_canvas))
        .route("/pairing-codes", get(list_pairing_codes))
        .route("/pairing-codes/{id}", delete(revoke_pairing_code))
        .route("/audit", get(list_audit))
        .fallback(copper_cloud_core::app::not_found)
        .layer(from_fn(guard))
}

/// CSRF check for unsafe methods + `Cache-Control: no-store` on every admin response.
async fn guard(req: Request, next: Next) -> Response {
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let mut resp = if safe
        || req
            .headers()
            .get(CSRF_HEADER)
            .is_some_and(|v| v.as_bytes() == CSRF_VALUE.as_bytes())
    {
        next.run(req).await
    } else {
        ApiError::Denied {
            code: "csrf",
            message: "missing X-Requested-With: copper-cloud-portal",
        }
        .into_response()
    };
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

// ---------------------------------------------------------------------------------------------
// Session extractor

/// The signed-in admin (from the `cc_admin` cookie).
#[derive(Clone, Debug)]
pub struct AdminSession {
    pub admin_id: Uuid,
    pub email: String,
    pub session_id: Uuid,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

/// The value of cookie `name` from the request's `Cookie` header(s).
pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(k, v)| (k.trim() == name).then(|| v.trim()))
}

const SESSION_SQL: &str = "
WITH s AS (
    SELECT s.id, s.admin_id, a.email, s.created_at, s.expires_at
    FROM admin_sessions s JOIN admins a ON a.id = s.admin_id
    WHERE s.token_sha256 = $1 AND s.expires_at > now()
), touch AS (
    UPDATE admin_sessions SET last_seen_at = now()
    WHERE id IN (SELECT id FROM s) AND last_seen_at < now() - interval '60 seconds'
)
SELECT id, admin_id, email, created_at, expires_at FROM s";

impl FromRequestParts<SharedState> for AdminSession {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &SharedState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(s) = parts.extensions.get::<Self>() {
            return Ok(s.clone());
        }
        let token = cookie(&parts.headers, COOKIE)
            .filter(|t| ids::is_token_shape(t))
            .ok_or(ApiError::Unauthorized("admin_session"))?;
        let digest = ids::sha256(token.as_bytes());
        let row: Option<(Uuid, Uuid, String, OffsetDateTime, OffsetDateTime)> =
            sqlx::query_as(SESSION_SQL)
                .bind(&digest[..])
                .fetch_optional(&state.db)
                .await?;
        let (session_id, admin_id, email, created_at, expires_at) =
            row.ok_or(ApiError::Unauthorized("admin_session"))?;
        let session = Self {
            admin_id,
            email,
            session_id,
            created_at,
            expires_at,
        };
        parts.extensions.insert(session.clone());
        Ok(session)
    }
}

fn session_cookie(state: &AppState, token: &str, max_age: i64) -> HeaderValue {
    let secure = if state.cfg.tls.mode == TlsMode::Off {
        ""
    } else {
        "; Secure"
    };
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; Path=/admin; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}"
    ))
    .expect("cookie is ASCII")
}

// ---------------------------------------------------------------------------------------------
// Admin accounts (shared with the CLI)

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct AdminView {
    pub id: Uuid,
    pub email: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_login_at: Option<OffsetDateTime>,
}

/// Create an admin. `email` is validated and stored lower-cased; 409 if taken.
pub async fn create_admin(
    db: &sqlx::PgPool,
    email: &str,
    password: String,
) -> ApiResult<AdminView> {
    let email = auth::normalize_email(email)?.to_lowercase();
    auth::validate_password(&password)?;
    let hash = auth::hash_password(password).await?;
    sqlx::query_as::<_, AdminView>(
        "INSERT INTO admins (id, email, password_hash) VALUES ($1, $2, $3)
         RETURNING id, email, created_at, last_login_at",
    )
    .bind(ids::uuid_v7())
    .bind(&email)
    .bind(&hash)
    .fetch_one(db)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::Conflict(json!({ "message": "an admin with this email already exists" }))
        } else {
            e.into()
        }
    })
}

/// Look up an admin by email (case-insensitive).
pub async fn find_admin(db: &sqlx::PgPool, email: &str) -> ApiResult<Option<AdminView>> {
    Ok(sqlx::query_as::<_, AdminView>(
        "SELECT id, email, created_at, last_login_at FROM admins WHERE lower(email) = lower($1)",
    )
    .bind(email.trim())
    .fetch_optional(db)
    .await?)
}

/// Every admin, oldest first.
pub async fn list_admins(db: &sqlx::PgPool) -> ApiResult<Vec<AdminView>> {
    Ok(sqlx::query_as::<_, AdminView>(
        "SELECT id, email, created_at, last_login_at FROM admins ORDER BY created_at, id",
    )
    .fetch_all(db)
    .await?)
}

/// Set an admin's password and revoke their sessions (except `keep_session`). Returns the
/// number of sessions revoked.
pub async fn set_admin_password(
    db: &sqlx::PgPool,
    admin_id: Uuid,
    password: String,
    keep_session: Option<Uuid>,
) -> ApiResult<u64> {
    auth::validate_password(&password)?;
    let hash = auth::hash_password(password).await?;
    let mut tx = db.begin().await?;
    let updated = sqlx::query("UPDATE admins SET password_hash = $2 WHERE id = $1")
        .bind(admin_id)
        .bind(&hash)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if updated == 0 {
        return Err(ApiError::NotFound);
    }
    let revoked = sqlx::query(
        "DELETE FROM admin_sessions WHERE admin_id = $1 AND ($2::uuid IS NULL OR id <> $2)",
    )
    .bind(admin_id)
    .bind(keep_session)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(revoked)
}

async fn audit<'e, E: sqlx::PgExecutor<'e>>(
    db: E,
    admin_id: Uuid,
    action: &str,
    target: &str,
    detail: Option<Value>,
) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO admin_audit (admin_id, action, target, detail) VALUES ($1, $2, $3, $4)",
    )
    .bind(admin_id)
    .bind(action)
    .bind(target)
    .bind(detail)
    .execute(db)
    .await?;
    tracing::info!(%admin_id, action, audit_target = target, "admin action");
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Shared bits

fn ok() -> Json<Value> {
    Json(json!({ "ok": true }))
}

/// UUID path segment; anything else is a 404 (not a plain-text 400).
fn parse_id(raw: &str) -> ApiResult<Uuid> {
    raw.parse().map_err(|_| ApiError::NotFound)
}

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    limit: Option<i64>,
    offset: Option<i64>,
    status: Option<String>,
    q: Option<String>,
    user_id: Option<Uuid>,
    kind: Option<String>,
}

impl ListQuery {
    fn page(&self) -> (i64, i64) {
        (
            self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
            self.offset.unwrap_or(0).max(0),
        )
    }
}

#[derive(Serialize)]
struct ListResponse<T> {
    items: Vec<T>,
    total: i64,
    limit: i64,
    offset: i64,
}

/// `ILIKE` pattern matching `q` literally anywhere.
fn like_pattern(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

fn empty_to_none(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------------------------
// Session routes

#[derive(Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

async fn login(
    State(state): State<SharedState>,
    JsonBody(req): JsonBody<LoginRequest, BODY_LIMIT>,
) -> ApiResult<Response> {
    if req.password.len() > auth::MAX_PASSWORD_BYTES {
        return Err(ApiError::Unauthorized("credentials"));
    }
    let row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id, password_hash FROM admins WHERE lower(email) = lower($1)")
            .bind(req.email.trim())
            .fetch_optional(&state.db)
            .await?;
    let ok = auth::verify_password(req.password, row.as_ref().map(|r| r.1.clone())).await?;
    let Some((admin_id, _)) = row.filter(|_| ok) else {
        tracing::info!("admin login failed");
        return Err(ApiError::Unauthorized("credentials"));
    };
    let token = ids::random_token();
    let digest = ids::sha256(token.as_bytes());
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "INSERT INTO admin_sessions (id, admin_id, token_sha256, expires_at)
         VALUES ($1, $2, $3, now() + make_interval(days => $4))",
    )
    .bind(ids::uuid_v7())
    .bind(admin_id)
    .bind(&digest[..])
    .bind(SESSION_DAYS)
    .execute(&mut *tx)
    .await?;
    let admin = sqlx::query_as::<_, AdminView>(
        "UPDATE admins SET last_login_at = now() WHERE id = $1
         RETURNING id, email, created_at, last_login_at",
    )
    .bind(admin_id)
    .fetch_one(&mut *tx)
    .await?;
    audit(&mut *tx, admin_id, "login", &admin_id.to_string(), None).await?;
    tx.commit().await?;
    let cookie = session_cookie(&state, &token, i64::from(SESSION_DAYS) * 86_400);
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "admin": admin })),
    )
        .into_response())
}

async fn logout(
    State(state): State<SharedState>,
    session: Result<AdminSession, ApiError>,
) -> ApiResult<Response> {
    if let Ok(session) = session {
        let mut tx = state.db.begin().await?;
        sqlx::query("DELETE FROM admin_sessions WHERE id = $1")
            .bind(session.session_id)
            .execute(&mut *tx)
            .await?;
        audit(
            &mut *tx,
            session.admin_id,
            "logout",
            &session.admin_id.to_string(),
            None,
        )
        .await?;
        tx.commit().await?;
    }
    Ok(([(header::SET_COOKIE, session_cookie(&state, "", 0))], ok()).into_response())
}

async fn me(State(state): State<SharedState>, s: AdminSession) -> ApiResult<Json<Value>> {
    let admin = sqlx::query_as::<_, AdminView>(
        "SELECT id, email, created_at, last_login_at FROM admins WHERE id = $1",
    )
    .bind(s.admin_id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({
        "admin": admin,
        "session": {
            "created_at": rfc3339(s.created_at),
            "expires_at": rfc3339(s.expires_at),
        },
    })))
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct PasswordRequest {
    old: String,
    new: String,
}

async fn change_password(
    State(state): State<SharedState>,
    s: AdminSession,
    JsonBody(req): JsonBody<PasswordRequest, BODY_LIMIT>,
) -> ApiResult<Json<Value>> {
    auth::validate_password(&req.new)?;
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM admins WHERE id = $1")
        .bind(s.admin_id)
        .fetch_one(&state.db)
        .await?;
    if req.old.len() > auth::MAX_PASSWORD_BYTES
        || !auth::verify_password(req.old, Some(hash)).await?
    {
        return Err(ApiError::Unauthorized("credentials"));
    }
    let revoked = set_admin_password(&state.db, s.admin_id, req.new, Some(s.session_id)).await?;
    audit(
        &state.db,
        s.admin_id,
        "password.change",
        &s.admin_id.to_string(),
        None,
    )
    .await?;
    Ok(Json(json!({ "ok": true, "revoked_sessions": revoked })))
}

// ---------------------------------------------------------------------------------------------
// Overview + settings

async fn overview(State(state): State<SharedState>, _s: AdminSession) -> ApiResult<Json<Value>> {
    let (users, devices, canvases, keys): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM users),
                (SELECT count(*) FROM devices),
                (SELECT count(*) FROM canvases),
                (SELECT count(*) FROM access_keys
                 WHERE revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())
                   AND (max_uses IS NULL OR uses < max_uses))",
    )
    .fetch_one(&state.db)
    .await?;
    let rooms = copper_cloud_canvas::rooms_metrics();
    let mode = access::access_mode(&state).await?;
    let fingerprint = match state.cfg.tls.mode {
        TlsMode::Acme => None,
        _ => copper_cloud_core::tls::leaf_fingerprint(&state.cfg.tls.cert_path).ok(),
    };
    Ok(Json(json!({
        "version": crate::version(),
        "uptime_s": state.started_at.elapsed().as_secs(),
        "access_mode": mode,
        "allow_signup": allow_signup_setting(&state).await?,
        "counts": {
            "users": users,
            "devices": devices,
            "canvases": canvases,
            "access_keys": keys,
            "live_rooms": rooms.rooms,
            "live_peers": rooms.peers,
        },
        "public_url": state.cfg.public_url,
        "tls": { "mode": state.cfg.tls.mode.to_string(), "fingerprint": fingerprint },
    })))
}

/// `allow_signup` as configured: the runtime override, else the config file value.
async fn allow_signup_setting(state: &AppState) -> ApiResult<bool> {
    let v: Option<Value> =
        sqlx::query_scalar("SELECT value FROM server_settings WHERE key = 'allow_signup'")
            .fetch_optional(&state.db)
            .await?;
    Ok(v.and_then(|v| v.as_bool())
        .unwrap_or(state.cfg.allow_signup))
}

async fn settings_view(state: &AppState) -> ApiResult<Value> {
    let mode = access::load_access_mode(&state.db).await?;
    let link = copper_cloud_core::tls::link_code(&state.cfg)
        .ok()
        .map(|c| c.to_string());
    Ok(json!({
        "access_mode": mode,
        "allow_signup": allow_signup_setting(state).await?,
        "instance_link_code": link,
    }))
}

async fn get_settings(
    State(state): State<SharedState>,
    _s: AdminSession,
) -> ApiResult<Json<Value>> {
    Ok(Json(settings_view(&state).await?))
}

#[derive(Deserialize)]
struct SettingsPatch {
    access_mode: Option<String>,
    allow_signup: Option<bool>,
}

async fn patch_settings(
    State(state): State<SharedState>,
    s: AdminSession,
    JsonBody(req): JsonBody<SettingsPatch, BODY_LIMIT>,
) -> ApiResult<Json<Value>> {
    let mode = req
        .access_mode
        .as_deref()
        .map(|m| {
            AccessMode::parse(m).ok_or_else(|| {
                ApiError::bad_request("access_mode must be \"open\" or \"directory\"")
            })
        })
        .transpose()?;
    let mut changed = Map::new();
    let mut tx = state.db.begin().await?;
    if let Some(mode) = mode {
        access::set_access_mode(&mut *tx, mode).await?;
        changed.insert("access_mode".into(), json!(mode));
    }
    if let Some(on) = req.allow_signup {
        sqlx::query(
            "INSERT INTO server_settings (key, value) VALUES ('allow_signup', $1)
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
        )
        .bind(Value::Bool(on))
        .execute(&mut *tx)
        .await?;
        changed.insert("allow_signup".into(), Value::Bool(on));
    }
    if !changed.is_empty() {
        audit(
            &mut *tx,
            s.admin_id,
            "settings.update",
            "settings",
            Some(Value::Object(changed)),
        )
        .await?;
    }
    tx.commit().await?;
    // This process follows immediately; other processes within the 5 s TTL.
    state.access_mode.clear();
    Ok(Json(settings_view(&state).await?))
}

// ---------------------------------------------------------------------------------------------
// Access keys

#[derive(Debug, Serialize, sqlx::FromRow)]
struct KeyView {
    id: Uuid,
    label: String,
    email: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    created_by: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    revoked_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    last_used_at: Option<OffsetDateTime>,
    uses: i32,
    max_uses: Option<i32>,
    status: String,
}

const KEY_VIEW_SQL: &str = "
SELECT * FROM (
    SELECT k.id, k.label, k.email, k.created_at, a.email AS created_by, k.expires_at,
           k.revoked_at, k.last_used_at, k.uses, k.max_uses,
           CASE WHEN k.revoked_at IS NOT NULL THEN 'revoked'
                WHEN k.expires_at IS NOT NULL AND k.expires_at <= now() THEN 'expired'
                WHEN k.max_uses IS NOT NULL AND k.uses >= k.max_uses THEN 'exhausted'
                ELSE 'active' END AS status
    FROM access_keys k LEFT JOIN admins a ON a.id = k.created_by_admin
) v
WHERE ($1::uuid IS NULL OR v.id = $1) AND ($2::text IS NULL OR v.status = $2)";

async fn key_by_id(db: &sqlx::PgPool, id: Uuid) -> ApiResult<KeyView> {
    sqlx::query_as::<_, KeyView>(KEY_VIEW_SQL)
        .bind(Some(id))
        .bind(None::<String>)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)
}

async fn list_keys(
    State(state): State<SharedState>,
    _s: AdminSession,
    QueryParams(q): QueryParams<ListQuery>,
) -> ApiResult<Json<ListResponse<KeyView>>> {
    let (limit, offset) = q.page();
    let status = empty_to_none(q.status.as_deref());
    if let Some(st) = status {
        if !matches!(st, "active" | "revoked" | "expired" | "exhausted") {
            return Err(ApiError::bad_request(
                "status must be active, revoked, expired or exhausted",
            ));
        }
    }
    let items = sqlx::query_as::<_, KeyView>(&format!(
        "{KEY_VIEW_SQL} ORDER BY v.created_at DESC, v.id DESC LIMIT $3 OFFSET $4"
    ))
    .bind(None::<Uuid>)
    .bind(status)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM ({KEY_VIEW_SQL}) c"))
        .bind(None::<Uuid>)
        .bind(status)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(ListResponse {
        items,
        total,
        limit,
        offset,
    }))
}

#[derive(Deserialize)]
struct CreateKeyRequest {
    label: String,
    email: Option<String>,
    expires_in_days: Option<i32>,
    max_uses: Option<i32>,
}

#[derive(Serialize)]
struct CreatedKey {
    #[serde(flatten)]
    view: KeyView,
    key: String,
    link_code: Option<String>,
}

async fn create_key(
    State(state): State<SharedState>,
    s: AdminSession,
    JsonBody(req): JsonBody<CreateKeyRequest, BODY_LIMIT>,
) -> ApiResult<Response> {
    let email = empty_to_none(req.email.as_deref())
        .map(auth::normalize_email)
        .transpose()?;
    let mut tx = state.db.begin().await?;
    let (id, key) = access::mint_access_key(
        &mut tx,
        &NewAccessKey {
            label: &req.label,
            email: email.as_deref(),
            expires_in_days: req.expires_in_days,
            max_uses: req.max_uses,
            created_by_admin: Some(s.admin_id),
            created_by_user: None,
        },
    )
    .await?;
    audit(
        &mut *tx,
        s.admin_id,
        "access_key.create",
        &id.to_string(),
        Some(json!({ "label": req.label.trim(), "email": email })),
    )
    .await?;
    tx.commit().await?;
    let view = key_by_id(&state.db, id).await?;
    let link_code = copper_cloud_core::tls::link_code(&state.cfg)
        .map(|c| c.with_key(&key).to_string())
        .map_err(|err| tracing::warn!(error = format!("{err:#}"), "no link code for access key"))
        .ok();
    Ok((
        axum::http::StatusCode::CREATED,
        Json(CreatedKey {
            view,
            key,
            link_code,
        }),
    )
        .into_response())
}

async fn revoke_key(
    State(state): State<SharedState>,
    s: AdminSession,
    Path(raw): Path<String>,
) -> ApiResult<Json<KeyView>> {
    let id = parse_id(&raw)?;
    let mut tx = state.db.begin().await?;
    let label: Option<String> = sqlx::query_scalar(
        "UPDATE access_keys SET revoked_at = COALESCE(revoked_at, now()) WHERE id = $1
         RETURNING label",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(label) = label else {
        return Err(ApiError::NotFound);
    };
    audit(
        &mut *tx,
        s.admin_id,
        "access_key.revoke",
        &id.to_string(),
        Some(json!({ "label": label })),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(key_by_id(&state.db, id).await?))
}

// ---------------------------------------------------------------------------------------------
// Users

#[derive(Debug, Serialize, sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    email: String,
    display_name: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    last_seen_at: Option<OffsetDateTime>,
    disabled: bool,
    device_count: i64,
    canvas_count: i64,
}

const USER_VIEW_SQL: &str = "
SELECT u.id, u.email, u.display_name, u.created_at, u.disabled,
       (SELECT max(d.last_seen_at) FROM devices d WHERE d.user_id = u.id) AS last_seen_at,
       (SELECT count(*) FROM devices d WHERE d.user_id = u.id) AS device_count,
       (SELECT count(*) FROM canvas_members m WHERE m.user_id = u.id) AS canvas_count
FROM users u
WHERE ($1::uuid IS NULL OR u.id = $1)
  AND ($2::text IS NULL OR u.email ILIKE $2 OR u.display_name ILIKE $2)";

async fn user_by_id(db: &sqlx::PgPool, id: Uuid) -> ApiResult<UserRow> {
    sqlx::query_as::<_, UserRow>(USER_VIEW_SQL)
        .bind(Some(id))
        .bind(None::<String>)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)
}

async fn list_users(
    State(state): State<SharedState>,
    _s: AdminSession,
    QueryParams(q): QueryParams<ListQuery>,
) -> ApiResult<Json<ListResponse<UserRow>>> {
    let (limit, offset) = q.page();
    let pattern = empty_to_none(q.q.as_deref()).map(like_pattern);
    let items = sqlx::query_as::<_, UserRow>(&format!(
        "{USER_VIEW_SQL} ORDER BY u.created_at DESC, u.id DESC LIMIT $3 OFFSET $4"
    ))
    .bind(None::<Uuid>)
    .bind(&pattern)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM users u
         WHERE ($1::text IS NULL OR u.email ILIKE $1 OR u.display_name ILIKE $1)",
    )
    .bind(&pattern)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(ListResponse {
        items,
        total,
        limit,
        offset,
    }))
}

#[derive(Deserialize)]
struct UserPatch {
    disabled: Option<bool>,
    display_name: Option<String>,
}

async fn update_user(
    State(state): State<SharedState>,
    s: AdminSession,
    Path(raw): Path<String>,
    JsonBody(req): JsonBody<UserPatch, BODY_LIMIT>,
) -> ApiResult<Json<UserRow>> {
    let id = parse_id(&raw)?;
    let display_name = req
        .display_name
        .as_deref()
        .map(|n| {
            auth::clean_name(Some(n), 200)
                .ok_or_else(|| ApiError::bad_request("display_name must not be empty"))
        })
        .transpose()?;
    let mut tx = state.db.begin().await?;
    let email: Option<String> = sqlx::query_scalar(
        "UPDATE users SET disabled = COALESCE($2, disabled),
                          display_name = COALESCE($3, display_name),
                          updated_at = now()
         WHERE id = $1 RETURNING email",
    )
    .bind(id)
    .bind(req.disabled)
    .bind(&display_name)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(email) = email else {
        return Err(ApiError::NotFound);
    };
    let mut detail = Map::new();
    detail.insert("email".into(), Value::from(email));
    if let Some(d) = req.disabled {
        detail.insert("disabled".into(), Value::Bool(d));
        if d {
            let revoked = sqlx::query("DELETE FROM sessions WHERE user_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            detail.insert("revoked_sessions".into(), Value::from(revoked));
        }
    }
    if let Some(n) = &display_name {
        detail.insert("display_name".into(), Value::from(n.clone()));
    }
    audit(
        &mut *tx,
        s.admin_id,
        "user.update",
        &id.to_string(),
        Some(Value::Object(detail)),
    )
    .await?;
    tx.commit().await?;
    if req.disabled == Some(true) {
        copper_cloud_canvas::admin::disconnect_user(id);
    }
    Ok(Json(user_by_id(&state.db, id).await?))
}

async fn delete_user(
    State(state): State<SharedState>,
    s: AdminSession,
    Path(raw): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw)?;
    let email = auth::user_email(&state.db, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owned = copper_cloud_canvas::admin::owned_canvases(&state, id).await?;
    let mut tx = state.db.begin().await?;
    // Credentials and invites addressed to this person; everything keyed by user_id
    // (sessions, devices, sync docs, history, pairing codes, owned canvases with their
    // members/invites/updates, memberships) goes with the user row (FK cascade).
    sqlx::query("DELETE FROM access_keys WHERE lower(email) = lower($1)")
        .bind(&email)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM canvas_invites WHERE lower(email) = lower($1)")
        .bind(&email)
        .execute(&mut *tx)
        .await?;
    let deleted = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    audit(
        &mut *tx,
        s.admin_id,
        "user.delete",
        &id.to_string(),
        Some(json!({ "email": email })),
    )
    .await?;
    tx.commit().await?;
    copper_cloud_canvas::admin::canvases_deleted(&state, &owned);
    copper_cloud_canvas::admin::disconnect_user(id);
    Ok(ok())
}

#[derive(Deserialize)]
struct ResetPassword {
    password: String,
}

async fn reset_user_password(
    State(state): State<SharedState>,
    s: AdminSession,
    Path(raw): Path<String>,
    JsonBody(req): JsonBody<ResetPassword, BODY_LIMIT>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw)?;
    auth::validate_password(&req.password)?;
    let email = auth::user_email(&state.db, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let hash = auth::hash_password(req.password).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE users SET password_hash = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    let revoked = sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    audit(
        &mut *tx,
        s.admin_id,
        "user.reset_password",
        &id.to_string(),
        Some(json!({ "email": email })),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true, "revoked_sessions": revoked })))
}

// ---------------------------------------------------------------------------------------------
// Devices

#[derive(Debug, Serialize, sqlx::FromRow)]
struct DeviceRow {
    id: Uuid,
    user_id: Uuid,
    user_email: String,
    name: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    last_seen_at: OffsetDateTime,
}

async fn list_devices(
    State(state): State<SharedState>,
    _s: AdminSession,
    QueryParams(q): QueryParams<ListQuery>,
) -> ApiResult<Json<ListResponse<DeviceRow>>> {
    let (limit, offset) = q.page();
    let items = sqlx::query_as::<_, DeviceRow>(
        "SELECT d.id, d.user_id, u.email AS user_email, d.name, d.created_at, d.last_seen_at
         FROM devices d JOIN users u ON u.id = d.user_id
         WHERE ($1::uuid IS NULL OR d.user_id = $1)
         ORDER BY d.created_at DESC, d.id DESC LIMIT $2 OFFSET $3",
    )
    .bind(q.user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM devices WHERE ($1::uuid IS NULL OR user_id = $1)")
            .bind(q.user_id)
            .fetch_one(&state.db)
            .await?;
    Ok(Json(ListResponse {
        items,
        total,
        limit,
        offset,
    }))
}

#[derive(Debug, Default, Deserialize)]
struct DeviceDeleteQuery {
    user_id: Option<Uuid>,
}

async fn delete_device(
    State(state): State<SharedState>,
    s: AdminSession,
    Path(raw): Path<String>,
    QueryParams(q): QueryParams<DeviceDeleteQuery>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw)?;
    let mut tx = state.db.begin().await?;
    // Sessions of the device go with it (FK cascade).
    let users: Vec<Uuid> = sqlx::query_scalar(
        "DELETE FROM devices WHERE id = $1 AND ($2::uuid IS NULL OR user_id = $2)
         RETURNING user_id",
    )
    .bind(id)
    .bind(q.user_id)
    .fetch_all(&mut *tx)
    .await?;
    if users.is_empty() {
        return Err(ApiError::NotFound);
    }
    sqlx::query("DELETE FROM sync_docs WHERE domain = $1 AND user_id = ANY($2)")
        .bind(copper_cloud_core::sync::tabs_domain(id))
        .bind(&users)
        .execute(&mut *tx)
        .await?;
    let detail = if let [user] = users.as_slice() {
        json!({ "user_id": user })
    } else {
        json!({ "user_ids": users })
    };
    audit(
        &mut *tx,
        s.admin_id,
        "device.delete",
        &id.to_string(),
        Some(detail),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true, "deleted": users.len() })))
}

// ---------------------------------------------------------------------------------------------
// Canvases

#[derive(Debug, Serialize, sqlx::FromRow)]
struct CanvasRow {
    id: Uuid,
    name: String,
    kind: String,
    owner_id: Uuid,
    owner_email: String,
    member_count: i64,
    share_link_count: i64,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

async fn list_canvases(
    State(state): State<SharedState>,
    _s: AdminSession,
    QueryParams(q): QueryParams<ListQuery>,
) -> ApiResult<Json<ListResponse<CanvasRow>>> {
    let (limit, offset) = q.page();
    let kind = empty_to_none(q.kind.as_deref());
    if kind.is_some_and(|k| !matches!(k, "personal" | "shared")) {
        return Err(ApiError::bad_request("kind must be personal or shared"));
    }
    let items = sqlx::query_as::<_, CanvasRow>(
        "SELECT c.id, c.name, c.kind, c.owner_id, u.email AS owner_email,
                (SELECT count(*) FROM canvas_members m WHERE m.canvas_id = c.id) AS member_count,
                (SELECT count(*) FROM canvas_share_links l WHERE l.canvas_id = c.id) AS share_link_count,
                c.created_at, c.updated_at
         FROM canvases c JOIN users u ON u.id = c.owner_id
         WHERE ($1::text IS NULL OR c.kind = $1)
         ORDER BY c.created_at DESC, c.id DESC LIMIT $2 OFFSET $3",
    )
    .bind(kind)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM canvases WHERE ($1::text IS NULL OR kind = $1)")
            .bind(kind)
            .fetch_one(&state.db)
            .await?;
    Ok(Json(ListResponse {
        items,
        total,
        limit,
        offset,
    }))
}

async fn delete_canvas(
    State(state): State<SharedState>,
    s: AdminSession,
    Path(raw): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw)?;
    let name = copper_cloud_canvas::admin::delete_canvas(&state, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    audit(
        &state.db,
        s.admin_id,
        "canvas.delete",
        &id.to_string(),
        Some(json!({ "name": name })),
    )
    .await?;
    Ok(ok())
}

// ---------------------------------------------------------------------------------------------
// Pairing codes

#[derive(Debug, Serialize, sqlx::FromRow)]
struct PairingRow {
    id: Uuid,
    user_id: Uuid,
    user_email: String,
    device_name: Option<String>,
    created_by_device: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

async fn list_pairing_codes(
    State(state): State<SharedState>,
    _s: AdminSession,
    QueryParams(q): QueryParams<ListQuery>,
) -> ApiResult<Json<ListResponse<PairingRow>>> {
    let (limit, offset) = q.page();
    let items = sqlx::query_as::<_, PairingRow>(
        "SELECT p.id, p.user_id, u.email AS user_email, p.device_name, p.created_by_device,
                p.created_at, p.expires_at
         FROM pairing_codes p JOIN users u ON u.id = p.user_id
         WHERE p.used_at IS NULL AND p.expires_at > now()
           AND ($1::uuid IS NULL OR p.user_id = $1)
         ORDER BY p.created_at DESC, p.id DESC LIMIT $2 OFFSET $3",
    )
    .bind(q.user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pairing_codes
         WHERE used_at IS NULL AND expires_at > now() AND ($1::uuid IS NULL OR user_id = $1)",
    )
    .bind(q.user_id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(ListResponse {
        items,
        total,
        limit,
        offset,
    }))
}

async fn revoke_pairing_code(
    State(state): State<SharedState>,
    s: AdminSession,
    Path(raw): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw)?;
    let mut tx = state.db.begin().await?;
    let user: Option<Uuid> = sqlx::query_scalar(
        "DELETE FROM pairing_codes WHERE id = $1 AND used_at IS NULL RETURNING user_id",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(user) = user else {
        return Err(ApiError::NotFound);
    };
    audit(
        &mut *tx,
        s.admin_id,
        "pairing_code.revoke",
        &id.to_string(),
        Some(json!({ "user_id": user })),
    )
    .await?;
    tx.commit().await?;
    Ok(ok())
}

// ---------------------------------------------------------------------------------------------
// Audit

#[derive(Debug, Serialize, sqlx::FromRow)]
struct AuditRow {
    id: i64,
    admin_id: Option<Uuid>,
    admin_email: Option<String>,
    action: String,
    target: String,
    detail: Option<Value>,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
}

async fn list_audit(
    State(state): State<SharedState>,
    _s: AdminSession,
    QueryParams(q): QueryParams<ListQuery>,
) -> ApiResult<Json<ListResponse<AuditRow>>> {
    let (limit, offset) = q.page();
    let items = sqlx::query_as::<_, AuditRow>(
        "SELECT l.id, l.admin_id, a.email AS admin_email, l.action, l.target, l.detail, l.at
         FROM admin_audit l LEFT JOIN admins a ON a.id = l.admin_id
         ORDER BY l.at DESC, l.id DESC LIMIT $1 OFFSET $2",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM admin_audit")
        .fetch_one(&state.db)
        .await?;
    Ok(Json(ListResponse {
        items,
        total,
        limit,
        offset,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_parsing() {
        let mut h = HeaderMap::new();
        h.append(
            header::COOKIE,
            HeaderValue::from_static("a=1; cc_admin=tok; b=2"),
        );
        assert_eq!(cookie(&h, COOKIE), Some("tok"));
        assert_eq!(cookie(&h, "b"), Some("2"));
        assert_eq!(cookie(&h, "cc"), None);
        let mut h = HeaderMap::new();
        h.append(header::COOKIE, HeaderValue::from_static("x=1"));
        h.append(header::COOKIE, HeaderValue::from_static("cc_admin=second"));
        assert_eq!(cookie(&h, COOKIE), Some("second"));
    }

    #[test]
    fn like_escapes_wildcards() {
        assert_eq!(like_pattern("a%b_c\\"), "%a\\%b\\_c\\\\%");
    }

    #[test]
    fn page_clamps() {
        let q = ListQuery {
            limit: Some(10_000),
            offset: Some(-5),
            ..ListQuery::default()
        };
        assert_eq!(q.page(), (MAX_LIMIT, 0));
        assert_eq!(ListQuery::default().page(), (DEFAULT_LIMIT, 0));
        let q = ListQuery {
            limit: Some(0),
            ..ListQuery::default()
        };
        assert_eq!(q.page().0, 1);
    }
}
