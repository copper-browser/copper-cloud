//! Company single sign-on for Coppers: Google or a generic OIDC provider, native-app loopback
//! flow (RFC 8252 + PKCE S256). Copper opens the system browser at the provider, receives the
//! code on `http://127.0.0.1:<port>/` and hands code + verifier to the server, which holds the
//! client secret, exchanges the code and validates the ID token.
//!
//! * `GET /v1/auth/sso` — no gate, no auth → `{enabled, password_signin, providers:[…]}`.
//! * `POST /v1/auth/sso/{provider_id}` `{code, code_verifier, redirect_uri, device}` — no gate,
//!   auth rate limited → the `POST /v1/auth/pair` response plus `created`.
//!
//! The ID token comes straight from the provider's token endpoint over TLS, which
//! authenticates it (OIDC Core, "ID Token Validation"), so its signature is not checked; `iss`, `aud`, `exp`,
//! `email_verified` and the email domain (Google: also `hd`) are.
//!
//! Storage: `sso_providers` (client secret AES-256-GCM sealed under a per-row data key wrapped
//! by the KEK, AAD `sso:<id>`), `user_identities` ((provider, subject) → user) and the
//! `server_settings` keys `password_signin` and `instance_name`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::auth::{self, DeviceInput, UserView};
use crate::config::Config;
use crate::crypto::Crypto;
use crate::error::{ApiError, ApiResult};
use crate::extract::JsonBody;
use crate::intelligence::Actor;
use crate::pairing::{self, DeviceSignIn};
use crate::state::{AppState, SharedState};

/// `GET` the providers; `POST {SSO_PATH}/{provider_id}` signs in. Both bypass the gate.
pub const SSO_PATH: &str = "/v1/auth/sso";
/// `server_settings` key: password signup/login for Copper users (default `true`).
pub const PASSWORD_SIGNIN_SETTING: &str = "password_signin";
/// `server_settings` key: the name shown on `/start` (default [`DEFAULT_INSTANCE_NAME`]).
pub const INSTANCE_NAME_SETTING: &str = "instance_name";
pub const DEFAULT_INSTANCE_NAME: &str = "Copper Cloud";
pub const MAX_INSTANCE_NAME_CHARS: usize = 100;

pub const GOOGLE_AUTHORIZATION_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const GOOGLE_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_ISSUERS: [&str; 2] = ["https://accounts.google.com", "accounts.google.com"];
pub const SCOPES: [&str; 3] = ["openid", "email", "profile"];
/// Allowed clock difference when checking `exp`.
const CLOCK_SKEW_SECS: i64 = 60;
const DISCOVERY_TTL: Duration = Duration::from_secs(3600);
/// A failed discovery is retried at most once a minute per issuer.
const DISCOVERY_FAILURE_TTL: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const BODY_LIMIT: usize = 16 * 1024;
const MAX_DOMAINS: usize = 50;

// ---------------------------------------------------------------------------------------------
// Providers

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// Google accounts (fixed endpoints; the `hd` claim must name an allowed domain).
    Google,
    /// Any OIDC provider, found by issuer discovery.
    Oidc,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Oidc => "oidc",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "google" => Some(Self::Google),
            "oidc" => Some(Self::Oidc),
            _ => None,
        }
    }
}

/// A configured provider (never the secret itself).
#[derive(Clone, Debug)]
pub struct Provider {
    pub id: String,
    pub kind: ProviderKind,
    pub name: String,
    pub client_id: String,
    pub has_secret: bool,
    pub issuer: Option<String>,
    pub domains: Vec<String>,
    pub auto_create: bool,
    pub enabled: bool,
    pub updated_at: OffsetDateTime,
    pub updated_by: Option<String>,
}

impl Provider {
    /// Enabled and complete enough to sign anyone in (Google needs the client secret).
    pub fn usable(&self) -> bool {
        self.enabled && (self.kind == ProviderKind::Oidc || self.has_secret)
    }

    /// The admin view (`GET /admin/api/sso` → `providers[]`).
    pub fn admin_view(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind,
            "name": self.name,
            "client_id": self.client_id,
            "has_secret": self.has_secret,
            "issuer": self.issuer,
            "domains": self.domains,
            "auto_create": self.auto_create,
            "enabled": self.enabled,
            "updated_at": rfc3339(self.updated_at),
        })
    }
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

type ProviderRow = (
    String,
    String,
    String,
    String,
    bool,
    Option<String>,
    Vec<String>,
    bool,
    bool,
    OffsetDateTime,
    Option<String>,
);

const PROVIDER_SQL: &str = "SELECT id, kind, name, client_id, client_secret_sealed IS NOT NULL,
       issuer, domains, auto_create, enabled, updated_at, updated_by
FROM sso_providers";

fn provider_from_row(row: ProviderRow) -> ApiResult<Provider> {
    let (id, kind, name, client_id, has_secret, issuer, domains, auto_create, enabled, at, by) =
        row;
    let kind = ProviderKind::parse(&kind)
        .ok_or_else(|| ApiError::internal(format!("sso provider {id}: unknown kind {kind}")))?;
    Ok(Provider {
        id,
        kind,
        name,
        client_id,
        has_secret,
        issuer,
        domains,
        auto_create,
        enabled,
        updated_at: at,
        updated_by: by,
    })
}

/// Every configured provider: `google` first, then by creation.
pub async fn list_providers(db: &sqlx::PgPool) -> ApiResult<Vec<Provider>> {
    let rows: Vec<ProviderRow> = sqlx::query_as(&format!(
        "{PROVIDER_SQL} ORDER BY id <> 'google', created_at, id"
    ))
    .fetch_all(db)
    .await?;
    rows.into_iter().map(provider_from_row).collect()
}

