//! Admin API / CLI / doctor helpers: link status and changes (audited), the canvas listing and
//! deletion, overview stats and per-user canvas counts — all sourced from Canvas Cloud's
//! `/v1/apps/self/*`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::response::{IntoResponse, Response};
use copper_cloud_core::error::{ApiError, ApiResult};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::settings::{self, NewLink};
use crate::upstream::{Probe, Stats, Upstream, UpstreamError, PROBE_TIMEOUT};
use crate::{upstream_failed, Active, CanvasLink, Health};

/// A cached reachability result older than this is refreshed by `GET /admin/api/canvas-link`.
pub const HEALTH_TTL: Duration = Duration::from_secs(15);
/// How long per-user canvas counts are reused for the people list.
pub const COUNTS_TTL: Duration = Duration::from_secs(30);

/// Who changed the link (audit + `updated_by`).
#[derive(Clone, Debug)]
pub enum Actor {
    Admin { id: Uuid, email: String },
    Cli,
}

impl Actor {
    fn label(&self) -> &str {
        match self {
            Self::Admin { email, .. } => email,
            Self::Cli => "cli",
        }
    }

    fn admin_id(&self) -> Option<Uuid> {
        match self {
            Self::Admin { id, .. } => Some(*id),
            Self::Cli => None,
        }
    }
}

/// Error of a link-backed admin call.
#[derive(Debug)]
pub enum LinkError {
    Api(ApiError),
    /// No Canvas Cloud linked (`503 canvas_unavailable`).
    Unlinked,
    /// Canvas Cloud failed (`502 canvas_upstream`).
    Upstream(String),
}

