//! Agent integrations: a service the admin trusts (e.g. a bot platform) signs a headless Copper in
//! to an allowed-domain user's account without that user's password.
//!
//! * The admin mints an integration key `cai_` + 32 random bytes base64url; only SHA-256(key)
//!   is stored and the key is shown once (rotate = new key, old one stops working).
//! * `POST /v1/agents/pair` — no gate header, `Authorization: Bearer cai_…`,
//!   `{email, device:{id,name}, display_name?}` → a single-use 10-minute pairing code
//!   **bound to that device** plus its link. The headless Copper redeems it with
//!   `POST /v1/auth/pair` like any pairing code.
//! * The email's domain must be in the integration's `domains` (empty = the enabled SSO
//!   providers' domains). A missing account is created (password-less) when the integration
//!   has `create_users`. The device is flagged as the integration's agent device; re-pairing
//!   it deletes its earlier sessions and revokes the gate keys minted for it.
//! * Every pairing writes an `admin_audit` row (`agent.pair`).

use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::{self, DeviceInput};
use crate::error::{ApiError, ApiResult};
use crate::extract::JsonBody;
use crate::ids;
use crate::intelligence::Actor;
use crate::pairing::{new_pairing_code, PAIRING_TTL_MINUTES};
use crate::sso;
use crate::state::SharedState;

/// Prefix of every agent-integration key.
pub const AGENT_KEY_PREFIX: &str = "cai_";
/// The (ungated) agent pairing route.
pub const AGENT_PAIR_PATH: &str = "/v1/agents/pair";
pub const MAX_NAME_CHARS: usize = 100;
const BODY_LIMIT: usize = 16 * 1024;

/// A fresh integration key: `cai_` + 43 base64url chars (32 random bytes).
pub fn new_agent_key() -> String {
    format!("{AGENT_KEY_PREFIX}{}", ids::random_token())
}

/// Shape check before touching the database.
pub fn plausible_agent_key(s: &str) -> bool {
    s.strip_prefix(AGENT_KEY_PREFIX)
        .is_some_and(ids::is_token_shape)
}

// ---------------------------------------------------------------------------------------------
// Admin side (shared by the admin API and the CLI)

/// `GET /admin/api/integrations` item.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct IntegrationView {
    pub id: Uuid,
    pub name: String,
    pub domains: Vec<String>,
    pub create_users: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
    pub device_count: i64,
}

const VIEW_SQL: &str = "SELECT i.id, i.name, i.domains, i.create_users, i.created_at,
       i.last_used_at, i.revoked_at,
       (SELECT count(*) FROM devices d WHERE d.agent_integration_id = i.id) AS device_count
FROM agent_integrations i";

/// Every integration, newest first.
pub async fn list_integrations(db: &sqlx::PgPool) -> ApiResult<Vec<IntegrationView>> {
    Ok(sqlx::query_as::<_, IntegrationView>(&format!(
        "{VIEW_SQL} ORDER BY i.created_at DESC, i.id DESC"
    ))
    .fetch_all(db)
    .await?)
}

pub async fn find_integration(db: &sqlx::PgPool, id: Uuid) -> ApiResult<Option<IntegrationView>> {
    Ok(
        sqlx::query_as::<_, IntegrationView>(&format!("{VIEW_SQL} WHERE i.id = $1"))
            .bind(id)
            .fetch_optional(db)
            .await?,
    )
}

