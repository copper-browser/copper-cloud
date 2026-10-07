//! Sync: last-writer-wins documents, append-only history (which its owner may delete), and the
//! per-user SSE event stream.
//!
//! Every query is scoped by the caller's `user_id`; payloads are sealed with the user's data
//! key (AAD `"<user_id>:<domain>"`, history uses domain `history`).
//!
//! History deletes never open payloads: the server selects rows by metadata only (`seq`,
//! `visited_at`, `device_id`); picking individual entries is done by the user on the client,
//! which sends the chosen `seq`s. See "History delete" below.

use std::borrow::Cow;
use std::convert::Infallible;
use std::io::Write as _;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderValue};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures::Stream;
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::value::RawValue;
use time::OffsetDateTime;
use tracing::Instrument as _;
use uuid::Uuid;

use crate::auth::{AuthUser, KeyedUser};
use crate::crypto::Crypto;
use crate::error::{ApiError, ApiResult};
use crate::events::Event;
use crate::extract::{check_content_length, read_body, QueryParams};
use crate::state::SharedState;

/// Whole-document domains every client may read and write.
pub const SHARED_DOMAINS: &[&str] = &["spaces", "settings", "bookmarks"];
/// Prefix of per-device open-tab domains: `tabs:<device_id>`.
pub const TABS_PREFIX: &str = "tabs:";
const HISTORY_AAD_DOMAIN: &str = "history";
const DEFAULT_HISTORY_LIMIT: usize = 500;
/// SSE keepalive comment interval.
pub const SSE_KEEPALIVE: Duration = Duration::from_secs(15);
/// How often an open SSE stream re-checks that its session still exists.
const SSE_REVALIDATE: Duration = Duration::from_secs(300);

pub(crate) fn routes() -> Router<SharedState> {
    Router::new()
        .route("/sync/docs", get(list_docs))
        .route("/sync/docs/{domain}", get(get_doc).put(put_doc))
        .route(
            "/sync/history",
            get(pull_history)
                .post(push_history)
                .delete(delete_history_route),
        )
        .route("/sync/events", get(events))
}

/// Canonical `tabs:<device_id>` domain string.
pub fn tabs_domain(device_id: Uuid) -> String {
    let mut s = String::with_capacity(TABS_PREFIX.len() + 36);
    s.push_str(TABS_PREFIX);
    s.push_str(
        device_id
            .hyphenated()
            .encode_lower(&mut Uuid::encode_buffer()),
    );
    s
}

/// A validated sync domain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Domain {
    Shared(&'static str),
    Tabs(Uuid),
}

impl Domain {
    pub fn parse(raw: &str) -> ApiResult<Self> {
        if let Some(d) = SHARED_DOMAINS.iter().find(|d| **d == raw) {
            return Ok(Self::Shared(d));
        }
        if let Some(id) = raw.strip_prefix(TABS_PREFIX) {
            return Uuid::parse_str(id)
                .map(Self::Tabs)
                .map_err(|_| ApiError::bad_request("tabs domain must be tabs:<device uuid>"));
        }
        Err(ApiError::bad_request(format!(
            "unknown sync domain; expected one of {SHARED_DOMAINS:?} or tabs:<device_id>"
        )))
    }

    pub fn as_str(&self) -> Cow<'static, str> {
        match self {
            Self::Shared(d) => Cow::Borrowed(d),
            Self::Tabs(id) => Cow::Owned(tabs_domain(*id)),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Docs

#[derive(Serialize, sqlx::FromRow)]
struct DocMeta {
    domain: String,
    version: i64,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
    device_id: Option<Uuid>,
    #[sqlx(rename = "payload_bytes")]
    bytes: i32,
}

async fn list_docs(
    State(state): State<SharedState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<DocMeta>>> {
    let docs = sqlx::query_as::<_, DocMeta>(
        "SELECT domain, version, updated_at, device_id, payload_bytes
         FROM sync_docs WHERE user_id = $1 ORDER BY domain",
    )
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(docs))
}

#[derive(sqlx::FromRow)]
struct DocRow {
    version: i64,
    updated_at: OffsetDateTime,
    device_id: Option<Uuid>,
    payload: Vec<u8>,
}

#[derive(Serialize)]
struct DocBody {
    domain: String,
    version: i64,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
    device_id: Option<Uuid>,
    /// Standard base64 of the document bytes (JSON).
    payload: String,
}

async fn load_doc(state: &SharedState, user_id: Uuid, domain: &str) -> ApiResult<Option<DocRow>> {
    Ok(sqlx::query_as::<_, DocRow>(
        "SELECT version, updated_at, device_id, payload FROM sync_docs
         WHERE user_id = $1 AND domain = $2",
    )
    .bind(user_id)
    .bind(domain)
    .fetch_optional(&state.db)
    .await?)
}

fn decrypt_doc(key: &[u8; 32], user_id: Uuid, domain: &str, row: &DocRow) -> ApiResult<DocBody> {
    let plain = Crypto::open(key, &Crypto::user_aad(user_id, domain), &row.payload)?;
    Ok(DocBody {
        domain: domain.to_owned(),
        version: row.version,
        updated_at: row.updated_at,
        device_id: row.device_id,
        payload: crate::ids::b64_std(&plain),
    })
}

async fn get_doc(
    State(state): State<SharedState>,
    user: KeyedUser,
    Path(domain): Path<String>,
) -> ApiResult<Json<DocBody>> {
    let domain = Domain::parse(&domain)?.as_str();
    let row = load_doc(&state, user.user.user_id, &domain)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(decrypt_doc(
        &user.data_key,
        user.user.user_id,
        &domain,
        &row,
    )?))
}

#[derive(Deserialize)]
struct PutDoc<'a> {
    /// Version the client last saw (0 = "I believe it does not exist yet").
    base_version: i64,
    #[serde(borrow)]
    payload: Cow<'a, str>,
}

