//! The stored link (`canvas_link`, singleton row): Canvas Cloud base URL, the app key sealed
//! under a per-row data key wrapped by the KEK (AAD [`AAD_APP_KEY`]), the optional TLS leaf
//! pin, and bookkeeping. The key never reaches logs, the admin API or the CLI output (only
//! its last four characters).

use std::net::IpAddr;

use copper_cloud_core::crypto::Crypto;
use copper_cloud_core::error::{ApiError, ApiResult};
use time::OffsetDateTime;
use url::Url;
use zeroize::Zeroizing;

/// AAD binding the sealed app key to its column.
pub const AAD_APP_KEY: &[u8] = b"canvas_link:app_key";
const MAX_KEY_LEN: usize = 4096;
const MAX_URL_LEN: usize = 2048;

/// The decrypted link settings.
#[derive(Clone)]
pub struct LinkSettings {
    /// Canvas Cloud base URL (scheme + authority + optional path prefix, no trailing `/`).
    pub url: Url,
    pub app_key: Zeroizing<String>,
    /// Lower-case hex SHA-256 of the pinned leaf certificate (self-signed Canvas Cloud).
    pub tls_fingerprint: Option<String>,
    pub enabled: bool,
    pub linked_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub updated_by: Option<String>,
}

impl std::fmt::Debug for LinkSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkSettings")
            .field("url", &self.url.as_str())
            .field("app_key", &"<redacted>")
            .field("tls_fingerprint", &self.tls_fingerprint)
            .field("enabled", &self.enabled)
            .field("linked_at", &self.linked_at)
            .field("updated_at", &self.updated_at)
            .field("updated_by", &self.updated_by)
            .finish()
    }
}

impl LinkSettings {
    /// The base URL as text without a trailing `/` (what the admin typed, normalised).
    pub fn base(&self) -> String {
        base_string(&self.url)
    }

    /// Whether `other` points at the same server with the same credentials (the HTTP client
    /// and the event bridge can be kept).
    pub fn same_target(&self, other: &Self) -> bool {
        self.url == other.url
            && *self.app_key == *other.app_key
            && self.tls_fingerprint == other.tls_fingerprint
    }
}

/// `url` without a trailing `/`.
pub fn base_string(url: &Url) -> String {
    url.as_str().trim_end_matches('/').to_owned()
}

/// Validated input for a new / replaced link.
pub struct NewLink {
    pub url: Url,
    pub app_key: Zeroizing<String>,
    pub tls_fingerprint: Option<String>,
}

impl NewLink {
    /// Validate raw admin/CLI input (see [`parse_base_url`], [`clean_app_key`],
    /// [`clean_fingerprint`]).
    pub fn parse(url: &str, app_key: &str, tls_fingerprint: Option<&str>) -> ApiResult<Self> {
        Ok(Self {
            url: parse_base_url(url)?,
            app_key: clean_app_key(app_key)?,
            tls_fingerprint: clean_fingerprint(tls_fingerprint)?,
        })
    }
}

/// Validate a Canvas Cloud base URL: `https://host[:port][/prefix]`, or `http://` for a
/// loopback host only (a Canvas Cloud on the same machine). No user info, query or fragment.
pub fn parse_base_url(raw: &str) -> ApiResult<Url> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_URL_LEN || raw.chars().any(char::is_whitespace) {
        return Err(ApiError::bad_request(
            "url must be the Canvas Cloud's https:// URL",
        ));
    }
    let url = Url::parse(raw).map_err(|_| {
        ApiError::bad_request(
            "url must be the Canvas Cloud's https:// URL (e.g. https://canvas.example.com)",
        )
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ApiError::bad_request("url must not contain credentials"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ApiError::bad_request(
            "url must not contain a query or fragment",
        ));
    }
    let Some(host) = url.host() else {
        return Err(ApiError::bad_request("url needs a host"));
    };
    match url.scheme() {
        "https" => {}
        "http" if is_loopback(&host) => {}
        "http" => return Err(ApiError::bad_request(
            "http:// is only allowed for a Canvas Cloud on this machine (localhost); use https://",
        )),
        _ => {
            return Err(ApiError::bad_request(
                "url must start with https:// (or http:// for localhost)",
            ))
        }
    }
    let base = base_string(&url);
    Url::parse(&base).map_err(|_| ApiError::bad_request("invalid url"))
}

