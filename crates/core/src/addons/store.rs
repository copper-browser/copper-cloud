//! Addons in Postgres (`addons`, `addon_versions`, `addon_audience`; migration 0014).
//!
//! Shared by the admin API and `copper-cloud addons …`: every write takes an [`Actor`], bumps
//! the addon's `revision` and writes an `admin_audit` row (`addon.create`, `addon.upload`,
//! `addon.update`, `addon.delete`). Publishing the SSE `addons` event is the caller's job
//! ([`super::notify`]; the CLI runs in another process and cannot).

use std::collections::{BTreeSet, HashMap};
use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use super::bundle::Built;
use super::manifest::Manifest;
use super::{rfc3339, Actor, AddonError, Slot};
use crate::error::{ApiError, ApiResult};

/// Most emails in one addon's audience.
pub const MAX_PEOPLE: usize = 5000;
/// `addons.priority` bounds (higher wins within a slot).
pub const PRIORITY: RangeInclusive<i32> = -1000..=1000;

/// Who gets an enabled addon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Audience {
    /// Every user of the instance.
    Everyone,
    /// Only the listed emails.
    People,
}

impl Audience {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Everyone => "everyone",
            Self::People => "people",
        }
    }
}

/// One uploaded version (admin views).
#[derive(Clone, Debug, Serialize)]
pub struct VersionView {
    pub version: String,
    /// Lowercase hex SHA-256 of the served bundle.
    pub sha256: String,
    /// Served bundle bytes.
    pub size: i64,
    pub uploaded_at: String,
    /// The uploading admin's email; `null` for the CLI (or a deleted admin).
    pub uploaded_by: Option<String>,
    pub current: bool,
}

/// One audience entry (admin views).
#[derive(Clone, Debug, Serialize)]
pub struct PersonView {
    /// The user's id once someone with this email has an account, else `null`.
    pub id: Option<Uuid>,
    /// Lower-cased.
    pub email: String,
    /// The user's display name, if they have an account and set one.
    pub name: Option<String>,
}

/// An addon as the admin API and CLI show it.
#[derive(Clone, Debug, Serialize)]
pub struct AddonView {
    pub id: String,
    pub slot: String,
    pub name: String,
    pub enabled: bool,
    /// `everyone` | `people`.
    pub audience: String,
    /// Audience emails (used when `audience` is `people`; kept either way).
    pub people: Vec<PersonView>,
    /// The admin's values only; manifest defaults fill the rest when served.
    pub settings: Value,
    pub priority: i32,
    pub revision: i64,
    pub current: Option<VersionView>,
    /// Newest first.
    pub versions: Vec<VersionView>,
    /// The current version's `manifest.json`.
    pub manifest: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
    /// Admin email or `cli`.
    pub updated_by: Option<String>,
}

type AddonRow = (
    String,
    String,
    String,
    bool,
    String,
    Value,
    i32,
    i64,
    Option<Uuid>,
    Option<Value>,
    OffsetDateTime,
    OffsetDateTime,
    Option<String>,
);

type VersionRow = (
    String,
    Uuid,
    String,
    String,
    i32,
    OffsetDateTime,
    Option<String>,
);

