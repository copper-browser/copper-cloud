//! Cloud-wide intelligence keys: one Jev (`TypeSafe`) key and one LLM router (`LiteLLM`) key
//! set by the instance admin and handed to every signed-in Copper by `GET /v1/intelligence`.
//!
//! Storage (`intelligence_settings`, singleton row): a random 32-byte data key wrapped by the
//! KEK, each API key AES-256-GCM sealed under it with AAD `intelligence:jev` /
//! `intelligence:router`. Endpoints/model/URL are plaintext. Key material never reaches logs
//! or the admin API (which only sees the last four characters).
//!
//! Writers: the admin API (`/admin/api/intelligence`) and `copper-cloud intelligence …`. Both
//! go through [`apply`] and [`clear`] and write an `admin_audit` row. User reads are audited
//! as `intelligence.read` at most once per user per hour.

use axum::extract::State;
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::auth::AuthUser;
use crate::crypto::Crypto;
use crate::error::{ApiError, ApiResult};
use crate::state::SharedState;

pub const DEFAULT_JEV_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
pub const DEFAULT_JEV_MODEL: &str = "jev-latest";
pub const DEFAULT_ROUTER_URL: &str = "https://llm.example.com";

const AAD_JEV: &[u8] = b"intelligence:jev";
const AAD_ROUTER: &[u8] = b"intelligence:router";
const MAX_KEY_LEN: usize = 4096;
const MAX_URL_LEN: usize = 2048;
const MAX_MODEL_LEN: usize = 200;

/// Who changed the settings (audit + `updated_by`).
#[derive(Clone, Debug)]
pub enum Actor {
    Admin { id: Uuid, email: String },
    Cli,
}

impl Actor {
    fn label(&self) -> &str {
        match self {
            Self::Admin { email, .. } => email,
            Self::Cli => "cli",
        }
    }

    fn admin_id(&self) -> Option<Uuid> {
        match self {
            Self::Admin { id, .. } => Some(*id),
            Self::Cli => None,
        }
    }

    fn via(&self) -> &'static str {
        match self {
            Self::Admin { .. } => "admin_api",
            Self::Cli => "cli",
        }
    }
}

/// Decrypted Jev settings.
#[derive(Clone, Serialize)]
pub struct Jev {
    pub key: String,
    pub endpoint: String,
    pub model: String,
}

/// Decrypted router settings.
#[derive(Clone, Serialize)]
pub struct RouterKey {
    pub key: String,
    pub url: String,
}

impl std::fmt::Debug for Jev {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jev")
            .field("key", &"<redacted>")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .finish()
    }
}

impl std::fmt::Debug for RouterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouterKey")
            .field("key", &"<redacted>")
            .field("url", &self.url)
            .finish()
    }
}

impl Drop for Jev {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.key);
    }
}

impl Drop for RouterKey {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.key);
    }
}

/// The stored settings, decrypted.
#[derive(Debug, Default)]
pub struct Settings {
    pub jev: Option<Jev>,
    pub router: Option<RouterKey>,
    pub enabled: bool,
    pub updated_at: Option<OffsetDateTime>,
    pub updated_by: Option<String>,
}

impl Settings {
    /// Nothing stored at all (no row, or a row with both blocks cleared).
    pub fn is_empty(&self) -> bool {
        self.jev.is_none() && self.router.is_none()
    }

    /// The `GET /v1/intelligence` body: keys in clear, nulls when unset or sharing is off.
    pub fn user_view(&self) -> Value {
        if !self.enabled || self.is_empty() {
            return json!({ "jev": null, "router": null, "updated_at": null });
        }
        json!({
            "jev": self.jev,
            "router": self.router,
            "updated_at": self.updated_at.map(rfc3339),
        })
    }

    /// The admin view: never the keys, only their last four characters.
    pub fn masked_view(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "jev": self.jev.as_ref().map(|j| json!({
                "key_last4": last4(&j.key),
                "endpoint": j.endpoint,
                "model": j.model,
            })),
            "router": self.router.as_ref().map(|r| json!({
                "key_last4": last4(&r.key),
                "url": r.url,
            })),
            "updated_at": self.updated_at.map(rfc3339),
            "updated_by": self.updated_by,
            "defaults": {
                "jev_endpoint": DEFAULT_JEV_ENDPOINT,
                "jev_model": DEFAULT_JEV_MODEL,
                "router_url": DEFAULT_ROUTER_URL,
            },
        })
    }
}

