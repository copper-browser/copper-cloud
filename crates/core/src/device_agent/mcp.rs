//! The agent side: `GET /v1/agent/status` and `POST /v1/agent/mcp` — MCP Streamable HTTP,
//! stateless — authenticated by a `cat_` token instead of the instance gate.
//!
//! `tools/call` is relayed to the device: offline, turned off, busy or an unknown tool answer at
//! once; otherwise a `request` frame goes down the device's stream and the reply comes back as
//! the JSON-RPC result. With `Accept: text/event-stream` the answer is an SSE stream (keepalive
//! comments, `notifications/progress` when the call carried a progress token, then the
//! response); otherwise one JSON response. A deadline miss, or the agent going away, sends the
//! device a `cancel`.
//!
//! Each relayed call waits in its own task, apart from the agent's response: the deadline ends it
//! whether or not the agent reads its answer, and a slow reader only loses progress lines
//! (up to [`AGENT_BUFFER`] wait for it, later ones are dropped) — it never holds up the device's
//! frames POST or anyone else's calls.

use std::convert::Infallible;
use std::time::{Duration, Instant};

use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::{ACCEPT, AUTHORIZATION};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::Event as SseEvent;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{json, Map, Value};
use time::OffsetDateTime;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use super::hub::{self, new_request_id, Call, Progress, Refused, Reply};
use super::{plausible_agent_token, sse_response};
use crate::error::{ApiError, ApiResult};
use crate::extract::{check_content_length, read_body};
use crate::ids;
use crate::state::SharedState;

/// `POST /v1/agent/mcp` body cap.
pub const MCP_BODY_LIMIT: usize = 4 * 1024 * 1024;
/// MCP protocol versions answered as asked; anything else gets the first.
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
/// How often (at most) a waiting call checks that its device is still online; a quarter of the
/// offline grace when that is shorter.
const WATCH_EVERY: Duration = Duration::from_secs(1);

/// What `initialize` tells the agent about driving the phone (after the Mac's MCP instructions).
pub const INSTRUCTIONS: &str = "This is the person's own Copper browser on their iPhone, \
signed in as them; every call acts as the person in their signed-in tabs, on the tab they are \
looking at. Nothing is sandboxed — act as they would. It only works while Copper is open on \
their screen: when a call says Copper isn't open, ask them to open it and keep it on screen, \
then try again. ANY task: the FIRST call is jev_run with the whole goal, every concrete \
place/date/name/filter and a stop condition. jev_observe = fast indexed read; jev_extract = \
JSON values. DONE is Jev's claim — verify it with jev_observe or jev_extract. When Jev is \
BLOCKED, use browser_snapshot, browser_click and browser_type for that part, then jev_run \
again. Without jev_* tools in tools/list, start with browser_snapshot. Calls run one at a \
time and each is shown on the page with a Stop button.";

// ---------------------------------------------------------------------------------------------
// Authentication

/// The agent behind a valid `cat_` token, and the device it may drive.
pub(crate) struct AgentCaller {
    pub token_id: Uuid,
    pub user_id: Uuid,
    pub device_id: Uuid,
    pub label: String,
    pub device_name: String,
}

const TOKEN_SQL: &str = "
WITH t AS (
    SELECT t.id, t.user_id, t.device_id, t.label
    FROM agent_tokens t
    JOIN users u ON u.id = t.user_id
    WHERE t.token_sha256 = $1 AND t.revoked_at IS NULL AND NOT u.disabled
), touch AS (
    UPDATE agent_tokens SET last_used_at = now()
    WHERE id IN (SELECT id FROM t)
      AND (last_used_at IS NULL OR last_used_at < now() - interval '60 seconds')
)
SELECT t.id, t.user_id, t.device_id, t.label, COALESCE(NULLIF(d.name, ''), 'Copper') AS device_name
FROM t JOIN devices d ON d.user_id = t.user_id AND d.id = t.device_id";