/// Mint a new integration; returns it and its key (shown once).
pub async fn create_integration(
    db: &sqlx::PgPool,
    actor: &Actor,
    name: &str,
    domains: &[String],
    create_users: bool,
) -> ApiResult<(IntegrationView, String)> {
    let name = auth::clean_name(Some(name), MAX_NAME_CHARS + 1)
        .ok_or_else(|| ApiError::bad_request("name must not be empty"))?;
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(ApiError::bad_request(format!(
            "name must be at most {MAX_NAME_CHARS} characters"
        )));
    }
    let domains = sso::clean_domains(domains, false)?;
    let key = new_agent_key();
    let id = ids::uuid_v7();
    let mut tx = db.begin().await?;
    sqlx::query(
        "INSERT INTO agent_integrations (id, name, key_sha256, domains, create_users, created_by_admin)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(&name)
    .bind(&ids::sha256(key.as_bytes())[..])
    .bind(&domains)
    .bind(create_users)
    .bind(actor.admin_id())
    .execute(&mut *tx)
    .await?;
    sso::audit(
        &mut *tx,
        actor.admin_id(),
        "integration.create",
        &id.to_string(),
        Some(json!({
            "name": name,
            "domains": domains,
            "create_users": create_users,
            "via": actor.via(),
        })),
    )
    .await?;
    tx.commit().await?;
    let view = find_integration(db, id).await?.ok_or(ApiError::NotFound)?;
    Ok((view, key))
}

/// Replace an active integration's key (the old key stops working at once). 404 if missing,
/// 400 if revoked.
pub async fn rotate_integration(db: &sqlx::PgPool, actor: &Actor, id: Uuid) -> ApiResult<String> {
    let key = new_agent_key();
    let mut tx = db.begin().await?;
    let row: Option<(String, Option<OffsetDateTime>)> =
        sqlx::query_as("SELECT name, revoked_at FROM agent_integrations WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((name, revoked)) = row else {
        return Err(ApiError::NotFound);
    };
    if revoked.is_some() {
        return Err(ApiError::bad_request(
            "this integration is revoked; create a new one",
        ));
    }
    sqlx::query("UPDATE agent_integrations SET key_sha256 = $2, rotated_at = now() WHERE id = $1")
        .bind(id)
        .bind(&ids::sha256(key.as_bytes())[..])
        .execute(&mut *tx)
        .await?;
    sso::audit(
        &mut *tx,
        actor.admin_id(),
        "integration.rotate",
        &id.to_string(),
        Some(json!({ "name": name, "via": actor.via() })),
    )
    .await?;
    tx.commit().await?;
    Ok(key)
}

/// Revoke an integration: its key stops working, its agent devices are signed out (sessions
/// deleted, gate keys revoked). Idempotent; 404 if missing.
pub async fn revoke_integration(
    db: &sqlx::PgPool,
    actor: &Actor,
    id: Uuid,
) -> ApiResult<IntegrationView> {
    let mut tx = db.begin().await?;
    let name: Option<String> = sqlx::query_scalar(
        "UPDATE agent_integrations SET revoked_at = COALESCE(revoked_at, now()) WHERE id = $1
         RETURNING name",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(name) = name else {
        return Err(ApiError::NotFound);
    };
    let sessions = sqlx::query(
        "DELETE FROM sessions s USING devices d
         WHERE d.agent_integration_id = $1 AND s.user_id = d.user_id AND s.device_id = d.id",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let keys = sqlx::query(
        "UPDATE access_keys k SET revoked_at = now() FROM devices d
         WHERE d.agent_integration_id = $1 AND k.created_by_user = d.user_id
           AND k.device_id = d.id AND k.revoked_at IS NULL",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sso::audit(
        &mut *tx,
        actor.admin_id(),
        "integration.revoke",
        &id.to_string(),
        Some(json!({
            "name": name,
            "revoked_sessions": sessions,
            "revoked_gate_keys": keys,
            "via": actor.via(),
        })),
    )
    .await?;
    tx.commit().await?;
    find_integration(db, id).await?.ok_or(ApiError::NotFound)
}

/// `GET /admin/api/integrations/{id}/devices` item.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct AgentDeviceView {
    pub user_email: String,
    pub device_id: Uuid,
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen_at: OffsetDateTime,
}

