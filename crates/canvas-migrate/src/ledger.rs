//! The `canvas_migrations` ledger (created by copper-cloud migration `0030_canvas_link.sql`):
//! `(canvas_id uuid pk, status text, digest text, updates int, migrated_at timestamptz,
//! detail jsonb)`.
//!
//! Statuses written here:
//!
//! * `imported` — canvas-cloud answered `200` to the import and the counts matched; the
//!   digest is the one the import returned. Written *before* the digest round trip, so a crash
//!   between the import and verification leaves evidence a re-run can match (a `409` whose
//!   digest equals it is then accepted).
//! * `migrated` — imported and verified (`digest` == `GET …/digest`). Re-runs skip these.
//! * `failed` — the last attempt failed (`detail.error`). A failure that did not change
//!   canvas-cloud (refused import, transport error, unreadable legacy row, "exists with other
//!   content") never downgrades an existing `migrated` row.

use std::collections::HashMap;

use anyhow::Context as _;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub status: String,
    pub digest: Option<String>,
    /// [`crate::legacy::LegacyCanvas::mark`] when it was imported (`None` in older ledgers).
    pub mark: Option<String>,
}

impl Entry {
    /// A record this tool wrote after canvas-cloud accepted the canvas.
    pub fn vouches_for(&self, digest: &str) -> bool {
        matches!(self.status.as_str(), "migrated" | "imported")
            && self.digest.as_deref() == Some(digest)
    }
}

pub(crate) async fn load(db: &PgPool) -> anyhow::Result<HashMap<Uuid, Entry>> {
    let rows: Vec<(Uuid, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT canvas_id, status, digest, detail->>'legacy_mark' FROM canvas_migrations",
    )
    .fetch_all(db)
    .await
    .context("reading the canvas_migrations ledger")?;
    Ok(rows
        .into_iter()
        .map(|(id, status, digest, mark)| {
            (
                id,
                Entry {
                    status,
                    digest,
                    mark,
                },
            )
        })
        .collect())
}

/// How a ledger write treats an existing row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Write {
    /// Always replace the row (canvas-cloud changed).
    Overwrite,
    /// Leave a `migrated` row alone (nothing changed in canvas-cloud).
    KeepMigrated,
}

pub(crate) async fn record(
    db: &PgPool,
    canvas_id: Uuid,
    status: &str,
    digest: Option<&str>,
    updates: u64,
    detail: &Value,
    mode: Write,
) -> anyhow::Result<()> {
    let sql = match mode {
        Write::Overwrite => {
            "INSERT INTO canvas_migrations (canvas_id, status, digest, updates, migrated_at, detail)
             VALUES ($1, $2, $3, $4, now(), $5)
             ON CONFLICT (canvas_id) DO UPDATE SET
               status = EXCLUDED.status,
               digest = COALESCE(EXCLUDED.digest, canvas_migrations.digest),
               updates = EXCLUDED.updates,
               migrated_at = EXCLUDED.migrated_at,
               detail = EXCLUDED.detail"
        }
        Write::KeepMigrated => {
            "INSERT INTO canvas_migrations (canvas_id, status, digest, updates, migrated_at, detail)
             VALUES ($1, $2, $3, $4, now(), $5)
             ON CONFLICT (canvas_id) DO UPDATE SET
               status = EXCLUDED.status,
               digest = COALESCE(EXCLUDED.digest, canvas_migrations.digest),
               updates = EXCLUDED.updates,
               migrated_at = EXCLUDED.migrated_at,
               detail = EXCLUDED.detail
             WHERE canvas_migrations.status <> 'migrated'"
        }
    };
    sqlx::query(sql)
        .bind(canvas_id)
        .bind(status)
        .bind(digest)
        .bind(i32::try_from(updates).unwrap_or(i32::MAX))
        .bind(detail)
        .execute(db)
        .await
        .with_context(|| format!("recording canvas {canvas_id} in canvas_migrations"))?;
    Ok(())
}

/// Marks an existing `imported` row (or a missing one) `migrated` after a re-run proved
/// canvas-cloud holds exactly what was imported.
pub(crate) async fn confirm(
    db: &PgPool,
    canvas_id: Uuid,
    digest: &str,
    detail: &Value,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO canvas_migrations (canvas_id, status, digest, updates, migrated_at, detail)
         VALUES ($1, 'migrated', $2, NULL, now(), $3)
         ON CONFLICT (canvas_id) DO UPDATE SET
           status = 'migrated',
           digest = EXCLUDED.digest,
           detail = COALESCE(canvas_migrations.detail, '{}'::jsonb) || EXCLUDED.detail
         WHERE canvas_migrations.status <> 'migrated'",
    )
    .bind(canvas_id)
    .bind(digest)
    .bind(detail)
    .execute(db)
    .await
    .with_context(|| format!("confirming canvas {canvas_id} in canvas_migrations"))?;
    Ok(())
}