#[derive(Serialize)]
struct PutResult {
    version: i64,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

/// Body cap for a doc PUT: base64 expansion of `max_blob_bytes` plus JSON envelope.
fn doc_body_limit(max_blob: usize) -> usize {
    max_blob.div_ceil(3) * 4 + 4096
}

async fn put_doc(
    State(state): State<SharedState>,
    user: KeyedUser,
    Path(domain): Path<String>,
    req: Request,
) -> ApiResult<Json<PutResult>> {
    let parsed = Domain::parse(&domain)?;
    if let Domain::Tabs(owner) = parsed {
        if owner != user.user.device_id {
            return Err(ApiError::Forbidden);
        }
    }
    let domain = parsed.as_str();
    let max_blob = state.cfg.limits.max_blob_bytes;
    let limit = doc_body_limit(max_blob);
    check_content_length(&req, limit)?;
    let bytes = read_body(req.into_body(), limit).await?;
    let (base_version, plain) = {
        let body: PutDoc<'_> = serde_json::from_slice(&bytes)
            .map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))?;
        if body.base_version < 0 {
            return Err(ApiError::bad_request("base_version must be >= 0"));
        }
        let plain = crate::ids::b64_decode_any(&body.payload)
            .map_err(|_| ApiError::bad_request("payload must be base64"))?;
        (body.base_version, plain)
    };
    drop(bytes);
    if plain.len() > max_blob {
        return Err(ApiError::PayloadTooLarge);
    }
    let user_id = user.user.user_id;
    let device_id = user.user.device_id;
    let sealed = Crypto::seal(&user.data_key, &Crypto::user_aad(user_id, &domain), &plain);
    let plain_len = i32::try_from(plain.len()).map_err(|_| ApiError::PayloadTooLarge)?;
    drop(plain);

    let written: Option<(i64, OffsetDateTime)> = if base_version == 0 {
        sqlx::query_as(
            "INSERT INTO sync_docs (user_id, domain, device_id, version, payload, payload_bytes)
             VALUES ($1, $2, $3, 1, $4, $5)
             ON CONFLICT (user_id, domain) DO NOTHING
             RETURNING version, updated_at",
        )
        .bind(user_id)
        .bind(domain.as_ref())
        .bind(device_id)
        .bind(&sealed)
        .bind(plain_len)
        .fetch_optional(&state.db)
        .await?
    } else {
        sqlx::query_as(
            "UPDATE sync_docs
             SET version = version + 1, payload = $4, payload_bytes = $5, device_id = $3,
                 updated_at = now()
             WHERE user_id = $1 AND domain = $2 AND version = $6
             RETURNING version, updated_at",
        )
        .bind(user_id)
        .bind(domain.as_ref())
        .bind(device_id)
        .bind(&sealed)
        .bind(plain_len)
        .bind(base_version)
        .fetch_optional(&state.db)
        .await?
    };

    let Some((version, updated_at)) = written else {
        // Lost the race / stale base: hand back the server copy so the client can merge.
        let current = load_doc(&state, user_id, &domain).await?;
        let conflict = match current {
            Some(row) => {
                let doc = decrypt_doc(&user.data_key, user_id, &domain, &row)?;
                serde_json::to_value(doc).map_err(anyhow::Error::from)?
            }
            None => json!({ "domain": domain, "version": 0, "payload": null }),
        };
        return Err(ApiError::Conflict(conflict));
    };

    metrics::histogram!("sync_doc_bytes").record(f64::from(plain_len));
    state.events.publish(
        user_id,
        Event::Doc {
            domain: domain.into_owned(),
            version,
            device_id: Some(device_id),
        },
    );
    Ok(Json(PutResult {
        version,
        updated_at,
    }))
}

// ---------------------------------------------------------------------------------------------
// History

#[derive(Deserialize)]
struct HistoryPush<'a> {
    #[serde(borrow)]
    entries: Vec<&'a RawValue>,
}

#[derive(Deserialize)]
struct EntryProbe {
    #[serde(default)]
    visited_at: Option<serde_json::Value>,
}

/// One history entry the server skipped; the rest of its batch is stored.
#[derive(Debug, Serialize)]
struct RejectedEntry {
    /// Position in the request's `entries`.
    index: usize,
    /// `too_large` | `not_object` | `invalid` | `bad_visited_at`.
    reason: &'static str,
    message: String,
}