/// The agent devices of an integration, most recently seen first. `None` if it does not exist.
pub async fn integration_devices(
    db: &sqlx::PgPool,
    id: Uuid,
) -> ApiResult<Option<Vec<AgentDeviceView>>> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM agent_integrations WHERE id = $1)")
            .bind(id)
            .fetch_one(db)
            .await?;
    if !exists {
        return Ok(None);
    }
    Ok(Some(
        sqlx::query_as::<_, AgentDeviceView>(
            "SELECT u.email AS user_email, d.id AS device_id, d.name, d.last_seen_at
             FROM devices d JOIN users u ON u.id = d.user_id
             WHERE d.agent_integration_id = $1
             ORDER BY d.last_seen_at DESC, d.id",
        )
        .bind(id)
        .fetch_all(db)
        .await?,
    ))
}

// ---------------------------------------------------------------------------------------------
// `POST /v1/agents/pair`

pub(crate) fn routes() -> Router<SharedState> {
    Router::new().route("/agents/pair", post(agent_pair))
}

#[derive(Deserialize)]
struct AgentPairRequest {
    email: String,
    device: DeviceInput,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Serialize)]
struct PairedUser {
    id: Uuid,
    email: String,
    display_name: String,
}

#[derive(Serialize)]
struct PairedDevice {
    id: Uuid,
    name: String,
}

#[derive(Serialize)]
struct AgentPairResponse {
    code: String,
    link: String,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    user: PairedUser,
    device: PairedDevice,
    created_user: bool,
}