/// Admin views of every addon (`only = None`) or of one.
#[allow(clippy::too_many_lines)]
async fn views(db: &PgPool, only: Option<&str>) -> ApiResult<Vec<AddonView>> {
    let rows: Vec<AddonRow> = sqlx::query_as(
        "SELECT a.id, a.slot, a.name, a.enabled, a.audience, a.settings, a.priority, a.revision,
                a.current_version_id, v.manifest, a.created_at, a.updated_at, a.updated_by
         FROM addons a LEFT JOIN addon_versions v ON v.id = a.current_version_id
         WHERE $1::text IS NULL OR a.id = $1
         ORDER BY a.slot, a.priority DESC, a.updated_at DESC, a.id",
    )
    .bind(only)
    .fetch_all(db)
    .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let versions: Vec<VersionRow> = sqlx::query_as(
        "SELECT v.addon_id, v.id, v.version, v.sha256, v.size_bytes, v.created_at, ad.email
             FROM addon_versions v LEFT JOIN admins ad ON ad.id = v.uploaded_by
             WHERE $1::text IS NULL OR v.addon_id = $1
             ORDER BY v.addon_id, v.created_at DESC, v.id DESC",
    )
    .bind(only)
    .fetch_all(db)
    .await?;
    let mut by_addon: HashMap<String, Vec<(Uuid, VersionView)>> = HashMap::new();
    for (addon_id, vid, version, sha256, size, at, by) in versions {
        by_addon.entry(addon_id).or_default().push((
            vid,
            VersionView {
                version,
                sha256,
                size: i64::from(size),
                uploaded_at: rfc3339(at),
                uploaded_by: by,
                current: false,
            },
        ));
    }

    let people: Vec<(String, String, Option<Uuid>, Option<String>)> = sqlx::query_as(
        "SELECT p.addon_id, p.email, u.id, NULLIF(u.display_name, '')
         FROM addon_audience p LEFT JOIN users u ON lower(u.email) = p.email
         WHERE $1::text IS NULL OR p.addon_id = $1
         ORDER BY p.addon_id, p.email",
    )
    .bind(only)
    .fetch_all(db)
    .await?;
    let mut people_by: HashMap<String, Vec<PersonView>> = HashMap::new();
    for (addon_id, email, id, name) in people {
        people_by
            .entry(addon_id)
            .or_default()
            .push(PersonView { id, email, name });
    }

    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                slot,
                name,
                enabled,
                audience,
                settings,
                priority,
                revision,
                current_id,
                manifest,
                created_at,
                updated_at,
                updated_by,
            )| {
                let versions: Vec<VersionView> = by_addon
                    .remove(&id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(vid, mut v)| {
                        v.current = Some(vid) == current_id;
                        v
                    })
                    .collect();
                let current = versions.iter().find(|v| v.current).cloned();
                AddonView {
                    people: people_by.remove(&id).unwrap_or_default(),
                    id,
                    slot,
                    name,
                    enabled,
                    audience,
                    settings,
                    priority,
                    revision,
                    current,
                    versions,
                    manifest,
                    created_at: rfc3339(created_at),
                    updated_at: rfc3339(updated_at),
                    updated_by,
                }
            },
        )
        .collect())
}

/// Every addon: by slot, then in resolution order (priority, latest change).
pub async fn list(db: &PgPool) -> ApiResult<Vec<AddonView>> {
    views(db, None).await
}

/// One addon, or 404.
pub async fn get(db: &PgPool, id: &str) -> ApiResult<AddonView> {
    views(db, Some(id))
        .await?
        .into_iter()
        .next()
        .ok_or(ApiError::NotFound)
}

async fn audit<'e, E: sqlx::PgExecutor<'e>>(
    db: E,
    actor: &Actor,
    action: &str,
    target: &str,
    mut detail: Map<String, Value>,
) -> ApiResult<()> {
    detail.insert("via".into(), Value::from(actor.via()));
    sqlx::query(
        "INSERT INTO admin_audit (admin_id, action, target, detail) VALUES ($1, $2, $3, $4)",
    )
    .bind(actor.admin_id())
    .bind(action)
    .bind(target)
    .bind(Value::Object(detail))
    .execute(db)
    .await?;
    Ok(())
}

/// What an upload did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadOutcome {
    /// New addon (disabled, audience `people` with nobody in it).
    Created,
    /// New version, now current.
    NewVersion,
    /// The same version with the same bytes was already stored: nothing changed.
    Unchanged,
}

