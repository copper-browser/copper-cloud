//! Canvas link: copper-cloud's thin client for a linked **Canvas Cloud** server.
//!
//! When an instance admin links a Canvas Cloud (URL + app key, see [`settings`]), copper-cloud
//! serves the unchanged canvas API to Copper by proxying it there, acting as the signed-in
//! Copper user (`Authorization: Bearer <app key>` + `X-Canvas-User: <copper user id>`):
//!
//! * [`CanvasLink::router`] — `/v1/canvases…` (incl. the y-websocket pipe, see [`ws`]),
//!   `/v1/invites…`, `/v1/canvas-links/…`, `/v1/mentions…`, `/v1/people` (see [`proxy`]);
//! * [`bridge`] — Canvas Cloud's app event stream re-published as `canvas` events on each
//!   user's `/v1/sync/events`;
//! * [`provision`] — Copper users mirrored as Canvas Cloud app users (same ids), JIT on
//!   `unknown_user`, a reconcile loop and an outbox for failures;
//! * [`admin`] — the admin API / CLI / doctor helpers;
//! * [`landing`] — the `copper://` share-link landing page (`/join/{token}`);
//! * [`info_layer`] — adds `"canvas": {"linked": bool}` to `GET /v1/info`.
//!
//! Unlinked, every canvas route answers `503 canvas_unavailable`; Canvas Cloud unreachable,
//! `502 canvas_upstream`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Extension, Router};
use copper_cloud_core::error::{ApiError, ApiResult, ErrorLog};
use copper_cloud_core::state::SharedState;
use serde_json::json;
use time::OffsetDateTime;
use tokio::sync::{watch, Notify};
use uuid::Uuid;

pub mod admin;
pub mod bridge;
mod info;
pub mod landing;
pub mod provision;
mod proxy;
pub mod settings;
pub mod tls;
pub mod upstream;
pub mod ws;

pub use info::info_layer;
pub use settings::LinkSettings;
pub use upstream::{Stats, Upstream, UpstreamError};

/// Largest accepted REST request body (except ops) — same as the old canvas router.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Largest accepted `POST /canvases/{id}/ops` body (a 2 MiB `data:` image plus JSON).
pub const MAX_OPS_BODY_BYTES: usize = 4 << 20;
/// How long the decrypted settings are cached before re-reading the row (CLI edits are
/// picked up within this window; admin API edits immediately).
pub const SETTINGS_TTL: Duration = Duration::from_secs(5);

/// The link as currently configured: settings + a client built for them.
#[derive(Debug)]
pub struct Active {
    pub settings: LinkSettings,
    pub upstream: Upstream,
}

impl Active {
    pub fn new(settings: LinkSettings) -> anyhow::Result<Self> {
        let upstream = Upstream::new(&settings)?;
        Ok(Self { settings, upstream })
    }
}

/// Event-bridge status (admin API / doctor).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct BridgeStatus {
    pub connected: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_event_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
}

/// Last reachability check of the linked Canvas Cloud.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Health {
    pub reachable: Option<bool>,
    pub version: Option<String>,
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub checked_at: Option<OffsetDateTime>,
}

type Cached = (Option<Arc<Active>>, Instant);

struct Inner {
    state: SharedState,
    cache: RwLock<Option<Cached>>,
    refresh: tokio::sync::Mutex<()>,
    generation: watch::Sender<u64>,
    closing: watch::Sender<bool>,
    bridge: Mutex<BridgeStatus>,
    health: Mutex<Health>,
    outbox_kick: Notify,
    /// Per-user hash of what was last pushed to Canvas Cloud (reconcile).
    synced: Mutex<HashMap<Uuid, u64>>,
    /// `subject → canvas_count` from `/v1/apps/self/users`, briefly cached for the people list.
    counts: Mutex<Option<(HashMap<Uuid, i64>, Instant)>>,
    started: AtomicBool,
}

/// Handle to the canvas link of one copper-cloud process. Cheap to clone.
#[derive(Clone)]
pub struct CanvasLink {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for CanvasLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasLink").finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl CanvasLink {
    pub fn new(state: SharedState) -> Self {
        Self {
            inner: Arc::new(Inner {
                state,
                cache: RwLock::new(None),
                refresh: tokio::sync::Mutex::new(()),
                generation: watch::channel(0).0,
                closing: watch::channel(false).0,
                bridge: Mutex::new(BridgeStatus::default()),
                health: Mutex::new(Health::default()),
                outbox_kick: Notify::new(),
                synced: Mutex::new(HashMap::new()),
                counts: Mutex::new(None),
                started: AtomicBool::new(false),
            }),
        }
    }

    pub fn state(&self) -> &SharedState {
        &self.inner.state
    }

    /// The `/v1` canvas routes (mount under `/v1`, where the canvas router used to be, so the
    /// instance gate, tracing/metrics and rate limits run first). Any method.
    pub fn router(&self) -> Router<SharedState> {
        Router::new()
            .route("/canvases", any(proxy::proxy))
            .route("/canvases/{*rest}", any(proxy::proxy))
            .route("/people", any(proxy::proxy))
            .route("/invites", any(proxy::proxy))
            .route("/invites/{*rest}", any(proxy::proxy))
            .route("/canvas-links/{*rest}", any(proxy::proxy))
            .route("/mentions", any(proxy::proxy))
            .route("/mentions/{*rest}", any(proxy::proxy))
            .layer(Extension(self.clone()))
    }