fn bad_token() -> ApiError {
    ApiError::Unauthorized("agent_token")
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let (scheme, token) = headers.get(AUTHORIZATION)?.to_str().ok()?.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

impl FromRequestParts<SharedState> for AgentCaller {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &SharedState,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer(&parts.headers)
            .filter(|t| plausible_agent_token(t))
            .ok_or_else(bad_token)?;
        let row: Option<(Uuid, Uuid, Uuid, String, String)> = sqlx::query_as(TOKEN_SQL)
            .bind(&ids::sha256(token.as_bytes())[..])
            .fetch_optional(&state.db)
            .await?;
        let Some((token_id, user_id, device_id, label, device_name)) = row else {
            tracing::info!("agent request refused: unknown or revoked token");
            return Err(bad_token());
        };
        tracing::Span::current().record("user_id", tracing::field::display(user_id));
        if !state.device_agent.allow(user_id, token_id) {
            metrics::counter!("agent_rate_limited_total").increment(1);
            tracing::warn!(%token_id, "agent rate limit exceeded");
            return Err(ApiError::RateLimited);
        }
        Ok(Self {
            token_id,
            user_id,
            device_id,
            label,
            device_name,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// The device's link, as an agent sees it

#[derive(sqlx::FromRow)]
struct LinkState {
    id: Uuid,
    label: String,
    revoked: bool,
    last_seen_at: Option<OffsetDateTime>,
    tools: i64,
    /// Whether the manifest has the tool asked about (`false` when none was).
    has_tool: bool,
}

async fn link_state(
    db: &sqlx::PgPool,
    caller: &AgentCaller,
    tool: Option<&str>,
) -> ApiResult<Option<LinkState>> {
    Ok(sqlx::query_as::<_, LinkState>(
        "SELECT id, label, revoked_at IS NOT NULL AS revoked, last_seen_at,
                jsonb_array_length(tools)::bigint AS tools,
                EXISTS (SELECT 1 FROM jsonb_array_elements(tools) e WHERE e->>'name' = $3)
                    AS has_tool
         FROM device_links WHERE user_id = $1 AND device_id = $2",
    )
    .bind(caller.user_id)
    .bind(caller.device_id)
    .bind(tool)
    .fetch_optional(db)
    .await?)
}

fn no_store(mut resp: Response) -> Response {
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// `GET /v1/agent/status`: the device this token drives and whether Copper is open on it.
pub(crate) async fn status(
    State(state): State<SharedState>,
    caller: AgentCaller,
) -> ApiResult<Response> {
    let link = link_state(&state.db, &caller, None).await?;
    let online = link
        .as_ref()
        .is_some_and(|l| !l.revoked && state.device_agent.is_online(l.id));
    let last_seen = link.as_ref().and_then(|l| l.last_seen_at).and_then(|t| {
        t.format(&time::format_description::well_known::Rfc3339)
            .ok()
    });
    Ok(no_store(
        Json(json!({
            "device": { "id": caller.device_id, "name": caller.device_name },
            "label": link.as_ref().map(|l| l.label.as_str()),
            "online": online,
            "last_seen_at": last_seen,
            "tools": link.as_ref().map_or(0, |l| l.tools),
        }))
        .into_response(),
    ))
}

/// `GET` / `DELETE /v1/agent/mcp`: no server-initiated stream and no session.
pub(crate) async fn no_stream(_caller: AgentCaller) -> Response {
    let mut resp = ApiError::coded(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "no event stream and no session here: POST one JSON-RPC message per request",
    )
    .into_response();
    resp.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    resp
}

// ---------------------------------------------------------------------------------------------
// JSON-RPC plumbing

#[derive(Serialize)]
struct RpcResult<'a> {
    jsonrpc: &'static str,
    id: &'a Value,
    result: &'a RawValue,
}

fn rpc_result(id: &Value, result: &RawValue) -> String {
    serde_json::to_string(&RpcResult {
        jsonrpc: "2.0",
        id,
        result,
    })
    .unwrap_or_default()
}

fn rpc_value(id: &Value, result: &Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn rpc_error(id: &Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

fn json_answer(status: StatusCode, body: String) -> Response {
    no_store(
        (
            status,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )],
            body,
        )
            .into_response(),
    )
}

/// `400` with a JSON-RPC error (no usable id): parse errors, batches, malformed messages.
fn rpc_rejected(code: i64, message: &str) -> Response {
    json_answer(
        StatusCode::BAD_REQUEST,
        rpc_error(&Value::Null, code, message),
    )
}

/// `202 Accepted`, empty body: notifications and responses from the client.
fn accepted() -> Response {
    StatusCode::ACCEPTED.into_response()
}

/// Whether the request's `Accept` names `text/event-stream`.
fn accepts_sse(headers: &HeaderMap) -> bool {
    headers
        .get_all(ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|m| {
            m.split(';')
                .next()
                .is_some_and(|t| t.trim().eq_ignore_ascii_case("text/event-stream"))
        })
}

/// A `CallToolResult` with one text block and `isError: true`.
fn tool_error(text: &str) -> Box<RawValue> {
    serde_json::value::to_raw_value(&json!({
        "content": [{ "type": "text", "text": text }],
        "isError": true,
    }))
    .expect("serializable")
}

/// Whether a device's result says `isError: true` (for the log line only).
fn is_error_result(result: &RawValue) -> bool {
    #[derive(Deserialize)]
    struct Flag {
        #[serde(rename = "isError", default)]
        is_error: bool,
    }
    serde_json::from_str::<Flag>(result.get()).is_ok_and(|f| f.is_error)
}

/// One SSE `data:` line: a reply may carry pretty-printed JSON, which is compacted so that
/// line-oriented clients see the whole message in a single `data:` field.
fn single_line(body: String) -> String {
    if body.contains(['\n', '\r']) {
        serde_json::from_str::<Value>(&body).map_or(body, |v| v.to_string())
    } else {
        body
    }
}

fn message_event(body: String) -> SseEvent {
    SseEvent::default().event("message").data(single_line(body))
}

// ---------------------------------------------------------------------------------------------
// `POST /v1/agent/mcp`

/// The MCP endpoint. One JSON-RPC 2.0 message per request (batches: `400`); no session.
pub(crate) async fn mcp(
    State(state): State<SharedState>,
    caller: AgentCaller,
    req: Request,
) -> Response {
    let wants_sse = accepts_sse(req.headers());
    if let Err(err) = check_content_length(&req, MCP_BODY_LIMIT) {
        return err.into_response();
    }
    let bytes = match read_body(req.into_body(), MCP_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(err) => return err.into_response(),
    };
    let Ok(message) = serde_json::from_slice::<Value>(&bytes) else {
        return rpc_rejected(
            -32700,
            "parse error: the body must be one JSON-RPC 2.0 message",
        );
    };
    drop(bytes);
    let mut message = match message {
        Value::Object(map) => map,
        Value::Array(_) => {
            return rpc_rejected(
                -32600,
                "batches are not supported: send one JSON-RPC message per request",
            )
        }
        _ => return rpc_rejected(-32600, "invalid request: expected a JSON-RPC 2.0 object"),
    };
    let Some(method) = message
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        // A response from the client (we never send requests): nothing to do.
        if message.contains_key("result") || message.contains_key("error") {
            return accepted();
        }
        return rpc_rejected(-32600, "invalid request: method is required");
    };
    if method.starts_with("notifications/") {
        return accepted();
    }
    let Some(id) = message.remove("id") else {
        // Any other notification: nothing to answer.
        return accepted();
    };
    let params = message.remove("params");
    match method.as_str() {
        "initialize" => json_answer(StatusCode::OK, rpc_value(&id, &initialize(params.as_ref()))),
        "ping" => json_answer(StatusCode::OK, rpc_value(&id, &json!({}))),
        "tools/list" => match tools_list(&state, &caller).await {
            Ok(tools) => json_answer(StatusCode::OK, rpc_value(&id, &json!({ "tools": tools }))),
            Err(err) => err.into_response(),
        },
        "tools/call" => tools_call(state, caller, id, params, wants_sse).await,
        _ => json_answer(
            StatusCode::OK,
            rpc_error(&id, -32601, &format!("method not found: {method}")),
        ),
    }
}

fn initialize(params: Option<&Value>) -> Value {
    let asked = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|v| **v == asked)
        .unwrap_or(&PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "copper-mobile", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

/// The manifest the device last registered (empty if none).
async fn tools_list(state: &SharedState, caller: &AgentCaller) -> ApiResult<Value> {
    let tools: Option<sqlx::types::Json<Value>> =
        sqlx::query_scalar("SELECT tools FROM device_links WHERE user_id = $1 AND device_id = $2")
            .bind(caller.user_id)
            .bind(caller.device_id)
            .fetch_optional(&state.db)
            .await?;
    Ok(tools.map_or_else(|| json!([]), |t| t.0))
}

// ---------------------------------------------------------------------------------------------
// `tools/call`

/// The one log line per agent call (never arguments or results) + `device_agent_calls_total`.
struct Audit {
    token_id: Uuid,
    device_id: Uuid,
    tool: String,
    started: Instant,
    span: tracing::Span,
}

impl Audit {
    fn log(&self, status: &'static str) {
        let ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        metrics::counter!("device_agent_calls_total", "status" => status).increment(1);
        self.span.in_scope(|| {
            tracing::info!(
                token_id = %self.token_id,
                device_id = %self.device_id,
                tool = %self.tool,
                ms,
                status,
                "agent call"
            );
        });
    }
}

/// A tool name fit for the log: at most 64 characters, no control characters.
fn loggable_tool(name: &str) -> String {
    name.chars()
        .take(64)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Cancels the call on the device (and logs it) if the agent goes away before it finished:
/// dropped with the handler future (JSON answer) or the response body (SSE answer).
struct CallGuard {
    state: SharedState,
    request_id: String,
    audit: Option<Audit>,
}

impl CallGuard {
    fn finish(&mut self, status: &'static str) {
        if let Some(audit) = self.audit.take() {
            audit.log(status);
        }
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        if let Some(audit) = self.audit.take() {
            self.state.device_agent.cancel(&self.request_id);
            audit.log("cancelled");
        }
    }
}

fn offline_text(device: &str) -> String {
    format!(
        "Copper isn't open on {device}. Ask the person to open Copper on their iPhone and keep it \
         on screen (Settings › Agents › Let agents use this iPhone must be on), then try again."
    )
}

fn turned_off_text(device: &str) -> String {
    format!(
        "Agents are turned off on {device}. Ask the person to turn on Settings › Agents › Let \
         agents use this iPhone in Copper, then try again."
    )
}

fn busy_text(device: &str) -> String {
    format!("Copper on {device} is busy with other calls; try again shortly.")
}

fn unknown_tool_text(tool: &str, device: &str) -> String {
    format!(
        "Copper on {device} has no tool named \"{tool}\". Call tools/list for the tools it offers."
    )
}

struct RelayInfo {
    id: Value,
    tool: String,
    device_name: String,
    link: Uuid,
    progress_token: Option<Value>,
}

fn progress_notification(token: &Value, p: &Progress) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": { "progressToken": token, "progress": p.seq, "message": p.message },
    })
    .to_string()
}

/// How a device reply (or the hub) ended the call: the result for the agent and the log status.
fn outcome(
    reply: Result<Reply, tokio::sync::oneshot::error::RecvError>,
    info: &RelayInfo,
) -> (Box<RawValue>, &'static str) {
    let (tool, device) = (&info.tool, &info.device_name);
    match reply {
        Ok(Reply::Result(result)) => {
            let status = if is_error_result(&result) {
                "tool_error"
            } else {
                "ok"
            };
            (result, status)
        }
        Ok(Reply::Error(message)) => (tool_error(&message), "error"),
        Ok(Reply::TurnedOff) => (
            tool_error(&format!(
                "Agents were turned off on {device} while {tool} was running; the call was stopped."
            )),
            "revoked",
        ),
        Ok(Reply::TokenRevoked) => (
            tool_error(&format!(
                "This agent's access to {device} was revoked while {tool} was running; the call was stopped."
            )),
            "token_revoked",
        ),
        Ok(Reply::UserRemoved) => (
            tool_error(&format!(
                "The account {device} is signed in to was disabled or deleted while {tool} was running; the call was stopped."
            )),
            "user_revoked",
        ),
        Err(_) => (
            tool_error(&format!("{tool} ended on {device} without a reply.")),
            "error",
        ),
    }
}

/// Where a relayed call's answer goes: `done` gets the JSON-RPC response; `progress` (SSE with a
/// progress token only) gets `notifications/progress` lines, best effort.
struct Answer {
    progress: Option<mpsc::Sender<String>>,
    done: oneshot::Sender<String>,
}

/// Progress lines buffered for the agent; a reader this far behind loses the newest.
const AGENT_BUFFER: usize = 32;

/// Wait for the device in a task of its own: forward progress while it works, then send the
/// JSON-RPC response to `answer.done`. The deadline, shutdown, an offline device and the agent
/// going away (its end of `done` dropped) all end the wait, whether or not the agent is reading.
async fn relay(
    state: SharedState,
    call: Call,
    mut guard: CallGuard,
    info: RelayInfo,
    mut answer: Answer,
) {
    let tuning = &state.cfg.device_agent;
    let mut shutdown = crate::shutdown::subscribe();
    let Call {
        mut reply,
        mut progress,
        deadline,
    } = call;
    let deadline = tokio::time::sleep_until(deadline.into());
    tokio::pin!(deadline);
    let mut watch = tokio::time::interval(
        WATCH_EVERY
            .min(tuning.offline_grace / 4)
            .max(Duration::from_millis(10)),
    );
    watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut offline_since: Option<Instant> = None;
    let mut progress_open = true;
    let (result, status) = loop {
        tokio::select! {
            // The deadline and shutdown first (a flood of progress cannot hold them off), then
            // progress before the reply, so frames uploaded just before it still go out.
            biased;
            () = &mut deadline => {
                state.device_agent.cancel(&guard.request_id);
                let text = format!(
                    "{} timed out: {} did not answer within {} s, so the call was cancelled. \
                     Try again, or ask the person to check Copper on their iPhone.",
                    info.tool, info.device_name, tuning.relay_deadline.as_secs_f64().ceil()
                );
                break (tool_error(&text), "timeout");
            }
            () = crate::shutdown::wait(&mut shutdown) => {
                state.device_agent.cancel(&guard.request_id);
                let text = format!(
                    "Copper Cloud is restarting, so {} was stopped. Try again in a moment.",
                    info.tool
                );
                break (tool_error(&text), "shutdown");
            }
            // The agent went away: dropping the guard cancels the call on the device.
            () = answer.done.closed() => return,
            p = progress.recv(), if progress_open => match p {
                Some(p) => {
                    if let (Some(out), Some(token)) = (&answer.progress, &info.progress_token) {
                        // Never wait for the agent: a full buffer drops this line.
                        let _ = out.try_send(progress_notification(token, &p));
                    }
                }
                None => progress_open = false,
            },
            r = &mut reply => break outcome(r, &info),
            _ = watch.tick() => {
                if state.device_agent.is_online(info.link) {
                    offline_since = None;
                } else if offline_since.get_or_insert_with(Instant::now).elapsed()
                    >= tuning.offline_grace
                {
                    state.device_agent.cancel(&guard.request_id);
                    let text = format!(
                        "Copper closed on {} before {} finished (it is no longer open on \
                         their screen). Ask the person to open Copper again, then try again.",
                        info.device_name, info.tool
                    );
                    break (tool_error(&text), "gone_offline");
                }
            }
        }
    };
    guard.finish(status);
    // Progress first, then the response: the agent's stream drains the lines, then reads this.
    drop(answer.progress);
    let _ = answer.done.send(rpc_result(&info.id, &result));
}

/// An SSE answer: the progress lines, then the response.
fn answer_stream(
    progress: Option<mpsc::Receiver<String>>,
    done: oneshot::Receiver<String>,
) -> impl Stream<Item = Result<SseEvent, Infallible>> {
    async_stream::stream! {
        if let Some(mut progress) = progress {
            while let Some(line) = progress.recv().await {
                yield Ok(message_event(line));
            }
        }
        if let Ok(body) = done.await {
            yield Ok(message_event(body));
        }
    }
}

/// An answer that needs no device: one JSON-RPC response (as SSE when the agent asked for it).
fn answer_now(id: &Value, result: &RawValue, wants_sse: bool) -> Response {
    let body = rpc_result(id, result);
    if wants_sse {
        sse_response(
            futures::stream::once(async move { Ok::<_, Infallible>(message_event(body)) }),
            None,
        )
    } else {
        json_answer(StatusCode::OK, body)
    }
}

async fn tools_call(
    state: SharedState,
    caller: AgentCaller,
    id: Value,
    params: Option<Value>,
    wants_sse: bool,
) -> Response {
    let params = params.as_ref().and_then(Value::as_object);
    let Some(name) = params.and_then(|p| p.get("name")).and_then(Value::as_str) else {
        return json_answer(
            StatusCode::OK,
            rpc_error(
                &id,
                -32602,
                "invalid params: name (the tool to call) is required",
            ),
        );
    };
    let arguments = match params.and_then(|p| p.get("arguments")) {
        None | Some(Value::Null) => json!({}),
        Some(args @ Value::Object(_)) => args.clone(),
        Some(_) => {
            return json_answer(
                StatusCode::OK,
                rpc_error(&id, -32602, "invalid params: arguments must be an object"),
            )
        }
    };
    let progress_token = params
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get("progressToken"))
        .filter(|t| t.is_string() || t.is_number())
        .cloned();
    let audit = Audit {
        token_id: caller.token_id,
        device_id: caller.device_id,
        tool: loggable_tool(name),
        started: Instant::now(),
        span: tracing::Span::current(),
    };
    let device = caller.device_name.as_str();
    let link = match link_state(&state.db, &caller, Some(name)).await {
        Ok(link) => link,
        Err(err) => return err.into_response(),
    };
    let Some(link) = link.filter(|l| !l.revoked) else {
        audit.log("turned_off");
        return answer_now(&id, &tool_error(&turned_off_text(device)), wants_sse);
    };
    if !link.has_tool {
        audit.log("unknown_tool");
        return answer_now(
            &id,
            &tool_error(&unknown_tool_text(name, device)),
            wants_sse,
        );
    }
    let tuning = &state.cfg.device_agent;
    let request_id = new_request_id();
    let mut frame = Map::new();
    frame.insert("id".into(), Value::from(request_id.as_str()));
    frame.insert("method".into(), Value::from("tools/call"));
    frame.insert(
        "params".into(),
        json!({ "name": name, "arguments": arguments }),
    );
    frame.insert(
        "caller".into(),
        json!({ "token_id": caller.token_id, "label": caller.label }),
    );
    let call = match state.device_agent.begin(hub::Request {
        link: link.id,
        user: caller.user_id,
        token: caller.token_id,
        id: request_id.clone(),
        frame,
        timeout: tuning.relay_deadline,
    }) {
        Ok(call) => call,
        Err(Refused::Offline) => {
            audit.log("offline");
            return answer_now(&id, &tool_error(&offline_text(device)), wants_sse);
        }
        Err(Refused::Busy) => {
            audit.log("busy");
            return answer_now(&id, &tool_error(&busy_text(device)), wants_sse);
        }
    };
    let info = RelayInfo {
        id,
        tool: name.to_owned(),
        device_name: caller.device_name.clone(),
        link: link.id,
        progress_token,
    };
    let guard = CallGuard {
        state: state.clone(),
        request_id,
        audit: Some(audit),
    };
    relayed(state, call, guard, info, wants_sse).await
}

/// Start the call's [`relay`] task and answer the agent from it: an SSE stream, or one JSON
/// response once it is done.
async fn relayed(
    state: SharedState,
    call: Call,
    guard: CallGuard,
    info: RelayInfo,
    wants_sse: bool,
) -> Response {
    let keepalive = state.cfg.device_agent.agent_keepalive;
    let (progress_tx, progress_rx) = if wants_sse && info.progress_token.is_some() {
        let (tx, rx) = mpsc::channel(AGENT_BUFFER);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let (done_tx, done_rx) = oneshot::channel();
    tokio::spawn(relay(
        state,
        call,
        guard,
        info,
        Answer {
            progress: progress_tx,
            done: done_tx,
        },
    ));
    if wants_sse {
        return sse_response(answer_stream(progress_rx, done_rx), Some(keepalive));
    }
    // Dropping this future (the agent went away) drops `done_rx`, which ends the call.
    match done_rx.await {
        Ok(body) => json_answer(StatusCode::OK, body),
        Err(_) => ApiError::internal("relay ended without an answer").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_header() {
        let h = |v: &str| {
            let mut m = HeaderMap::new();
            m.insert(ACCEPT, HeaderValue::from_str(v).unwrap());
            m
        };
        assert!(accepts_sse(&h("application/json, text/event-stream")));
        assert!(accepts_sse(&h("Text/Event-Stream;q=0.9")));
        assert!(!accepts_sse(&h("application/json")));
        assert!(!accepts_sse(&h("*/*")));
        assert!(!accepts_sse(&HeaderMap::new()));
    }

    #[test]
    fn protocol_negotiation() {
        let v = |asked: Value| {
            initialize(Some(&json!({ "protocolVersion": asked })))["protocolVersion"].clone()
        };
        assert_eq!(v(json!("2025-03-26")), "2025-03-26");
        assert_eq!(v(json!("2024-11-05")), "2024-11-05");
        assert_eq!(v(json!("1999-01-01")), "2025-06-18");
        assert_eq!(v(json!(7)), "2025-06-18");
        assert_eq!(initialize(None)["serverInfo"]["name"], "copper-mobile");
    }

    #[test]
    fn results_and_lines() {
        let err = tool_error("nope");
        assert!(is_error_result(&err));
        let ok = RawValue::from_string(r#"{"content":[],"isError":false}"#.into()).unwrap();
        assert!(!is_error_result(&ok));
        assert_eq!(
            rpc_result(&json!(7), &ok),
            r#"{"jsonrpc":"2.0","id":7,"result":{"content":[],"isError":false}}"#
        );
        assert_eq!(single_line("{\n \"a\": 1\n}".into()), r#"{"a":1}"#);
        assert_eq!(loggable_tool("a\nb"), "a b");
        assert_eq!(loggable_tool(&"x".repeat(100)).len(), 64);
    }
}