/// One provider by id.
pub async fn find_provider(db: &sqlx::PgPool, id: &str) -> ApiResult<Option<Provider>> {
    let row: Option<ProviderRow> = sqlx::query_as(&format!("{PROVIDER_SQL} WHERE id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await?;
    row.map(provider_from_row).transpose()
}

/// The provider onboarding links point at: the first usable one (`google` first).
pub async fn primary_provider(db: &sqlx::PgPool) -> ApiResult<Option<Provider>> {
    Ok(list_providers(db).await?.into_iter().find(Provider::usable))
}

fn secret_aad(id: &str) -> Vec<u8> {
    format!("sso:{id}").into_bytes()
}

/// The decrypted client secret, if one is stored.
async fn client_secret(
    db: &sqlx::PgPool,
    crypto: &Crypto,
    id: &str,
) -> ApiResult<Option<Zeroizing<String>>> {
    let row: Option<(Vec<u8>, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT data_key_wrapped, client_secret_sealed FROM sso_providers WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    let Some((wrapped, Some(sealed))) = row else {
        return Ok(None);
    };
    let data_key = Zeroizing::new(crypto.unwrap_key(&wrapped)?);
    let plain = Zeroizing::new(Crypto::open(&data_key, &secret_aad(id), &sealed)?);
    let secret = std::str::from_utf8(&plain)
        .map_err(|_| ApiError::internal("sso client secret is not UTF-8"))?;
    Ok(Some(Zeroizing::new(secret.to_owned())))
}

/// `PUT /admin/api/sso/providers/{id}` body (and the CLI's `set-google` / `set-oidc`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderInput {
    pub kind: ProviderKind,
    #[serde(default)]
    pub name: Option<String>,
    pub client_id: String,
    /// Omitted (or empty) = keep the stored secret.
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub issuer: Option<String>,
    pub domains: Vec<String>,
    #[serde(default)]
    pub auto_create: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// A provider id: `[a-z0-9][a-z0-9_-]{0,31}`.
pub fn valid_provider_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
}

/// Lower-cased, de-duplicated email domains (`@` prefix tolerated). `required`: at least one.
pub fn clean_domains(raw: &[String], required: bool) -> ApiResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for d in raw {
        let d = d.trim().trim_start_matches('@').trim_end_matches('.');
        let d = d.to_ascii_lowercase();
        let valid = d.len() <= 253
            && d.contains('.')
            && !d.starts_with(['.', '-'])
            && !d.contains("..")
            && d.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'));
        if !valid {
            return Err(ApiError::bad_request(format!(
                "{d:?} is not a valid email domain (e.g. example.com)"
            )));
        }
        if !out.contains(&d) {
            out.push(d);
        }
    }
    if required && out.is_empty() {
        return Err(ApiError::bad_request(
            "at least one allowed email domain is required",
        ));
    }
    if out.len() > MAX_DOMAINS {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_DOMAINS} domains"
        )));
    }
    Ok(out)
}

fn clean_token(raw: &str, what: &str, max: usize) -> ApiResult<String> {
    let v = raw.trim();
    if v.is_empty() || v.len() > max || !v.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ApiError::bad_request(format!(
            "{what} must be 1-{max} printable characters without spaces"
        )));
    }
    Ok(v.to_owned())
}

/// An issuer / endpoint URL: `https://` (plain `http://` only with the test hook).
fn clean_issuer(raw: &str, allow_http: bool) -> ApiResult<String> {
    let url = crate::intelligence::clean_url(raw, "issuer")?;
    if !(url.starts_with("https://") || allow_http) {
        return Err(ApiError::bad_request("issuer must be an https:// URL"));
    }
    Ok(url)
}

/// Create or replace provider `id` (the stored secret is kept unless a new one is given).
#[allow(clippy::too_many_lines)]
pub async fn put_provider(
    db: &sqlx::PgPool,
    cfg: &Config,
    crypto: &Crypto,
    actor: &Actor,
    id: &str,
    input: ProviderInput,
) -> ApiResult<Provider> {
    if !valid_provider_id(id) {
        return Err(ApiError::bad_request(
            "provider id must be 1-32 characters: a-z, 0-9, '-' or '_'",
        ));
    }
    if (input.kind == ProviderKind::Google) != (id == "google") {
        return Err(ApiError::bad_request(
            "the Google provider has id \"google\" (and only it); other providers are kind \"oidc\"",
        ));
    }
    let name = match auth::clean_name(input.name.as_deref(), 100) {
        Some(n) => n,
        None if input.kind == ProviderKind::Google => "Google".to_owned(),
        None => id.to_owned(),
    };
    let client_id = clean_token(&input.client_id, "client_id", 512)?;
    let secret = input
        .client_secret
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| clean_token(s, "client_secret", 1024).map(Zeroizing::new))
        .transpose()?;
    let issuer = match input.kind {
        ProviderKind::Google => None,
        ProviderKind::Oidc => Some(clean_issuer(
            input
                .issuer
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("an oidc provider needs an issuer URL"))?,
            cfg.sso_test.allow_http,
        )?),
    };
    let domains = clean_domains(&input.domains, true)?;
    let auto_create = input.auto_create.unwrap_or(true);
    let enabled = input.enabled.unwrap_or(true);

    let mut tx = db.begin().await?;
    let existing: Option<(Vec<u8>, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT data_key_wrapped, client_secret_sealed FROM sso_providers WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let (wrapped, kept_sealed) = match existing {
        Some((w, s)) => (w, s),
        None => (crypto.wrap_key(&Crypto::new_key()), None),
    };
    let sealed = match &secret {
        Some(secret) => {
            let data_key = Zeroizing::new(crypto.unwrap_key(&wrapped)?);
            Some(Crypto::seal(&data_key, &secret_aad(id), secret.as_bytes()))
        }
        None => kept_sealed,
    };
    if input.kind == ProviderKind::Google && sealed.is_none() {
        return Err(ApiError::bad_request(
            "client_secret is required for Google (Desktop app client)",
        ));
    }
    sqlx::query(
        "INSERT INTO sso_providers (id, kind, name, client_id, data_key_wrapped,
             client_secret_sealed, issuer, domains, auto_create, enabled, updated_at, updated_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now(), $11)
         ON CONFLICT (id) DO UPDATE SET kind = EXCLUDED.kind, name = EXCLUDED.name,
             client_id = EXCLUDED.client_id, data_key_wrapped = EXCLUDED.data_key_wrapped,
             client_secret_sealed = EXCLUDED.client_secret_sealed, issuer = EXCLUDED.issuer,
             domains = EXCLUDED.domains, auto_create = EXCLUDED.auto_create,
             enabled = EXCLUDED.enabled, updated_at = now(), updated_by = EXCLUDED.updated_by",
    )
    .bind(id)
    .bind(input.kind.as_str())
    .bind(&name)
    .bind(&client_id)
    .bind(&wrapped)
    .bind(&sealed)
    .bind(&issuer)
    .bind(&domains)
    .bind(auto_create)
    .bind(enabled)
    .bind(actor.label())
    .execute(&mut *tx)
    .await?;
    audit(
        &mut *tx,
        actor.admin_id(),
        "sso.provider.put",
        id,
        Some(json!({
            "kind": input.kind,
            "name": name,
            "client_id": client_id,
            "issuer": issuer,
            "domains": domains,
            "auto_create": auto_create,
            "enabled": enabled,
            "secret_changed": secret.is_some(),
            "via": actor.via(),
        })),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(provider = id, via = actor.via(), "sso provider saved");
    find_provider(db, id).await?.ok_or(ApiError::NotFound)
}

