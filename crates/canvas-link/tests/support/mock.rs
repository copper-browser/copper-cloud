//! In-process mock Canvas Cloud (axum, ephemeral port) implementing just enough of CONTRACT
//! §5.1/§5.3 for the canvas-link tests: app-key + `X-Canvas-User` auth with `unknown_user`,
//! a canvas list, an echo route (what the proxy forwarded), status routes, a WebSocket echo
//! with scripted close codes, the app event stream (SSE), `/v1/info` and
//! `/v1/apps/self/{stats,users…,canvases…}`.
//!
//! Shared by `crates/canvas-link/tests` and `crates/copper-cloud/tests` (via `#[path]`).

#![allow(
    dead_code,
    clippy::missing_panics_doc,
    clippy::too_many_lines,
    clippy::result_large_err
)]

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::Path as FsPath;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get, post, put};
use axum::{Json, Router};
use serde_json::{json, Map, Value};
use tokio::sync::broadcast;

/// The app key the mock accepts by default.
pub const APP_KEY: &str = "cva_mock_app_key_0123456789abcdef";

#[derive(Clone, Debug)]
pub struct MockUser {
    pub subject: String,
    pub user_id: String,
    pub email: String,
    pub display_name: Option<String>,
    pub disabled: bool,
}

#[derive(Clone, Debug)]
pub struct MockCanvas {
    pub id: String,
    pub name: String,
    pub kind: String,
    /// Owner subject.
    pub owner: String,
    pub members: Vec<String>,
}

/// One request as received (no body).
#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: HashMap<String, String>,
}

pub struct MockState {
    pub app_key: Mutex<String>,
    pub users: Mutex<HashMap<String, MockUser>>,
    pub canvases: Mutex<Vec<MockCanvas>>,
    pub requests: Mutex<Vec<Recorded>>,
    /// Close frames received from the proxy on the WebSocket: `(code, reason)`
    /// (`1005` = a close without a code).
    pub ws_closes: Mutex<Vec<(u16, String)>>,
    pub disconnects: Mutex<Vec<String>>,
    pub deleted_users: Mutex<Vec<String>>,
    pub last_event_ids: Mutex<Vec<Option<String>>>,
    pub user_puts: AtomicUsize,
    pub batch_calls: AtomicUsize,
    /// `PUT users/{subject}` answers 500.
    pub fail_provision: AtomicBool,
    /// `PUT users/{subject}` answers 200 but does not store the user (a second
    /// `unknown_user` follows).
    pub sticky_unknown: AtomicBool,
    pub live_peers: AtomicUsize,
    pub event_seq: AtomicU64,
    pub events: broadcast::Sender<(u64, String, String)>,
}

impl MockState {
    fn new() -> Self {
        Self {
            app_key: Mutex::new(APP_KEY.to_owned()),
            users: Mutex::default(),
            canvases: Mutex::default(),
            requests: Mutex::default(),
            ws_closes: Mutex::default(),
            disconnects: Mutex::default(),
            deleted_users: Mutex::default(),
            last_event_ids: Mutex::default(),
            user_puts: AtomicUsize::new(0),
            batch_calls: AtomicUsize::new(0),
            fail_provision: AtomicBool::new(false),
            sticky_unknown: AtomicBool::new(false),
            live_peers: AtomicUsize::new(0),
            event_seq: AtomicU64::new(0),
            events: broadcast::channel(256).0,
        }
    }

    /// Accept a different app key from now on (simulates a revoked/rotated key).
    pub fn set_app_key(&self, key: &str) {
        key.clone_into(&mut self.app_key.lock().unwrap());
    }

    pub fn has_user(&self, subject: &str) -> bool {
        self.users.lock().unwrap().contains_key(subject)
    }

    pub fn user(&self, subject: &str) -> Option<MockUser> {
        self.users.lock().unwrap().get(subject).cloned()
    }

    pub fn add_user(&self, subject: &str, email: &str) {
        self.users.lock().unwrap().insert(
            subject.to_owned(),
            MockUser {
                subject: subject.to_owned(),
                user_id: subject.to_owned(),
                email: email.to_owned(),
                display_name: None,
                disabled: false,
            },
        );
    }

