//! An in-process mock Canvas Cloud implementing just the app endpoints the migration uses:
//! `GET /v1/apps/self/stats`, `POST /v1/apps/self/users:batch`, `POST /v1/apps/self/import`
//! and `GET /v1/apps/self/canvases/{id}/digest`.
//!
//! The import handler really streams and parses the NDJSON body line by line, validates the
//! order and the subjects, applies every update to a fresh `yrs::Doc`, stores one compacted
//! state (like canvas-cloud's single snapshot) and answers with the §6.3 digest. The digest
//! endpoint reloads the stored state into a fresh `Doc`, so "import digest == stored digest"
//! also exercises compaction invariance.

#![allow(dead_code, clippy::missing_panics_doc, clippy::too_many_lines)]

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use futures::StreamExt as _;
use serde_json::{json, Value};
use uuid::Uuid;
use yrs::updates::decoder::Decode as _;
use yrs::{Doc, ReadTxn as _, StateVector, Transact as _, Update};

use super::digest;

pub const APP_KEY: &str = "cva_mock_app_key";

/// One import request as the mock parsed it.
#[derive(Debug, Clone, Default)]
pub struct Received {
    pub canvas: Value,
    pub members: Vec<Value>,
    pub invites: Vec<Value>,
    pub links: Vec<Value>,
    pub mentions: Vec<Value>,
    pub updates: usize,
    pub end: Option<Value>,
    pub mode: String,
    /// Status the mock answered.
    pub status: u16,
    pub error: Option<String>,
}

/// A stored canvas.
#[derive(Debug, Clone)]
pub struct Stored {
    pub state: Vec<u8>,
    pub owner: String,
    pub kind: String,
    pub imported: bool,
    pub empty: bool,
    pub received: Received,
}

#[derive(Debug)]
pub struct MockState {
    /// subject → user id
    pub users: HashMap<String, Uuid>,
    /// Every user payload `users:batch` received.
    pub user_payloads: Vec<Value>,
    pub canvases: HashMap<Uuid, Stored>,
    pub imports: Vec<Received>,
    pub stats_gets: usize,
    pub batch_posts: usize,
    pub import_posts: usize,
    pub digest_gets: usize,
    // Knobs.
    pub allow_import: bool,
    /// `GET …/digest` lies for these canvases.
    pub corrupt_digest: HashSet<Uuid>,
    /// `users:batch` maps these subjects to another user id.
    pub remap_user: HashMap<String, Uuid>,
    /// `users:batch` claims success for these subjects but does not keep them.
    pub forget_subjects: HashSet<String>,
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            users: HashMap::new(),
            user_payloads: Vec::new(),
            canvases: HashMap::new(),
            imports: Vec::new(),
            stats_gets: 0,
            batch_posts: 0,
            import_posts: 0,
            digest_gets: 0,
            allow_import: true,
            corrupt_digest: HashSet::new(),
            remap_user: HashMap::new(),
            forget_subjects: HashSet::new(),
        }
    }
}

type Shared = Arc<Mutex<MockState>>;

pub struct Mock {
    pub addr: SocketAddr,
    pub url: url::Url,
    pub state: Shared,
}

impl Mock {
    pub async fn start() -> Self {
        let state: Shared = Arc::new(Mutex::new(MockState::default()));
        let app = Router::new()
            .route("/v1/apps/self/stats", get(stats))
            .route("/v1/apps/self/users:batch", post(users_batch))
            .route("/v1/apps/self/import", post(import))
            .route("/v1/apps/self/canvases/{id}/digest", get(canvas_digest))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            addr,
            url: url::Url::parse(&format!("http://{addr}/")).unwrap(),
            state,
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut MockState) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }

    /// Gives `owner` an existing Personal canvas (as canvas-cloud's `ensure_personal_canvas`
    /// would), empty or with content.
    pub fn seed_personal(&self, owner: Uuid, with_content: bool) -> Uuid {
        let doc = Doc::new();
        if with_content {
            let meta = doc.get_or_insert_map("meta");
            let mut txn = doc.transact_mut();
            yrs::Map::insert(&meta, &mut txn, "name", "Personal (canvas-cloud)");
        }
        let state = doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        let id = Uuid::new_v4();
        self.with(|s| {
            s.canvases.insert(
                id,
                Stored {
                    state,
                    owner: owner.to_string(),
                    kind: "personal".into(),
                    imported: false,
                    empty: !with_content,
                    received: Received::default(),
                },
            );
        });
        id
    }

    /// The digest the mock stores for `id`, recomputed from its compacted state.
    pub fn stored_digest(&self, id: Uuid) -> Option<String> {
        self.with(|s| s.canvases.get(&id).map(|c| state_digest(&c.state)))
    }

    pub fn received(&self, id: Uuid) -> Option<Received> {
        self.with(|s| s.canvases.get(&id).map(|c| c.received.clone()))
    }
}