/// Delete provider `id` (its identity links go with it; accounts stay). `false` if missing.
pub async fn delete_provider(db: &sqlx::PgPool, actor: &Actor, id: &str) -> ApiResult<bool> {
    let mut tx = db.begin().await?;
    let deleted = sqlx::query("DELETE FROM sso_providers WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if deleted > 0 {
        audit(
            &mut *tx,
            actor.admin_id(),
            "sso.provider.delete",
            id,
            Some(json!({ "via": actor.via() })),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(deleted > 0)
}

/// Turn every provider (or just `id`) on or off without forgetting it. Returns rows changed.
pub async fn set_providers_enabled(
    db: &sqlx::PgPool,
    actor: &Actor,
    id: Option<&str>,
    enabled: bool,
) -> ApiResult<u64> {
    let mut tx = db.begin().await?;
    let changed = sqlx::query(
        "UPDATE sso_providers SET enabled = $2, updated_at = now(), updated_by = $3
         WHERE ($1::text IS NULL OR id = $1) AND enabled <> $2",
    )
    .bind(id)
    .bind(enabled)
    .bind(actor.label())
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if changed > 0 {
        audit(
            &mut *tx,
            actor.admin_id(),
            if enabled {
                "sso.provider.enable"
            } else {
                "sso.provider.disable"
            },
            id.unwrap_or("*"),
            Some(json!({ "via": actor.via(), "providers": changed })),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(changed)
}

// ---------------------------------------------------------------------------------------------
// Server settings: password_signin, instance_name

async fn setting(db: &sqlx::PgPool, key: &str) -> ApiResult<Option<Value>> {
    Ok(
        sqlx::query_scalar("SELECT value FROM server_settings WHERE key = $1")
            .bind(key)
            .fetch_optional(db)
            .await?,
    )
}

async fn put_setting<'e, E: sqlx::PgExecutor<'e>>(db: E, key: &str, value: Value) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES ($1, $2)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(key)
    .bind(value)
    .execute(db)
    .await?;
    Ok(())
}

/// Whether Copper users may sign up / log in with a password (default `true`).
pub async fn password_signin(db: &sqlx::PgPool) -> ApiResult<bool> {
    Ok(setting(db, PASSWORD_SIGNIN_SETTING)
        .await?
        .and_then(|v| v.as_bool())
        .unwrap_or(true))
}

pub async fn set_password_signin(db: &sqlx::PgPool, actor: &Actor, on: bool) -> ApiResult<()> {
    let mut tx = db.begin().await?;
    put_setting(&mut *tx, PASSWORD_SIGNIN_SETTING, Value::Bool(on)).await?;
    audit(
        &mut *tx,
        actor.admin_id(),
        "sso.password_signin",
        "settings",
        Some(json!({ "password_signin": on, "via": actor.via() })),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The instance's display name (`/start`), default [`DEFAULT_INSTANCE_NAME`].
pub async fn instance_name(db: &sqlx::PgPool) -> ApiResult<String> {
    Ok(setting(db, INSTANCE_NAME_SETTING)
        .await?
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_INSTANCE_NAME.to_owned()))
}

/// Set (or with an empty/`None` name, reset) the instance name; returns the stored value.
pub async fn set_instance_name<'e, E: sqlx::PgExecutor<'e>>(
    db: E,
    name: Option<&str>,
) -> ApiResult<String> {
    let name = auth::clean_name(name, MAX_INSTANCE_NAME_CHARS + 1)
        .unwrap_or_else(|| DEFAULT_INSTANCE_NAME.to_owned());
    if name.chars().count() > MAX_INSTANCE_NAME_CHARS {
        return Err(ApiError::bad_request(format!(
            "instance_name must be at most {MAX_INSTANCE_NAME_CHARS} characters"
        )));
    }
    put_setting(db, INSTANCE_NAME_SETTING, Value::from(name.clone())).await?;
    Ok(name)
}

pub(crate) async fn audit<'e, E: sqlx::PgExecutor<'e>>(
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
// Onboarding links

/// `copper-cloud://HOST:PORT/#sso=<provider>[&fp=…]` on the onboarding host (see
/// [`crate::tls::onboarding_link`]).
pub fn join_link(cfg: &Config, provider_id: &str) -> String {
    onboarding_link(cfg).sso_link(provider_id)
}

/// `https://HOST[:PORT]/start` on the onboarding host — the URL admins share.
pub fn start_url(cfg: &Config) -> String {
    onboarding_link(cfg).https_url("/start")
}

/// [`crate::tls::onboarding_link`], falling back to `public_url` without a fingerprint when
/// the certificate cannot be read.
pub fn onboarding_link(cfg: &Config) -> crate::link::LinkCode {
    crate::tls::onboarding_link(cfg).unwrap_or_else(|err| {
        tracing::warn!(
            error = format!("{err:#}"),
            "onboarding link without fingerprint"
        );
        crate::link::LinkCode {
            host: cfg.public_host().to_owned(),
            port: cfg.public_port(),
            instance_key: cfg.instance_key.clone(),
            fingerprint: None,
        }
    })
}

// ---------------------------------------------------------------------------------------------
// Provider HTTP: discovery (cached) and the code exchange

/// OIDC discovery fields we use.
#[derive(Clone, Debug, Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
}

/// A discovery result and when it was fetched.
type CachedDiscovery = (Instant, Result<Discovery, String>);

/// Per-process HTTP client and OIDC discovery cache (lives in [`AppState`]).
#[derive(Default)]
pub struct SsoCache {
    http: OnceLock<reqwest::Client>,
    discovery: Mutex<HashMap<String, CachedDiscovery>>,
}

impl std::fmt::Debug for SsoCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SsoCache").finish_non_exhaustive()
    }
}

impl SsoCache {
    /// Bounded, redirect-free client (HTTPS only unless the test hook allows HTTP).
    fn http(&self, cfg: &Config) -> ApiResult<&reqwest::Client> {
        if let Some(client) = self.http.get() {
            return Ok(client);
        }
        let client = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .connect_timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .https_only(!cfg.sso_test.allow_http)
            .user_agent(concat!("copper-cloud/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ApiError::internal(format!("building the SSO HTTP client: {e}")))?;
        Ok(self.http.get_or_init(|| client))
    }

    /// Forget cached discovery documents (after a provider changes).
    pub fn clear(&self) {
        self.discovery
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

fn same_url(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// `<issuer>/.well-known/openid-configuration`, cached for an hour (a failure for a minute).
/// The document's `issuer` must equal the configured one and its endpoints must be HTTPS.
pub async fn discover(state: &AppState, issuer: &str) -> anyhow::Result<Discovery> {
    let issuer = issuer.trim_end_matches('/');
    let cached = state
        .sso
        .discovery
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(issuer)
        .cloned();
    if let Some((at, cached)) = cached {
        let ttl = if cached.is_ok() {
            DISCOVERY_TTL
        } else {
            DISCOVERY_FAILURE_TTL
        };
        if at.elapsed() < ttl {
            return cached.map_err(anyhow::Error::msg);
        }
    }
    let result = fetch_discovery(state, issuer)
        .await
        .map_err(|e| format!("{e:#}"));
    state
        .sso
        .discovery
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(issuer.to_owned(), (Instant::now(), result.clone()));
    result.map_err(anyhow::Error::msg)
}

async fn fetch_discovery(state: &AppState, issuer: &str) -> anyhow::Result<Discovery> {
    let http = state
        .sso
        .http(&state.cfg)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let url = format!("{issuer}/.well-known/openid-configuration");
    let resp = http.get(&url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("{url} answered HTTP {}", resp.status().as_u16());
    }
    let d: Discovery = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("{url}: not an OpenID configuration ({e})"))?;
    if !same_url(&d.issuer, issuer) {
        anyhow::bail!(
            "discovery issuer {:?} does not match the configured issuer {issuer:?}",
            d.issuer
        );
    }
    let https = |u: &str| u.starts_with("https://") || state.cfg.sso_test.allow_http;
    if !https(&d.authorization_endpoint) || !https(&d.token_endpoint) {
        anyhow::bail!("discovery endpoints must be https:// URLs");
    }
    Ok(d)
}

/// Endpoints and accepted issuers for a provider.
struct Endpoints {
    authorization: String,
    token: String,
    issuers: Vec<String>,
}

async fn endpoints(state: &AppState, p: &Provider) -> anyhow::Result<Endpoints> {
    match p.kind {
        ProviderKind::Google => Ok(Endpoints {
            authorization: GOOGLE_AUTHORIZATION_ENDPOINT.to_owned(),
            token: state
                .cfg
                .sso_test
                .google_token_endpoint
                .clone()
                .unwrap_or_else(|| GOOGLE_TOKEN_ENDPOINT.to_owned()),
            issuers: GOOGLE_ISSUERS.iter().map(|s| (*s).to_owned()).collect(),
        }),
        ProviderKind::Oidc => {
            let issuer = p.issuer.as_deref().unwrap_or_default();
            let d = discover(state, issuer).await?;
            Ok(Endpoints {
                authorization: d.authorization_endpoint,
                token: d.token_endpoint,
                issuers: vec![d.issuer],
            })
        }
    }
}

/// Why a code exchange failed.
#[derive(Debug)]
enum ExchangeError {
    /// The token endpoint answered with an OAuth error (`invalid_grant`, `invalid_client`, …).
    OAuth { status: u16, error: String },
    /// Network / TLS / unparsable response.
    Transport(String),
}

impl std::fmt::Display for ExchangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OAuth { status, error } => write!(f, "{error} (HTTP {status})"),
            Self::Transport(msg) => f.write_str(msg),
        }
    }
}

struct Exchange<'a> {
    token_endpoint: &'a str,
    client_id: &'a str,
    client_secret: Option<&'a str>,
    code: &'a str,
    code_verifier: &'a str,
    redirect_uri: &'a str,
}

/// Authorization-code exchange (client secret in the form body); returns the `id_token`.
async fn exchange_code(http: &reqwest::Client, x: &Exchange<'_>) -> Result<String, ExchangeError> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", x.code),
        ("code_verifier", x.code_verifier),
        ("redirect_uri", x.redirect_uri),
        ("client_id", x.client_id),
    ];
    if let Some(secret) = x.client_secret {
        form.push(("client_secret", secret));
    }
    let resp = http
        .post(x.token_endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|e| ExchangeError::Transport(format!("token endpoint unreachable: {e}")))?;
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.map_err(|_| {
        ExchangeError::Transport(format!(
            "token endpoint answered HTTP {status} without JSON"
        ))
    })?;
    if !(200..300).contains(&status) {
        let error = body["error"].as_str().unwrap_or("error").to_owned();
        return Err(ExchangeError::OAuth { status, error });
    }
    body["id_token"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| ExchangeError::Transport("the token response has no id_token".into()))
}

// ---------------------------------------------------------------------------------------------
// ID token validation

/// Who the provider says signed in.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct VerifiedIdentity {
    pub subject: String,
    pub email: String,
    pub name: Option<String>,
}