fn is_loopback(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(ip) => IpAddr::V4(*ip).is_loopback(),
        url::Host::Ipv6(ip) => IpAddr::V6(*ip).is_loopback(),
    }
}

/// Validate an app key: trimmed, 8–4096 printable ASCII characters without spaces.
pub fn clean_app_key(raw: &str) -> ApiResult<Zeroizing<String>> {
    let key = raw.trim();
    if key.len() < 8 || key.len() > MAX_KEY_LEN || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ApiError::bad_request(
            "app_key must be the Canvas Cloud app key (cva_…), printable ASCII without spaces",
        ));
    }
    Ok(Zeroizing::new(key.to_owned()))
}

/// Normalise an optional SHA-256 leaf fingerprint (hex, any case, `:` separators allowed);
/// empty means none.
pub fn clean_fingerprint(raw: Option<&str>) -> ApiResult<Option<String>> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(fp) => copper_cloud_core::link::normalize_fingerprint(fp)
            .map(Some)
            .ok_or_else(|| {
                ApiError::bad_request(
                    "tls_fingerprint must be the SHA-256 of the certificate: 64 hex characters (colons allowed)",
                )
            }),
    }
}

/// Last four characters of a key (`""` when too short to reveal any).
pub fn last4(key: &str) -> String {
    copper_cloud_core::intelligence::last4(key)
}

type Row = (
    String,
    Vec<u8>,
    Vec<u8>,
    Option<String>,
    bool,
    OffsetDateTime,
    OffsetDateTime,
    Option<String>,
);

const SELECT_SQL: &str = "SELECT url, app_key_sealed, data_key_wrapped, tls_fingerprint, enabled,
       linked_at, updated_at, updated_by
FROM canvas_link WHERE id = 1";

fn decode(crypto: &Crypto, row: Row) -> ApiResult<LinkSettings> {
    let (url, sealed, wrapped, tls_fingerprint, enabled, linked_at, updated_at, updated_by) = row;
    let data_key = Zeroizing::new(crypto.unwrap_key(&wrapped)?);
    let plain = Zeroizing::new(Crypto::open(&data_key, AAD_APP_KEY, &sealed)?);
    let app_key = Zeroizing::new(
        String::from_utf8(plain.to_vec())
            .map_err(|_| ApiError::internal("stored canvas link key is not UTF-8"))?,
    );
    let url = Url::parse(&url).map_err(|e| ApiError::internal(format!("stored url: {e}")))?;
    Ok(LinkSettings {
        url,
        app_key,
        tls_fingerprint,
        enabled,
        linked_at,
        updated_at,
        updated_by,
    })
}

/// Load and decrypt the link (`None` when no Canvas Cloud is linked).
pub async fn load(db: &sqlx::PgPool, crypto: &Crypto) -> ApiResult<Option<LinkSettings>> {
    let row: Option<Row> = sqlx::query_as(SELECT_SQL).fetch_optional(db).await?;
    row.map(|r| decode(crypto, r)).transpose()
}

/// Whether a link row exists (cheap; no decryption).
pub async fn exists<'e, E: sqlx::PgExecutor<'e>>(db: E) -> ApiResult<bool> {
    Ok(
        sqlx::query_scalar::<_, i32>("SELECT 1 FROM canvas_link WHERE id = 1")
            .fetch_optional(db)
            .await?
            .is_some(),
    )
}