/// Digest of a stored (compacted) state, reloaded into a fresh document.
pub fn state_digest(state: &[u8]) -> String {
    let doc = Doc::new();
    {
        let mut txn = doc.transact_mut();
        txn.apply_update(Update::decode_v1(state).unwrap()).unwrap();
    }
    digest::digest(&doc)
}

fn err(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({"error": code, "message": message}))).into_response()
}

fn authorized(headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {APP_KEY}"))
        && headers.get("x-canvas-user").is_none()
}

async fn stats(State(state): State<Shared>, headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return err(StatusCode::UNAUTHORIZED, "unauthorized", "bad app key");
    }
    let mut s = state.lock().unwrap();
    s.stats_gets += 1;
    Json(json!({
        "users": s.users.len(),
        "canvases": s.canvases.len(),
        "rooms": {"rooms": 0, "peers": 0},
    }))
    .into_response()
}

async fn users_batch(
    State(state): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return err(StatusCode::UNAUTHORIZED, "unauthorized", "bad app key");
    }
    let mut s = state.lock().unwrap();
    s.batch_posts += 1;
    let users = body["users"].as_array().cloned().unwrap_or_default();
    if users.len() > 1000 {
        return err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "at most 1000 users per batch",
        );
    }
    let mut results = Vec::new();
    for u in users {
        s.user_payloads.push(u.clone());
        let subject = u["subject"].as_str().unwrap_or_default().to_owned();
        let hint = u["id"].as_str().and_then(|i| Uuid::parse_str(i).ok());
        let created = !s.users.contains_key(&subject);
        let user_id = s
            .remap_user
            .get(&subject)
            .copied()
            .or_else(|| s.users.get(&subject).copied())
            .or(hint)
            .unwrap_or_else(Uuid::now_v7);
        if !s.forget_subjects.contains(&subject) {
            s.users.insert(subject.clone(), user_id);
        }
        results.push(json!({
            "subject": subject,
            "user_id": user_id,
            "email": u["email"],
            "display_name": u["display_name"],
            "disabled": u["disabled"],
            "created": created,
        }));
    }
    Json(json!({ "results": results })).into_response()
}

#[derive(serde::Deserialize)]
struct ImportQuery {
    mode: Option<String>,
}

