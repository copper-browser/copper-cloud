//! Who may pass the instance gate.
//!
//! * [`AccessMode`] (`server_settings.access_mode`): `open` accepts the shared instance key
//!   *and* access keys; `directory` accepts only access keys. Read through a 5-second
//!   in-memory cache ([`AccessModeCache`]) so the gate costs at most one query per 5 s.
//! * Access keys: `ck_` + 32 random bytes base64url, minted by admins (or by pairing in
//!   directory mode). Only SHA-256(key) is stored. A key passes the gate while it is neither
//!   revoked nor expired; `max_uses` caps how many accounts may be **created** with it
//!   (`uses` counts signups), so a `max_uses = 1` key is a one-person invite that keeps
//!   working for that person's Copper afterwards.
//! * [`GateIdentity`] is attached to every gated request as an extension; read it with the
//!   [`Gate`] extractor.

use std::convert::Infallible;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::ids;
use crate::state::AppState;

/// Prefix of every access key.
pub const ACCESS_KEY_PREFIX: &str = "ck_";
/// How long a read of `access_mode` is trusted before the gate re-reads it.
pub const ACCESS_MODE_TTL: Duration = Duration::from_secs(5);
/// `access_keys.last_used_at` is written at most this often per key.
pub const KEY_TOUCH_SECS: i32 = 60;
/// `server_settings` key holding the mode (JSON string `"open"` / `"directory"`).
pub const ACCESS_MODE_SETTING: &str = "access_mode";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessMode {
    /// The shared instance key and access keys are accepted.
    Open,
    /// Only access keys are accepted; signup requires one.
    Directory,
}

impl AccessMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Directory => "directory",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Self::Open),
            "directory" => Some(Self::Directory),
            _ => None,
        }
    }
}

impl std::fmt::Display for AccessMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The credential a gated request presented in `X-Copper-Instance`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateIdentity {
    /// The shared instance key (only accepted in `open` mode).
    Instance,
    /// A per-person access key.
    AccessKey { id: Uuid, email: Option<String> },
}

impl GateIdentity {
    pub fn access_key(&self) -> Option<(Uuid, Option<&str>)> {
        match self {
            Self::Instance => None,
            Self::AccessKey { id, email } => Some((*id, email.as_deref())),
        }
    }
}

/// Extractor: the gate identity of this request (`None` on ungated routes such as
/// `/v1/auth/pair`).
pub struct Gate(pub Option<GateIdentity>);

impl<S: Send + Sync> FromRequestParts<S> for Gate {
    type Rejection = Infallible;

    fn from_request_parts(
        parts: &mut Parts,
        _: &S,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(Ok(Self(parts.extensions.get::<GateIdentity>().cloned())))
    }
}

// ---------------------------------------------------------------------------------------------
// Access mode cache

/// `access_mode` cached for [`ACCESS_MODE_TTL`]. A refresh is single-flight: concurrent
/// requests at expiry wait for one query instead of each issuing their own.
#[derive(Default)]
pub struct AccessModeCache {
    slot: RwLock<Option<(AccessMode, Instant)>>,
    refresh: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for AccessModeCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessModeCache")
            .field("cached", &self.peek())
            .finish()
    }
}