/// Last four characters of a key, or `""` for keys too short to reveal any of.
pub fn last4(key: &str) -> String {
    let n = key.chars().count();
    if n < 12 {
        return String::new();
    }
    key.chars().skip(n - 4).collect()
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

type Row = (
    Vec<u8>,
    Option<Vec<u8>>,
    Option<String>,
    Option<String>,
    Option<Vec<u8>>,
    Option<String>,
    bool,
    OffsetDateTime,
    Option<String>,
);

const SELECT_SQL: &str = "SELECT data_key_wrapped, jev_key_sealed, jev_endpoint, jev_model,
       router_key_sealed, router_url, enabled, updated_at, updated_by
FROM intelligence_settings WHERE id = 1";

fn open_key(data_key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> ApiResult<String> {
    let plain = Zeroizing::new(Crypto::open(data_key, aad, sealed)?);
    String::from_utf8(plain.to_vec()).map_err(|_| ApiError::internal("stored key is not UTF-8"))
}

fn decode(crypto: &Crypto, row: Option<Row>) -> ApiResult<Settings> {
    let Some((
        wrapped,
        jev_sealed,
        jev_endpoint,
        jev_model,
        router_sealed,
        router_url,
        enabled,
        at,
        by,
    )) = row
    else {
        return Ok(Settings {
            enabled: true,
            ..Settings::default()
        });
    };
    let data_key = Zeroizing::new(crypto.unwrap_key(&wrapped)?);
    let jev = match (jev_sealed, jev_endpoint, jev_model) {
        (Some(sealed), Some(endpoint), Some(model)) => Some(Jev {
            key: open_key(&data_key, AAD_JEV, &sealed)?,
            endpoint,
            model,
        }),
        _ => None,
    };
    let router = match (router_sealed, router_url) {
        (Some(sealed), Some(url)) => Some(RouterKey {
            key: open_key(&data_key, AAD_ROUTER, &sealed)?,
            url,
        }),
        _ => None,
    };
    let empty = jev.is_none() && router.is_none();
    Ok(Settings {
        jev,
        router,
        enabled,
        updated_at: (!empty).then_some(at),
        updated_by: if empty { None } else { by },
    })
}

/// Load and decrypt the current settings (defaults when nothing was ever set).
pub async fn load(db: &sqlx::PgPool, crypto: &Crypto) -> ApiResult<Settings> {
    let row: Option<Row> = sqlx::query_as(SELECT_SQL).fetch_optional(db).await?;
    decode(crypto, row)
}

// ---------------------------------------------------------------------------------------------
// Updates

/// A change to one block: leave it alone, clear it, or set (parts of) it.
#[derive(Debug, Default, Clone)]
pub enum Change<T> {
    #[default]
    Keep,
    Clear,
    Set(T),
}

/// New Jev values; `None` fields keep the stored value (or take the default for a new block).
#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JevInput {
    pub key: Option<String>,
    pub endpoint: Option<String>,
    pub model: Option<String>,
}

/// New router values; `None` fields keep the stored value (or take the default).
#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouterInput {
    pub key: Option<String>,
    pub url: Option<String>,
}

impl std::fmt::Debug for JevInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JevInput")
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .finish()
    }
}

impl std::fmt::Debug for RouterInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouterInput")
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("url", &self.url)
            .finish()
    }
}

impl Drop for JevInput {
    fn drop(&mut self) {
        if let Some(k) = self.key.as_mut() {
            zeroize::Zeroize::zeroize(k);
        }
    }
}

impl Drop for RouterInput {
    fn drop(&mut self) {
        if let Some(k) = self.key.as_mut() {
            zeroize::Zeroize::zeroize(k);
        }
    }
}