/// Validates one entry and returns its `visited_at`, or why it is skipped.
fn check_history_entry(
    index: usize,
    text: &str,
    max_bytes: usize,
    now: OffsetDateTime,
) -> Result<OffsetDateTime, RejectedEntry> {
    let reject = |reason, message: String| RejectedEntry {
        index,
        reason,
        message,
    };
    if text.len() > max_bytes {
        return Err(reject(
            "too_large",
            format!("entry is {} bytes; the limit is {max_bytes}", text.len()),
        ));
    }
    if !text.starts_with('{') {
        return Err(reject("not_object", "entry must be a JSON object".into()));
    }
    let probe: EntryProbe =
        serde_json::from_str(text).map_err(|e| reject("invalid", format!("entry: {e}")))?;
    parse_visited_at(probe.visited_at.as_ref(), now).map_err(|_| {
        reject(
            "bad_visited_at",
            "visited_at must be RFC 3339 or a Unix timestamp".into(),
        )
    })
}

/// `visited_at` may be RFC 3339 or a Unix timestamp (seconds, or milliseconds when > 1e11).
fn parse_visited_at(
    v: Option<&serde_json::Value>,
    now: OffsetDateTime,
) -> ApiResult<OffsetDateTime> {
    use time::format_description::well_known::Rfc3339;
    let bad = || ApiError::bad_request("visited_at must be RFC 3339 or a Unix timestamp");
    let ts = match v {
        None | Some(serde_json::Value::Null) => return Ok(now),
        Some(serde_json::Value::String(s)) => {
            OffsetDateTime::parse(s, &Rfc3339).map_err(|_| bad())?
        }
        Some(serde_json::Value::Number(n)) => {
            let mut secs = n.as_f64().ok_or_else(bad)?;
            if secs.abs() > 1e11 {
                secs /= 1000.0;
            }
            if !secs.is_finite() || secs.abs() > 1e11 {
                return Err(bad());
            }
            #[allow(clippy::cast_possible_truncation)]
            let nanos = (secs * 1e9) as i128;
            OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| bad())?
        }
        Some(_) => return Err(bad()),
    };
    Ok(ts)
}

fn history_body_limit(cfg: &crate::config::Limits) -> usize {
    (cfg.max_history_batch * (cfg.max_history_entry_bytes + 8) + 1024).min(64 * 1024 * 1024)
}

async fn current_history_seq(state: &SharedState, user_id: Uuid) -> ApiResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COALESCE(max(seq), 0) FROM history WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&state.db)
            .await?,
    )
}

/// `POST /sync/history`. Only problems with the request as a whole are errors (`400`: not
/// JSON / no `entries` array / more than `max_history_batch` entries; `413`: body over the
/// cap). A bad *entry* (too large, not an object, unparsable `visited_at`) is skipped and
/// listed in `rejected` while the rest of the batch is stored, so a client that resends the
/// same batch can never wedge on one poison entry.
async fn push_history(
    State(state): State<SharedState>,
    user: KeyedUser,
    req: Request,
) -> ApiResult<Json<serde_json::Value>> {
    let limits = &state.cfg.limits;
    let limit = history_body_limit(limits);
    check_content_length(&req, limit)?;
    let bytes = read_body(req.into_body(), limit).await?;
    // Invalid UTF-8 inside one entry's string would fail the whole parse; repair it to U+FFFD
    // instead (borrowed, no copy, when the body is valid).
    let text = String::from_utf8_lossy(&bytes);
    let body: HistoryPush<'_> = serde_json::from_str(&text).map_err(|e| {
        ApiError::bad_request(format!(
            "invalid JSON body: {e}; expected an object with an `entries` array"
        ))
    })?;
    if body.entries.len() > limits.max_history_batch {
        return Err(ApiError::bad_request(format!(
            "too many entries: {} in one request; at most {} (see GET /v1/info limits)",
            body.entries.len(),
            limits.max_history_batch
        )));
    }
    let user_id = user.user.user_id;
    let aad = Crypto::user_aad(user_id, HISTORY_AAD_DOMAIN);
    let now = OffsetDateTime::now_utc();
    let mut visited = Vec::with_capacity(body.entries.len());
    let mut payloads = Vec::with_capacity(body.entries.len());
    let mut rejected = Vec::new();
    for (i, raw) in body.entries.iter().enumerate() {
        let text = raw.get();
        match check_history_entry(i, text, limits.max_history_entry_bytes, now) {
            Ok(at) => {
                visited.push(at);
                payloads.push(Crypto::seal(&user.data_key, &aad, text.as_bytes()));
            }
            Err(r) => rejected.push(r),
        }
    }
    drop(body);
    drop(text);
    drop(bytes);
    if let Some(first) = rejected.first() {
        metrics::counter!("history_rejected")
            .increment(u64::try_from(rejected.len()).unwrap_or(u64::MAX));
        tracing::warn!(
            rejected = rejected.len(),
            accepted = payloads.len(),
            first_index = first.index,
            first_reason = first.reason,
            first_message = %crate::error::loggable_message(&first.message),
            "history entries skipped"
        );
    }
    if payloads.is_empty() {
        let seq = current_history_seq(&state, user_id).await?;
        return Ok(Json(
            json!({ "seq": seq, "inserted": 0, "rejected": rejected }),
        ));
    }

    let (seq, inserted): (Option<i64>, i64) = sqlx::query_as(
        "WITH ins AS (
             INSERT INTO history (user_id, device_id, visited_at, payload)
             SELECT $1, $2, v, p FROM UNNEST($3::timestamptz[], $4::bytea[]) AS t(v, p)
             RETURNING seq
         )
         SELECT max(seq), count(*) FROM ins",
    )
    .bind(user_id)
    .bind(user.user.device_id)
    .bind(&visited)
    .bind(&payloads)
    .fetch_one(&state.db)
    .await?;
    let seq = seq.unwrap_or(0);
    #[allow(clippy::cast_precision_loss)]
    metrics::counter!("history_rows").increment(u64::try_from(inserted).unwrap_or(0));
    state.events.publish(user_id, Event::History { seq });
    Ok(Json(
        json!({ "seq": seq, "inserted": inserted, "rejected": rejected }),
    ))
}