impl AccessModeCache {
    /// Forget the cached value: the next gated request re-reads the database. Called after
    /// the admin API changes the mode; also the test hook.
    pub fn clear(&self) {
        *self
            .slot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    fn peek(&self) -> Option<(AccessMode, Instant)> {
        *self
            .slot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn fresh(&self) -> Option<AccessMode> {
        self.peek()
            .filter(|(_, at)| at.elapsed() < ACCESS_MODE_TTL)
            .map(|(m, _)| m)
    }

    fn put(&self, mode: AccessMode) {
        *self
            .slot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((mode, Instant::now()));
    }
}

/// The current access mode (cached ≤ 5 s). If the database cannot be read, a stale cached
/// value is used; with nothing cached the error propagates (fail closed).
pub async fn access_mode(state: &AppState) -> ApiResult<AccessMode> {
    let cache = &state.access_mode;
    if let Some(mode) = cache.fresh() {
        return Ok(mode);
    }
    let _flight = cache.refresh.lock().await;
    if let Some(mode) = cache.fresh() {
        return Ok(mode);
    }
    match load_access_mode(&state.db).await {
        Ok(mode) => {
            cache.put(mode);
            Ok(mode)
        }
        Err(err) => match cache.peek() {
            Some((mode, _)) => {
                tracing::warn!(error = %err, "reading access_mode failed; using cached value");
                Ok(mode)
            }
            None => Err(err),
        },
    }
}

/// `access_mode` straight from the database (no row → `open`).
pub async fn load_access_mode(db: &sqlx::PgPool) -> ApiResult<AccessMode> {
    let value: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT value FROM server_settings WHERE key = $1")
            .bind(ACCESS_MODE_SETTING)
            .fetch_optional(db)
            .await?;
    Ok(match value {
        None => AccessMode::Open,
        Some(v) => v.as_str().and_then(AccessMode::parse).unwrap_or_else(|| {
            tracing::warn!(value = %v, "invalid server_settings.access_mode; treating as open");
            AccessMode::Open
        }),
    })
}

/// Persist the access mode (the gate follows within [`ACCESS_MODE_TTL`] in other processes;
/// call [`AccessModeCache::clear`] to make this process follow immediately).
pub async fn set_access_mode<'e, E>(db: E, mode: AccessMode) -> ApiResult<()>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES ($1, $2)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(ACCESS_MODE_SETTING)
    .bind(serde_json::Value::from(mode.as_str()))
    .execute(db)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Access keys

/// A fresh access key: `ck_` + 43 base64url chars (32 random bytes).
pub fn new_access_key() -> String {
    format!("{ACCESS_KEY_PREFIX}{}", ids::random_token())
}

/// Shape check before touching the database.
pub fn plausible_access_key(s: &str) -> bool {
    s.strip_prefix(ACCESS_KEY_PREFIX)
        .is_some_and(ids::is_token_shape)
}

const LOOKUP_SQL: &str = "
WITH k AS (
    SELECT id, email FROM access_keys
    WHERE key_sha256 = $1 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())
), touch AS (
    UPDATE access_keys SET last_used_at = now()
    WHERE id IN (SELECT id FROM k)
      AND (last_used_at IS NULL OR last_used_at < now() - make_interval(secs => $2))
)
SELECT id, email FROM k";

/// Resolve a presented access key (valid = not revoked, not expired) and record the use
/// (`last_used_at`, at most once a minute).
pub async fn lookup_access_key(db: &sqlx::PgPool, key: &str) -> ApiResult<Option<GateIdentity>> {
    if !plausible_access_key(key) {
        return Ok(None);
    }
    let digest = ids::sha256(key.as_bytes());
    let row: Option<(Uuid, Option<String>)> = sqlx::query_as(LOOKUP_SQL)
        .bind(&digest[..])
        .bind(f64::from(KEY_TOUCH_SECS))
        .fetch_optional(db)
        .await?;
    Ok(row.map(|(id, email)| GateIdentity::AccessKey { id, email }))
}

/// Atomically count one signup against key `id`; `false` if the key is revoked, expired or
/// has reached `max_uses`. Run it in the signup transaction so a failed signup does not
/// consume a use.
pub async fn claim_signup_use(conn: &mut sqlx::PgConnection, id: Uuid) -> ApiResult<bool> {
    let claimed: Option<Uuid> = sqlx::query_scalar(
        "UPDATE access_keys SET uses = uses + 1, last_used_at = now()
         WHERE id = $1 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())
           AND (max_uses IS NULL OR uses < max_uses)
         RETURNING id",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Ok(claimed.is_some())
}

/// Parameters for [`mint_access_key`]. `email` must already be normalized.
#[derive(Debug, Default)]
pub struct NewAccessKey<'a> {
    pub label: &'a str,
    pub email: Option<&'a str>,
    pub expires_in_days: Option<i32>,
    pub max_uses: Option<i32>,
    pub created_by_admin: Option<Uuid>,
    pub created_by_user: Option<Uuid>,
}