/// JSON: a missing field is [`Change::Keep`] (with `#[serde(default)]`), `null` is
/// [`Change::Clear`], an object is [`Change::Set`].
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Change<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Option::<T>::deserialize(d)?.map_or(Self::Clear, Self::Set))
    }
}

#[derive(Debug, Default, Clone)]
pub struct Update {
    pub jev: Change<JevInput>,
    pub router: Change<RouterInput>,
    pub enabled: Option<bool>,
}

impl Update {
    pub fn is_noop(&self) -> bool {
        matches!(self.jev, Change::Keep)
            && matches!(self.router, Change::Keep)
            && self.enabled.is_none()
    }
}

/// Validate an API key: trimmed, 8–4096 characters, printable ASCII without spaces.
pub fn clean_key(raw: &str, what: &str) -> ApiResult<String> {
    let key = raw.trim();
    if key.len() < 8 || key.len() > MAX_KEY_LEN {
        return Err(ApiError::bad_request(format!(
            "{what} must be 8-{MAX_KEY_LEN} characters"
        )));
    }
    if !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ApiError::bad_request(format!(
            "{what} must be printable ASCII without spaces"
        )));
    }
    Ok(key.to_owned())
}

/// Validate an `http(s)://` URL (no spaces, ≤ 2048 chars); a trailing `/` is dropped.
pub fn clean_url(raw: &str, what: &str) -> ApiResult<String> {
    let url = raw.trim().trim_end_matches('/');
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    let ok = url.len() <= MAX_URL_LEN
        && rest.is_some_and(|r| !r.is_empty() && !r.starts_with('/'))
        && url.bytes().all(|b| b.is_ascii_graphic());
    if ok {
        Ok(url.to_owned())
    } else {
        Err(ApiError::bad_request(format!(
            "{what} must be an http(s):// URL"
        )))
    }
}

fn clean_model(raw: &str) -> ApiResult<String> {
    let m = raw.trim();
    if m.is_empty() || m.len() > MAX_MODEL_LEN || m.chars().any(char::is_control) {
        return Err(ApiError::bad_request(format!(
            "jev model must be 1-{MAX_MODEL_LEN} characters"
        )));
    }
    Ok(m.to_owned())
}

fn merge_jev(current: Option<&Jev>, input: &JevInput) -> ApiResult<Jev> {
    let key = match (&input.key, current) {
        (Some(k), _) => clean_key(k, "jev key")?,
        (None, Some(c)) => c.key.clone(),
        (None, None) => return Err(ApiError::bad_request("jev key is required")),
    };
    let endpoint = match (&input.endpoint, current) {
        (Some(u), _) if !u.trim().is_empty() => clean_url(u, "jev endpoint")?,
        (_, Some(c)) => c.endpoint.clone(),
        _ => DEFAULT_JEV_ENDPOINT.to_owned(),
    };
    let model = match (&input.model, current) {
        (Some(m), _) if !m.trim().is_empty() => clean_model(m)?,
        (_, Some(c)) => c.model.clone(),
        _ => DEFAULT_JEV_MODEL.to_owned(),
    };
    Ok(Jev {
        key,
        endpoint,
        model,
    })
}

fn merge_router(current: Option<&RouterKey>, input: &RouterInput) -> ApiResult<RouterKey> {
    let key = match (&input.key, current) {
        (Some(k), _) => clean_key(k, "router key")?,
        (None, Some(c)) => c.key.clone(),
        (None, None) => return Err(ApiError::bad_request("router key is required")),
    };
    let url = match (&input.url, current) {
        (Some(u), _) if !u.trim().is_empty() => clean_url(u, "router url")?,
        (_, Some(c)) => c.url.clone(),
        _ => DEFAULT_ROUTER_URL.to_owned(),
    };
    Ok(RouterKey { key, url })
}