fn sso_failed(why: impl std::fmt::Display) -> ApiError {
    ApiError::coded(
        StatusCode::UNAUTHORIZED,
        "sso_failed",
        format!("single sign-on failed: {why}"),
    )
}

/// 403 `sso_domain` naming the allowed domains.
pub(crate) fn domain_error(code: &'static str, domains: &[String]) -> ApiError {
    let allowed = if domains.is_empty() {
        "no email domains are allowed yet (ask your administrator)".to_owned()
    } else {
        let list: Vec<String> = domains.iter().map(|d| format!("@{d}")).collect();
        format!("use your {} account", list.join(" or "))
    };
    ApiError::coded(
        StatusCode::FORBIDDEN,
        code,
        format!("this email domain is not allowed on this Copper Cloud: {allowed}"),
    )
}

/// The domain part of an email, lower-cased.
pub(crate) fn email_domain(email: &str) -> Option<String> {
    email
        .rsplit_once('@')
        .map(|(_, d)| d.trim_end_matches('.').to_ascii_lowercase())
        .filter(|d| !d.is_empty())
}

/// Decode the (unverified) payload of a compact JWT.
fn jwt_claims(token: &str) -> ApiResult<Value> {
    let mut parts = token.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(sso_failed("the provider returned a malformed ID token"));
    };
    let bytes = crate::ids::b64url_decode(payload.trim_end_matches('='))
        .map_err(|_| sso_failed("the provider returned a malformed ID token"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| sso_failed("the provider returned a malformed ID token"))
}