fn bad_key() -> ApiError {
    ApiError::Unauthorized("agent_key")
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let (scheme, token) = headers.get(AUTHORIZATION)?.to_str().ok()?.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// The allowed domains for an integration: its own, else every enabled SSO provider's.
async fn allowed_domains(db: &sqlx::PgPool, own: Vec<String>) -> ApiResult<Vec<String>> {
    if !own.is_empty() {
        return Ok(own);
    }
    let mut out: Vec<String> = Vec::new();
    for p in sso::list_providers(db)
        .await?
        .into_iter()
        .filter(|p| p.enabled)
    {
        for d in p.domains {
            if !out.contains(&d) {
                out.push(d);
            }
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_lines)]
async fn agent_pair(
    State(state): State<SharedState>,
    headers: HeaderMap,
    JsonBody(req): JsonBody<AgentPairRequest, BODY_LIMIT>,
) -> ApiResult<Json<AgentPairResponse>> {
    let key = bearer(&headers)
        .filter(|k| plausible_agent_key(k))
        .ok_or_else(bad_key)?;
    let integration: Option<(Uuid, String, Vec<String>, bool)> = sqlx::query_as(
        "UPDATE agent_integrations SET last_used_at = now()
         WHERE key_sha256 = $1 AND revoked_at IS NULL
         RETURNING id, name, domains, create_users",
    )
    .bind(&ids::sha256(key.as_bytes())[..])
    .fetch_optional(&state.db)
    .await?;
    let Some((integration_id, integration_name, own_domains, create_users)) = integration else {
        tracing::info!("agent pairing refused: unknown or revoked key");
        return Err(bad_key());
    };
    let email = auth::normalize_email(&req.email)?;
    let device_id = req
        .device
        .id
        .ok_or_else(|| ApiError::bad_request("device.id (a stable UUID) is required"))?;
    let device_name =
        auth::clean_name(req.device.name.as_deref(), 200).unwrap_or_else(|| "Agent".to_owned());
    let domains = allowed_domains(&state.db, own_domains).await?;
    if !sso::email_domain(&email).is_some_and(|d| domains.contains(&d)) {
        return Err(sso::domain_error("agent_domain", &domains));
    }

    let mut tx = state.db.begin().await?;
    let existing: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE lower(email) = lower($1)")
            .bind(&email)
            .fetch_optional(&mut *tx)
            .await?;
    let (user_id, created_user) = match existing {
        Some(id) => (id, false),
        None if create_users => {
            let user =
                sso::create_sso_user(&mut tx, &state.crypto, &email, req.display_name.as_deref())
                    .await?;
            (user.id, true)
        }
        None => {
            return Err(ApiError::coded(
                StatusCode::NOT_FOUND,
                "no_account",
                format!("there is no Copper Cloud account for {email}"),
            ))
        }
    };
    let (user, disabled) = sso::user_with_status(&mut tx, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if disabled {
        return Err(ApiError::Denied {
            code: "account_disabled",
            message: "this account is disabled",
        });
    }

    // Re-pairing: sign out every earlier instance of this agent device — under this user, and
    // under whichever users it was this integration's agent device for before — and drop its
    // unused codes.
    let owners = "SELECT $2::uuid UNION
                  SELECT user_id FROM devices WHERE id = $1 AND agent_integration_id = $3";
    let revoked_sessions = sqlx::query(&format!(
        "DELETE FROM sessions WHERE device_id = $1 AND user_id IN ({owners})"
    ))
    .bind(device_id)
    .bind(user_id)
    .bind(integration_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let revoked_keys = sqlx::query(&format!(
        "UPDATE access_keys SET revoked_at = now()
         WHERE device_id = $1 AND revoked_at IS NULL AND created_by_user IN ({owners})"
    ))
    .bind(device_id)
    .bind(user_id)
    .bind(integration_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query(&format!(
        "DELETE FROM pairing_codes WHERE device_id = $1 AND used_at IS NULL AND user_id IN ({owners})"
    ))
    .bind(device_id)
    .bind(user_id)
    .bind(integration_id)
    .execute(&mut *tx)
    .await?;

    let device = auth::upsert_device(
        &mut tx,
        user_id,
        Some(DeviceInput {
            id: Some(device_id),
            name: Some(device_name),
        }),
    )
    .await?;
    sqlx::query("UPDATE devices SET agent_integration_id = $3 WHERE user_id = $1 AND id = $2")
        .bind(user_id)
        .bind(device_id)
        .bind(integration_id)
        .execute(&mut *tx)
        .await?;

    let code = new_pairing_code();
    let pairing_id = ids::uuid_v7();
    let expires_at: OffsetDateTime = sqlx::query_scalar(
        "INSERT INTO pairing_codes (id, user_id, device_name, code_sha256, expires_at, device_id)
         VALUES ($1, $2, $3, $4, now() + make_interval(mins => $5), $6)
         RETURNING expires_at",
    )
    .bind(pairing_id)
    .bind(user_id)
    .bind(&device.name)
    .bind(&ids::sha256(code.as_bytes())[..])
    .bind(PAIRING_TTL_MINUTES)
    .bind(device_id)
    .fetch_one(&mut *tx)
    .await?;
    sso::audit(
        &mut *tx,
        None,
        "agent.pair",
        &device_id.to_string(),
        Some(json!({
            "integration_id": integration_id,
            "integration": integration_name,
            "user_id": user_id,
            "email": user.email,
            "device_name": device.name,
            "created_user": created_user,
            "revoked_sessions": revoked_sessions,
            "revoked_gate_keys": revoked_keys,
        })),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(
        %integration_id,
        user_id = %user_id,
        device_id = %device_id,
        %pairing_id,
        created_user,
        revoked_sessions,
        "agent pairing code issued"
    );
    let link = sso::onboarding_link(&state.cfg).pairing_link(&code);
    Ok(Json(AgentPairResponse {
        code,
        link,
        expires_at,
        user: PairedUser {
            id: user.id,
            email: user.email,
            display_name: user.display_name,
        },
        device: PairedDevice {
            id: device.id,
            name: device.name,
        },
        created_user,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_key_format() {
        let k = new_agent_key();
        assert!(k.starts_with("cai_") && k.len() == 4 + 43, "{k}");
        assert!(plausible_agent_key(&k));
        assert_ne!(k, new_agent_key());
        for bad in ["", "cai_", "cai_short", &k[4..], &format!("ck_{}", &k[4..])] {
            assert!(!plausible_agent_key(bad), "{bad}");
        }
    }
}