/// Store (insert or replace) the link, enabled. A replaced link keeps its `linked_at` when the
/// URL is unchanged (key rotation); a new URL counts as a new link.
pub async fn save(
    db: &sqlx::PgPool,
    crypto: &Crypto,
    link: &NewLink,
    updated_by: &str,
) -> ApiResult<LinkSettings> {
    let data_key = Zeroizing::new(Crypto::new_key());
    let wrapped = crypto.wrap_key(&data_key);
    let sealed = Crypto::seal(&data_key, AAD_APP_KEY, link.app_key.as_bytes());
    let base = base_string(&link.url);
    sqlx::query(
        "INSERT INTO canvas_link (id, url, app_key_sealed, data_key_wrapped, tls_fingerprint,
                                  enabled, linked_at, updated_at, updated_by)
         VALUES (1, $1, $2, $3, $4, true, now(), now(), $5)
         ON CONFLICT (id) DO UPDATE SET
             url = EXCLUDED.url,
             app_key_sealed = EXCLUDED.app_key_sealed,
             data_key_wrapped = EXCLUDED.data_key_wrapped,
             tls_fingerprint = EXCLUDED.tls_fingerprint,
             enabled = true,
             linked_at = CASE WHEN canvas_link.url = EXCLUDED.url THEN canvas_link.linked_at
                              ELSE now() END,
             updated_at = now(),
             updated_by = EXCLUDED.updated_by",
    )
    .bind(&base)
    .bind(&sealed)
    .bind(&wrapped)
    .bind(&link.tls_fingerprint)
    .bind(updated_by)
    .execute(db)
    .await?;
    load(db, crypto)
        .await?
        .ok_or_else(|| ApiError::internal("canvas link vanished after save"))
}

/// Remove the link. Returns the URL that was linked, if any. Queued outbox work goes too
/// (it targeted that server).
pub async fn remove(db: &sqlx::PgPool) -> ApiResult<Option<String>> {
    let mut tx = db.begin().await?;
    let url: Option<String> =
        sqlx::query_scalar("DELETE FROM canvas_link WHERE id = 1 RETURNING url")
            .fetch_optional(&mut *tx)
            .await?;
    if url.is_some() {
        sqlx::query("DELETE FROM canvas_link_outbox")
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        for ok in [
            "https://canvas.example.com",
            "https://canvas.example.com/",
            "https://canvas.example.com:8443/prefix/",
            "http://127.0.0.1:8480",
            "http://localhost:8480",
            "http://[::1]:8480",
            " https://c.example ",
        ] {
            let u = parse_base_url(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
            assert!(!u.as_str().ends_with("//"), "{u}");
        }
        assert_eq!(
            base_string(&parse_base_url("https://canvas.example.com/").unwrap()),
            "https://canvas.example.com"
        );
        assert_eq!(
            base_string(&parse_base_url("https://c.example:8443/p/").unwrap()),
            "https://c.example:8443/p"
        );
        for bad in [
            "",
            "canvas.example.com",
            "ftp://canvas.example.com",
            "http://canvas.example.com",
            "http://10.0.0.5:8480",
            "https://user:pw@canvas.example.com",
            "https://canvas.example.com/?x=1",
            "https://canvas.example.com/#f",
            "https://can vas.example.com",
        ] {
            assert!(parse_base_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn keys_and_fingerprints() {
        assert_eq!(
            clean_app_key("  cva_abcdefgh  ").unwrap().as_str(),
            "cva_abcdefgh"
        );
        assert!(clean_app_key("short").is_err());
        assert!(clean_app_key("has space inside").is_err());
        let hex = "AB".repeat(32);
        assert_eq!(
            clean_fingerprint(Some(&hex)).unwrap().unwrap(),
            "ab".repeat(32)
        );
        let colons = vec!["Ab"; 32].join(":");
        assert_eq!(
            clean_fingerprint(Some(&colons)).unwrap().unwrap(),
            "ab".repeat(32)
        );
        assert_eq!(clean_fingerprint(Some("  ")).unwrap(), None);
        assert_eq!(clean_fingerprint(None).unwrap(), None);
        assert!(clean_fingerprint(Some("abcd")).is_err());
        assert!(clean_fingerprint(Some(&"zz".repeat(32))).is_err());
    }

    #[test]
    fn debug_redacts_key() {
        let s = LinkSettings {
            url: Url::parse("https://c.example").unwrap(),
            app_key: Zeroizing::new("cva_supersecretkey".into()),
            tls_fingerprint: None,
            enabled: true,
            linked_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            updated_by: None,
        };
        assert!(!format!("{s:?}").contains("supersecret"));
    }
}