    /// Add a canvas owned by `owner` (subject), returning its id.
    pub fn add_canvas(&self, owner: &str, name: &str, kind: &str) -> String {
        let id = uuid::Uuid::now_v7().to_string();
        self.canvases.lock().unwrap().push(MockCanvas {
            id: id.clone(),
            name: name.to_owned(),
            kind: kind.to_owned(),
            owner: owner.to_owned(),
            members: vec![owner.to_owned()],
        });
        id
    }

    pub fn canvas_count(&self) -> usize {
        self.canvases.lock().unwrap().len()
    }

    /// Requests received for `path` (exact).
    pub fn requests_for(&self, path: &str) -> Vec<Recorded> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path == path)
            .cloned()
            .collect()
    }

    /// Publish an app `canvas` event; returns its id.
    pub fn publish_canvas(&self, subject: &str, canvas_id: &str, kind: &str) -> u64 {
        let id = self.event_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let data = json!({
            "subject": subject, "user_id": subject, "canvas_id": canvas_id, "kind": kind,
        });
        let _ = self.events.send((id, "canvas".into(), data.to_string()));
        id
    }

    /// Number of connected event-stream clients.
    pub fn event_clients(&self) -> usize {
        self.events.receiver_count()
    }
}

pub struct Mock {
    pub addr: SocketAddr,
    pub base: String,
    pub st: Arc<MockState>,
}

fn router(st: Arc<MockState>) -> Router {
    Router::new()
        .route("/v1/info", get(info))
        .route("/v1/canvases", get(list_canvases).post(create_canvas))
        .route("/v1/canvases/{id}/ws", any(ws))
        .route("/v1/canvases/{id}/status/{code}", any(status))
        .route("/v1/canvases/{id}/ops", post(ops))
        .route("/v1/apps/self/stats", get(stats))
        .route("/v1/apps/self/users", get(app_users))
        .route("/v1/apps/self/users:batch", post(batch))
        .route(
            "/v1/apps/self/users/{subject}",
            put(put_user).delete(delete_user),
        )
        .route("/v1/apps/self/users/{subject}/disconnect", post(disconnect))
        .route("/v1/apps/self/events", get(events))
        .route("/v1/apps/self/canvases", get(app_canvases))
        .route("/v1/apps/self/canvases/{id}", delete(app_delete_canvas))
        .fallback(echo)
        .layer(DefaultBodyLimit::max(64 << 20))
        .layer(axum::middleware::from_fn_with_state(st.clone(), record))
        .with_state(st)
}

/// Plain HTTP on `127.0.0.1:0`.
pub async fn start() -> Mock {
    let st = Arc::new(MockState::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(st.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Mock {
        addr,
        base: format!("http://{addr}"),
        st,
    }
}

/// HTTPS on `127.0.0.1:0` with the given PEM certificate/key.
pub async fn start_tls(cert: &FsPath, key: &FsPath) -> Mock {
    copper_cloud_core::tls::install_default_provider();
    let st = Arc::new(MockState::new());
    let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
        .await
        .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(st.clone());
    let server = axum_server::from_tcp_rustls(listener, config).unwrap();
    tokio::spawn(async move {
        server.serve(app.into_make_service()).await.unwrap();
    });
    Mock {
        addr,
        base: format!("https://127.0.0.1:{}", addr.port()),
        st,
    }
}

async fn record(State(st): State<Arc<MockState>>, req: Request, next: Next) -> Response {
    let headers = req
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_owned(),
                v.to_str().unwrap_or("<binary>").to_owned(),
            )
        })
        .collect();
    st.requests.lock().unwrap().push(Recorded {
        method: req.method().to_string(),
        path: req.uri().path().to_owned(),
        query: req.uri().query().map(str::to_owned),
        headers,
    });
    next.run(req).await
}