    /// The public `copper://` landing page for share links (merge at the application root —
    /// outside `/v1`, so the instance gate does not run for it).
    pub fn landing_router() -> Router<SharedState> {
        Router::new().route("/join/{token}", get(landing::join_landing))
    }

    fn peek(&self) -> Option<Cached> {
        self.inner
            .cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    async fn load(&self, force: bool) -> ApiResult<Option<Arc<Active>>> {
        if !force {
            if let Some((v, at)) = self.peek() {
                if at.elapsed() < SETTINGS_TTL {
                    return Ok(v);
                }
            }
        }
        let _single = self.inner.refresh.lock().await;
        let prev = self.peek();
        if !force {
            if let Some((v, at)) = &prev {
                if at.elapsed() < SETTINGS_TTL {
                    return Ok(v.clone());
                }
            }
        }
        let state = &self.inner.state;
        let loaded = settings::load(&state.db, &state.crypto).await?;
        let prev_active = prev.and_then(|(v, _)| v);
        let next = match (loaded, &prev_active) {
            (None, _) => None,
            (Some(s), Some(p))
                if p.settings.same_target(&s)
                    && p.settings.enabled == s.enabled
                    && p.settings.updated_at == s.updated_at =>
            {
                Some(p.clone())
            }
            (Some(s), _) => Some(Arc::new(Active::new(s).map_err(ApiError::Internal)?)),
        };
        let changed = match (&prev_active, &next) {
            (None, None) => false,
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            _ => true,
        };
        *self
            .inner
            .cache
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some((next.clone(), Instant::now()));
        if changed {
            self.inner.generation.send_modify(|g| *g += 1);
            *lock(&self.inner.counts) = None;
            // A new target (or none) starts from scratch: everyone is pushed again.
            lock(&self.inner.synced).clear();
            if next.is_none() {
                *lock(&self.inner.health) = Health::default();
            }
        }
        Ok(next)
    }

    /// The stored link (enabled or not), cached for [`SETTINGS_TTL`].
    pub async fn current(&self) -> ApiResult<Option<Arc<Active>>> {
        self.load(false).await
    }

    /// The link if one is stored **and** enabled.
    pub async fn enabled(&self) -> ApiResult<Option<Arc<Active>>> {
        Ok(self.current().await?.filter(|a| a.settings.enabled))
    }

    /// Re-read the settings now (after this process changed them) and wake the background
    /// tasks.
    pub async fn reload(&self) -> ApiResult<Option<Arc<Active>>> {
        let v = self.load(true).await?;
        self.inner.outbox_kick.notify_one();
        Ok(v)
    }

    /// Bumps whenever the effective link changes (linked, unlinked, re-keyed).
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.generation.subscribe()
    }

    /// Close every proxied WebSocket with `1001` (server shutting down).
    pub fn close_all(&self) {
        self.inner.closing.send_replace(true);
    }

    pub(crate) fn closing(&self) -> watch::Receiver<bool> {
        self.inner.closing.subscribe()
    }

    pub fn bridge_status(&self) -> BridgeStatus {
        lock(&self.inner.bridge).clone()
    }

    pub(crate) fn update_bridge(&self, f: impl FnOnce(&mut BridgeStatus)) {
        f(&mut lock(&self.inner.bridge));
    }

    pub fn health(&self) -> Health {
        lock(&self.inner.health).clone()
    }

    pub(crate) fn set_health(&self, h: Health) {
        *lock(&self.inner.health) = h;
    }

    /// Ask the outbox worker to run now (after enqueueing work).
    pub fn kick_outbox(&self) {
        self.inner.outbox_kick.notify_one();
    }

    pub(crate) async fn outbox_wait(&self, max: Duration) {
        let _ = tokio::time::timeout(max, self.inner.outbox_kick.notified()).await;
    }

    pub(crate) fn synced(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, u64>> {
        lock(&self.inner.synced)
    }

    pub(crate) fn counts_cache(
        &self,
    ) -> std::sync::MutexGuard<'_, Option<(HashMap<Uuid, i64>, Instant)>> {
        lock(&self.inner.counts)
    }

    /// Start the background tasks once: the event bridge, the reconcile loop and the outbox
    /// worker. No-op outside a tokio runtime or when already started.
    pub fn start(&self) {
        if tokio::runtime::Handle::try_current().is_err()
            || self.inner.started.swap(true, Ordering::SeqCst)
        {
            return;
        }
        tokio::spawn(bridge::run(self.clone()));
        tokio::spawn(provision::reconcile_loop(self.clone()));
        tokio::spawn(provision::outbox_loop(self.clone()));
    }
}

/// `503 {"error":"canvas_unavailable"}` — no Canvas Cloud is linked.
pub fn unavailable() -> Response {
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "canvas_unavailable",
        "no Canvas Cloud is linked to this Copper Cloud",
    )
}

/// `502 {"error":"canvas_upstream"}` — the linked Canvas Cloud failed us.
pub fn upstream_failed(message: impl Into<String>) -> Response {
    error_response(StatusCode::BAD_GATEWAY, "canvas_upstream", message)
}

fn error_response(status: StatusCode, code: &'static str, message: impl Into<String>) -> Response {
    let message = message.into();
    let mut resp = (
        status,
        axum::Json(json!({ "error": code, "message": message })),
    )
        .into_response();
    resp.extensions_mut().insert(ErrorLog {
        code,
        message: Some(copper_cloud_core::error::loggable_message(&message)),
    });
    resp
}