/// Apply `update` (one transaction, row-locked) and audit it. Returns the new settings.
#[allow(clippy::too_many_lines)]
pub async fn apply(
    db: &sqlx::PgPool,
    crypto: &Crypto,
    actor: &Actor,
    update: &Update,
) -> ApiResult<Settings> {
    let mut tx = db.begin().await?;
    let row: Option<Row> = sqlx::query_as(&format!("{SELECT_SQL} FOR UPDATE"))
        .fetch_optional(&mut *tx)
        .await?;
    let existed = row.is_some();
    let wrapped_existing = row.as_ref().map(|r| r.0.clone());
    let current = decode(crypto, row)?;

    let mut changed = serde_json::Map::new();
    let jev = match &update.jev {
        Change::Keep => current.jev.clone(),
        Change::Clear => {
            changed.insert("jev".into(), Value::Null);
            None
        }
        Change::Set(input) => {
            let j = merge_jev(current.jev.as_ref(), input)?;
            changed.insert(
                "jev".into(),
                json!({
                    "key_changed": input.key.is_some(),
                    "key_last4": last4(&j.key),
                    "endpoint": j.endpoint,
                    "model": j.model,
                }),
            );
            Some(j)
        }
    };
    let router = match &update.router {
        Change::Keep => current.router.clone(),
        Change::Clear => {
            changed.insert("router".into(), Value::Null);
            None
        }
        Change::Set(input) => {
            let r = merge_router(current.router.as_ref(), input)?;
            changed.insert(
                "router".into(),
                json!({
                    "key_changed": input.key.is_some(),
                    "key_last4": last4(&r.key),
                    "url": r.url,
                }),
            );
            Some(r)
        }
    };
    let enabled = update.enabled.unwrap_or(current.enabled);
    if let Some(e) = update.enabled {
        changed.insert("enabled".into(), Value::Bool(e));
    }

    // Reuse the row's data key (re-sealing the kept key under it); mint one for a new row.
    let data_key = Zeroizing::new(match &wrapped_existing {
        Some(w) => crypto.unwrap_key(w)?,
        None => Crypto::new_key(),
    });
    let wrapped = wrapped_existing.unwrap_or_else(|| crypto.wrap_key(&data_key));
    let jev_sealed = jev
        .as_ref()
        .map(|j| Crypto::seal(&data_key, AAD_JEV, j.key.as_bytes()));
    let router_sealed = router
        .as_ref()
        .map(|r| Crypto::seal(&data_key, AAD_ROUTER, r.key.as_bytes()));

    if jev.is_none() && router.is_none() && enabled {
        // Nothing left to store (and sharing is on, the default): drop the row.
        if existed {
            sqlx::query("DELETE FROM intelligence_settings WHERE id = 1")
                .execute(&mut *tx)
                .await?;
        }
    } else {
        sqlx::query(
            "INSERT INTO intelligence_settings (id, data_key_wrapped, jev_key_sealed, jev_endpoint,
                 jev_model, router_key_sealed, router_url, enabled, updated_at, updated_by)
             VALUES (1, $1, $2, $3, $4, $5, $6, $7, now(), $8)
             ON CONFLICT (id) DO UPDATE SET
                 data_key_wrapped = EXCLUDED.data_key_wrapped,
                 jev_key_sealed = EXCLUDED.jev_key_sealed,
                 jev_endpoint = EXCLUDED.jev_endpoint,
                 jev_model = EXCLUDED.jev_model,
                 router_key_sealed = EXCLUDED.router_key_sealed,
                 router_url = EXCLUDED.router_url,
                 enabled = EXCLUDED.enabled,
                 updated_at = now(),
                 updated_by = EXCLUDED.updated_by",
        )
        .bind(&wrapped)
        .bind(jev_sealed)
        .bind(jev.as_ref().map(|j| j.endpoint.as_str()))
        .bind(jev.as_ref().map(|j| j.model.as_str()))
        .bind(router_sealed)
        .bind(router.as_ref().map(|r| r.url.as_str()))
        .bind(enabled)
        .bind(actor.label())
        .execute(&mut *tx)
        .await?;
    }
    if !changed.is_empty() {
        changed.insert("via".into(), Value::from(actor.via()));
        audit(
            &mut *tx,
            actor.admin_id(),
            "intelligence.update",
            "intelligence",
            Some(Value::Object(changed)),
        )
        .await?;
    }
    tx.commit().await?;
    tracing::info!(via = actor.via(), "intelligence settings updated");
    load(db, crypto).await
}

