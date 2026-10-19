//! Provisioning (CONTRACT §7.4): every Copper user exists on Canvas Cloud as an app user whose
//! subject **and** id are the Copper `users.id`, so member lists, chat `authorId`s and mention
//! ids inside documents stay valid.
//!
//! * [`sync_all`] — bulk `users:batch` (on link, `canvas sync-users`, and the first reconcile).
//! * [`provision_user`] — one `PUT …/users/{subject}` (JIT after `401 unknown_user`).
//! * [`reconcile_loop`] — every 10 min (and whenever the link changes): push users whose
//!   email / display name / disabled flag changed (a hash per user is kept in memory),
//!   disconnect newly disabled users, delete app users whose Copper account is gone.
//! * The outbox (`canvas_link_outbox`): admin edits/deletes enqueue work in their own
//!   transaction ([`enqueue_user_changed`], [`enqueue_user_deleted`]); [`outbox_loop`] pushes
//!   it and retries failures with backoff, so nothing is lost while Canvas Cloud is down.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash as _, Hasher as _};
use std::time::Duration;

use copper_cloud_core::error::ApiResult;
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::upstream::{Upstream, UpstreamError};
use crate::{Active, CanvasLink};

/// Reconcile period.
pub const RECONCILE_EVERY: Duration = Duration::from_secs(600);
/// Idle outbox poll (work is also kicked right after it is enqueued).
pub const OUTBOX_POLL: Duration = Duration::from_secs(30);
/// Users per `users:batch` call (Canvas Cloud's limit).
pub const BATCH: usize = 1000;
/// Outbox items are dropped (with a warning) after this many failed attempts.
const MAX_ATTEMPTS: i32 = 50;
/// Page size when listing Canvas Cloud's app users.
const LIST_PAGE: i64 = 500;
/// Safety bound on pages listed per pass.
const MAX_PAGES: i64 = 1000;

/// Outbox kinds.
pub mod kind {
    pub const UPSERT_USER: &str = "upsert_user";
    pub const DELETE_USER: &str = "delete_user";
    pub const DISCONNECT_USER: &str = "disconnect_user";
}

/// A Copper account as mirrored to Canvas Cloud.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct CopperUser {
    pub id: Uuid,
    pub email: String,
    pub display_name: Option<String>,
    pub disabled: bool,
}

impl CopperUser {
    fn digest(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.email.hash(&mut h);
        self.display_name.hash(&mut h);
        self.disabled.hash(&mut h);
        h.finish()
    }

    fn upsert_body(&self) -> Value {
        json!({
            "email": self.email,
            "display_name": self.display_name,
            "disabled": self.disabled,
            "id": self.id,
        })
    }

    fn batch_item(&self) -> Value {
        json!({
            "subject": self.id.to_string(),
            "email": self.email,
            "display_name": self.display_name,
            "disabled": self.disabled,
            "id": self.id,
        })
    }
}

/// Copper stores a missing display name as `''`; Canvas Cloud takes `null`.
const USER_SQL: &str =
    "SELECT id, email, NULLIF(display_name, '') AS display_name, disabled FROM users";

async fn copper_user(db: &sqlx::PgPool, id: Uuid) -> ApiResult<Option<CopperUser>> {
    Ok(
        sqlx::query_as::<_, CopperUser>(&format!("{USER_SQL} WHERE id = $1"))
            .bind(id)
            .fetch_optional(db)
            .await?,
    )
}

async fn copper_users(db: &sqlx::PgPool) -> ApiResult<Vec<CopperUser>> {
    Ok(
        sqlx::query_as::<_, CopperUser>(&format!("{USER_SQL} ORDER BY created_at, id"))
            .fetch_all(db)
            .await?,
    )
}

fn subject_path(id: Uuid) -> String {
    format!("/v1/apps/self/users/{id}")
}

/// Mark `users` as pushed (reconcile bookkeeping).
fn remember(link: &CanvasLink, users: &[&CopperUser]) {
    let mut synced = link.synced();
    for u in users {
        synced.insert(u.id, u.digest());
    }
}