#[derive(Deserialize)]
struct PullQuery {
    #[serde(default)]
    since: i64,
    limit: Option<usize>,
    /// `me` (the calling device) or a device UUID whose entries to skip.
    exclude_device: Option<String>,
}

#[derive(sqlx::FromRow)]
struct HistoryRow {
    seq: i64,
    device_id: Uuid,
    visited_at: OffsetDateTime,
    payload: Vec<u8>,
}

async fn pull_history(
    State(state): State<SharedState>,
    user: KeyedUser,
    Query(q): Query<PullQuery>,
) -> ApiResult<Response> {
    let max = state.cfg.limits.max_history_batch;
    let limit = q
        .limit
        .unwrap_or(DEFAULT_HISTORY_LIMIT.min(max))
        .clamp(1, max);
    let exclude = match q.exclude_device.as_deref() {
        None | Some("") => None,
        Some("me") => Some(user.user.device_id),
        Some(id) => Some(
            Uuid::parse_str(id)
                .map_err(|_| ApiError::bad_request("exclude_device must be `me` or a uuid"))?,
        ),
    };
    let user_id = user.user.user_id;
    let rows = sqlx::query_as::<_, HistoryRow>(
        "SELECT seq, device_id, visited_at, payload FROM history
         WHERE user_id = $1 AND seq > $2 AND ($3::uuid IS NULL OR device_id <> $3)
         ORDER BY seq
         LIMIT $4",
    )
    .bind(user_id)
    .bind(q.since.max(0))
    .bind(exclude)
    .bind(i64::try_from(limit + 1).unwrap_or(i64::MAX))
    .fetch_all(&state.db)
    .await?;

    let more = rows.len() > limit;
    let aad = Crypto::user_aad(user_id, HISTORY_AAD_DOMAIN);
    // Payloads are the entry JSON exactly as validated on insert; splice them in verbatim
    // instead of re-parsing every entry.
    let mut out: Vec<u8> =
        Vec::with_capacity(64 + rows.iter().map(|r| r.payload.len() + 96).sum::<usize>());
    out.extend_from_slice(b"{\"entries\":[");
    let mut next = q.since.max(0);
    for (i, row) in rows.into_iter().take(limit).enumerate() {
        let plain = Crypto::open(&user.data_key, &aad, &row.payload)?;
        if i > 0 {
            out.push(b',');
        }
        let visited = row
            .visited_at
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(anyhow::Error::from)?;
        write!(
            out,
            "{{\"seq\":{},\"device_id\":\"{}\",\"visited_at\":\"{}\",\"payload\":",
            row.seq, row.device_id, visited
        )
        .map_err(anyhow::Error::from)?;
        out.extend_from_slice(&plain);
        out.push(b'}');
        next = row.seq;
    }
    write!(out, "],\"next\":{next},\"more\":{more}}}").map_err(anyhow::Error::from)?;
    Ok((
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        Body::from(out),
    )
        .into_response())
}

// ---------------------------------------------------------------------------------------------
// History delete
//
// PRIVACY RULE: the server never inspects history to decide what to delete. Payloads are never
// opened here — no `Crypto::open`, no data key (the route takes `AuthUser`, not `KeyedUser`).
// Deletes select rows only by metadata the server already stores in the clear: the owner's
// `user_id`, the row `seq`, `visited_at` and `device_id`. Choosing individual entries (a site,
// a page, a search) is the *user's* decision and happens on the client: it pulls its history
// (`GET /sync/history` returns every row's `seq`), filters locally, and sends the chosen `seqs`.
// An admin can delete wholesale by the same metadata (time window, device), never by content.
// Keep every code path in this section free of payload reads.

/// Most `seqs` in one `DELETE /sync/history` body (`400` above it).
pub const MAX_HISTORY_DELETE_SEQS: usize = 5000;
/// Body cap for `DELETE /sync/history` (`413` above it): [`MAX_HISTORY_DELETE_SEQS`] seqs of
/// up to 20 characters each plus separators, with room for whitespace.
pub const HISTORY_DELETE_BODY_LIMIT: usize = 256 * 1024;
/// `DELETE /sync/history` calls per user per minute (GCRA: bursts of this many, then one every
/// `60 / n` s).
pub const HISTORY_DELETES_PER_MINUTE: u32 = 20;