/// Remove every stored key (and reset sharing to on). Returns whether anything was stored.
pub async fn clear(db: &sqlx::PgPool, actor: &Actor) -> ApiResult<bool> {
    let mut tx = db.begin().await?;
    let removed = sqlx::query("DELETE FROM intelligence_settings WHERE id = 1")
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    if removed {
        audit(
            &mut *tx,
            actor.admin_id(),
            "intelligence.clear",
            "intelligence",
            Some(json!({ "via": actor.via() })),
        )
        .await?;
    }
    tx.commit().await?;
    if removed {
        tracing::info!(via = actor.via(), "intelligence settings cleared");
    }
    Ok(removed)
}

async fn audit<'e, E: sqlx::PgExecutor<'e>>(
    db: E,
    admin_id: Option<Uuid>,
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
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// `GET /v1/intelligence`

pub(crate) fn routes() -> Router<SharedState> {
    Router::new().route("/intelligence", get(get_intelligence))
}

async fn get_intelligence(State(state): State<SharedState>, user: AuthUser) -> ApiResult<Response> {
    let settings = load(&state.db, &state.crypto).await?;
    let served = settings.enabled && !settings.is_empty();
    if served {
        // Audit at most once per user per hour (Coppers fetch at launch and on refresh).
        sqlx::query(
            "INSERT INTO admin_audit (admin_id, action, target, detail)
             SELECT NULL, 'intelligence.read', $1, $2
             WHERE NOT EXISTS (
                 SELECT 1 FROM admin_audit
                 WHERE action = 'intelligence.read' AND target = $1
                   AND at > now() - interval '1 hour')",
        )
        .bind(user.user_id.to_string())
        .bind(json!({
            "email": user.email,
            "device_id": user.device_id,
            "jev": settings.jev.is_some(),
            "router": settings.router.is_some(),
        }))
        .execute(&state.db)
        .await?;
        metrics::counter!("intelligence_reads_total").increment(1);
    }
    tracing::info!(served, "intelligence keys requested");
    let mut resp = Json(settings.user_view()).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masking() {
        assert_eq!(last4("sk-1234567890abcd"), "abcd");
        assert_eq!(last4("short-key"), "");
    }

    #[test]
    fn validation() {
        assert!(clean_key("  sk-12345678  ", "k").is_ok());
        assert!(clean_key("short", "k").is_err());
        assert!(clean_key("sk 12345678", "k").is_err());
        assert_eq!(
            clean_url("https://llm.example.com/", "u").unwrap(),
            "https://llm.example.com"
        );
        assert!(clean_url("ftp://x", "u").is_err());
        assert!(clean_url("https://", "u").is_err());
        assert!(clean_url("https:///path", "u").is_err());
        assert!(clean_url("https://a b", "u").is_err());
    }

    #[test]
    fn user_view_contract() {
        let empty = Settings {
            enabled: true,
            ..Settings::default()
        };
        assert_eq!(
            empty.user_view(),
            json!({ "jev": null, "router": null, "updated_at": null })
        );
        let s = Settings {
            jev: Some(Jev {
                key: "jev-key-0123456789".into(),
                endpoint: DEFAULT_JEV_ENDPOINT.into(),
                model: DEFAULT_JEV_MODEL.into(),
            }),
            router: None,
            enabled: true,
            updated_at: Some(time::macros::datetime!(2026-10-03 12:00 UTC)),
            updated_by: Some("cli".into()),
        };
        assert_eq!(
            s.user_view(),
            json!({
                "jev": { "key": "jev-key-0123456789", "endpoint": DEFAULT_JEV_ENDPOINT, "model": DEFAULT_JEV_MODEL },
                "router": null,
                "updated_at": "2026-10-03T12:00:00Z",
            })
        );
        let masked = s.masked_view().to_string();
        assert!(!masked.contains("jev-key-0123456789"));
        assert!(masked.contains("6789"));
        let off = Settings {
            enabled: false,
            ..s
        };
        assert_eq!(off.user_view()["jev"], Value::Null);
    }
}