fn err(status: u16, code: &str, message: &str) -> Response {
    (
        StatusCode::from_u16(status).unwrap(),
        Json(json!({ "error": code, "message": message })),
    )
        .into_response()
}

fn app_ok(st: &MockState, h: &HeaderMap) -> bool {
    let want = format!("Bearer {}", st.app_key.lock().unwrap());
    h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some(want.as_str())
}

fn app_auth(st: &MockState, h: &HeaderMap) -> Result<(), Response> {
    if app_ok(st, h) {
        Ok(())
    } else {
        Err(err(401, "unauthorized", "invalid app key"))
    }
}

fn user_auth(st: &MockState, h: &HeaderMap) -> Result<MockUser, Response> {
    app_auth(st, h)?;
    let Some(subject) = h.get("x-canvas-user").and_then(|v| v.to_str().ok()) else {
        return Err(err(401, "unauthorized", "X-Canvas-User is required"));
    };
    match st.users.lock().unwrap().get(subject) {
        None => Err(err(
            401,
            "unknown_user",
            "this app has no user with that subject",
        )),
        Some(u) if u.disabled => Err(err(403, "user_disabled", "user is disabled")),
        Some(u) => Ok(u.clone()),
    }
}

async fn info() -> Json<Value> {
    Json(json!({
        "name": "canvas-cloud", "version": "0.0.0-mock", "signup": false,
        "features": [], "limits": {},
    }))
}

async fn echo(
    State(st): State<Arc<MockState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let user = match user_auth(&st, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let hdrs: Map<String, Value> = headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_owned(),
                Value::from(v.to_str().unwrap_or("<binary>")),
            )
        })
        .collect();
    let mut resp = Json(json!({
        "method": method.as_str(),
        "path": uri.path(),
        "query": uri.query(),
        "headers": hdrs,
        "body": String::from_utf8_lossy(&body),
        "user": user.subject,
    }))
    .into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=5"),
    );
    h.insert(header::ETAG, HeaderValue::from_static("\"mock-etag\""));
    h.insert(
        "x-mock-internal",
        HeaderValue::from_static("do-not-forward"),
    );
    h.insert(header::SET_COOKIE, HeaderValue::from_static("mock=1"));
    resp
}

async fn status(
    State(st): State<Arc<MockState>>,
    Path((_id, code)): Path<(String, u16)>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = user_auth(&st, &headers) {
        return r;
    }
    let mut resp = err(code, &format!("mock_{code}"), "from the mock Canvas Cloud");
    if code == 429 {
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("7"));
    }
    resp
}

async fn ops(State(st): State<Arc<MockState>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = user_auth(&st, &headers) {
        return r;
    }
    Json(json!({ "received": body.len() })).into_response()
}

async fn list_canvases(State(st): State<Arc<MockState>>, headers: HeaderMap) -> Response {
    let user = match user_auth(&st, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let canvases: Vec<Value> = st
        .canvases
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.members.contains(&user.subject))
        .map(|c| json!({ "id": c.id, "name": c.name, "kind": c.kind }))
        .collect();
    Json(json!({ "canvases": canvases })).into_response()
}