/// Store a validated bundle. `slot` is the slot the admin picked; it must equal the
/// manifest's. A new id creates a disabled addon; a new version becomes current; the same
/// version with other bytes is [`AddonError::VersionExists`]; the same bytes change nothing.
#[allow(clippy::too_many_lines)]
pub async fn upload(
    db: &PgPool,
    actor: &Actor,
    slot: Slot,
    built: &Built,
) -> Result<(UploadOutcome, AddonView), AddonError> {
    let m = &built.manifest;
    if m.slot != slot {
        return Err(ApiError::bad_request(format!(
            "this addon is for the \"{}\" slot, not \"{slot}\" (manifest.json \"slot\")",
            m.slot
        ))
        .into());
    }
    let size = i32::try_from(built.bytes.len()).map_err(ApiError::internal)?;
    let mut tx = db.begin().await?;
    let created = sqlx::query(
        "INSERT INTO addons (id, slot, name, updated_by) VALUES ($1, $2, $3, $4)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(&m.id)
    .bind(m.slot.id())
    .bind(&m.name)
    .bind(actor.label())
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    let existing_slot: String =
        sqlx::query_scalar("SELECT slot FROM addons WHERE id = $1 FOR UPDATE")
            .bind(&m.id)
            .fetch_one(&mut *tx)
            .await?;
    if existing_slot != m.slot.id() {
        return Err(ApiError::bad_request(format!(
            "addon {} already exists for the \"{existing_slot}\" slot; an addon cannot change slots (use a new id)",
            m.id
        ))
        .into());
    }
    let prior: Option<String> = sqlx::query_scalar(
        "SELECT sha256 FROM addon_versions WHERE addon_id = $1 AND version = $2",
    )
    .bind(&m.id)
    .bind(&m.version)
    .fetch_optional(&mut *tx)
    .await?;
    match prior {
        Some(sha) if sha == built.sha256 => {
            tx.rollback().await?;
            return Ok((UploadOutcome::Unchanged, get(db, &m.id).await?));
        }
        Some(sha256) => {
            return Err(AddonError::VersionExists {
                id: m.id.clone(),
                version: m.version.clone(),
                sha256,
            })
        }
        None => {}
    }
    let version_id = crate::ids::uuid_v7();
    sqlx::query(
        "INSERT INTO addon_versions
             (id, addon_id, version, manifest, bundle, sha256, size_bytes, uploaded_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(version_id)
    .bind(&m.id)
    .bind(&m.version)
    .bind(&m.raw)
    .bind(&built.bytes)
    .bind(&built.sha256)
    .bind(size)
    .bind(actor.admin_id())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE addons SET current_version_id = $2, name = $3,
                revision = CASE WHEN $4 THEN revision ELSE revision + 1 END,
                updated_at = now(), updated_by = $5
         WHERE id = $1",
    )
    .bind(&m.id)
    .bind(version_id)
    .bind(&m.name)
    .bind(created)
    .bind(actor.label())
    .execute(&mut *tx)
    .await?;
    let mut detail = Map::new();
    detail.insert("slot".into(), Value::from(m.slot.id()));
    detail.insert("name".into(), Value::from(m.name.as_str()));
    detail.insert("version".into(), Value::from(m.version.as_str()));
    detail.insert("sha256".into(), Value::from(built.sha256.as_str()));
    detail.insert("size".into(), Value::from(size));
    detail.insert("files".into(), Value::from(built.files));
    let action = if created {
        "addon.create"
    } else {
        "addon.upload"
    };
    audit(&mut *tx, actor, action, &m.id, detail).await?;
    tx.commit().await?;
    let outcome = if created {
        UploadOutcome::Created
    } else {
        UploadOutcome::NewVersion
    };
    Ok((outcome, get(db, &m.id).await?))
}

/// `PATCH /admin/api/addons/{id}` body (also built by the CLI). Every field is optional;
/// `people` replaces the whole audience list, `settings` replaces the admin's values.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub enabled: Option<bool>,
    pub audience: Option<Audience>,
    pub people: Option<Vec<String>>,
    pub settings: Option<Value>,
    pub priority: Option<i32>,
    pub current_version: Option<String>,
}

impl Patch {
    pub fn is_noop(&self) -> bool {
        self.enabled.is_none()
            && self.audience.is_none()
            && self.people.is_none()
            && self.settings.is_none()
            && self.priority.is_none()
            && self.current_version.is_none()
    }
}

/// Lower-case, validate and de-duplicate audience emails.
pub fn normalize_people(raw: &[String]) -> ApiResult<Vec<String>> {
    let mut out = BTreeSet::new();
    for e in raw {
        let email = crate::auth::normalize_email(e)
            .map_err(|_| ApiError::bad_request(format!("\"{e}\" is not an email address")))?;
        out.insert(email.to_lowercase());
    }
    if out.len() > MAX_PEOPLE {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_PEOPLE} people per addon"
        )));
    }
    Ok(out.into_iter().collect())
}

