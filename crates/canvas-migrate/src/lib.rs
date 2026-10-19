//! `copper-cloud canvas migrate`: moves copper-cloud's legacy canvases (tables from
//! migrations `0100`–`0103`) into a linked Canvas Cloud through its import format v1
//! (`POST /v1/apps/self/import`, CONTRACT §6/§8), preserving canvas ids, user ids, share-link
//! tokens, invites and mentions.
//!
//! * **Read-only on the legacy side.** Legacy rows are never modified or deleted (rollback =
//!   reinstall the old version); the only writes are to the `canvas_migrations` ledger.
//! * **Users first.** Every copper user is provisioned (`users:batch`, subject = id hint = the
//!   copper user id) and must come back with that same id — otherwise chat author ids and
//!   member lists would break, so nothing is imported.
//! * **Streamed.** Each canvas is one NDJSON request body produced lazily from the database
//!   (one page of updates in memory at a time); `concurrency` canvases are in flight.
//! * **Verified.** A canvas counts as migrated only when canvas-cloud's import counts equal
//!   what was sent and the import's digest equals `GET /v1/apps/self/canvases/{id}/digest`.
//! * **Idempotent.** Canvases recorded `migrated` are skipped (unless `--replace`/`--only`);
//!   a canvas that already exists in canvas-cloud is accepted only when its digest matches
//!   this ledger's record — anything else needs `--replace`.

mod ledger;
mod legacy;
mod remote;
mod report;
mod wire;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use copper_cloud_core::crypto::Crypto;
use futures::{StreamExt as _, TryStreamExt as _};
use reqwest::StatusCode;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

pub use legacy::{doc_aad, excerpt_aad, EXCERPT_AAD_PREFIX};
pub use report::{CanvasReport, MigrateReport, Status, Totals, UserFailure, UsersReport};
pub use wire::{CONTENT_TYPE, VERSION};

use crate::legacy::{LegacyCanvas, Schema, SharedSent};
use crate::remote::{Failure, Remote};

/// Canvases in flight when `MigrateOptions::concurrency` is 0.
pub const DEFAULT_CONCURRENCY: usize = 2;

/// Options of one run (`copper-cloud canvas migrate …`).
#[derive(Debug, Clone)]
pub struct MigrateOptions {
    /// Read, decrypt and encode everything, send nothing (only the preflight request).
    pub dry_run: bool,
    /// Only these canvases (even if the ledger says `migrated`). Empty = all.
    pub only: Vec<Uuid>,
    /// Import with `mode=replace` (overwrite earlier imports by this app), ledger ignored.
    pub replace: bool,
    /// Canvases in flight (0 → [`DEFAULT_CONCURRENCY`]).
    pub concurrency: usize,
    /// The report's `Display` is JSON instead of a table.
    pub json: bool,
}

impl Default for MigrateOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            only: Vec::new(),
            replace: false,
            concurrency: DEFAULT_CONCURRENCY,
            json: false,
        }
    }
}