/// `PUT /v1/apps/self/users/{id}` for one Copper user (JIT, outbox). A user that no longer
/// exists in Copper is deleted from Canvas Cloud instead.
pub async fn provision_user(
    link: &CanvasLink,
    active: &Active,
    user_id: Uuid,
) -> anyhow::Result<()> {
    let Some(user) = copper_user(&link.state().db, user_id).await? else {
        delete_remote_user(&active.upstream, user_id).await?;
        return Ok(());
    };
    let resp: Value = Upstream::json(
        active
            .upstream
            .request(Method::PUT, &subject_path(user_id))
            .json(&user.upsert_body()),
    )
    .await?;
    check_same_id(&resp, user_id);
    remember(link, &[&user]);
    tracing::info!(%user_id, "provisioned user on Canvas Cloud");
    Ok(())
}

fn check_same_id(item: &Value, expected: Uuid) -> bool {
    let got = item
        .get("user_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<Uuid>().ok());
    match got {
        Some(id) if id != expected => {
            tracing::warn!(
                copper_user_id = %expected,
                canvas_user_id = %id,
                "Canvas Cloud linked this Copper user to an existing account with a different id; chat author ids will not match"
            );
            false
        }
        _ => true,
    }
}

async fn delete_remote_user(up: &Upstream, user_id: Uuid) -> Result<(), UpstreamError> {
    match Upstream::json::<Value>(up.request(Method::DELETE, &subject_path(user_id))).await {
        Ok(_) => Ok(()),
        Err(e) if e.status() == Some(StatusCode::NOT_FOUND) => Ok(()),
        Err(e) => Err(e),
    }
}

async fn disconnect_remote_user(up: &Upstream, user_id: Uuid) -> Result<(), UpstreamError> {
    let path = format!("{}/disconnect", subject_path(user_id));
    match Upstream::json::<Value>(up.request(Method::POST, &path)).await {
        Ok(_) => Ok(()),
        Err(e) if e.status() == Some(StatusCode::NOT_FOUND) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Outcome of a bulk sync.
#[derive(Debug, Default, Serialize)]
pub struct SyncReport {
    pub total: usize,
    pub provisioned: usize,
    /// `(user id, error)` for users Canvas Cloud refused.
    pub failed: Vec<(Uuid, String)>,
    /// Users Canvas Cloud linked to an existing account with a different id.
    pub id_mismatch: Vec<Uuid>,
}

impl SyncReport {
    pub fn ok(&self) -> bool {
        self.failed.is_empty() && self.id_mismatch.is_empty()
    }
}

/// Push `users` with `POST /v1/apps/self/users:batch`, [`BATCH`] at a time.
async fn push_batch(
    link: &CanvasLink,
    active: &Active,
    users: &[CopperUser],
    report: &mut SyncReport,
) -> Result<(), UpstreamError> {
    for chunk in users.chunks(BATCH) {
        let body = json!({ "users": chunk.iter().map(CopperUser::batch_item).collect::<Vec<_>>() });
        let resp: Value = Upstream::json(
            active
                .upstream
                .request(Method::POST, "/v1/apps/self/users:batch")
                .timeout(crate::upstream::LONG_REQUEST_TIMEOUT)
                .json(&body),
        )
        .await?;
        let items = resp
            .get("results")
            .or_else(|| resp.get("users"))
            .and_then(Value::as_array);
        let mut ok: Vec<&CopperUser> = Vec::with_capacity(chunk.len());
        match items {
            Some(items) => {
                let by_subject: HashMap<String, &Value> = items
                    .iter()
                    .filter_map(|i| Some((i.get("subject")?.as_str()?.to_owned(), i)))
                    .collect();
                for (n, user) in chunk.iter().enumerate() {
                    let item = by_subject
                        .get(&user.id.to_string())
                        .copied()
                        .or_else(|| items.get(n));
                    let err = item.and_then(|i| i.get("error")).filter(|e| !e.is_null());
                    if let Some(err) = err {
                        let msg = item
                            .and_then(|i| i.get("message"))
                            .and_then(Value::as_str)
                            .map_or_else(
                                || err.as_str().unwrap_or("error").to_owned(),
                                |m| format!("{}: {m}", err.as_str().unwrap_or("error")),
                            );
                        report.failed.push((user.id, msg));
                    } else {
                        if item.is_some_and(|i| !check_same_id(i, user.id)) {
                            report.id_mismatch.push(user.id);
                        }
                        ok.push(user);
                    }
                }
            }
            None => ok.extend(chunk.iter()),
        }
        report.provisioned += ok.len();
        remember(link, &ok);
    }
    Ok(())
}

/// Provision every Copper user (link time, `copper-cloud canvas sync-users`).
pub async fn sync_all(link: &CanvasLink, active: &Active) -> anyhow::Result<SyncReport> {
    let users = copper_users(&link.state().db).await?;
    let mut report = SyncReport {
        total: users.len(),
        ..SyncReport::default()
    };
    push_batch(link, active, &users, &mut report).await?;
    for (id, err) in &report.failed {
        tracing::warn!(user_id = %id, error = %err, "Canvas Cloud refused a user");
    }
    Ok(report)
}

/// List Canvas Cloud's users of this app: `subject → canvas_count`.
pub async fn remote_users(
    up: &Upstream,
    timeout: Duration,
) -> Result<HashMap<String, i64>, UpstreamError> {
    let mut out = HashMap::new();
    for page in 0..MAX_PAGES {
        let path = format!(
            "/v1/apps/self/users?limit={LIST_PAGE}&offset={}",
            page * LIST_PAGE
        );
        let resp: Value = Upstream::json(up.request(Method::GET, &path).timeout(timeout)).await?;
        let items = resp
            .get("users")
            .or_else(|| resp.get("items"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let n = items.len();
        for item in items {
            if let Some(subject) = item.get("subject").and_then(Value::as_str) {
                let count = item
                    .get("canvas_count")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                out.insert(subject.to_owned(), count);
            }
        }
        let total = resp.get("total").and_then(Value::as_i64);
        let fetched = i64::try_from(out.len()).unwrap_or(i64::MAX);
        if n < usize::try_from(LIST_PAGE).unwrap_or(usize::MAX)
            || total.is_some_and(|t| fetched >= t)
        {
            break;
        }
    }
    Ok(out)
}

/// One reconcile pass. Returns `(pushed, disconnected, deleted)`.
pub async fn reconcile_once(
    link: &CanvasLink,
    active: &Active,
) -> anyhow::Result<(usize, usize, usize)> {
    let users = copper_users(&link.state().db).await?;
    let (changed, newly_disabled): (Vec<CopperUser>, Vec<Uuid>) = {
        let synced = link.synced();
        let mut changed = Vec::new();
        let mut disabled = Vec::new();
        for u in &users {
            if synced.get(&u.id) != Some(&u.digest()) {
                if u.disabled {
                    disabled.push(u.id);
                }
                changed.push(u.clone());
            }
        }
        (changed, disabled)
    };
    let mut report = SyncReport::default();
    if !changed.is_empty() {
        push_batch(link, active, &changed, &mut report).await?;
    }
    let mut disconnected = 0;
    for id in newly_disabled {
        if disconnect_remote_user(&active.upstream, id).await.is_ok() {
            disconnected += 1;
        }
    }
    // App users whose Copper account is gone (e.g. `copper-cloud admin delete-user`).
    let ours: HashSet<String> = users.iter().map(|u| u.id.to_string()).collect();
    let remote = remote_users(&active.upstream, crate::upstream::REQUEST_TIMEOUT).await?;
    let mut deleted = 0;
    for subject in remote.keys().filter(|s| !ours.contains(*s)) {
        let Ok(id) = subject.parse::<Uuid>() else {
            continue;
        };
        match delete_remote_user(&active.upstream, id).await {
            Ok(()) => {
                deleted += 1;
                link.synced().remove(&id);
            }
            Err(e) => {
                tracing::warn!(user_id = %id, error = %e, "removing a deleted user from Canvas Cloud failed");
            }
        }
    }
    Ok((report.provisioned, disconnected, deleted))
}

/// Background reconcile: right away, then every [`RECONCILE_EVERY`] and on link changes.
pub async fn reconcile_loop(link: CanvasLink) {
    let mut generation = link.subscribe();
    let mut shutdown = copper_cloud_core::shutdown::subscribe();
    loop {
        if copper_cloud_core::shutdown::is_shutting_down() {
            break;
        }
        if let Ok(Some(active)) = link.enabled().await {
            match reconcile_once(&link, &active).await {
                Ok((0, 0, 0)) => {}
                Ok((pushed, disconnected, deleted)) => {
                    tracing::info!(pushed, disconnected, deleted, "canvas users reconciled");
                }
                Err(err) => {
                    tracing::warn!(error = format!("{err:#}"), "canvas user reconcile failed");
                }
            }
        }
        tokio::select! {
            () = tokio::time::sleep(RECONCILE_EVERY) => {}
            r = generation.changed() => { if r.is_err() { break; } }
            _ = shutdown.changed() => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Outbox

/// Queue `kind` for `subject` (in the caller's transaction) — only while a link exists.
async fn enqueue(
    tx: &mut sqlx::PgConnection,
    kind: &str,
    subject: Uuid,
    payload: Option<Value>,
) -> ApiResult<bool> {
    let n = sqlx::query(
        "INSERT INTO canvas_link_outbox (kind, subject, payload)
         SELECT $1, $2, $3 WHERE EXISTS (SELECT 1 FROM canvas_link WHERE id = 1)",
    )
    .bind(kind)
    .bind(subject)
    .bind(payload)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// A user's email / name / disabled flag changed (admin API): push it, and disconnect their
/// canvas sockets when `disabled`. Call inside the change's transaction, then
/// [`CanvasLink::kick_outbox`] after commit.
pub async fn enqueue_user_changed(
    tx: &mut sqlx::PgConnection,
    user_id: Uuid,
    disabled: bool,
) -> ApiResult<()> {
    enqueue(tx, kind::UPSERT_USER, user_id, None).await?;
    if disabled {
        enqueue(tx, kind::DISCONNECT_USER, user_id, None).await?;
    }
    Ok(())
}

/// A user was deleted (admin API): delete them on Canvas Cloud (their canvases go with them
/// there). Call inside the delete's transaction, then [`CanvasLink::kick_outbox`].
pub async fn enqueue_user_deleted(tx: &mut sqlx::PgConnection, user_id: Uuid) -> ApiResult<()> {
    enqueue(tx, kind::DELETE_USER, user_id, None).await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct OutboxRow {
    id: i64,
    kind: String,
    subject: Uuid,
    attempts: i32,
}

/// Run every due outbox item once. Returns `(done, failed)`.
pub async fn process_outbox(link: &CanvasLink, active: &Active) -> ApiResult<(usize, usize)> {
    let db = &link.state().db;
    let rows: Vec<OutboxRow> = sqlx::query_as(
        "SELECT id, kind, subject, attempts FROM canvas_link_outbox
         WHERE next_attempt_at <= now() ORDER BY id LIMIT 200",
    )
    .fetch_all(db)
    .await?;
    let (mut done, mut failed) = (0, 0);
    for row in rows {
        let result: anyhow::Result<()> = match row.kind.as_str() {
            kind::UPSERT_USER => provision_user(link, active, row.subject).await,
            kind::DELETE_USER => {
                let r = delete_remote_user(&active.upstream, row.subject).await;
                link.synced().remove(&row.subject);
                r.map_err(Into::into)
            }
            kind::DISCONNECT_USER => disconnect_remote_user(&active.upstream, row.subject)
                .await
                .map_err(Into::into),
            other => Err(anyhow::anyhow!("unknown outbox kind {other}")),
        };
        match result {
            Ok(()) => {
                done += 1;
                sqlx::query("DELETE FROM canvas_link_outbox WHERE id = $1")
                    .bind(row.id)
                    .execute(db)
                    .await?;
            }
            Err(err) if row.attempts + 1 >= MAX_ATTEMPTS => {
                failed += 1;
                tracing::warn!(kind = %row.kind, user_id = %row.subject, error = format!("{err:#}"), "giving up on canvas link outbox item");
                sqlx::query("DELETE FROM canvas_link_outbox WHERE id = $1")
                    .bind(row.id)
                    .execute(db)
                    .await?;
            }
            Err(err) => {
                failed += 1;
                sqlx::query(
                    "UPDATE canvas_link_outbox
                     SET attempts = attempts + 1, last_error = $2,
                         next_attempt_at = now() + make_interval(secs => least(30 * power(2, least(attempts, 7)), 3600))
                     WHERE id = $1",
                )
                .bind(row.id)
                .bind(format!("{err:#}"))
                .execute(db)
                .await?;
            }
        }
    }
    Ok((done, failed))
}

/// Background outbox worker: kicked after enqueueing, else every [`OUTBOX_POLL`].
pub async fn outbox_loop(link: CanvasLink) {
    loop {
        if copper_cloud_core::shutdown::is_shutting_down() {
            break;
        }
        if let Ok(Some(active)) = link.enabled().await {
            match process_outbox(&link, &active).await {
                Ok((0, 0)) => {}
                Ok((done, failed)) => tracing::info!(done, failed, "canvas link outbox processed"),
                Err(err) => tracing::warn!(error = %err, "canvas link outbox failed"),
            }
        }
        link.outbox_wait(OUTBOX_POLL).await;
    }
}