/// A metadata-only history filter. Every set field narrows the selection; an empty filter
/// selects all of the user's history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistoryFilter {
    /// Only visits with `visited_at >= since`.
    pub since: Option<OffsetDateTime>,
    /// Only visits with `visited_at < until`.
    pub until: Option<OffsetDateTime>,
    /// Only rows pushed by this device.
    pub device: Option<Uuid>,
}

impl HistoryFilter {
    /// A validated filter: times in UTC, `since` earlier than `until`.
    pub fn new(
        since: Option<OffsetDateTime>,
        until: Option<OffsetDateTime>,
        device: Option<Uuid>,
    ) -> ApiResult<Self> {
        let utc = |t: OffsetDateTime| t.to_offset(time::UtcOffset::UTC);
        let (since, until) = (since.map(utc), until.map(utc));
        if let (Some(s), Some(u)) = (since, until) {
            if s >= u {
                return Err(ApiError::bad_request("since must be earlier than until"));
            }
        }
        Ok(Self {
            since,
            until,
            device,
        })
    }

    /// No field set (selects everything).
    pub fn is_empty(&self) -> bool {
        self.since.is_none() && self.until.is_none() && self.device.is_none()
    }

    /// The `DELETE /sync/history` query: `since`, `until` and `device`, at most once each;
    /// `token` (the `?token=` session form) is ignored; anything else is a `400`, so a misspelt
    /// filter never widens a delete to everything.
    fn from_query(pairs: &[(String, String)]) -> ApiResult<Self> {
        fn once<T>(slot: &mut Option<T>, name: &str, value: T) -> ApiResult<()> {
            if slot.replace(value).is_some() {
                return Err(ApiError::bad_request(format!(
                    "{name} may be given at most once"
                )));
            }
            Ok(())
        }
        let (mut since, mut until, mut device) = (None, None, None);
        for (key, value) in pairs {
            match key.as_str() {
                "since" => once(&mut since, "since", parse_history_time("since", value)?)?,
                "until" => once(&mut until, "until", parse_history_time("until", value)?)?,
                "device" => {
                    let id = Uuid::parse_str(value.trim())
                        .map_err(|_| ApiError::bad_request("device must be a device uuid"))?;
                    once(&mut device, "device", id)?;
                }
                "token" => {}
                other => {
                    return Err(ApiError::bad_request(format!(
                        "unknown query parameter `{}`; expected since, until or device \
                         (pick individual entries with a {{\"seqs\": [...]}} body)",
                        other.chars().take(64).collect::<String>()
                    )))
                }
            }
        }
        Self::new(since, until, device)
    }
}

/// An RFC 3339 timestamp from a query or CLI value, in UTC. An unencoded `+` offset arrives as
/// a space (form decoding turns `+` into a space); it is put back.
pub fn parse_history_time(name: &str, raw: &str) -> ApiResult<OffsetDateTime> {
    use time::format_description::well_known::Rfc3339;
    let raw = raw.trim();
    OffsetDateTime::parse(raw, &Rfc3339)
        .or_else(|_| OffsetDateTime::parse(&raw.replace(' ', "+"), &Rfc3339))
        .map(|t| t.to_offset(time::UtcOffset::UTC))
        .map_err(|_| {
            ApiError::bad_request(format!(
                "{name} must be an RFC 3339 timestamp, e.g. 2026-10-01T12:00:00Z"
            ))
        })
}

/// Which history rows a delete removes — always by metadata, never by payload content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistorySelection {
    /// Exactly these rows, which the user picked on their client (sorted, de-duplicated).
    /// Seqs that do not exist or belong to someone else are ignored.
    Seqs(Vec<i64>),
    /// Every row matching the filter (all of the user's history when it is empty).
    Filter(HistoryFilter),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteBody {
    seqs: Vec<i64>,
}

/// The optional `DELETE /sync/history` body: none (empty or whitespace) → `None`; otherwise
/// exactly `{"seqs": [<i64>, …]}` with at most [`MAX_HISTORY_DELETE_SEQS`] values (any other
/// shape, field or `null` is a `400`, so a malformed body never turns into "delete
/// everything"). Returned sorted and de-duplicated.
fn parse_delete_body(bytes: &[u8]) -> ApiResult<Option<Vec<i64>>> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let body: DeleteBody = serde_json::from_slice(bytes).map_err(|e| {
        ApiError::bad_request(format!(
            "invalid JSON body: {e}; expected {{\"seqs\": [<seq>, ...]}} or no body"
        ))
    })?;
    if body.seqs.len() > MAX_HISTORY_DELETE_SEQS {
        return Err(ApiError::bad_request(format!(
            "too many seqs: {} in one request; at most {MAX_HISTORY_DELETE_SEQS}",
            body.seqs.len()
        )));
    }
    let mut seqs = body.seqs;
    seqs.sort_unstable();
    seqs.dedup();
    Ok(Some(seqs))
}

