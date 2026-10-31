//! Device agent relay: an agent (any MCP client) calls the tools of a person's Copper for iOS
//! through the cloud. Nothing listens on the phone: it dials out and holds an event stream while
//! Copper is on screen, and the cloud hands it each call.
//!
//! ```text
//! agent ── POST /v1/agent/mcp (cat_ token) ──▶ copper-cloud ── SSE "request" ──▶ device
//! agent ◀── JSON-RPC result (JSON or SSE) ─── copper-cloud ◀── POST frames "reply" ── device
//! ```
//!
//! * **Device side** (gate + session, like every `/v1` user route): `POST/GET /v1/me/links`,
//!   `GET/DELETE /v1/me/links/{id}`, the SSE mailbox `GET /v1/me/links/{id}/frames` and its
//!   upload `POST /v1/me/links/{id}/frames` (body up to [`FRAMES_BODY_LIMIT`]), and agent tokens
//!   `POST/GET /v1/me/agent-tokens`, `DELETE /v1/me/agent-tokens/{id}`. One link per
//!   (user, device); only the link's own device may open or feed its stream.
//! * **Agent side** (ungated, see [`is_agent_path`]): `Authorization: Bearer cat_…` — `cat_` + 32
//!   random bytes base64url, only SHA-256 stored — on `GET /v1/agent/status` and
//!   `POST /v1/agent/mcp` (MCP Streamable HTTP, stateless; [`mcp`]). 600 requests/minute per
//!   token.
//! * Which link is online and the calls in flight live in the in-memory [`Hub`] (one daemon per
//!   instance); links and tokens persist (`0016_device_agent.sql`).
//! * Logs: one line per agent call (token id, device id, tool, ms, status), never arguments or
//!   results. Token mints/revokes and link registrations/revokes are logged (like pairing codes,
//!   these user actions are not in the admin audit log).

pub mod hub;
pub(crate) mod mcp;

use std::collections::HashSet;
use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{json, Value};
use time::OffsetDateTime;
use tokio::sync::mpsc;
use uuid::Uuid;

pub use hub::Hub;
use hub::{Down, Reply};

use crate::auth::{self, AuthUser};
use crate::config::{Config, TlsMode};
use crate::error::{ApiError, ApiResult};
use crate::extract::{check_content_length, read_body, JsonBody, OptionalJsonBody};
use crate::ids;
use crate::state::SharedState;

/// Prefix of every agent token.
pub const AGENT_TOKEN_PREFIX: &str = "cat_";
/// The agent-side routes, outside the instance gate.
pub const AGENT_PATH: &str = "/v1/agent";
/// The MCP endpoint (built into `mcp_url`).
pub const MCP_PATH: &str = "/v1/agent/mcp";
/// `POST /v1/me/links/{id}/frames` body cap (replies may carry screenshots).
pub const FRAMES_BODY_LIMIT: usize = 32 * 1024 * 1024;
/// Most tools in a manifest.
pub const MAX_TOOLS: usize = 100;
/// Largest manifest (compact JSON).
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
/// Longest progress message forwarded (longer ones are cut).
pub const MAX_PROGRESS_BYTES: usize = 16 * 1024;
/// Most live agent tokens one user may hold.
pub const MAX_ACTIVE_TOKENS: i64 = 100;
/// Frames protocol version announced in `hello`.
pub const FRAMES_PROTOCOL: u32 = 1;
const MAX_LABEL_CHARS: usize = 100;
const MAX_TOOL_NAME_CHARS: usize = 128;
/// `POST /v1/me/links` body cap: room for a too-big manifest to get a clear 400.
const LINK_BODY_LIMIT: usize = 1024 * 1024;
const TOKEN_BODY_LIMIT: usize = 16 * 1024;