/// Apply a [`Patch`]. Settings are validated against the manifest of the version that is
/// current after the patch.
#[allow(clippy::too_many_lines)]
pub async fn update(db: &PgPool, actor: &Actor, id: &str, patch: &Patch) -> ApiResult<AddonView> {
    if patch.is_noop() {
        return Err(ApiError::bad_request(
            "nothing to change (enabled, audience, people, settings, priority, current_version)",
        ));
    }
    if let Some(p) = patch.priority {
        if !PRIORITY.contains(&p) {
            return Err(ApiError::bad_request(format!(
                "priority must be between {} and {}",
                PRIORITY.start(),
                PRIORITY.end()
            )));
        }
    }
    let people = patch.people.as_deref().map(normalize_people).transpose()?;

    let mut tx = db.begin().await?;
    let current: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT current_version_id FROM addons WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let mut current = current.ok_or(ApiError::NotFound)?;
    let mut detail = Map::new();

    if let Some(version) = &patch.current_version {
        let vid: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM addon_versions WHERE addon_id = $1 AND version = $2",
        )
        .bind(id)
        .bind(version)
        .fetch_optional(&mut *tx)
        .await?;
        current =
            Some(vid.ok_or_else(|| {
                ApiError::bad_request(format!("{id} has no version \"{version}\""))
            })?);
        detail.insert("current_version".into(), Value::from(version.as_str()));
    }
    let settings = match &patch.settings {
        None => None,
        Some(values) => {
            let vid = current
                .ok_or_else(|| ApiError::bad_request(format!("{id} has no current version")))?;
            let raw: Value =
                sqlx::query_scalar("SELECT manifest FROM addon_versions WHERE id = $1")
                    .bind(vid)
                    .fetch_one(&mut *tx)
                    .await?;
            let manifest = Manifest::from_value(raw).map_err(|e| {
                ApiError::internal(format!("stored manifest of {id} no longer validates: {e}"))
            })?;
            let values = manifest.validate_settings(values)?;
            // Keys only: values may be credentials.
            detail.insert(
                "settings".into(),
                Value::from(values.keys().cloned().collect::<Vec<_>>()),
            );
            Some(Value::Object(values))
        }
    };
    if let Some(e) = patch.enabled {
        detail.insert("enabled".into(), Value::from(e));
    }
    if let Some(a) = patch.audience {
        detail.insert("audience".into(), Value::from(a.as_str()));
    }
    if let Some(p) = patch.priority {
        detail.insert("priority".into(), Value::from(p));
    }

    sqlx::query(
        "UPDATE addons SET
             enabled = COALESCE($2, enabled),
             audience = COALESCE($3, audience),
             settings = COALESCE($4, settings),
             priority = COALESCE($5, priority),
             current_version_id = $6,
             name = COALESCE((SELECT manifest->>'name' FROM addon_versions WHERE id = $6), name),
             revision = revision + 1,
             updated_at = now(),
             updated_by = $7
         WHERE id = $1",
    )
    .bind(id)
    .bind(patch.enabled)
    .bind(patch.audience.map(Audience::as_str))
    .bind(settings)
    .bind(patch.priority)
    .bind(current)
    .bind(actor.label())
    .execute(&mut *tx)
    .await?;

    if let Some(people) = people {
        sqlx::query("DELETE FROM addon_audience WHERE addon_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO addon_audience (addon_id, email) SELECT $1, unnest($2::text[])")
            .bind(id)
            .bind(&people)
            .execute(&mut *tx)
            .await?;
        detail.insert("people".into(), Value::from(people.len()));
        if people.len() <= 50 {
            detail.insert("emails".into(), Value::from(people));
        }
    }
    audit(&mut *tx, actor, "addon.update", id, detail).await?;
    tx.commit().await?;
    get(db, id).await
}