impl From<ApiError> for LinkError {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

impl From<UpstreamError> for LinkError {
    fn from(e: UpstreamError) -> Self {
        Self::Upstream(e.to_string())
    }
}

impl IntoResponse for LinkError {
    fn into_response(self) -> Response {
        match self {
            Self::Api(e) => e.into_response(),
            Self::Unlinked => crate::unavailable(),
            Self::Upstream(msg) => upstream_failed(msg),
        }
    }
}

/// Write an `admin_audit` row (`admin_id` NULL for the CLI, with `detail.via = "cli"`).
pub async fn audit(
    db: &sqlx::PgPool,
    actor: &Actor,
    action: &str,
    target: &str,
    detail: Value,
) -> ApiResult<()> {
    let mut detail = detail;
    if let (Actor::Cli, Value::Object(map)) = (actor, &mut detail) {
        map.insert("via".into(), Value::from("cli"));
    }
    sqlx::query(
        "INSERT INTO admin_audit (admin_id, action, target, detail) VALUES ($1, $2, $3, $4)",
    )
    .bind(actor.admin_id())
    .bind(action)
    .bind(target.chars().take(256).collect::<String>())
    .bind(detail)
    .execute(db)
    .await?;
    tracing::info!(
        actor = actor.label(),
        action,
        audit_target = target,
        "admin action"
    );
    Ok(())
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// `{migrated, failed, legacy_canvases}` from the migration ledger and the legacy table.
pub async fn migrations_summary(db: &sqlx::PgPool) -> ApiResult<Value> {
    let (migrated, failed, legacy): (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM canvas_migrations WHERE status = 'migrated'),
                (SELECT count(*) FROM canvas_migrations WHERE status = 'failed'),
                (SELECT count(*) FROM canvases)",
    )
    .fetch_one(db)
    .await?;
    Ok(json!({ "migrated": migrated, "failed": failed, "legacy_canvases": legacy }))
}

fn health_from(result: &Result<Probe, UpstreamError>) -> Health {
    match result {
        Ok(p) => Health {
            reachable: Some(true),
            version: p.version.clone(),
            error: None,
            checked_at: Some(OffsetDateTime::now_utc()),
        },
        Err(e) => Health {
            reachable: Some(false),
            version: None,
            error: Some(e.to_string()),
            checked_at: Some(OffsetDateTime::now_utc()),
        },
    }
}

/// Probe the linked Canvas Cloud now and remember the result.
pub async fn check(link: &CanvasLink, active: &Active) -> Result<Probe, UpstreamError> {
    let result = active.upstream.check(PROBE_TIMEOUT).await;
    link.set_health(health_from(&result));
    result
}

/// The `GET /admin/api/canvas-link` body (re-probes when the last check is stale).
pub async fn view(link: &CanvasLink) -> ApiResult<Value> {
    let db = &link.state().db;
    let current = link.current().await?;
    let migrations = migrations_summary(db).await?;
    let Some(active) = current else {
        return Ok(json!({
            "linked": false,
            "url": null,
            "enabled": false,
            "tls_fingerprint": null,
            "linked_at": null,
            "updated_at": null,
            "updated_by": null,
            "status": { "reachable": null, "version": null, "error": null, "checked_at": null },
            "bridge": { "connected": false, "last_event_at": null, "last_error": null },
            "migrations": migrations,
        }));
    };
    let stale = link.health().checked_at.is_none_or(|at| {
        (OffsetDateTime::now_utc() - at) > time::Duration::try_from(HEALTH_TTL).unwrap_or_default()
    });
    if stale {
        let _ = check(link, &active).await;
    }
    let s = &active.settings;
    Ok(json!({
        "linked": true,
        "url": s.base(),
        "enabled": s.enabled,
        "tls_fingerprint": s.tls_fingerprint,
        "linked_at": rfc3339(s.linked_at),
        "updated_at": rfc3339(s.updated_at),
        "updated_by": s.updated_by,
        "status": link.health(),
        "bridge": link.bridge_status(),
        "migrations": migrations,
    }))
}

fn probe_failure(url: &str, err: &UpstreamError) -> ApiError {
    let msg = match err {
        UpstreamError::Transport(m) => format!("could not reach Canvas Cloud at {url}: {m}"),
        UpstreamError::Status { status, .. }
            if *status == StatusCode::UNAUTHORIZED || *status == StatusCode::FORBIDDEN =>
        {
            format!("Canvas Cloud at {url} rejected the app key ({err})")
        }
        other => format!("Canvas Cloud at {url} did not pass the check ({other})"),
    };
    ApiError::bad_request(msg)
}

/// Validate, test (`/v1/info` + `/v1/apps/self/stats`) and store a link; audited as
/// `canvas_link.set`. Users are provisioned in the background by the reconcile loop (the
/// CLI provisions them itself unless `--no-sync`). Returns the stored settings.
pub async fn set(
    link: &CanvasLink,
    actor: &Actor,
    url: &str,
    app_key: &str,
    tls_fingerprint: Option<&str>,
) -> ApiResult<std::sync::Arc<Active>> {
    let input = NewLink::parse(url, app_key, tls_fingerprint)?;
    let base = settings::base_string(&input.url);
    let probe_settings = settings::LinkSettings {
        url: input.url.clone(),
        app_key: input.app_key.clone(),
        tls_fingerprint: input.tls_fingerprint.clone(),
        enabled: true,
        linked_at: OffsetDateTime::now_utc(),
        updated_at: OffsetDateTime::now_utc(),
        updated_by: None,
    };
    let upstream = Upstream::new(&probe_settings).map_err(ApiError::Internal)?;
    let probe = upstream
        .check(PROBE_TIMEOUT)
        .await
        .map_err(|e| probe_failure(&base, &e))?;
    let state = link.state();
    settings::save(&state.db, &state.crypto, &input, actor.label()).await?;
    audit(
        &state.db,
        actor,
        "canvas_link.set",
        &base,
        json!({
            "url": base,
            "tls_fingerprint": input.tls_fingerprint,
            "key_last4": settings::last4(&input.app_key),
            "version": probe.version,
        }),
    )
    .await?;
    let active = link
        .reload()
        .await?
        .ok_or_else(|| ApiError::internal("canvas link vanished after save"))?;
    link.set_health(health_from(&Ok(probe)));
    Ok(active)
}

/// Remove the link (`canvas_link.remove`). Returns the URL that was linked.
pub async fn remove(link: &CanvasLink, actor: &Actor) -> ApiResult<Option<String>> {
    let db = &link.state().db;
    let url = settings::remove(db).await?;
    if let Some(u) = &url {
        audit(db, actor, "canvas_link.remove", u, json!({ "url": u })).await?;
    }
    link.reload().await?;
    Ok(url)
}

/// `POST /admin/api/canvas-link/test` body (`canvas_link.test` audited).
pub async fn test(link: &CanvasLink, actor: &Actor) -> ApiResult<Value> {
    let Some(active) = link.current().await? else {
        return Ok(json!({
            "ok": false, "version": null, "latency_ms": null,
            "error": "no Canvas Cloud is linked",
        }));
    };
    let result = check(link, &active).await;
    let body = match &result {
        Ok(p) => json!({
            "ok": true,
            "version": p.version,
            "latency_ms": u64::try_from(p.latency.as_millis()).unwrap_or(u64::MAX),
            "error": null,
        }),
        Err(e) => {
            json!({ "ok": false, "version": null, "latency_ms": null, "error": e.to_string() })
        }
    };
    audit(
        &link.state().db,
        actor,
        "canvas_link.test",
        &active.settings.base(),
        json!({ "ok": body["ok"], "error": body["error"] }),
    )
    .await?;
    Ok(body)
}

/// Canvas Cloud stats for the overview (`None` when unlinked or unreachable).
pub async fn stats(link: &CanvasLink) -> Option<Stats> {
    let active = link.enabled().await.ok()??;
    active.upstream.stats(PROBE_TIMEOUT).await.ok()
}

/// Copper user id → canvas count (`None` when unlinked or unreachable). Cached briefly.
pub async fn canvas_counts(link: &CanvasLink) -> Option<HashMap<Uuid, i64>> {
    if let Some((counts, at)) = link.counts_cache().as_ref() {
        if at.elapsed() < COUNTS_TTL {
            return Some(counts.clone());
        }
    }
    let active = link.enabled().await.ok()??;
    let remote = crate::provision::remote_users(&active.upstream, PROBE_TIMEOUT)
        .await
        .ok()?;
    let counts: HashMap<Uuid, i64> = remote
        .into_iter()
        .filter_map(|(s, n)| Some((s.parse::<Uuid>().ok()?, n)))
        .collect();
    *link.counts_cache() = Some((counts.clone(), Instant::now()));
    Some(counts)
}

/// One row of the admin canvas list (`GET /admin/api/canvases`), the shape the portal knows.
fn admin_canvas(c: &Value) -> Value {
    let owner = c.get("owner").cloned().unwrap_or(Value::Null);
    let owner_id = owner
        .get("user_id")
        .cloned()
        .filter(|v| !v.is_null())
        .or_else(|| owner.get("subject").cloned())
        .unwrap_or(Value::Null);
    json!({
        "id": c.get("id"),
        "name": c.get("name"),
        "kind": c.get("kind"),
        "owner_id": owner_id,
        "owner_email": owner.get("email"),
        "member_count": c.get("member_count").and_then(Value::as_i64).unwrap_or(0),
        "share_link_count": c.get("share_link_count").and_then(Value::as_i64).unwrap_or(0),
        "created_at": c.get("created_at"),
        "updated_at": c.get("updated_at"),
    })
}

/// Canvases owned by Copper users, from `GET /v1/apps/self/canvases`. `Ok(None)` = unlinked.
pub async fn list_canvases(
    link: &CanvasLink,
    kind: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Option<(Vec<Value>, i64)>, LinkError> {
    let Some(active) = link.enabled().await? else {
        return Ok(None);
    };
    let mut path = format!("/v1/apps/self/canvases?limit={limit}&offset={offset}");
    if let Some(k) = kind {
        path.push_str("&kind=");
        path.push_str(k);
    }
    let resp: Value = Upstream::json(active.upstream.request(Method::GET, &path)).await?;
    let items: Vec<Value> = resp
        .get("canvases")
        .or_else(|| resp.get("items"))
        .and_then(Value::as_array)
        .map(|a| a.iter().map(admin_canvas).collect())
        .unwrap_or_default();
    let total = resp
        .get("total")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| i64::try_from(items.len()).unwrap_or(i64::MAX));
    Ok(Some((items, total)))
}

/// Delete one canvas on Canvas Cloud (`DELETE /v1/apps/self/canvases/{id}`; Canvas Cloud
/// closes its room with 4404 and notifies members). `Ok(None)` = not found; the returned
/// value is the canvas's name when Canvas Cloud reports it.
pub async fn delete_canvas(
    link: &CanvasLink,
    id: Uuid,
) -> Result<Option<Option<String>>, LinkError> {
    let Some(active) = link.enabled().await? else {
        return Err(LinkError::Unlinked);
    };
    let path = format!("/v1/apps/self/canvases/{id}");
    match Upstream::json::<Value>(active.upstream.request(Method::DELETE, &path)).await {
        Ok(v) => Ok(Some(
            v.get("name").and_then(Value::as_str).map(str::to_owned),
        )),
        Err(e) if e.status() == Some(StatusCode::NOT_FOUND) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