/// Delete `user_id`'s history rows chosen by `selection` (with `dry_run`, only count them).
/// One statement, scoped to `user_id`, selecting by `seq` / `visited_at` / `device_id` only —
/// payloads are never read (see the privacy rule above). Deleting never renumbers anything, so
/// pull cursors stay valid.
pub async fn delete_history(
    db: &sqlx::PgPool,
    user_id: Uuid,
    selection: &HistorySelection,
    dry_run: bool,
) -> ApiResult<u64> {
    let count = |n: i64| u64::try_from(n).unwrap_or(0);
    Ok(match selection {
        HistorySelection::Seqs(seqs) if seqs.is_empty() => 0,
        HistorySelection::Seqs(seqs) if dry_run => count(
            sqlx::query_scalar("SELECT count(*) FROM history WHERE user_id = $1 AND seq = ANY($2)")
                .bind(user_id)
                .bind(seqs)
                .fetch_one(db)
                .await?,
        ),
        HistorySelection::Seqs(seqs) => {
            sqlx::query("DELETE FROM history WHERE user_id = $1 AND seq = ANY($2)")
                .bind(user_id)
                .bind(seqs)
                .execute(db)
                .await?
                .rows_affected()
        }
        HistorySelection::Filter(f) if dry_run => count(
            sqlx::query_scalar(
                "SELECT count(*) FROM history
                 WHERE user_id = $1
                   AND ($2::timestamptz IS NULL OR visited_at >= $2)
                   AND ($3::timestamptz IS NULL OR visited_at < $3)
                   AND ($4::uuid IS NULL OR device_id = $4)",
            )
            .bind(user_id)
            .bind(f.since)
            .bind(f.until)
            .bind(f.device)
            .fetch_one(db)
            .await?,
        ),
        HistorySelection::Filter(f) => sqlx::query(
            "DELETE FROM history
                 WHERE user_id = $1
                   AND ($2::timestamptz IS NULL OR visited_at >= $2)
                   AND ($3::timestamptz IS NULL OR visited_at < $3)
                   AND ($4::uuid IS NULL OR device_id = $4)",
        )
        .bind(user_id)
        .bind(f.since)
        .bind(f.until)
        .bind(f.device)
        .execute(db)
        .await?
        .rows_affected(),
    })
}

struct DeleteLimiter {
    limiter: DefaultKeyedRateLimiter<Uuid>,
    calls: AtomicU64,
}

static DELETE_LIMITER: LazyLock<DeleteLimiter> = LazyLock::new(|| DeleteLimiter {
    limiter: RateLimiter::keyed(Quota::per_minute(
        NonZeroU32::new(HISTORY_DELETES_PER_MINUTE).expect("non-zero"),
    )),
    calls: AtomicU64::new(0),
});

/// `true` if `user` may delete history now (process-wide, per user).
fn allow_delete(user: Uuid) -> bool {
    let l = &*DELETE_LIMITER;
    if l.calls.fetch_add(1, Ordering::Relaxed) % 4096 == 4095 {
        l.limiter.retain_recent();
        l.limiter.shrink_to_fit();
    }
    l.limiter.check_key(&user).is_ok()
}

/// `DELETE /sync/history[?since=&until=&device=]` with an optional `{"seqs": [...]}` body →
/// `{"deleted": n}`, then a `history_deleted` event on the user's streams. Seqs and query
/// filters are mutually exclusive; neither = all of the caller's history. Takes `AuthUser`
/// (no data key): nothing here can open a payload.
async fn delete_history_route(
    State(state): State<SharedState>,
    user: AuthUser,
    QueryParams(pairs): QueryParams<Vec<(String, String)>>,
    req: Request,
) -> ApiResult<Json<serde_json::Value>> {
    let filter = HistoryFilter::from_query(&pairs)?;
    drop(pairs);
    check_content_length(&req, HISTORY_DELETE_BODY_LIMIT)?;
    let bytes = read_body(req.into_body(), HISTORY_DELETE_BODY_LIMIT).await?;
    let seqs = parse_delete_body(&bytes)?;
    drop(bytes);
    let selection = match seqs {
        Some(_) if !filter.is_empty() => {
            return Err(ApiError::bad_request(
                "give either seqs in the body or since/until/device in the query, not both",
            ))
        }
        Some(seqs) => HistorySelection::Seqs(seqs),
        None => HistorySelection::Filter(filter),
    };
    let user_id = user.user_id;
    if !allow_delete(user_id) {
        metrics::counter!("history_delete_rate_limited_total").increment(1);
        tracing::warn!(%user_id, "history delete rate limit exceeded");
        return Err(ApiError::RateLimited);
    }
    let device_id = user.device_id;
    // Finish even if the client goes away mid-delete, so the event always goes out with what
    // was actually removed.
    let task = tokio::spawn(
        async move {
            let deleted = delete_history(&state.db, user_id, &selection, false).await?;
            metrics::counter!("history_deleted").increment(deleted);
            let seqs = match &selection {
                HistorySelection::Seqs(seqs) => Some(u64::try_from(seqs.len()).unwrap_or(0)),
                HistorySelection::Filter(_) => None,
            };
            let f = match selection {
                HistorySelection::Filter(f) => f,
                HistorySelection::Seqs(_) => HistoryFilter::default(),
            };
            tracing::info!(
                deleted,
                seqs,
                since = f.since.is_some(),
                until = f.until.is_some(),
                device = f.device.is_some(),
                "history deleted"
            );
            state.events.publish(
                user_id,
                Event::HistoryDeleted {
                    deleted,
                    since: f.since,
                    until: f.until,
                    device: f.device,
                    seqs,
                    device_id: Some(device_id),
                },
            );
            Ok::<_, ApiError>(deleted)
        }
        .in_current_span(),
    );
    let deleted = task.await??;
    Ok(Json(json!({ "deleted": deleted })))
}