/// Runs the migration against the Canvas Cloud at `base_url` using `app_key` (a `cva_…` key
/// of an app with `allow_import`). `client` is pre-configured by the caller (timeouts, TLS
/// pinning); this function adds `Authorization: Bearer <app_key>` itself.
///
/// Returns `Err` only for run-level failures (preflight, the app lacks `allow_import`,
/// database errors). Per-canvas failures and unprovisionable users are in the report —
/// check [`MigrateReport::ok`] for the exit status.
pub async fn run(
    db: &PgPool,
    crypto: &Crypto,
    client: &reqwest::Client,
    base_url: &url::Url,
    app_key: &str,
    opts: MigrateOptions,
) -> anyhow::Result<MigrateReport> {
    let remote = Remote::new(client, base_url, app_key)?;
    let mut report = MigrateReport::new(remote.base(), opts.dry_run, opts.replace, opts.json);

    let stats = remote.preflight().await?;
    if stats
        .get("allow_import")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
        && !opts.dry_run
    {
        anyhow::bail!(
            "the canvas-cloud app lacks allow_import (recreate it with `canvas-cloud apps create --allow-import`)"
        );
    }

    let schema = legacy::schema(db).await?;
    if !schema.canvases {
        tracing::info!("no legacy canvas tables: nothing to migrate");
        report.finish();
        return Ok(report);
    }
    if !schema.ledger && !opts.dry_run {
        anyhow::bail!(
            "the canvas_migrations ledger table is missing — run copper-cloud's database migrations (0030_canvas_link.sql) first"
        );
    }
    legacy::check_master_key(db, crypto).await?;
    let ledger = if schema.ledger {
        ledger::load(db).await?
    } else {
        HashMap::new()
    };

    // 1. Users.
    let users = legacy::users(db).await?;
    report.users.total = users.len();
    if !opts.dry_run {
        provision(&remote, &users, &mut report).await?;
        if report.error.is_some() {
            report.finish();
            return Ok(report);
        }
    }

    // 2. Which canvases.
    let (plan, unknown_only) = plan(legacy::canvases(db).await?, &ledger, &opts);

    // 3. Canvases, `concurrency` at a time, results in legacy order.
    let ctx = Ctx {
        db,
        crypto,
        remote: &remote,
        schema,
        ledger: &ledger,
        dry_run: opts.dry_run,
        replace: opts.replace,
        forbidden: AtomicBool::new(false),
    };
    let rows: Vec<CanvasReport> = futures::stream::iter(plan)
        .map(|p| ctx.handle(p))
        .buffered(if opts.concurrency == 0 {
            DEFAULT_CONCURRENCY
        } else {
            opts.concurrency
        })
        .collect()
        .await;
    if ctx.forbidden.load(Ordering::SeqCst) {
        anyhow::bail!(
            "canvas-cloud refused the import (403): the app lacks allow_import \
             (recreate it with `canvas-cloud apps create --allow-import`); nothing was migrated by this run"
        );
    }
    report.canvases = rows;
    report.canvases.extend(unknown_only);
    report.finish();
    Ok(report)
}

/// Provisions every copper user; any user that does not come back with its own id is a
/// run-level error (recorded in the report, nothing gets imported).
async fn provision(
    remote: &Remote<'_>,
    users: &[legacy::LegacyUser],
    report: &mut MigrateReport,
) -> anyhow::Result<()> {
    let outcomes = remote.provision(users).await?;
    for (user, outcome) in users.iter().zip(outcomes) {
        match outcome.error {
            None => {
                report.users.provisioned += 1;
                report.users.created += usize::from(outcome.created);
            }
            Some(error) => report.users.failed.push(UserFailure {
                user_id: user.id,
                email: user.email.clone(),
                error,
            }),
        }
    }
    if report.users.failed.is_empty() {
        tracing::info!(
            users = report.users.provisioned,
            created = report.users.created,
            "users provisioned"
        );
    } else {
        report.error = Some(format!(
            "{} user(s) could not be provisioned with their copper user id; no canvas was migrated \
             (canvas-cloud user ids must equal copper ids for chat authors, members and mentions)",
            report.users.failed.len()
        ));
    }
    Ok(())
}

/// Selects the canvases of this run (legacy order) and reports `--only` ids that are not
/// legacy canvases.
fn plan(
    all: Vec<LegacyCanvas>,
    ledger: &HashMap<Uuid, ledger::Entry>,
    opts: &MigrateOptions,
) -> (Vec<Plan>, Vec<CanvasReport>) {
    let only: HashSet<Uuid> = opts.only.iter().copied().collect();
    let mut plan: Vec<Plan> = Vec::new();
    for canvas in all {
        if !only.is_empty() && !only.contains(&canvas.id) {
            continue;
        }
        let migrated = ledger
            .get(&canvas.id)
            .filter(|e| e.status == "migrated" && !opts.replace && !only.contains(&canvas.id));
        plan.push(match migrated {
            Some(entry) => Plan::Skip(canvas, entry.digest.clone()),
            None => Plan::Go(canvas),
        });
    }
    let known: HashSet<Uuid> = plan.iter().map(|p| p.canvas().id).collect();
    let mut seen = HashSet::new();
    let mut unknown = Vec::new();
    for id in &opts.only {
        if !known.contains(id) && seen.insert(*id) {
            let mut row = CanvasReport::new(*id, "", "", None);
            row.error =
                Some("not a legacy canvas (no such id in copper-cloud's canvases table)".into());
            unknown.push(row);
        }
    }
    (plan, unknown)
}