pub const MAX_LABEL_CHARS: usize = 200;
pub const MAX_EXPIRES_IN_DAYS: i32 = 3650;
pub const MAX_MAX_USES: i32 = 1_000_000;

/// Insert a new access key; returns `(id, plaintext key)`. The plaintext is never stored.
pub async fn mint_access_key(
    conn: &mut sqlx::PgConnection,
    new: &NewAccessKey<'_>,
) -> ApiResult<(Uuid, String)> {
    let label = crate::auth::clean_name(Some(new.label), MAX_LABEL_CHARS)
        .ok_or_else(|| ApiError::bad_request("label must not be empty"))?;
    if let Some(d) = new.expires_in_days {
        if !(1..=MAX_EXPIRES_IN_DAYS).contains(&d) {
            return Err(ApiError::bad_request(format!(
                "expires_in_days must be between 1 and {MAX_EXPIRES_IN_DAYS}"
            )));
        }
    }
    if let Some(n) = new.max_uses {
        if !(1..=MAX_MAX_USES).contains(&n) {
            return Err(ApiError::bad_request(format!(
                "max_uses must be between 1 and {MAX_MAX_USES}"
            )));
        }
    }
    let key = new_access_key();
    let digest = ids::sha256(key.as_bytes());
    let id = ids::uuid_v7();
    sqlx::query(
        "INSERT INTO access_keys
             (id, key_sha256, label, email, created_by_admin, created_by_user, expires_at, max_uses)
         VALUES ($1, $2, $3, $4, $5, $6,
                 CASE WHEN $7::int IS NULL THEN NULL ELSE now() + make_interval(days => $7) END,
                 $8)",
    )
    .bind(id)
    .bind(&digest[..])
    .bind(&label)
    .bind(new.email)
    .bind(new.created_by_admin)
    .bind(new.created_by_user)
    .bind(new.expires_in_days)
    .bind(new.max_uses)
    .execute(conn)
    .await?;
    Ok((id, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_key_format() {
        let k = new_access_key();
        assert!(k.starts_with("ck_"));
        assert_eq!(k.len(), 3 + 43);
        assert!(plausible_access_key(&k));
        assert_eq!(ids::b64url_decode(&k[3..]).unwrap().len(), 32);
        assert_ne!(k, new_access_key());
        // Valid as a link-code `k=` value (instance-key charset, ≥ 32 chars).
        crate::config::validate_instance_key(&k).unwrap();
        for bad in ["", "ck_", "ck_short", &k[3..], &format!("cp_{}", &k[3..])] {
            assert!(!plausible_access_key(bad), "{bad}");
        }
        assert!(!plausible_access_key(&format!("{k}x")));
    }

    #[test]
    fn mode_parse() {
        assert_eq!(
            AccessMode::parse(" Directory "),
            Some(AccessMode::Directory)
        );
        assert_eq!(AccessMode::parse("open"), Some(AccessMode::Open));
        assert_eq!(AccessMode::parse("closed"), None);
        assert_eq!(
            serde_json::to_value(AccessMode::Directory).unwrap(),
            "directory"
        );
    }

    #[test]
    fn cache_expires_and_clears() {
        let c = AccessModeCache::default();
        assert_eq!(c.fresh(), None);
        c.put(AccessMode::Directory);
        assert_eq!(c.fresh(), Some(AccessMode::Directory));
        c.clear();
        assert_eq!(c.fresh(), None);
        *c.slot.write().unwrap() = Some((
            AccessMode::Open,
            Instant::now().checked_sub(ACCESS_MODE_TTL).unwrap(),
        ));
        assert_eq!(c.fresh(), None, "stale after the TTL");
        assert!(c.peek().is_some());
    }
}