// ---------------------------------------------------------------------------------------------
// Events (SSE)

struct SubscriberGauge;

impl SubscriberGauge {
    fn new() -> Self {
        metrics::gauge!("sse_subscribers").increment(1.0);
        Self
    }
}

impl Drop for SubscriberGauge {
    fn drop(&mut self) {
        metrics::gauge!("sse_subscribers").decrement(1.0);
    }
}

async fn session_alive(db: &sqlx::PgPool, session_id: Uuid) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM sessions WHERE id = $1 AND expires_at > now())",
    )
    .bind(session_id)
    .fetch_one(db)
    .await
    // Fail open on transient DB errors; the next check (or reconnect) settles it.
    .unwrap_or(true)
}

// `state`/`user` are moved into the stream; clippy cannot see through `stream!`.
#[allow(clippy::needless_pass_by_value)]
fn sse_stream(
    state: SharedState,
    user: AuthUser,
) -> impl Stream<Item = Result<SseEvent, Infallible>> {
    let mut rx = state.events.subscribe(user.user_id);
    let mut shutdown = crate::shutdown::subscribe();
    async_stream::stream! {
        let _gauge = SubscriberGauge::new();
        yield Ok(SseEvent::default().event("ready").data(
            json!({ "device_id": user.device_id }).to_string(),
        ));
        let mut revalidate = tokio::time::interval(SSE_REVALIDATE);
        revalidate.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        revalidate.tick().await;
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Ok(ev) => {
                        let data = serde_json::to_string(&ev).unwrap_or_default();
                        yield Ok(SseEvent::default().event(ev.name()).data(data));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        yield Ok(SseEvent::default().event("resync").data(
                            json!({ "skipped": skipped }).to_string(),
                        ));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                _ = revalidate.tick() => {
                    if !session_alive(&state.db, user.session_id).await {
                        break;
                    }
                }
                () = crate::shutdown::wait(&mut shutdown) => break,
            }
        }
    }
}