/// Delete an addon and every version. 404 if it does not exist.
pub async fn delete(db: &PgPool, actor: &Actor, id: &str) -> ApiResult<()> {
    let mut tx = db.begin().await?;
    let row: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT a.slot, a.name, (SELECT count(*) FROM addon_versions v WHERE v.addon_id = a.id)
         FROM addons a WHERE a.id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let (slot, name, versions) = row.ok_or(ApiError::NotFound)?;
    sqlx::query("DELETE FROM addons WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let mut detail = Map::new();
    detail.insert("slot".into(), Value::from(slot));
    detail.insert("name".into(), Value::from(name));
    detail.insert("versions".into(), Value::from(versions));
    audit(&mut *tx, actor, "addon.delete", id, detail).await?;
    tx.commit().await?;
    Ok(())
}

/// The served bundle of one stored version (admin inspection), or 404.
pub async fn version_bundle(db: &PgPool, id: &str, version: &str) -> ApiResult<Vec<u8>> {
    sqlx::query_scalar("SELECT bundle FROM addon_versions WHERE addon_id = $1 AND version = $2")
        .bind(id)
        .bind(version)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)
}

/// One addon a person gets (`GET /v1/addons` entry).
#[derive(Clone, Debug, Serialize)]
pub struct ResolvedAddon {
    pub id: String,
    pub slot: String,
    pub name: String,
    pub version: String,
    pub sha256: String,
    /// Served bundle bytes.
    pub size: i64,
    pub entry: String,
    pub bridge: u64,
    pub platforms: Vec<String>,
    pub permissions: Value,
    /// Manifest defaults overlaid by the admin's values.
    pub settings: Map<String, Value>,
    /// `/v1/addons/{id}/bundle/{sha256}`.
    pub bundle: String,
    pub updated_at: String,
}

/// `GET /v1/addons` body.
#[derive(Clone, Debug, Serialize)]
pub struct Resolved {
    /// `r_` + 16 hex: changes whenever the list, a bundle or a resolved setting changes.
    pub revision: String,
    pub addons: Vec<ResolvedAddon>,
}

/// `r_` + the first 16 hex of SHA-256 over `[[id, slot, sha256, settings], …]`.
pub fn revision_of(addons: &[ResolvedAddon]) -> String {
    let key: Vec<Value> = addons
        .iter()
        .map(|a| json!([a.id, a.slot, a.sha256, a.settings]))
        .collect();
    let bytes = serde_json::to_vec(&key).unwrap_or_default();
    format!("r_{}", &crate::ids::sha256_hex(&bytes)[..16])
}

type ResolveRow = (
    String,
    String,
    String,
    Value,
    OffsetDateTime,
    String,
    String,
    i32,
    Value,
);