fn claim_true(v: &Value) -> bool {
    v.as_bool() == Some(true) || v.as_str() == Some("true")
}

/// Check the ID token claims against the provider (`now` = Unix seconds).
pub(crate) fn check_claims(
    claims: &Value,
    kind: ProviderKind,
    issuers: &[String],
    client_id: &str,
    domains: &[String],
    now: i64,
) -> ApiResult<VerifiedIdentity> {
    let iss = claims["iss"].as_str().unwrap_or_default();
    if !issuers.iter().any(|i| same_url(i, iss)) {
        return Err(sso_failed("the ID token is from an unexpected issuer"));
    }
    let aud_ok = match &claims["aud"] {
        Value::String(a) => a == client_id,
        Value::Array(list) => {
            list.iter().any(|a| a.as_str() == Some(client_id))
                && (list.len() == 1 || claims["azp"].as_str() == Some(client_id))
        }
        _ => false,
    };
    if !aud_ok {
        return Err(sso_failed("the ID token is for a different client"));
    }
    let exp = claims["exp"].as_f64().unwrap_or(0.0);
    #[allow(clippy::cast_precision_loss)]
    if exp + (CLOCK_SKEW_SECS as f64) < now as f64 {
        return Err(sso_failed(
            "the ID token has expired (check the server clock)",
        ));
    }
    let subject = claims["sub"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 255)
        .ok_or_else(|| sso_failed("the ID token has no subject"))?
        .to_owned();
    let email = claims["email"]
        .as_str()
        .ok_or_else(|| sso_failed("the provider did not share the email address (scope \"email\")"))
        .and_then(|e| {
            auth::normalize_email(e).map_err(|_| sso_failed("the email address is invalid"))
        })?;
    if !claim_true(&claims["email_verified"]) {
        return Err(sso_failed(
            "the provider has not verified this email address",
        ));
    }
    let domain = email_domain(&email).unwrap_or_default();
    if !domains.contains(&domain) {
        return Err(domain_error("sso_domain", domains));
    }
    // Google: only Workspace accounts of an allowed domain (`hd`), never a consumer account
    // that merely uses an allowed address.
    if kind == ProviderKind::Google {
        let hd = claims["hd"].as_str().map(str::to_ascii_lowercase);
        if !hd.is_some_and(|hd| domains.contains(&hd)) {
            return Err(domain_error("sso_domain", domains));
        }
    }
    let name = auth::clean_name(claims["name"].as_str(), 200);
    Ok(VerifiedIdentity {
        subject,
        email,
        name,
    })
}

/// A loopback redirect URI (RFC 8252 §7.3): `http://127.0.0.1:<port>/…`,
/// `http://[::1]:<port>/…` or `http://localhost:<port>/…`, no query or fragment.
pub fn loopback_redirect(uri: &str) -> bool {
    let Some(rest) = uri.strip_prefix("http://") else {
        return false;
    };
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let Some((host, port)) = authority.rsplit_once(':') else {
        return false;
    };
    let port_ok = !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|p| p > 0);
    let path_ok = rest.contains('/')
        && path.len() <= 512
        && path
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'?' | b'#' | b'\\'));
    matches!(host, "127.0.0.1" | "[::1]" | "localhost") && port_ok && path_ok
}