async fn events(State(state): State<SharedState>, user: AuthUser) -> impl IntoResponse {
    let sse = Sse::new(sse_stream(state, user))
        .keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE).text("keepalive"));
    (
        [
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            (
                header::HeaderName::from_static("x-accel-buffering"),
                HeaderValue::from_static("no"),
            ),
        ],
        sse,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains() {
        assert_eq!(Domain::parse("spaces").unwrap(), Domain::Shared("spaces"));
        let id = Uuid::now_v7();
        let upper = format!("tabs:{}", id.to_string().to_uppercase());
        assert_eq!(Domain::parse(&upper).unwrap(), Domain::Tabs(id));
        assert_eq!(Domain::parse(&upper).unwrap().as_str(), tabs_domain(id));
        assert!(Domain::parse("passwords").is_err());
        assert!(Domain::parse("tabs:nope").is_err());
        assert!(Domain::parse("").is_err());
    }

    #[test]
    fn visited_at_formats() {
        let now = OffsetDateTime::now_utc();
        let v = |j: serde_json::Value| parse_visited_at(Some(&j), now);
        assert_eq!(
            v(json!("2026-10-01T12:00:00Z")).unwrap().unix_timestamp(),
            1_790_856_000
        );
        assert_eq!(
            v(json!(1_790_856_000)).unwrap().unix_timestamp(),
            1_790_856_000
        );
        assert_eq!(
            v(json!(1_790_856_000_123_i64)).unwrap().unix_timestamp(),
            1_790_856_000
        );
        assert_eq!(parse_visited_at(None, now).unwrap(), now);
        assert!(v(json!("yesterday")).is_err());
        assert!(v(json!([1])).is_err());
    }

    #[test]
    fn entry_checks_are_per_entry() {
        let now = OffsetDateTime::now_utc();
        let check = |text: &str| check_history_entry(7, text, 64, now);
        assert_eq!(
            check(r#"{"url":"u","visited_at":1790856000}"#)
                .unwrap()
                .unix_timestamp(),
            1_790_856_000
        );
        assert_eq!(check(r#"{"url":"u"}"#).unwrap(), now);
        let reason = |text: &str| {
            let r = check(text).unwrap_err();
            assert_eq!(r.index, 7);
            r.reason
        };
        assert_eq!(
            reason(&format!(r#"{{"url":"{}"}}"#, "x".repeat(64))),
            "too_large"
        );
        assert_eq!(reason("[1]"), "not_object");
        assert_eq!(reason(r#""str""#), "not_object");
        assert_eq!(
            reason(r#"{"visited_at":1,"visited_at":2}"#),
            "invalid",
            "duplicate field"
        );
        assert_eq!(reason(r#"{"visited_at":"soon"}"#), "bad_visited_at");
        assert_eq!(reason(r#"{"visited_at":1e300}"#), "bad_visited_at");
    }

    #[test]
    fn delete_query_parsing() {
        let q = |pairs: &[(&str, &str)]| {
            let pairs: Vec<(String, String)> = pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect();
            HistoryFilter::from_query(&pairs)
        };
        assert_eq!(q(&[]).unwrap(), HistoryFilter::default());
        assert!(q(&[]).unwrap().is_empty());
        assert!(q(&[("token", "ignored")]).unwrap().is_empty());
        let f = q(&[
            ("since", "2026-10-01T12:00:00Z"),
            // Form decoding turned an unencoded `+` into a space.
            ("until", "2026-10-01T14:00:01 02:00"),
            ("device", "00000000-0000-0000-0000-000000000001"),
            ("token", "ignored"),
        ])
        .unwrap();
        assert!(!f.is_empty());
        assert_eq!(f.since.unwrap().unix_timestamp(), 1_790_856_000);
        assert_eq!(f.until.unwrap().unix_timestamp(), 1_790_856_000 + 1);
        assert_eq!(f.until.unwrap().offset(), time::UtcOffset::UTC);
        assert_eq!(f.device, Some(Uuid::from_u128(1)));

        let err = |pairs: &[(&str, &str)]| match q(pairs) {
            Err(ApiError::BadRequest(msg)) => msg,
            other => panic!("{pairs:?}: {other:?}"),
        };
        // Content filters do not exist: the server never looks inside history to delete.
        assert!(err(&[("host", "x.com")]).contains("unknown query parameter `host`"));
        assert!(err(&[("url", "https://x.com")]).contains("unknown query parameter"));
        assert!(err(&[("seqs", "1,2")]).contains("unknown query parameter"));
        assert!(err(&[("since", "yesterday")]).contains("RFC 3339"));
        assert!(err(&[("until", "")]).contains("RFC 3339"));
        assert!(err(&[("since", "1790856000")]).contains("RFC 3339"));
        assert!(err(&[
            ("since", "2026-10-01T12:00:00Z"),
            ("since", "2026-10-01T13:00:00Z")
        ])
        .contains("at most once"));
        assert!(err(&[
            ("since", "2026-10-01T12:00:00Z"),
            ("until", "2026-10-01T12:00:00Z")
        ])
        .contains("earlier than until"));
        assert!(err(&[("device", "me")]).contains("uuid"));
    }

    /// Guard for the privacy rule: no code (comments aside) in the history-delete section may
    /// open a payload or take a data key.
    #[test]
    fn history_delete_never_opens_payloads() {
        let src = include_str!("sync.rs");
        let start = src.find("\n// History delete\n").expect("section start");
        let end = start
            + src[start..]
                .find("\n// Events (SSE)\n")
                .expect("section end");
        let code: Vec<&str> = src[start..end]
            .lines()
            .map(|l| l.split("//").next().unwrap_or_default())
            .collect();
        assert!(code.iter().any(|l| l.contains("async fn delete_history(")));
        for line in code {
            for banned in ["Crypto", "KeyedUser", "data_key", "open(", "payload"] {
                assert!(!line.contains(banned), "{banned} in: {line}");
            }
        }
    }

    #[test]
    fn delete_body_parsing() {
        let p = |s: &str| parse_delete_body(s.as_bytes());
        assert_eq!(p("").unwrap(), None);
        assert_eq!(p(" \n\t").unwrap(), None);
        assert_eq!(p(r#"{"seqs":[]}"#).unwrap(), Some(vec![]));
        assert_eq!(p(r#"{"seqs":[9,3,9,-1,3]}"#).unwrap(), Some(vec![-1, 3, 9]));
        let max: Vec<i64> = (1..=i64::try_from(MAX_HISTORY_DELETE_SEQS).unwrap()).collect();
        let body = json!({ "seqs": max }).to_string();
        assert!(body.len() < HISTORY_DELETE_BODY_LIMIT / 2);
        assert_eq!(p(&body).unwrap().unwrap().len(), MAX_HISTORY_DELETE_SEQS);
        // Anything else is a 400 — never "delete everything".
        for bad in [
            "{}",
            "null",
            "[]",
            "[1,2]",
            r#"{"seqs":null}"#,
            r#"{"seqs":"1,2"}"#,
            r#"{"seqs":[1.5]}"#,
            r#"{"seqs":["1"]}"#,
            r#"{"seqs":[99999999999999999999]}"#,
            r#"{"seq":[1]}"#,
            r#"{"seqs":[1],"host":"x.com"}"#,
            "not json",
        ] {
            assert!(
                matches!(p(bad), Err(ApiError::BadRequest(_))),
                "{bad}: {:?}",
                p(bad)
            );
        }
        let over: Vec<i64> = (0..=i64::try_from(MAX_HISTORY_DELETE_SEQS).unwrap()).collect();
        match p(&json!({ "seqs": over }).to_string()) {
            Err(ApiError::BadRequest(msg)) => assert!(msg.contains("too many seqs"), "{msg}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn body_limits() {
        assert!(doc_body_limit(8_000_000) >= 8_000_000 * 4 / 3);
        let l = crate::config::Limits::default();
        assert!(history_body_limit(&l) >= l.max_history_batch * l.max_history_entry_bytes);
    }
}