/// Whether `path` is an agent-side route (`/v1/agent`, `/v1/agent/…`), which carries its own
/// credential instead of the gate header. `/v1/agents/…` is not.
pub fn is_agent_path(path: &str) -> bool {
    path.strip_prefix(AGENT_PATH)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// A fresh agent token: `cat_` + 43 base64url chars (32 random bytes).
pub fn new_agent_token() -> String {
    format!("{AGENT_TOKEN_PREFIX}{}", ids::random_token())
}

/// Shape check before touching the database.
pub fn plausible_agent_token(s: &str) -> bool {
    s.strip_prefix(AGENT_TOKEN_PREFIX)
        .is_some_and(ids::is_token_shape)
}

/// `https://HOST[:PORT]/v1/agent/mcp` for agents: the ACME domain when there is one (agents
/// need a publicly trusted certificate), else `public_url`.
pub fn mcp_url(cfg: &Config) -> String {
    let host = match (&cfg.tls.domain, cfg.tls.mode) {
        (Some(domain), TlsMode::Acme) => domain.clone(),
        _ => cfg.public_host().to_owned(),
    };
    crate::link::LinkCode {
        host,
        port: cfg.public_port(),
        instance_key: String::new(),
        fingerprint: None,
    }
    .https_url(MCP_PATH)
}

pub(crate) fn routes() -> Router<SharedState> {
    Router::new()
        .route("/me/links", get(list_links).post(register_link))
        .route("/me/links/{id}", get(get_link).delete(revoke_link))
        .route("/me/links/{id}/frames", get(open_frames).post(post_frames))
        .route("/me/agent-tokens", get(list_tokens).post(mint_token))
        .route("/me/agent-tokens/{id}", delete(revoke_token))
        .route("/agent/status", get(mcp::status))
        .route(
            "/agent/mcp",
            post(mcp::mcp).get(mcp::no_stream).delete(mcp::no_stream),
        )
}

/// An SSE response with the relay's headers: `Cache-Control: no-store` and
/// `X-Accel-Buffering: no` (so a proxy passes each event through at once).
pub(crate) fn sse_response<S>(stream: S, keepalive: Option<Duration>) -> Response
where
    S: Stream<Item = Result<SseEvent, Infallible>> + Send + 'static,
{
    let sse = Sse::new(stream);
    let mut resp = match keepalive {
        Some(every) => sse
            .keep_alive(KeepAlive::new().interval(every).text("keepalive"))
            .into_response(),
        None => sse.into_response(),
    };
    let headers = resp.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    resp
}

// ---------------------------------------------------------------------------------------------
// Links

#[derive(sqlx::FromRow)]
struct LinkRow {
    id: Uuid,
    device_id: Uuid,
    device_name: String,
    label: String,
    revoked: bool,
    created_at: OffsetDateTime,
    last_seen_at: Option<OffsetDateTime>,
    tools: i64,
}

/// The link object every link route returns (and `hello` carries).
#[derive(Debug, Serialize)]
pub struct LinkView {
    pub id: Uuid,
    pub device_id: Uuid,
    pub device_name: String,
    pub label: String,
    pub online: bool,
    pub revoked: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_seen_at: Option<OffsetDateTime>,
    /// How many tools the registered manifest has.
    pub tools: i64,
}

const LINK_SQL: &str =
    "SELECT l.id, l.device_id, COALESCE(NULLIF(d.name, ''), l.label) AS device_name,
       l.label, l.revoked_at IS NOT NULL AS revoked, l.created_at, l.last_seen_at,
       jsonb_array_length(l.tools)::bigint AS tools
FROM device_links l
LEFT JOIN devices d ON d.user_id = l.user_id AND d.id = l.device_id";

impl LinkRow {
    fn view(self, state: &SharedState) -> LinkView {
        let online = !self.revoked && state.device_agent.is_online(self.id);
        LinkView {
            id: self.id,
            device_id: self.device_id,
            device_name: self.device_name,
            label: self.label,
            online,
            revoked: self.revoked,
            created_at: self.created_at,
            last_seen_at: self.last_seen_at,
            tools: self.tools,
        }
    }
}

async fn load_link(state: &SharedState, user_id: Uuid, id: Uuid) -> ApiResult<Option<LinkRow>> {
    Ok(
        sqlx::query_as::<_, LinkRow>(&format!("{LINK_SQL} WHERE l.user_id = $1 AND l.id = $2"))
            .bind(user_id)
            .bind(id)
            .fetch_optional(&state.db)
            .await?,
    )
}

/// The caller's link `id`, which must be this device's own (`404` for another user's link,
/// `403` for another device's).
async fn own_link(state: &SharedState, user: &AuthUser, id: Uuid) -> ApiResult<LinkRow> {
    let row = load_link(state, user.user_id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if row.device_id != user.device_id {
        return Err(ApiError::Forbidden);
    }
    Ok(row)
}

#[derive(Deserialize)]
struct RegisterRequest {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    client_version: Option<String>,
    #[serde(default)]
    tools: Option<Value>,
}

/// A tool manifest as registered: a JSON array of ≤ [`MAX_TOOLS`] objects, each with a unique,
/// non-empty `name`, ≤ [`MAX_MANIFEST_BYTES`] as compact JSON.
pub fn validate_manifest(tools: &Value) -> ApiResult<()> {
    let Value::Array(items) = tools else {
        return Err(ApiError::bad_request(
            "tools must be an array of tool objects ({name, description, inputSchema})",
        ));
    };
    if items.len() > MAX_TOOLS {
        return Err(ApiError::bad_request(format!(
            "too many tools ({}); at most {MAX_TOOLS}",
            items.len()
        )));
    }
    let mut names = HashSet::with_capacity(items.len());
    for (i, tool) in items.iter().enumerate() {
        let name = tool
            .as_object()
            .and_then(|o| o.get("name"))
            .and_then(Value::as_str)
            .filter(|n| !n.trim().is_empty() && n.chars().count() <= MAX_TOOL_NAME_CHARS)
            .ok_or_else(|| {
                ApiError::bad_request(format!(
                    "tools[{i}] must be an object with a non-empty name \
                     (at most {MAX_TOOL_NAME_CHARS} characters)"
                ))
            })?;
        if !names.insert(name) {
            return Err(ApiError::bad_request(format!(
                "tools[{i}]: duplicate tool name \"{name}\""
            )));
        }
    }
    let bytes = serde_json::to_vec(tools)
        .map_err(anyhow::Error::from)?
        .len();
    if bytes > MAX_MANIFEST_BYTES {
        return Err(ApiError::bad_request(format!(
            "the tool manifest is {bytes} bytes; at most {MAX_MANIFEST_BYTES} (256 KiB)"
        )));
    }
    Ok(())
}

/// `POST /v1/me/links`: register (or update) this device's link. Idempotent per device: same
/// id every time, a revoked link comes back un-revoked. Omitted fields keep their stored value
/// (a new link: label = the device name, no tools).
async fn register_link(
    State(state): State<SharedState>,
    user: AuthUser,
    JsonBody(req): JsonBody<RegisterRequest, LINK_BODY_LIMIT>,
) -> ApiResult<Json<LinkView>> {
    if let Some(tools) = &req.tools {
        validate_manifest(tools)?;
    }
    let label = auth::clean_name(req.label.as_deref(), MAX_LABEL_CHARS);
    let client_version = auth::clean_name(req.client_version.as_deref(), 64);
    let (id, inserted): (Uuid, bool) = sqlx::query_as(
        "INSERT INTO device_links (id, user_id, device_id, label, client_version, tools)
         VALUES ($1, $2, $3,
                 COALESCE($4, (SELECT NULLIF(name, '') FROM devices WHERE user_id = $2 AND id = $3),
                          'Copper'),
                 $5, COALESCE($6, '[]'::jsonb))
         ON CONFLICT (user_id, device_id) DO UPDATE SET
             label = COALESCE($4, device_links.label),
             client_version = COALESCE($5, device_links.client_version),
             tools = COALESCE($6, device_links.tools),
             revoked_at = NULL
         RETURNING id, (xmax = 0) AS inserted",
    )
    .bind(ids::uuid_v7())
    .bind(user.user_id)
    .bind(user.device_id)
    .bind(label)
    .bind(client_version)
    .bind(req.tools.map(sqlx::types::Json))
    .fetch_one(&state.db)
    .await?;
    let row = load_link(&state, user.user_id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tracing::info!(
        user_id = %user.user_id,
        device_id = %user.device_id,
        link_id = %id,
        tools = row.tools,
        inserted,
        "device link registered"
    );
    Ok(Json(row.view(&state)))
}

/// `GET /v1/me/links`: the caller's live (not revoked) links, oldest first.
async fn list_links(
    State(state): State<SharedState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<LinkView>>> {
    let rows = sqlx::query_as::<_, LinkRow>(&format!(
        "{LINK_SQL} WHERE l.user_id = $1 AND l.revoked_at IS NULL ORDER BY l.created_at, l.id"
    ))
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.into_iter().map(|r| r.view(&state)).collect()))
}

/// `GET /v1/me/links/{id}` (revoked included).
async fn get_link(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LinkView>> {
    let row = load_link(&state, user.user_id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(row.view(&state)))
}

/// `DELETE /v1/me/links/{id}`: turn the link off (any of the user's devices may). Closes its
/// stream with `revoked` and fails its calls in flight; agent calls answer "turned off" until
/// the device registers again. Idempotent.
async fn revoke_link(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let revoked: Option<Uuid> = sqlx::query_scalar(
        "UPDATE device_links SET revoked_at = COALESCE(revoked_at, now())
         WHERE user_id = $1 AND id = $2 RETURNING id",
    )
    .bind(user.user_id)
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    if revoked.is_none() {
        return Err(ApiError::NotFound);
    }
    state.device_agent.revoke_link(id);
    tracing::info!(user_id = %user.user_id, link_id = %id, by_device = %user.device_id, "device link revoked");
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------------------------
// Frames: the device's SSE mailbox and its uploads

/// Detaches the stream from the hub when the response body is dropped (client gone, superseded,
/// revoked, shutdown) and records when the device was last seen.
struct StreamGuard {
    state: SharedState,
    link: Uuid,
    stream: u64,
}

impl StreamGuard {
    fn new(state: SharedState, link: Uuid, stream: u64) -> Self {
        metrics::gauge!("device_agent_streams").increment(1.0);
        Self {
            state,
            link,
            stream,
        }
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        metrics::gauge!("device_agent_streams").decrement(1.0);
        if !self.state.device_agent.detach(self.link, self.stream) {
            return;
        }
        tracing::info!(link_id = %self.link, "device frames stream closed");
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let db = self.state.db.clone();
            let link = self.link;
            rt.spawn(async move {
                let _ = sqlx::query("UPDATE device_links SET last_seen_at = now() WHERE id = $1")
                    .bind(link)
                    .execute(&db)
                    .await;
            });
        }
    }
}

fn sse_event(name: &str, data: impl AsRef<str>) -> SseEvent {
    SseEvent::default().event(name).data(data)
}

// `state` is moved into the stream; clippy cannot see through `stream!`.
#[allow(clippy::needless_pass_by_value)]
fn frames_stream(
    state: SharedState,
    session_id: Uuid,
    hello: String,
    attached: Option<(StreamGuard, mpsc::UnboundedReceiver<Down>)>,
) -> impl Stream<Item = Result<SseEvent, Infallible>> {
    let ping_every = state.cfg.device_agent.device_ping;
    let mut shutdown = crate::shutdown::subscribe();
    async_stream::stream! {
        yield Ok(sse_event("hello", hello));
        let Some((_guard, mut rx)) = attached else {
            // A revoked link: say so, then close (the device registers again to turn it on).
            yield Ok(sse_event("revoked", "{}"));
            return;
        };
        let start = tokio::time::Instant::now();
        let mut ping = tokio::time::interval_at(start + ping_every, ping_every);
        let revalidate_every = crate::sync::SSE_REVALIDATE;
        let mut revalidate = tokio::time::interval_at(start + revalidate_every, revalidate_every);
        loop {
            tokio::select! {
                down = rx.recv() => match down {
                    Some(Down::Request(frame)) => {
                        yield Ok(sse_event("request", frame));
                    }
                    Some(Down::Cancel(id)) => {
                        yield Ok(sse_event("cancel", json!({ "id": id }).to_string()));
                    }
                    Some(Down::Superseded) => {
                        yield Ok(sse_event("superseded", "{}"));
                        break;
                    }
                    Some(Down::Revoked) => {
                        yield Ok(sse_event("revoked", "{}"));
                        break;
                    }
                    None => break,
                },
                _ = ping.tick() => {
                    yield Ok(sse_event("ping", "{}"));
                }
                _ = revalidate.tick() => {
                    if !crate::sync::session_alive(&state.db, session_id).await {
                        break;
                    }
                }
                () = crate::shutdown::wait(&mut shutdown) => break,
            }
        }
    }
}

/// `GET /v1/me/links/{id}/frames`: the device's mailbox. Only the link's own device may open it.
/// A new stream supersedes the previous one; a revoked link gets `hello` then `revoked`.
async fn open_frames(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let mut row = own_link(&state, &user, id).await?;
    let attached = if row.revoked {
        None
    } else {
        let (stream, rx) = state.device_agent.attach(id);
        let guard = StreamGuard::new(state.clone(), id, stream);
        // Re-checked after attaching: a revoke that committed since `own_link` found no stream
        // to close, so this one must not stay attached.
        let live = sqlx::query(
            "UPDATE device_links SET last_seen_at = now() WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(id)
        .execute(&state.db)
        .await?
        .rows_affected()
            > 0;
        if live {
            tracing::info!(user_id = %user.user_id, link_id = %id, stream, "device frames stream opened");
            Some((guard, rx))
        } else {
            row.revoked = true;
            None
        }
    };
    let mut view = row.view(&state);
    if attached.is_some() {
        view.last_seen_at = Some(OffsetDateTime::now_utc());
    }
    let hello = json!({ "link": view, "protocol": FRAMES_PROTOCOL }).to_string();
    Ok(sse_response(
        frames_stream(state, user.session_id, hello, attached),
        None,
    ))
}

#[derive(Deserialize)]
struct FramesBody {
    frames: Vec<Box<RawValue>>,
}

#[derive(Deserialize)]
struct FrameIn {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    result: Option<Box<RawValue>>,
    #[serde(default)]
    error: Option<FrameError>,
    #[serde(default)]
    seq: Option<i64>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    level: Option<String>,
}

#[derive(Deserialize)]
struct FrameError {
    #[serde(default)]
    code: Option<Value>,
    #[serde(default)]
    message: Option<String>,
}

/// `s` cut to at most `max` bytes on a character boundary.
fn truncate_utf8(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

/// Apply one uploaded frame; `false` if it is malformed or of an unknown type (skipped).
fn handle_frame(state: &SharedState, link: Uuid, raw: &RawValue) -> bool {
    let Ok(frame) = serde_json::from_str::<FrameIn>(raw.get()) else {
        return false;
    };
    let hub = &state.device_agent;
    match frame.kind.as_str() {
        "reply" => {
            let Some(id) = frame.id else { return false };
            let reply = match (frame.result, frame.error) {
                (Some(result), _) => Reply::Result(result),
                (None, Some(err)) => Reply::Error(
                    err.message
                        .filter(|m| !m.trim().is_empty())
                        .or_else(|| err.code.map(|c| format!("Copper reported an error ({c})")))
                        .unwrap_or_else(|| "Copper reported an error".to_owned()),
                ),
                (None, None) => Reply::Error("Copper sent an empty reply".to_owned()),
            };
            hub.reply(link, &id, reply);
            true
        }
        "progress" => {
            let Some(id) = frame.id else { return false };
            let message = truncate_utf8(frame.message.unwrap_or_default(), MAX_PROGRESS_BYTES);
            hub.progress(link, &id, frame.seq, message);
            true
        }
        // Every frames POST counts as being heard from (see `post_frames`).
        "heartbeat" => true,
        "log" => {
            tracing::debug!(
                link_id = %link,
                level = frame.level.as_deref().unwrap_or("info"),
                message = frame.message.as_deref().unwrap_or_default(),
                "device log"
            );
            true
        }
        _ => false,
    }
}

/// `POST /v1/me/links/{id}/frames`: `{"frames": [...]}` from the link's own device (replies,
/// progress, heartbeats, logs). `200 {"ok": true, "accepted": n}` — `n` well-formed frames of
/// a known type (a reply for an unknown or finished request is still accepted, and ignored).
async fn post_frames(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    req: Request,
) -> ApiResult<Json<Value>> {
    check_content_length(&req, FRAMES_BODY_LIMIT)?;
    own_link(&state, &user, id).await?;
    let bytes = read_body(req.into_body(), FRAMES_BODY_LIMIT).await?;
    let body: FramesBody = serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::bad_request(format!("body must be {{\"frames\": [...]}}: {e}")))?;
    drop(bytes);
    state.device_agent.heard(id);
    let accepted = body
        .frames
        .iter()
        .filter(|raw| handle_frame(&state, id, raw))
        .count();
    sqlx::query(
        "UPDATE device_links SET last_seen_at = now()
         WHERE id = $1 AND (last_seen_at IS NULL OR last_seen_at < now() - interval '30 seconds')",
    )
    .bind(id)
    .execute(&state.db)
    .await?;
    Ok(Json(json!({ "ok": true, "accepted": accepted })))
}

// ---------------------------------------------------------------------------------------------
// Agent tokens

#[derive(Deserialize, Default)]
struct MintRequest {
    #[serde(default)]
    label: Option<String>,
}

#[derive(Serialize)]
struct MintedToken {
    id: Uuid,
    token: String,
    label: String,
    device_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    mcp_url: String,
}

#[derive(Serialize, sqlx::FromRow)]
struct TokenView {
    id: Uuid,
    label: String,
    device_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    last_used_at: Option<OffsetDateTime>,
}

/// `POST /v1/me/agent-tokens`: mint a `cat_` token for one agent, bound to the calling user
/// and device. The token is in this response only.
async fn mint_token(
    State(state): State<SharedState>,
    user: AuthUser,
    OptionalJsonBody(req): OptionalJsonBody<MintRequest, TOKEN_BODY_LIMIT>,
) -> ApiResult<(StatusCode, Json<MintedToken>)> {
    let label =
        auth::clean_name(req.label.as_deref(), MAX_LABEL_CHARS).unwrap_or_else(|| "Agent".into());
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent_tokens WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user.user_id)
    .fetch_one(&state.db)
    .await?;
    if active >= MAX_ACTIVE_TOKENS {
        return Err(ApiError::bad_request(format!(
            "too many agent tokens (max {MAX_ACTIVE_TOKENS}); revoke some first"
        )));
    }
    let token = new_agent_token();
    let id = ids::uuid_v7();
    let created_at: OffsetDateTime = sqlx::query_scalar(
        "INSERT INTO agent_tokens (id, user_id, device_id, label, token_sha256)
         VALUES ($1, $2, $3, $4, $5) RETURNING created_at",
    )
    .bind(id)
    .bind(user.user_id)
    .bind(user.device_id)
    .bind(&label)
    .bind(&ids::sha256(token.as_bytes())[..])
    .fetch_one(&state.db)
    .await?;
    tracing::info!(
        user_id = %user.user_id,
        device_id = %user.device_id,
        token_id = %id,
        "agent token minted"
    );
    Ok((
        StatusCode::CREATED,
        Json(MintedToken {
            id,
            token,
            label,
            device_id: user.device_id,
            created_at,
            mcp_url: mcp_url(&state.cfg),
        }),
    ))
}

/// `GET /v1/me/agent-tokens`: the user's live tokens, newest first (never the token itself).
async fn list_tokens(
    State(state): State<SharedState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<TokenView>>> {
    Ok(Json(
        sqlx::query_as::<_, TokenView>(
            "SELECT id, label, device_id, created_at, last_used_at FROM agent_tokens
             WHERE user_id = $1 AND revoked_at IS NULL ORDER BY created_at DESC, id DESC",
        )
        .bind(user.user_id)
        .fetch_all(&state.db)
        .await?,
    ))
}

/// `DELETE /v1/me/agent-tokens/{id}`: revoke at once (its calls in flight fail and are
/// cancelled on the device). Idempotent; `404` if not the caller's.
async fn revoke_token(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let revoked: Option<Uuid> = sqlx::query_scalar(
        "UPDATE agent_tokens SET revoked_at = COALESCE(revoked_at, now())
         WHERE id = $1 AND user_id = $2 RETURNING id",
    )
    .bind(id)
    .bind(user.user_id)
    .fetch_optional(&state.db)
    .await?;
    if revoked.is_none() {
        return Err(ApiError::NotFound);
    }
    state.device_agent.revoke_token(id);
    tracing::info!(user_id = %user.user_id, token_id = %id, by_device = %user.device_id, "agent token revoked");
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------------------------
// Device deletion

/// What deleting a device turned off. Apply it to the hub ([`RevokedDevice::apply`]) once the
/// transaction has committed.
#[derive(Debug, Default)]
#[must_use]
pub struct RevokedDevice {
    pub links: Vec<Uuid>,
    pub tokens: Vec<Uuid>,
}

impl RevokedDevice {
    pub fn is_empty(&self) -> bool {
        self.links.is_empty() && self.tokens.is_empty()
    }

    /// Close the links' streams (failing their calls in flight) and fail the tokens' calls.
    pub fn apply(&self, hub: &Hub) {
        for link in &self.links {
            hub.revoke_link(*link);
        }
        for token in &self.tokens {
            hub.revoke_token(*token);
        }
    }
}

/// Revoke the link and every agent token of device `device_id` under `users` (inside the
/// transaction that deletes the device).
pub async fn revoke_device(
    conn: &mut sqlx::PgConnection,
    users: &[Uuid],
    device_id: Uuid,
) -> ApiResult<RevokedDevice> {
    let links: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE device_links SET revoked_at = COALESCE(revoked_at, now())
         WHERE device_id = $1 AND user_id = ANY($2) RETURNING id",
    )
    .bind(device_id)
    .bind(users)
    .fetch_all(&mut *conn)
    .await?;
    let tokens: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE agent_tokens SET revoked_at = now()
         WHERE device_id = $1 AND user_id = ANY($2) AND revoked_at IS NULL RETURNING id",
    )
    .bind(device_id)
    .bind(users)
    .fetch_all(&mut *conn)
    .await?;
    Ok(RevokedDevice { links, tokens })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_token_format() {
        let t = new_agent_token();
        assert!(t.starts_with("cat_") && t.len() == 4 + 43, "{t}");
        assert!(plausible_agent_token(&t));
        assert_ne!(t, new_agent_token());
        for bad in [
            "",
            "cat_",
            "cat_short",
            &t[4..],
            &format!("cai_{}", &t[4..]),
        ] {
            assert!(!plausible_agent_token(bad), "{bad}");
        }
    }

    #[test]
    fn agent_paths() {
        for p in [
            "/v1/agent",
            "/v1/agent/",
            "/v1/agent/mcp",
            "/v1/agent/status",
        ] {
            assert!(is_agent_path(p), "{p}");
        }
        for p in [
            "/v1/agents",
            "/v1/agents/pair",
            "/v1/agentx",
            "/v1/me/links",
            "/agent",
        ] {
            assert!(!is_agent_path(p), "{p}");
        }
    }

    #[test]
    fn manifests() {
        let tool =
            |n: &str| json!({ "name": n, "description": "d", "inputSchema": {"type": "object"} });
        assert!(validate_manifest(&json!([])).is_ok());
        assert!(validate_manifest(&json!([tool("a"), tool("b")])).is_ok());
        assert!(validate_manifest(&json!({"name": "a"})).is_err());
        assert!(validate_manifest(&json!([tool("a"), tool("a")])).is_err());
        assert!(validate_manifest(&json!([{"description": "no name"}])).is_err());
        assert!(validate_manifest(&json!([tool(" ")])).is_err());
        assert!(validate_manifest(&json!(["jev_run"])).is_err());
        let many: Vec<Value> = (0..=MAX_TOOLS).map(|i| tool(&format!("t{i}"))).collect();
        assert!(validate_manifest(&Value::Array(many)).is_err());
        let big = json!([{ "name": "a", "description": "x".repeat(MAX_MANIFEST_BYTES) }]);
        assert!(validate_manifest(&big).is_err());
    }

    #[test]
    fn utf8_truncation() {
        assert_eq!(truncate_utf8("héllo".into(), 2), "h");
        assert_eq!(truncate_utf8("héllo".into(), 3), "hé");
        assert_eq!(truncate_utf8("abc".into(), 10), "abc");
    }

    #[test]
    fn mcp_url_uses_public_url_or_acme_domain() {
        let mut cfg = Config::for_tests("postgres://localhost/unused");
        assert_eq!(mcp_url(&cfg), "https://localhost:8443/v1/agent/mcp");
        cfg.public_url = "cloud.example.com".into();
        cfg.tls.mode = TlsMode::Acme;
        cfg.tls.domain = Some("cloud.example.com".into());
        assert_eq!(mcp_url(&cfg), "https://cloud.example.com/v1/agent/mcp");
    }
}