enum Plan {
    /// Ledger says migrated (with this digest).
    Skip(LegacyCanvas, Option<String>),
    Go(LegacyCanvas),
}

impl Plan {
    fn canvas(&self) -> &LegacyCanvas {
        match self {
            Self::Skip(c, _) | Self::Go(c) => c,
        }
    }
}

struct Ctx<'a> {
    db: &'a PgPool,
    crypto: &'a Crypto,
    remote: &'a Remote<'a>,
    schema: Schema,
    ledger: &'a HashMap<Uuid, ledger::Entry>,
    dry_run: bool,
    replace: bool,
    /// An import answered 403: the app lacks `allow_import`; stop starting new imports.
    forbidden: AtomicBool,
}

/// Import request timeout: 2 minutes plus 2 s per MiB stored.
fn import_timeout(stored_bytes: i64) -> Duration {
    let mib = u64::try_from(stored_bytes).unwrap_or(0) / (1024 * 1024);
    Duration::from_secs(120 + 2 * mib)
}

impl Ctx<'_> {
    async fn handle(&self, plan: Plan) -> CanvasReport {
        let canvas = plan.canvas().clone();
        let mut row =
            CanvasReport::new(canvas.id, &canvas.name, &canvas.kind, Some(canvas.owner_id));
        match plan {
            Plan::Skip(_, digest) => {
                row.status = Status::Already;
                row.digest = digest;
                row.note = Some("recorded as migrated in the ledger".into());
            }
            Plan::Go(_) if self.dry_run => self.dry_run(&canvas, &mut row).await,
            Plan::Go(_) => {
                if self.forbidden.load(Ordering::SeqCst) {
                    row.error = Some("not attempted: the app lacks allow_import".into());
                } else if let Err(e) = self.migrate(&canvas, &mut row).await {
                    // Only ledger/database errors reach here.
                    row.status = Status::Failed;
                    row.error = Some(format!("{e:#}"));
                }
            }
        }
        let level = if row.status == Status::Failed {
            "failed"
        } else {
            "ok"
        };
        tracing::info!(
            canvas_id = %row.id,
            status = row.status.as_str(),
            updates = row.updates,
            bytes = row.bytes,
            error = row.error.as_deref().unwrap_or(""),
            "canvas {level}"
        );
        row
    }

    fn stream(
        &self,
        canvas: &LegacyCanvas,
        sent: &SharedSent,
    ) -> impl futures::Stream<Item = anyhow::Result<bytes::Bytes>> + Send + 'static {
        legacy::canvas_stream(
            self.db.clone(),
            self.crypto.clone(),
            canvas.id,
            self.schema,
            SharedSent::clone(sent),
        )
    }

    async fn dry_run(&self, canvas: &LegacyCanvas, row: &mut CanvasReport) {
        let sent = SharedSent::default();
        let result = self
            .stream(canvas, &sent)
            .try_fold(0_u64, |n, line| async move { Ok(n + line.len() as u64) })
            .await;
        copy_counts(row, &sent);
        match result {
            Ok(_) => row.status = Status::DryRun,
            Err(e) => row.error = Some(format!("{e:#}")),
        }
    }

    /// One canvas, for real. `Err` = ledger/database trouble only.
    async fn migrate(&self, canvas: &LegacyCanvas, row: &mut CanvasReport) -> anyhow::Result<()> {
        // Cheap re-run path: a canvas that already exists is judged without uploading it.
        if !self.replace {
            match self.remote.digest(canvas.id).await {
                Ok(Some(stored)) => return self.exists(canvas, row, &stored, &stored).await,
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(canvas_id = %canvas.id, "digest pre-check failed ({e}); importing");
                }
            }
        }

        let sent = SharedSent::default();
        let body = reqwest::Body::wrap_stream(self.stream(canvas, &sent));
        let result = self
            .remote
            .import(body, self.replace, import_timeout(canvas.stored_bytes))
            .await;
        copy_counts(row, &sent);
        let local_error = legacy::with_sent(&sent, |s| s.error.clone());

        let imported = match result {
            Ok(imported) => imported,
            Err(Failure::Http(h))
                if h.status == StatusCode::CONFLICT && h.code == "canvas_exists" =>
            {
                let stored = match remote::conflict_digest(&h.body) {
                    Some(d) => Some(d),
                    None => self.remote.digest(canvas.id).await.ok().flatten(),
                };
                let Some(conflict) = stored else {
                    return self.fail(canvas, row, "canvas exists in canvas-cloud but no digest is available (use --replace)".into(), None).await;
                };
                let now = self
                    .remote
                    .digest(canvas.id)
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| conflict.clone());
                return self.exists(canvas, row, &conflict, &now).await;
            }
            Err(Failure::Http(h)) if h.status == StatusCode::FORBIDDEN => {
                self.forbidden.store(true, Ordering::SeqCst);
                row.error = Some(format!("{h} — the app lacks allow_import"));
                return Ok(());
            }
            Err(Failure::Http(h))
                if h.status == StatusCode::CONFLICT && h.code == "personal_exists" =>
            {
                let msg = format!("{h} — the owner already has a non-empty Personal canvas in canvas-cloud (use --replace)");
                return self.fail(canvas, row, msg, None).await;
            }
            Err(failure) => {
                // A local read/decrypt error aborts the body; report it rather than the
                // resulting transport error.
                let msg = match (local_error, &failure) {
                    (Some(local), Failure::Transport(_)) => {
                        format!("reading the legacy canvas: {local}")
                    }
                    (Some(local), Failure::Http(h)) => {
                        format!("reading the legacy canvas: {local} (canvas-cloud: {h})")
                    }
                    (None, f) => f.to_string(),
                };
                return self.fail(canvas, row, msg, None).await;
            }
        };

        self.verify(canvas, row, imported).await
    }

    /// Checks a `200` import: counts equal what was sent, and the response digest equals the
    /// stored one; then the ledger says `migrated`.
    async fn verify(
        &self,
        canvas: &LegacyCanvas,
        row: &mut CanvasReport,
        imported: remote::Imported,
    ) -> anyhow::Result<()> {
        let replaced = imported.status.as_deref() == Some("replaced");
        let sent_counts = (
            row.updates,
            row.members,
            row.invites,
            row.links,
            row.mentions,
        );
        let got_counts = (
            imported.updates_applied,
            imported.members,
            imported.invites,
            imported.links,
            imported.mentions,
        );
        let digest = imported.digest.trim().to_ascii_lowercase();
        if sent_counts != got_counts {
            let msg = format!(
                "count mismatch: sent (updates, members, invites, links, mentions) = {sent_counts:?}, canvas-cloud applied {got_counts:?}"
            );
            return self.fail_changed(canvas, row, msg, &digest).await;
        }
        let detail = self.detail(row, if replaced { "replaced" } else { "created" });
        ledger::record(
            self.db,
            canvas.id,
            "imported",
            Some(&digest),
            row.updates,
            &detail,
            ledger::Write::Overwrite,
        )
        .await?;

        match self.remote.digest(canvas.id).await {
            Ok(Some(stored)) if stored == digest => {
                ledger::record(
                    self.db,
                    canvas.id,
                    "migrated",
                    Some(&digest),
                    row.updates,
                    &detail,
                    ledger::Write::Overwrite,
                )
                .await?;
                row.status = if replaced {
                    Status::Replaced
                } else {
                    Status::Migrated
                };
                row.digest = Some(digest);
                Ok(())
            }
            Ok(Some(stored)) => {
                let msg = format!("digest mismatch: the import returned {digest} but canvas-cloud now stores {stored}");
                self.fail_changed(canvas, row, msg, &digest).await
            }
            Ok(None) => {
                let msg =
                    "verification failed: canvas-cloud reports the imported canvas does not exist"
                        .to_owned();
                self.fail_changed(canvas, row, msg, &digest).await
            }
            Err(e) => {
                let msg = format!("verification failed: GET digest: {e} (the ledger keeps status `imported`; re-run to verify)");
                row.status = Status::Failed;
                row.digest = Some(digest);
                row.error = Some(msg);
                Ok(())
            }
        }
    }

    /// The canvas already exists in canvas-cloud with digest `conflict` (and `stored` now):
    /// `already` only if this ledger recorded exactly that import.
    async fn exists(
        &self,
        canvas: &LegacyCanvas,
        row: &mut CanvasReport,
        conflict: &str,
        stored: &str,
    ) -> anyhow::Result<()> {
        let entry = self.ledger.get(&canvas.id);
        if conflict == stored && entry.is_some_and(|e| e.vouches_for(stored)) {
            if entry.is_some_and(|e| e.status != "migrated") {
                ledger::confirm(
                    self.db,
                    canvas.id,
                    stored,
                    &json!({ "confirmed_by_rerun": true }),
                )
                .await?;
            }
            row.status = Status::Already;
            row.digest = Some(stored.to_owned());
            row.note = Some("exists in canvas-cloud with the digest recorded in the ledger".into());
            return Ok(());
        }
        let why = match entry {
            Some(e) if e.digest.is_some() && e.status == "migrated" => {
                "it changed since this ledger recorded it (edited in canvas-cloud?)"
            }
            Some(_) | None => "this ledger has no verified record of importing it",
        };
        let msg = format!(
            "exists in canvas-cloud with different content (digest {}…; {why}) — use --replace to overwrite it from the legacy data",
            &stored[..stored.len().min(12)]
        );
        self.fail(canvas, row, msg, None).await
    }

    fn detail(&self, row: &CanvasReport, import_status: &str) -> serde_json::Value {
        json!({
            "name": row.name,
            "kind": row.kind,
            "owner": row.owner,
            "import_status": import_status,
            "bytes": row.bytes,
            "snapshot": row.snapshot,
            "members": row.members,
            "invites": row.invites,
            "links": row.links,
            "mentions": row.mentions,
            "warnings": row.warnings,
            "canvas_cloud": self.remote.base(),
            "tool": concat!("copper-cloud-canvas-migrate ", env!("CARGO_PKG_VERSION")),
        })
    }

    /// Failed without changing canvas-cloud: never downgrades a `migrated` ledger row.
    async fn fail(
        &self,
        canvas: &LegacyCanvas,
        row: &mut CanvasReport,
        msg: String,
        digest: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut detail = self.detail(row, "none");
        detail["error"] = json!(msg);
        ledger::record(
            self.db,
            canvas.id,
            "failed",
            digest,
            row.updates,
            &detail,
            ledger::Write::KeepMigrated,
        )
        .await?;
        row.status = Status::Failed;
        row.error = Some(msg);
        Ok(())
    }

    /// Failed after canvas-cloud accepted (and stored) an import: always recorded.
    async fn fail_changed(
        &self,
        canvas: &LegacyCanvas,
        row: &mut CanvasReport,
        msg: String,
        digest: &str,
    ) -> anyhow::Result<()> {
        let mut detail = self.detail(row, "unverified");
        detail["error"] = json!(msg);
        ledger::record(
            self.db,
            canvas.id,
            "failed",
            Some(digest),
            row.updates,
            &detail,
            ledger::Write::Overwrite,
        )
        .await?;
        row.status = Status::Failed;
        row.digest = Some(digest.to_owned());
        row.error = Some(msg);
        Ok(())
    }
}

fn copy_counts(row: &mut CanvasReport, sent: &SharedSent) {
    legacy::with_sent(sent, |s| {
        row.updates = s.updates;
        row.bytes = s.bytes;
        row.snapshot = s.snapshot;
        row.members = s.members;
        row.invites = s.invites;
        row.links = s.links;
        row.mentions = s.mentions;
        row.warnings.clone_from(&s.warnings);
    });
}