/// Why an import is refused (status, code, message).
type Refusal = (StatusCode, &'static str, String);

struct Importer {
    doc: Doc,
    got: Received,
    users: HashSet<String>,
    phase: u8,
    failure: Option<Refusal>,
}

impl Importer {
    fn subject(&mut self, v: &Value) -> Result<(), Refusal> {
        match v {
            Value::Null => Ok(()),
            Value::String(s) if self.users.contains(s) => Ok(()),
            other => Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                "unknown_subject",
                format!("subject {other} is not provisioned for this app"),
            )),
        }
    }

    fn line(&mut self, raw: &[u8]) -> Result<(), Refusal> {
        let bad = |m: String| (StatusCode::BAD_REQUEST, "bad_request", m);
        let v: Value = serde_json::from_slice(raw).map_err(|e| bad(format!("bad line: {e}")))?;
        let kind = v["type"].as_str().unwrap_or_default().to_owned();
        let order = match kind.as_str() {
            "canvas" => 0,
            "member" => 1,
            "invite" => 2,
            "link" => 3,
            "mention" => 4,
            "update" => 5,
            "end" => 6,
            other => return Err(bad(format!("unknown line type {other:?}"))),
        };
        if self.got.end.is_some()
            || (self.phase == 0 && order != 0)
            || order < self.phase
            || (order == 0 && self.got.canvas != Value::Null)
        {
            return Err(bad(format!("line {kind:?} out of order")));
        }
        self.phase = order.max(1);
        match kind.as_str() {
            "canvas" => {
                if v["version"] != 1 {
                    return Err(bad("unsupported version".into()));
                }
                Uuid::parse_str(v["id"].as_str().unwrap_or_default())
                    .map_err(|_| bad("bad canvas id".into()))?;
                self.subject(&v["owner"])?;
                self.got.canvas = v;
            }
            "member" => {
                self.subject(&v["subject"])?;
                self.got.members.push(v);
            }
            "invite" => {
                self.subject(&v["invited_by"])?;
                self.got.invites.push(v);
            }
            "link" => {
                self.subject(&v["created_by"])?;
                let hex = v["token_sha256_hex"].as_str().unwrap_or_default();
                if hex.len() != 64 {
                    return Err(bad("token_sha256_hex must be 64 hex digits".into()));
                }
                self.got.links.push(v);
            }
            "mention" => {
                self.subject(&v["from"])?;
                self.subject(&v["to"])?;
                self.got.mentions.push(v);
            }
            "update" => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(v["b64"].as_str().unwrap_or_default())
                    .map_err(|e| bad(format!("bad base64: {e}")))?;
                let update = Update::decode_v1(&bytes).map_err(|e| {
                    (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "bad_update",
                        format!("undecodable update: {e}"),
                    )
                })?;
                self.doc.transact_mut().apply_update(update).map_err(|e| {
                    (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "bad_update",
                        format!("update does not apply: {e}"),
                    )
                })?;
                self.got.updates += 1;
            }
            _ => {
                let counts = [
                    ("updates", self.got.updates),
                    ("members", self.got.members.len()),
                    ("invites", self.got.invites.len()),
                    ("links", self.got.links.len()),
                    ("mentions", self.got.mentions.len()),
                ];
                for (k, n) in counts {
                    if v[k].as_u64() != Some(n as u64) {
                        return Err(bad(format!("end.{k} = {} but {n} received", v[k])));
                    }
                }
                self.got.end = Some(v);
            }
        }
        Ok(())
    }
}

