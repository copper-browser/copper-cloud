//! Sync: last-writer-wins documents, append-only history, and the per-user SSE event stream.
//!
//! Every query is scoped by the caller's `user_id`; payloads are sealed with the user's data
//! key (AAD `"<user_id>:<domain>"`, history uses domain `history`).

use std::borrow::Cow;
use std::convert::Infallible;
use std::io::Write as _;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderValue};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::value::RawValue;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::{AuthUser, KeyedUser};
use crate::crypto::Crypto;
use crate::error::{ApiError, ApiResult};
use crate::events::Event;
use crate::extract::{check_content_length, read_body};
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
        .route("/sync/history", get(pull_history).post(push_history))
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

async fn push_history(
    State(state): State<SharedState>,
    user: KeyedUser,
    req: Request,
) -> ApiResult<Json<serde_json::Value>> {
    let limits = &state.cfg.limits;
    let limit = history_body_limit(limits);
    check_content_length(&req, limit)?;
    let bytes = read_body(req.into_body(), limit).await?;
    let body: HistoryPush<'_> = serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))?;
    if body.entries.len() > limits.max_history_batch {
        return Err(ApiError::bad_request(format!(
            "at most {} entries per request",
            limits.max_history_batch
        )));
    }
    let user_id = user.user.user_id;
    if body.entries.is_empty() {
        let seq: i64 =
            sqlx::query_scalar("SELECT COALESCE(max(seq), 0) FROM history WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&state.db)
                .await?;
        return Ok(Json(json!({ "seq": seq, "inserted": 0 })));
    }

    let aad = Crypto::user_aad(user_id, HISTORY_AAD_DOMAIN);
    let now = OffsetDateTime::now_utc();
    let mut visited = Vec::with_capacity(body.entries.len());
    let mut payloads = Vec::with_capacity(body.entries.len());
    for (i, raw) in body.entries.iter().enumerate() {
        let text = raw.get();
        if text.len() > limits.max_history_entry_bytes {
            return Err(ApiError::bad_request(format!(
                "entry {i} exceeds {} bytes",
                limits.max_history_entry_bytes
            )));
        }
        if !text.starts_with('{') {
            return Err(ApiError::bad_request(format!(
                "entry {i} must be a JSON object"
            )));
        }
        let probe: EntryProbe = serde_json::from_str(text)
            .map_err(|e| ApiError::bad_request(format!("entry {i}: {e}")))?;
        visited.push(parse_visited_at(probe.visited_at.as_ref(), now)?);
        payloads.push(Crypto::seal(&user.data_key, &aad, text.as_bytes()));
    }
    drop(bytes);

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
    Ok(Json(json!({ "seq": seq, "inserted": inserted })))
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
    fn body_limits() {
        assert!(doc_body_limit(8_000_000) >= 8_000_000 * 4 / 3);
        let l = crate::config::Limits::default();
        assert!(history_body_limit(&l) >= l.max_history_batch * l.max_history_entry_bytes);
    }
}