async fn create_canvas(
    State(st): State<Arc<MockState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let user = match user_auth(&st, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let name = body["name"].as_str().unwrap_or("Untitled");
    let id = st.add_canvas(&user.subject, name, "shared");
    Json(json!({ "id": id, "name": name, "kind": "shared", "role": "owner" })).into_response()
}

async fn ws(
    State(st): State<Arc<MockState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    up: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if let Err(r) = user_auth(&st, &headers) {
        return r;
    }
    match id.as_str() {
        "forbidden" => return err(403, "forbidden", "not a member of this canvas"),
        "missing" => return err(404, "not_found", "no such canvas"),
        _ => {}
    }
    let Ok(up) = up else {
        return err(400, "websocket", "WebSocket upgrade required");
    };
    up.max_message_size(64 << 20)
        .max_frame_size(16 << 20)
        .on_upgrade(move |sock| ws_session(st, sock))
}

/// Echo text/binary; text `close <code> <reason>` makes the server close with that code.
async fn ws_session(st: Arc<MockState>, mut sock: WebSocket) {
    st.live_peers.fetch_add(1, Ordering::SeqCst);
    while let Some(Ok(msg)) = sock.recv().await {
        match msg {
            Message::Text(t) => {
                if let Some(rest) = t.as_str().strip_prefix("close ") {
                    let (code, reason) = rest.split_once(' ').unwrap_or((rest, ""));
                    let _ = sock
                        .send(Message::Close(Some(CloseFrame {
                            code: code.parse().unwrap(),
                            reason: reason.into(),
                        })))
                        .await;
                } else if sock.send(Message::Text(t)).await.is_err() {
                    break;
                }
            }
            Message::Binary(b) => {
                if sock.send(Message::Binary(b)).await.is_err() {
                    break;
                }
            }
            Message::Close(frame) => {
                st.ws_closes
                    .lock()
                    .unwrap()
                    .push(frame.map_or((1005, String::new()), |f| (f.code, f.reason.to_string())));
                // Keep reading: that flushes our close reply and ends the stream.
            }
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }
    st.live_peers.fetch_sub(1, Ordering::SeqCst);
}

async fn stats(State(st): State<Arc<MockState>>, headers: HeaderMap) -> Response {
    if let Err(r) = app_auth(&st, &headers) {
        return r;
    }
    let peers = st.live_peers.load(Ordering::SeqCst);
    Json(json!({
        "users": st.users.lock().unwrap().len(),
        "canvases": st.canvas_count(),
        "rooms": { "rooms": usize::from(peers > 0), "peers": peers },
    }))
    .into_response()
}

fn user_json(u: &MockUser, canvases: &[MockCanvas]) -> Value {
    let count = canvases
        .iter()
        .filter(|c| c.members.contains(&u.subject))
        .count();
    json!({
        "subject": u.subject, "user_id": u.user_id, "email": u.email,
        "display_name": u.display_name, "disabled": u.disabled, "canvas_count": count,
    })
}

async fn app_users(
    State(st): State<Arc<MockState>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = app_auth(&st, &headers) {
        return r;
    }
    let limit: usize = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(100);
    let offset: usize = q.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    let canvases = st.canvases.lock().unwrap().clone();
    let mut users: Vec<MockUser> = st.users.lock().unwrap().values().cloned().collect();
    users.sort_by(|a, b| a.subject.cmp(&b.subject));
    let total = users.len();
    let page: Vec<Value> = users
        .iter()
        .skip(offset)
        .take(limit)
        .map(|u| user_json(u, &canvases))
        .collect();
    Json(json!({ "users": page, "total": total })).into_response()
}

fn upsert(st: &MockState, subject: &str, body: &Value) -> (MockUser, bool) {
    let mut users = st.users.lock().unwrap();
    let created = !users.contains_key(subject);
    let user_id = users.get(subject).map_or_else(
        || {
            body["id"]
                .as_str()
                .map_or_else(|| subject.to_owned(), str::to_owned)
        },
        |u| u.user_id.clone(),
    );
    let u = MockUser {
        subject: subject.to_owned(),
        user_id,
        email: body["email"].as_str().unwrap_or_default().to_owned(),
        display_name: body["display_name"].as_str().map(str::to_owned),
        disabled: body["disabled"].as_bool().unwrap_or(false),
    };
    users.insert(subject.to_owned(), u.clone());
    (u, created)
}

async fn put_user(
    State(st): State<Arc<MockState>>,
    Path(subject): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = app_auth(&st, &headers) {
        return r;
    }
    st.user_puts.fetch_add(1, Ordering::SeqCst);
    if st.fail_provision.load(Ordering::SeqCst) {
        return err(500, "internal", "mock provisioning failure");
    }
    if st.sticky_unknown.load(Ordering::SeqCst) {
        return Json(json!({ "user_id": subject, "subject": subject, "created": true }))
            .into_response();
    }
    let (u, created) = upsert(&st, &subject, &body);
    Json(json!({
        "user_id": u.user_id, "subject": u.subject, "email": u.email,
        "display_name": u.display_name, "disabled": u.disabled, "created": created,
    }))
    .into_response()
}

async fn batch(
    State(st): State<Arc<MockState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = app_auth(&st, &headers) {
        return r;
    }
    st.batch_calls.fetch_add(1, Ordering::SeqCst);
    let mut results = Vec::new();
    for item in body["users"].as_array().cloned().unwrap_or_default() {
        let subject = item["subject"].as_str().unwrap_or_default().to_owned();
        let (u, created) = upsert(&st, &subject, &item);
        results.push(json!({ "subject": subject, "user_id": u.user_id, "created": created }));
    }
    Json(json!({ "results": results })).into_response()
}

async fn delete_user(
    State(st): State<Arc<MockState>>,
    Path(subject): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = app_auth(&st, &headers) {
        return r;
    }
    if st.users.lock().unwrap().remove(&subject).is_none() {
        return err(404, "not_found", "no such user");
    }
    st.canvases.lock().unwrap().retain(|c| c.owner != subject);
    st.deleted_users.lock().unwrap().push(subject);
    Json(json!({ "ok": true })).into_response()
}

async fn disconnect(
    State(st): State<Arc<MockState>>,
    Path(subject): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = app_auth(&st, &headers) {
        return r;
    }
    st.disconnects.lock().unwrap().push(subject);
    Json(json!({ "ok": true })).into_response()
}

async fn events(State(st): State<Arc<MockState>>, headers: HeaderMap) -> Response {
    if let Err(r) = app_auth(&st, &headers) {
        return r;
    }
    st.last_event_ids.lock().unwrap().push(
        headers
            .get("last-event-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    );
    let rx = st.events.subscribe();
    let live = futures::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok((id, event, data)) => {
                    let frame = format!("event: {event}\nid: {id}\ndata: {data}\n\n");
                    return Some((Ok::<Bytes, Infallible>(Bytes::from(frame)), rx));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    let hello = futures::stream::once(async {
        Ok::<Bytes, Infallible>(Bytes::from_static(b": hello\n\n"))
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(futures::StreamExt::chain(hello, live)))
        .unwrap()
}

fn canvas_json(c: &MockCanvas, users: &HashMap<String, MockUser>) -> Value {
    let owner = users.get(&c.owner);
    json!({
        "id": c.id, "name": c.name, "kind": c.kind,
        "owner": {
            "subject": c.owner,
            "user_id": owner.map_or(c.owner.as_str(), |u| u.user_id.as_str()),
            "email": owner.map(|u| u.email.clone()),
            "display_name": owner.and_then(|u| u.display_name.clone()),
        },
        "member_count": c.members.len(),
        "share_link_count": 0,
        "created_at": "2026-10-19T00:00:00Z",
        "updated_at": "2026-10-19T00:00:00Z",
    })
}

async fn app_canvases(
    State(st): State<Arc<MockState>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = app_auth(&st, &headers) {
        return r;
    }
    let limit: usize = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(100);
    let offset: usize = q.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    let kind = q.get("kind").filter(|k| !k.is_empty());
    let users = st.users.lock().unwrap().clone();
    let all: Vec<MockCanvas> = st
        .canvases
        .lock()
        .unwrap()
        .iter()
        .filter(|c| kind.is_none_or(|k| &c.kind == k))
        .cloned()
        .collect();
    let page: Vec<Value> = all
        .iter()
        .skip(offset)
        .take(limit)
        .map(|c| canvas_json(c, &users))
        .collect();
    Json(json!({ "canvases": page, "total": all.len() })).into_response()
}

async fn app_delete_canvas(
    State(st): State<Arc<MockState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = app_auth(&st, &headers) {
        return r;
    }
    let mut canvases = st.canvases.lock().unwrap();
    let before = canvases.len();
    canvases.retain(|c| c.id != id);
    if canvases.len() == before {
        return err(404, "not_found", "no such canvas");
    }
    Json(json!({ "ok": true })).into_response()
}
