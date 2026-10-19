//! `/admin/api/addons*` — addon administration for the portal (contract: `docs/addons.md`,
//! `docs/admin-api.md`). Merged into [`crate::admin_api::router`], so the CSRF guard and
//! `Cache-Control: no-store` apply; every handler takes [`AdminSession`].
//!
//! | Route | |
//! |---|---|
//! | `GET /addons/slots` | the closed slot list |
//! | `GET /addons` | every addon (plain array, by slot then resolution order) |
//! | `POST /addons?slot=…` | upload a `.copperaddon` zip (raw body, `application/zip`) |
//! | `GET /addons/{id}` | one addon |
//! | `PATCH /addons/{id}` | `enabled`, `audience`, `people`, `settings`, `priority`, `current_version` |
//! | `DELETE /addons/{id}` | the addon and every version |
//! | `GET /addons/{id}/versions/{version}/bundle` | a version's served bundle |
//!
//! Every change writes `admin_audit` (in [`addons::store`]) and publishes the SSE `addons`
//! event to every connected Copper.

use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use copper_cloud_core::addons::{self, store, Actor, AddonError, Slot};
use copper_cloud_core::error::{ApiError, ApiResult};
use copper_cloud_core::extract::{check_content_length, read_body, JsonBody, QueryParams};
use copper_cloud_core::state::SharedState;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::admin_api::AdminSession;

/// JSON bodies (PATCH): the admin API's 16 KiB cap (settings are at most 16 KiB).
const BODY_LIMIT: usize = 16 * 1024;

/// Content types accepted for an upload (the essence, before any `;` parameters).
const ZIP_TYPES: &[&str] = &[
    "application/zip",
    "application/x-zip-compressed",
    "application/octet-stream",
];

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/addons", get(list).post(upload))
        .route("/addons/slots", get(slots))
        .route("/addons/{id}", get(get_one).patch(update).delete(remove))
        .route(
            "/addons/{id}/versions/{version}/bundle",
            get(version_bundle),
        )
}

fn actor(s: &AdminSession) -> Actor {
    Actor::Admin {
        id: s.admin_id,
        email: s.email.clone(),
    }
}

/// Addon id path segment; anything else is a 404.
fn addon_id(raw: &str) -> ApiResult<&str> {
    if addons::manifest::valid_id(raw) {
        Ok(raw)
    } else {
        Err(ApiError::NotFound)
    }
}

async fn slots(_s: AdminSession) -> Json<Value> {
    Json(addons::slots_view())
}

async fn list(State(state): State<SharedState>, _s: AdminSession) -> ApiResult<Response> {
    Ok(Json(store::list(&state.db).await?).into_response())
}

async fn get_one(
    State(state): State<SharedState>,
    _s: AdminSession,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    Ok(Json(store::get(&state.db, addon_id(&id)?).await?).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct UploadQuery {
    slot: Option<String>,
}

fn is_zip_content_type(req: &Request) -> bool {
    req.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        })
        .is_some_and(|essence| ZIP_TYPES.contains(&essence.as_str()))
}

async fn upload(
    State(state): State<SharedState>,
    s: AdminSession,
    QueryParams(q): QueryParams<UploadQuery>,
    req: Request,
) -> Result<Response, AddonError> {
    let slot_raw = q
        .slot
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "?slot= is required (the slot this addon replaces: {})",
                Slot::known_list()
            ))
        })?;
    let slot = Slot::parse(slot_raw).ok_or_else(|| {
        ApiError::bad_request(format!(
            "unknown slot \"{slot_raw}\" (known: {})",
            Slot::known_list()
        ))
    })?;
    if !is_zip_content_type(&req) {
        return Err(AddonError::UnsupportedMediaType);
    }
    let limit = state.cfg.limits.max_addon_bytes;
    check_content_length(&req, limit)?;
    let zip = read_body(req.into_body(), limit).await?;
    let built = tokio::task::spawn_blocking(move || addons::bundle::ingest_zip(&zip)).await??;
    let (outcome, view) = store::upload(&state.db, &actor(&s), slot, &built).await?;
    if outcome != store::UploadOutcome::Unchanged {
        addons::notify(&state.events);
    }
    tracing::info!(
        admin_id = %s.admin_id,
        addon = %view.id,
        version = %built.manifest.version,
        outcome = ?outcome,
        "addon uploaded"
    );
    let status = if outcome == store::UploadOutcome::Created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let mut body = serde_json::to_value(&view).map_err(ApiError::internal)?;
    body["upload"] = json!(outcome);
    Ok((status, Json(body)).into_response())
}

async fn update(
    State(state): State<SharedState>,
    s: AdminSession,
    Path(id): Path<String>,
    JsonBody(patch): JsonBody<store::Patch, BODY_LIMIT>,
) -> ApiResult<Response> {
    let view = store::update(&state.db, &actor(&s), addon_id(&id)?, &patch).await?;
    addons::notify(&state.events);
    tracing::info!(admin_id = %s.admin_id, addon = %view.id, "addon updated");
    Ok(Json(view).into_response())
}

async fn remove(
    State(state): State<SharedState>,
    s: AdminSession,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    store::delete(&state.db, &actor(&s), addon_id(&id)?).await?;
    addons::notify(&state.events);
    tracing::info!(admin_id = %s.admin_id, addon = %id, "addon deleted");
    Ok(Json(json!({ "ok": true })))
}

async fn version_bundle(
    State(state): State<SharedState>,
    _s: AdminSession,
    Path((id, version)): Path<(String, String)>,
) -> ApiResult<Response> {
    let bytes = store::version_bundle(&state.db, addon_id(&id)?, &version).await?;
    let mut resp = bytes.into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(resp)
}