/// PKCE verifier shape (RFC 7636 §4.1): 43-128 unreserved characters.
fn plausible_verifier(v: &str) -> bool {
    (43..=128).contains(&v.len())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

// ---------------------------------------------------------------------------------------------
// Accounts

/// Create a password-less account (SSO, agent pairing). `name` falls back to the email's
/// local part.
pub(crate) async fn create_sso_user(
    tx: &mut sqlx::PgConnection,
    crypto: &Crypto,
    email: &str,
    name: Option<&str>,
) -> ApiResult<UserView> {
    let display_name =
        auth::clean_name(name, 200).unwrap_or_else(|| auth::default_display_name(email));
    auth::insert_user(tx, crypto, email, &display_name, None).await
}

/// `(user, disabled)` by id.
pub(crate) async fn user_with_status(
    tx: &mut sqlx::PgConnection,
    id: Uuid,
) -> ApiResult<Option<(UserView, bool)>> {
    let row: Option<(Uuid, String, String, OffsetDateTime, bool)> = sqlx::query_as(
        "SELECT id, email, display_name, created_at, disabled FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(row.map(|(id, email, display_name, created_at, disabled)| {
        (
            UserView {
                id,
                email,
                display_name,
                created_at,
            },
            disabled,
        )
    }))
}

/// The account for a verified identity: linked identity → same email (linked now) → new
/// account when `auto_create`. Returns `(user id, created)`.
async fn resolve_user(
    tx: &mut sqlx::PgConnection,
    crypto: &Crypto,
    p: &Provider,
    who: &VerifiedIdentity,
) -> ApiResult<(Uuid, bool)> {
    let linked: Option<Uuid> = sqlx::query_scalar(
        "UPDATE user_identities SET last_login_at = now(), email = $3
         WHERE provider_id = $1 AND subject = $2 RETURNING user_id",
    )
    .bind(&p.id)
    .bind(&who.subject)
    .bind(&who.email)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = linked {
        return Ok((id, false));
    }
    let by_email: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE lower(email) = lower($1)")
            .bind(&who.email)
            .fetch_optional(&mut *tx)
            .await?;
    let (user_id, created) = match by_email {
        Some(id) => (id, false),
        None if p.auto_create => {
            let user = create_sso_user(tx, crypto, &who.email, who.name.as_deref()).await?;
            (user.id, true)
        }
        None => {
            return Err(ApiError::coded(
                StatusCode::FORBIDDEN,
                "sso_no_account",
                format!(
                    "there is no Copper Cloud account for {}; ask your administrator to create one",
                    who.email
                ),
            ))
        }
    };
    sqlx::query(
        "INSERT INTO user_identities (provider_id, subject, user_id, email) VALUES ($1, $2, $3, $4)
         ON CONFLICT (provider_id, subject) DO NOTHING",
    )
    .bind(&p.id)
    .bind(&who.subject)
    .bind(user_id)
    .bind(&who.email)
    .execute(&mut *tx)
    .await?;
    audit(
        &mut *tx,
        None,
        if created {
            "sso.user_create"
        } else {
            "sso.link"
        },
        &user_id.to_string(),
        Some(json!({ "provider": p.id, "email": who.email })),
    )
    .await?;
    Ok((user_id, created))
}

// ---------------------------------------------------------------------------------------------
// Handlers

pub(crate) fn routes() -> Router<SharedState> {
    Router::new()
        .route("/auth/sso", get(sso_info))
        .route("/auth/sso/{provider_id}", post(sso_sign_in))
}