async fn import(
    State(state): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<ImportQuery>,
    body: Body,
) -> Response {
    let ok_key = authorized(&headers);
    let (allow, users) = {
        let mut s = state.lock().unwrap();
        s.import_posts += 1;
        (
            s.allow_import,
            s.users.keys().cloned().collect::<HashSet<_>>(),
        )
    };
    let mode = q.mode.unwrap_or_else(|| "create".into());
    let mut imp = Importer {
        doc: Doc::new(),
        got: Received {
            mode: mode.clone(),
            ..Received::default()
        },
        users,
        phase: 0,
        failure: None,
    };
    if headers.get("content-type").and_then(|v| v.to_str().ok()) != Some("application/x-ndjson") {
        imp.failure = Some((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "bad_request",
            "expected application/x-ndjson".into(),
        ));
    }
    // Read (and parse) the whole body line by line even after a failure, so the client always
    // gets the response rather than a reset connection.
    let mut stream = body.into_data_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut broken = None;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                broken = Some(e.to_string());
                break;
            }
        };
        buf.extend_from_slice(&chunk);
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            if imp.failure.is_none() && ok_key && allow {
                if let Err(f) = imp.line(&line[..line.len() - 1]) {
                    imp.failure = Some(f);
                }
            }
        }
    }
    if let Some(e) = broken {
        // The client aborted the body: nothing is committed.
        let mut s = state.lock().unwrap();
        imp.got.status = 400;
        imp.got.error = Some(format!("body aborted: {e}"));
        s.imports.push(imp.got);
        return err(StatusCode::BAD_REQUEST, "bad_request", "body aborted");
    }
    if !ok_key {
        return err(StatusCode::UNAUTHORIZED, "unauthorized", "bad app key");
    }
    if !allow {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "this app lacks allow_import",
        );
    }
    if imp.failure.is_none() && !buf.is_empty() {
        imp.failure = Some((
            StatusCode::BAD_REQUEST,
            "bad_request",
            "trailing bytes without newline".into(),
        ));
    }
    if imp.failure.is_none() && imp.got.end.is_none() {
        imp.failure = Some((
            StatusCode::BAD_REQUEST,
            "bad_request",
            "truncated: no end line".into(),
        ));
    }
    let mut s = state.lock().unwrap();
    if let Some((status, code, message)) = imp.failure.take() {
        imp.got.status = status.as_u16();
        imp.got.error = Some(format!("{code}: {message}"));
        s.imports.push(imp.got);
        return err(status, code, &message);
    }
    let id = Uuid::parse_str(imp.got.canvas["id"].as_str().unwrap()).unwrap();
    let owner = imp.got.canvas["owner"].as_str().unwrap().to_owned();
    let kind = imp.got.canvas["kind"].as_str().unwrap().to_owned();
    let replace = mode == "replace";

    if let Some(existing) = s.canvases.get(&id) {
        if !replace {
            let d = state_digest(&existing.state);
            imp.got.status = 409;
            s.imports.push(imp.got);
            return (
                StatusCode::CONFLICT,
                Json(json!({"error": "canvas_exists", "message": "a canvas with this id exists", "digest": d})),
            )
                .into_response();
        }
        if !existing.imported {
            imp.got.status = 409;
            s.imports.push(imp.got);
            return err(
                StatusCode::CONFLICT,
                "not_imported",
                "only imported canvases can be replaced",
            );
        }
    }
    if kind == "personal" {
        let other = s
            .canvases
            .iter()
            .find(|(cid, c)| **cid != id && c.kind == "personal" && c.owner == owner)
            .map(|(cid, c)| (*cid, c.empty));
        if let Some((other_id, empty)) = other {
            if !empty && !replace {
                imp.got.status = 409;
                s.imports.push(imp.got);
                return err(
                    StatusCode::CONFLICT,
                    "personal_exists",
                    "the owner already has a non-empty Personal canvas",
                );
            }
            s.canvases.remove(&other_id);
        }
    }
    let status = if s.canvases.contains_key(&id) {
        "replaced"
    } else {
        "created"
    };
    let state_bytes = imp
        .doc
        .transact()
        .encode_state_as_update_v1(&StateVector::default());
    let d = digest::digest(&imp.doc);
    imp.got.status = 200;
    let got = imp.got;
    let body = json!({
        "canvas_id": id,
        "status": status,
        "updates_applied": got.updates,
        "members": got.members.len(),
        "invites": got.invites.len(),
        "links": got.links.len(),
        "mentions": got.mentions.len(),
        "digest": d,
    });
    s.imports.push(got.clone());
    s.canvases.insert(
        id,
        Stored {
            state: state_bytes,
            owner,
            kind,
            imported: true,
            empty: got.updates == 0,
            received: got,
        },
    );
    Json(body).into_response()
}

async fn canvas_digest(
    State(state): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    if !authorized(&headers) {
        return err(StatusCode::UNAUTHORIZED, "unauthorized", "bad app key");
    }
    let (stored, corrupt) = {
        let mut s = state.lock().unwrap();
        s.digest_gets += 1;
        (
            s.canvases.get(&id).map(|c| c.state.clone()),
            s.corrupt_digest.contains(&id),
        )
    };
    let Some(stored) = stored else {
        return err(StatusCode::NOT_FOUND, "not_found", "no such canvas");
    };
    let d = if corrupt {
        "0".repeat(64)
    } else {
        state_digest(&stored)
    };
    Json(json!({"canvas_id": id, "digest": d})).into_response()
}