/// What `email` gets: enabled addons whose audience is everyone or lists the email
/// (case-insensitive); per slot the highest `priority`, then the latest change, wins.
pub async fn resolve(db: &PgPool, email: &str) -> ApiResult<Resolved> {
    let rows: Vec<ResolveRow> = sqlx::query_as(
        "SELECT DISTINCT ON (a.slot)
                a.id, a.slot, a.name, a.settings, a.updated_at,
                v.version, v.sha256, v.size_bytes, v.manifest
         FROM addons a JOIN addon_versions v ON v.id = a.current_version_id
         WHERE a.enabled
           AND (a.audience = 'everyone'
                OR EXISTS (SELECT 1 FROM addon_audience p
                           WHERE p.addon_id = a.id AND p.email = lower($1)))
         ORDER BY a.slot, a.priority DESC, a.updated_at DESC, a.id",
    )
    .bind(email.trim())
    .fetch_all(db)
    .await?;
    let mut addons = Vec::with_capacity(rows.len());
    for (id, slot, name, settings, updated_at, version, sha256, size, manifest) in rows {
        let manifest = match Manifest::from_value(manifest) {
            Ok(m) => m,
            Err(err) => {
                tracing::warn!(addon = %id, error = %err, "stored addon manifest no longer validates; skipped");
                continue;
            }
        };
        addons.push(ResolvedAddon {
            bundle: format!("/v1/addons/{id}/bundle/{sha256}"),
            settings: manifest.resolve_settings(&settings),
            entry: manifest.entry.clone(),
            bridge: manifest.bridge,
            platforms: manifest.platforms(),
            permissions: manifest.permissions_view(),
            id,
            slot,
            name,
            version,
            sha256,
            size: i64::from(size),
            updated_at: rfc3339(updated_at),
        });
    }
    Ok(Resolved {
        revision: revision_of(&addons),
        addons,
    })
}

/// The served bundle `sha256` of addon `id`, only while that version is what `email`
/// currently resolves to; otherwise 404.
pub async fn resolved_bundle(
    db: &PgPool,
    email: &str,
    id: &str,
    sha256: &str,
) -> ApiResult<Vec<u8>> {
    let resolved = resolve(db, email).await?;
    if !resolved
        .addons
        .iter()
        .any(|a| a.id == id && a.sha256 == sha256)
    {
        return Err(ApiError::NotFound);
    }
    sqlx::query_scalar("SELECT bundle FROM addon_versions WHERE addon_id = $1 AND sha256 = $2")
        .bind(id)
        .bind(sha256)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_are_normalized() {
        let got = normalize_people(&[
            " Bob@Example.com".into(),
            "bob@example.com".into(),
            "amy@example.org".into(),
        ])
        .unwrap();
        assert_eq!(got, ["amy@example.org", "bob@example.com"]);
        assert!(normalize_people(&["not an email".into()]).is_err());
        assert_eq!(normalize_people(&[]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn patch_rejects_unknown_fields() {
        let p: Patch = serde_json::from_value(json!({ "audience": "everyone" })).unwrap();
        assert_eq!(p.audience, Some(Audience::Everyone));
        assert!(serde_json::from_value::<Patch>(json!({ "audience": "some" })).is_err());
        assert!(serde_json::from_value::<Patch>(json!({ "slot": "x" })).is_err());
        assert!(Patch::default().is_noop());
    }

    #[test]
    fn revision_tracks_ids_bundles_and_settings() {
        let a = ResolvedAddon {
            id: "com.example.a".into(),
            slot: "agent-sidebar".into(),
            name: "A".into(),
            version: "1.0.0".into(),
            sha256: "0".repeat(64),
            size: 10,
            entry: "index.html".into(),
            bridge: 1,
            platforms: vec!["mac".into()],
            permissions: json!({}),
            settings: Map::new(),
            bundle: String::new(),
            updated_at: String::new(),
        };
        let empty = revision_of(&[]);
        let one = revision_of(std::slice::from_ref(&a));
        assert!(empty.starts_with("r_") && empty.len() == 18);
        assert_ne!(empty, one);
        let mut renamed = a.clone();
        renamed.name = "B".into();
        renamed.updated_at = "2026-10-19T00:00:00Z".into();
        assert_eq!(revision_of(&[renamed]), one);
        let mut set = a.clone();
        set.settings.insert("k".into(), json!(1));
        assert_ne!(revision_of(&[set]), one);
        let mut other = a;
        other.sha256 = "1".repeat(64);
        assert_ne!(revision_of(&[other]), one);
    }
}