/// Whether `path` is one of the ungated SSO routes.
pub fn is_sso_path(path: &str) -> bool {
    path == SSO_PATH
        || path
            .strip_prefix(SSO_PATH)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

/// The public provider list (`GET /v1/auth/sso`). An OIDC provider whose discovery fails is
/// left out (and logged).
pub async fn public_view(state: &AppState) -> ApiResult<Value> {
    let mut providers = Vec::new();
    for p in list_providers(&state.db)
        .await?
        .iter()
        .filter(|p| p.usable())
    {
        let authorization_endpoint = match endpoints(state, p).await {
            Ok(e) => e.authorization,
            Err(err) => {
                tracing::warn!(provider = %p.id, error = format!("{err:#}"), "sso provider unavailable");
                continue;
            }
        };
        let hd =
            (p.kind == ProviderKind::Google && p.domains.len() == 1).then(|| p.domains[0].clone());
        providers.push(json!({
            "id": p.id,
            "kind": p.kind,
            "name": p.name,
            "client_id": p.client_id,
            "authorization_endpoint": authorization_endpoint,
            "scopes": SCOPES,
            "domains": p.domains,
            "hd": hd,
        }));
    }
    Ok(json!({
        "enabled": !providers.is_empty(),
        "password_signin": password_signin(&state.db).await?,
        "providers": providers,
    }))
}

async fn sso_info(State(state): State<SharedState>) -> ApiResult<Json<Value>> {
    Ok(Json(public_view(&state).await?))
}

#[derive(Deserialize)]
struct SsoRequest {
    code: String,
    code_verifier: String,
    redirect_uri: String,
    #[serde(default)]
    device: Option<DeviceInput>,
}

#[derive(Serialize)]
struct SsoResponse {
    #[serde(flatten)]
    signed_in: DeviceSignIn,
    created: bool,
}

fn sso_disabled() -> ApiError {
    ApiError::NotFoundCode {
        code: "sso_disabled",
        message: "single sign-on with this provider is not set up on this Copper Cloud",
    }
}

async fn sso_sign_in(
    State(state): State<SharedState>,
    Path(provider_id): Path<String>,
    JsonBody(req): JsonBody<SsoRequest, BODY_LIMIT>,
) -> ApiResult<Json<SsoResponse>> {
    let provider = find_provider(&state.db, &provider_id)
        .await?
        .filter(Provider::usable)
        .ok_or_else(sso_disabled)?;
    if !loopback_redirect(&req.redirect_uri) {
        return Err(ApiError::coded(
            StatusCode::BAD_REQUEST,
            "bad_redirect",
            "redirect_uri must be http://127.0.0.1:<port>/, http://[::1]:<port>/ or http://localhost:<port>/",
        ));
    }
    let code = req.code.trim();
    if code.is_empty() || code.len() > 4096 {
        return Err(ApiError::bad_request("code is required"));
    }
    if !plausible_verifier(&req.code_verifier) {
        return Err(ApiError::bad_request(
            "code_verifier must be 43-128 characters (RFC 7636)",
        ));
    }
    let ep = endpoints(&state, &provider).await.map_err(|err| {
        tracing::warn!(provider = %provider.id, error = format!("{err:#}"), "sso discovery failed");
        sso_failed("the identity provider could not be reached")
    })?;
    let secret = client_secret(&state.db, &state.crypto, &provider.id).await?;
    let http = state.sso.http(&state.cfg)?;
    let id_token = exchange_code(
        http,
        &Exchange {
            token_endpoint: &ep.token,
            client_id: &provider.client_id,
            client_secret: secret.as_deref().map(String::as_str),
            code,
            code_verifier: &req.code_verifier,
            redirect_uri: &req.redirect_uri,
        },
    )
    .await
    .map_err(|err| {
        tracing::info!(provider = %provider.id, error = %err, "sso code exchange failed");
        sso_failed(format!(
            "the identity provider rejected the sign-in ({err})"
        ))
    })?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let who = check_claims(
        &jwt_claims(&id_token)?,
        provider.kind,
        &ep.issuers,
        &provider.client_id,
        &provider.domains,
        now,
    )
    .inspect_err(|err| tracing::info!(provider = %provider.id, code = err.code(), "sso refused"))?;

    let mut tx = state.db.begin().await?;
    let (user_id, created) = resolve_user(&mut tx, &state.crypto, &provider, &who).await?;
    let (user, disabled) = user_with_status(&mut tx, user_id)
        .await?
        .ok_or_else(|| sso_failed("the account disappeared"))?;
    if disabled {
        return Err(ApiError::Denied {
            code: "account_disabled",
            message: "this account is disabled",
        });
    }
    let signed_in =
        pairing::sign_in_device(&state, &mut tx, user, req.device.unwrap_or_default(), "sso")
            .await?;
    tx.commit().await?;
    tracing::Span::current().record("user_id", tracing::field::display(user_id));
    tracing::info!(
        user_id = %user_id,
        device_id = %signed_in.session.device.id,
        provider = %provider.id,
        created,
        "sso sign-in"
    );
    Ok(Json(SsoResponse { signed_in, created }))
}

// ---------------------------------------------------------------------------------------------
// Admin: provider self-test

/// `POST /admin/api/sso/providers/{id}/test`: discovery (OIDC) and a token-endpoint probe with
/// a dummy code — `invalid_grant` means the client id/secret were accepted, `invalid_client`
/// that they were not. `None` if the provider does not exist.
pub async fn test_provider(state: &AppState, id: &str) -> ApiResult<Option<Value>> {
    let Some(p) = find_provider(&state.db, id).await? else {
        return Ok(None);
    };
    let mut checks = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| {
        checks.push(json!({ "name": name, "ok": ok, "detail": detail }));
        ok
    };
    check(
        "enabled",
        p.enabled,
        if p.enabled {
            "enabled"
        } else {
            "disabled: Coppers do not see it"
        }
        .to_owned(),
    );
    let secret_ok = p.has_secret || p.kind == ProviderKind::Oidc;
    check(
        "client_secret",
        secret_ok,
        match (p.has_secret, p.kind) {
            (true, _) => "stored (encrypted)".to_owned(),
            (false, ProviderKind::Oidc) => "none (public client)".to_owned(),
            (false, ProviderKind::Google) => "missing: Google needs the client secret".to_owned(),
        },
    );
    state.sso.clear();
    let ep = match endpoints(state, &p).await {
        Ok(ep) => {
            if p.kind == ProviderKind::Oidc {
                check("discovery", true, format!("token endpoint {}", ep.token));
            }
            Some(ep)
        }
        Err(err) => {
            check("discovery", false, format!("{err:#}"));
            None
        }
    };
    if let Some(ep) = ep {
        let secret = client_secret(&state.db, &state.crypto, &p.id).await?;
        let http = state.sso.http(&state.cfg)?;
        let probe = exchange_code(
            http,
            &Exchange {
                token_endpoint: &ep.token,
                client_id: &p.client_id,
                client_secret: secret.as_deref().map(String::as_str),
                code: "copper-cloud-connection-test",
                code_verifier: "copper-cloud-connection-test-0000000000000000",
                redirect_uri: "http://127.0.0.1:1/",
            },
        )
        .await;
        let (ok, detail) = match probe {
            Err(ExchangeError::OAuth { error, .. }) if error == "invalid_grant" => (
                true,
                "client id and secret accepted (the test code was rejected as expected)".to_owned(),
            ),
            Err(ExchangeError::OAuth { error, .. })
                if matches!(error.as_str(), "invalid_client" | "unauthorized_client") =>
            {
                (false, format!("{error}: the client id or secret is wrong"))
            }
            Err(err) => (false, err.to_string()),
            Ok(_) => (false, "unexpected: the test code was accepted".to_owned()),
        };
        check("token_endpoint", ok, detail);
    }
    let ok = checks.iter().all(|c| c["ok"] == true);
    Ok(Some(json!({ "ok": ok, "checks": checks })))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT: &str = "123-abc.apps.googleusercontent.com";

    fn google_claims() -> Value {
        json!({
            "iss": "https://accounts.google.com",
            "aud": CLIENT,
            "exp": 2_000_000_000,
            "sub": "1171",
            "email": "Ana@Example.com",
            "email_verified": true,
            "hd": "example.com",
            "name": "Ana A",
        })
    }

    fn check(claims: &Value, kind: ProviderKind) -> ApiResult<VerifiedIdentity> {
        let issuers: Vec<String> = match kind {
            ProviderKind::Google => GOOGLE_ISSUERS.iter().map(|s| (*s).to_owned()).collect(),
            ProviderKind::Oidc => vec!["https://idp.example.com/".to_owned()],
        };
        check_claims(
            claims,
            kind,
            &issuers,
            CLIENT,
            &["example.com".to_owned()],
            1_900_000_000,
        )
    }

    fn code_of(r: ApiResult<VerifiedIdentity>) -> &'static str {
        r.unwrap_err().code()
    }

    #[test]
    fn google_claims_are_checked() {
        let who = check(&google_claims(), ProviderKind::Google).unwrap();
        assert_eq!(who.subject, "1171");
        assert_eq!(who.email, "Ana@Example.com");
        assert_eq!(who.name.as_deref(), Some("Ana A"));

        let with = |k: &str, v: Value| {
            let mut c = google_claims();
            c[k] = v;
            check(&c, ProviderKind::Google)
        };
        assert!(with("iss", json!("accounts.google.com")).is_ok());
        assert_eq!(
            code_of(with("iss", json!("https://evil.example"))),
            "sso_failed"
        );
        assert_eq!(code_of(with("aud", json!("other"))), "sso_failed");
        assert!(with("aud", json!([CLIENT])).is_ok());
        assert_eq!(code_of(with("aud", json!([CLIENT, "x"]))), "sso_failed");
        assert_eq!(code_of(with("exp", json!(1_800_000_000))), "sso_failed");
        assert!(
            with("exp", json!(1_900_000_000 - 30)).is_ok(),
            "within skew"
        );
        assert_eq!(code_of(with("email_verified", json!(false))), "sso_failed");
        assert!(with("email_verified", json!("true")).is_ok());
        assert_eq!(code_of(with("email", json!("ana@gmail.com"))), "sso_domain");
        assert_eq!(code_of(with("hd", json!("other.com"))), "sso_domain");
        assert_eq!(code_of(with("hd", Value::Null)), "sso_domain");
        assert_eq!(code_of(with("sub", json!(""))), "sso_failed");
        let err = with("email", json!("ana@gmail.com")).unwrap_err();
        assert!(err.to_string().contains("@example.com"), "{err}");
    }

    #[test]
    fn oidc_claims_need_no_hd() {
        let mut c = google_claims();
        c["iss"] = json!("https://idp.example.com");
        c["hd"] = Value::Null;
        assert!(check(&c, ProviderKind::Oidc).is_ok());
        c["iss"] = json!("https://accounts.google.com");
        assert_eq!(code_of(check(&c, ProviderKind::Oidc)), "sso_failed");
    }

    #[test]
    fn jwt_payload_decoding() {
        let payload = crate::ids::b64url(br#"{"sub":"x"}"#);
        assert_eq!(
            jwt_claims(&format!("e30.{payload}.sig")).unwrap()["sub"],
            "x"
        );
        assert_eq!(jwt_claims(&format!("e30.{payload}.")).unwrap()["sub"], "x");
        for bad in ["", "a.b", "a.b.c.d", "e30.!!!.x", "e30.e30x.x"] {
            assert!(jwt_claims(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn redirect_uris_are_loopback_only() {
        for ok in [
            "http://127.0.0.1:53682/",
            "http://[::1]:1/",
            "http://localhost:65535/",
            "http://127.0.0.1:8080/callback",
        ] {
            assert!(loopback_redirect(ok), "{ok}");
        }
        for bad in [
            "https://127.0.0.1:53682/",
            "http://127.0.0.1/",
            "http://127.0.0.1:53682",
            "http://127.0.0.1:0/",
            "http://127.0.0.1:65536/",
            "http://127.0.0.2:53682/",
            "http://evil.com:80/",
            "http://localhost.evil.com:80/",
            "http://user@127.0.0.1:80/",
            "http://127.0.0.1:80/?x=1",
            "http://127.0.0.1:80/#f",
            "http://127.0.0.1:80/ a",
            "",
        ] {
            assert!(!loopback_redirect(bad), "{bad}");
        }
    }

    #[test]
    fn domains_and_ids() {
        assert_eq!(
            clean_domains(
                &[
                    " @Example.COM ".into(),
                    "example.com".into(),
                    "b.io.".into()
                ],
                true
            )
            .unwrap(),
            ["example.com", "b.io"]
        );
        assert!(clean_domains(&[], true).is_err());
        assert_eq!(clean_domains(&[], false).unwrap(), Vec::<String>::new());
        for bad in [
            "",
            "localhost",
            "a b.com",
            "x@y.com",
            ".a.com",
            "a..com",
            "-a.com",
        ] {
            assert!(clean_domains(&[bad.into()], false).is_err(), "{bad}");
        }
        for ok in ["google", "okta", "entra-id", "a_1"] {
            assert!(valid_provider_id(ok), "{ok}");
        }
        for bad in ["", "Google", "-x", "a/b", &"x".repeat(33)] {
            assert!(!valid_provider_id(bad), "{bad}");
        }
        assert!(is_sso_path("/v1/auth/sso"));
        assert!(is_sso_path("/v1/auth/sso/google"));
        for bad in [
            "/v1/auth/sso/",
            "/v1/auth/sso/a/b",
            "/v1/auth/ssox",
            "/v1/auth/sso2/x",
        ] {
            assert!(!is_sso_path(bad), "{bad}");
        }
        assert!(plausible_verifier(&"a".repeat(43)));
        assert!(!plausible_verifier(&"a".repeat(42)));
        assert!(!plausible_verifier(&format!("{}+", "a".repeat(43))));
    }
}
