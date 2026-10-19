//! Reads copper-cloud's legacy canvas tables (migrations `0100`–`0103`) — strictly read-only.
//!
//! Decryption follows the old `copper-cloud-canvas` crate exactly:
//!
//! * doc key: `canvases.doc_key_wrapped`, unwrapped with the copper KEK
//!   ([`Crypto::unwrap_key`]);
//! * snapshot (`canvas_snapshots.state`) and updates (`canvas_updates."update"`): AES-256-GCM
//!   under the doc key, AAD = the canvas id's 16 raw bytes (`store.rs`);
//! * mention excerpts (`canvas_mentions.excerpt_sealed`): same doc key, AAD =
//!   `b"copper-cloud/v1/mention:"` ‖ the mention row id's 16 raw bytes (`mentions.rs`).
//!
//! A canvas is read inside one `REPEATABLE READ, READ ONLY` transaction, so the snapshot and
//! the update tail are consistent even if a compaction ran concurrently (the runbook stops
//! copper-cloud first anyway). Updates are paged by `seq`, so memory stays bounded by one
//! page however large the canvas is.

use std::sync::{Arc, Mutex, PoisonError};

use anyhow::Context as _;
use base64::Engine as _;
use bytes::Bytes;
use copper_cloud_core::crypto::Crypto;
use futures::Stream;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::wire::{subject, Line, VERSION};

/// AAD prefix of a sealed mention excerpt in copper-cloud (`mentions.rs`).
pub const EXCERPT_AAD_PREFIX: &[u8] = b"copper-cloud/v1/mention:";

/// Update rows fetched per page while streaming.
const UPDATE_PAGE: i64 = 16;

/// The AAD binding a sealed update/snapshot to its canvas (`store.rs`).
pub fn doc_aad(canvas_id: Uuid) -> [u8; 16] {
    *canvas_id.as_bytes()
}

/// The AAD binding a sealed mention excerpt to its row (`mentions.rs`).
pub fn excerpt_aad(mention_id: Uuid) -> Vec<u8> {
    let mut aad = Vec::with_capacity(EXCERPT_AAD_PREFIX.len() + 16);
    aad.extend_from_slice(EXCERPT_AAD_PREFIX);
    aad.extend_from_slice(mention_id.as_bytes());
    aad
}

/// Optional parts of the legacy schema (older deployments may predate some migrations).
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)] // independent "does this table/column exist" flags
pub(crate) struct Schema {
    pub canvases: bool,
    pub nudged_at: bool,
    pub links: bool,
    pub mentions: bool,
    pub ledger: bool,
}

pub(crate) async fn schema(db: &PgPool) -> anyhow::Result<Schema> {
    let (canvases, nudged_at, links, mentions, ledger): (bool, bool, bool, bool, bool) =
        sqlx::query_as(
            "SELECT to_regclass('canvases') IS NOT NULL,
                    EXISTS (SELECT 1 FROM pg_attribute
                             WHERE attrelid = to_regclass('canvas_invites')
                               AND attname = 'nudged_at' AND NOT attisdropped),
                    to_regclass('canvas_share_links') IS NOT NULL,
                    to_regclass('canvas_mentions') IS NOT NULL,
                    to_regclass('canvas_migrations') IS NOT NULL",
        )
        .fetch_one(db)
        .await
        .context("inspecting the legacy canvas schema")?;
    Ok(Schema {
        canvases,
        nudged_at,
        links,
        mentions,
        ledger,
    })
}

/// Fails fast when `crypto` cannot unwrap the doc keys (not copper-cloud's `master_key`),
/// instead of failing every canvas one by one. Checks the oldest and the newest canvas.
pub(crate) async fn check_master_key(db: &PgPool, crypto: &Crypto) -> anyhow::Result<()> {
    let keys: Vec<(Uuid, Vec<u8>)> = sqlx::query_as(
        "(SELECT id, doc_key_wrapped FROM canvases ORDER BY created_at, id LIMIT 1)
         UNION
         (SELECT id, doc_key_wrapped FROM canvases ORDER BY created_at DESC, id DESC LIMIT 1)",
    )
    .fetch_all(db)
    .await
    .context("reading canvas doc keys")?;
    for (id, wrapped) in keys {
        crypto.unwrap_key(&wrapped).with_context(|| {
            format!("cannot unwrap the doc key of canvas {id}: is this copper-cloud's master_key?")
        })?;
    }
    Ok(())
}

/// A copper-cloud account, as provisioned into canvas-cloud.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct LegacyUser {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub disabled: bool,
}

pub(crate) async fn users(db: &PgPool) -> anyhow::Result<Vec<LegacyUser>> {
    sqlx::query_as("SELECT id, email, display_name, disabled FROM users ORDER BY created_at, id")
        .fetch_all(db)
        .await
        .context("reading users")
}

/// One legacy canvas (metadata only).
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct LegacyCanvas {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub owner_id: Uuid,
    /// Sealed bytes of snapshot + update tail (sizing the import timeout).
    pub stored_bytes: i64,
}

pub(crate) async fn canvases(db: &PgPool) -> anyhow::Result<Vec<LegacyCanvas>> {
    sqlx::query_as(
        r#"SELECT c.id, c.name, c.kind, c.owner_id,
                  (COALESCE((SELECT octet_length(s.state) FROM canvas_snapshots s
                              WHERE s.canvas_id = c.id), 0)
                   + COALESCE((SELECT sum(octet_length(u."update")) FROM canvas_updates u
                                WHERE u.canvas_id = c.id), 0))::bigint AS stored_bytes
           FROM canvases c
           ORDER BY c.created_at, c.id"#,
    )
    .fetch_all(db)
    .await
    .context("reading canvases")
}

/// What one canvas stream has produced so far (shared with the caller, which reads it after
/// the stream ends or fails).
#[derive(Debug, Default, Clone)]
pub(crate) struct Sent {
    pub updates: u64,
    /// Plaintext Yjs bytes (snapshot + updates).
    pub bytes: u64,
    pub members: u64,
    pub invites: u64,
    pub links: u64,
    pub mentions: u64,
    pub snapshot: bool,
    pub warnings: Vec<String>,
    /// The first error that aborted the stream (an HTTP error then only says "body error").
    pub error: Option<String>,
    /// The `end` line was produced.
    pub complete: bool,
}

pub(crate) type SharedSent = Arc<Mutex<Sent>>;

pub(crate) fn with_sent<R>(sent: &SharedSent, f: impl FnOnce(&mut Sent) -> R) -> R {
    f(&mut sent.lock().unwrap_or_else(PoisonError::into_inner))
}

#[derive(sqlx::FromRow)]
struct CanvasRow {
    name: String,
    kind: String,
    owner_id: Uuid,
    doc_key_wrapped: Vec<u8>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct MemberRow {
    user_id: Uuid,
    role: String,
    added_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct InviteRow {
    id: Uuid,
    email: String,
    invited_by: Option<Uuid>,
    token: String,
    status: String,
    created_at: OffsetDateTime,
    accepted_at: Option<OffsetDateTime>,
    nudged_at: Option<OffsetDateTime>,
}

#[derive(sqlx::FromRow)]
struct LinkRow {
    id: Uuid,
    token_sha256: Vec<u8>,
    role: String,
    created_by: Option<Uuid>,
    created_at: OffsetDateTime,
    uses: i64,
}

#[derive(sqlx::FromRow)]
struct MentionRow {
    id: Uuid,
    message_id: String,
    from_user: Uuid,
    to_user: Uuid,
    excerpt_sealed: Vec<u8>,
    created_at: OffsetDateTime,
    read_at: Option<OffsetDateTime>,
}

fn invites_sql(schema: Schema) -> &'static str {
    if schema.nudged_at {
        "SELECT id, email, invited_by, token, status, created_at, accepted_at, nudged_at
         FROM canvas_invites WHERE canvas_id = $1 ORDER BY created_at, id"
    } else {
        "SELECT id, email, invited_by, token, status, created_at, accepted_at,
                NULL::timestamptz AS nudged_at
         FROM canvas_invites WHERE canvas_id = $1 ORDER BY created_at, id"
    }
}

/// Lines buffered between the database reader and the HTTP body.
const CHANNEL_LINES: usize = 4;

/// The import v1 body of one canvas, produced lazily by a reader task behind a small bounded
/// channel (backpressure: at most a few lines in flight). Never writes to the database. The
/// first error is recorded in `sent.error` and ends the stream with `Err`, which aborts the
/// HTTP body — canvas-cloud then never sees the `end` line and commits nothing.
pub(crate) fn canvas_stream(
    db: PgPool,
    crypto: Crypto,
    canvas_id: Uuid,
    schema: Schema,
    sent: SharedSent,
) -> impl Stream<Item = anyhow::Result<Bytes>> + Send + 'static {
    let (out, rx) = tokio::sync::mpsc::channel::<anyhow::Result<Bytes>>(CHANNEL_LINES);
    tokio::spawn(async move {
        let result = produce(&db, &crypto, canvas_id, schema, &sent, &out).await;
        match result {
            Ok(()) => with_sent(&sent, |s| s.complete = true),
            Err(e) => {
                with_sent(&sent, |s| {
                    s.error.get_or_insert_with(|| format!("{e:#}"));
                });
                // The receiver may be gone (request already failed); nothing else to do.
                let _ = out.send(Err(e)).await;
            }
        }
    });
    futures::stream::unfold(rx, |mut rx| async move {
        let item = rx.recv().await?;
        Some((item, rx))
    })
}

/// canvas-cloud's default import line limit (CONTRACT §6.1). Configurable server-side, so
/// an oversized line is only a warning here (visible in a dry run); the server decides.
pub(crate) const LINE_LIMIT: usize = 32 * 1024 * 1024;

fn check_line(sent: &SharedSent, what: &str, line: &[u8]) {
    if line.len() > LINE_LIMIT {
        with_sent(sent, |s| {
            s.warnings.push(format!(
                "{what} is a {} MiB import line, over canvas-cloud's default 32 MiB line limit",
                line.len() / (1024 * 1024)
            ));
        });
    }
}

/// Sends one line; fails when the consumer went away (the request was dropped).
async fn emit(
    out: &tokio::sync::mpsc::Sender<anyhow::Result<Bytes>>,
    bytes: Bytes,
) -> anyhow::Result<()> {
    out.send(Ok(bytes))
        .await
        .map_err(|_| anyhow::anyhow!("the import request ended before the body was sent"))
}

#[allow(clippy::too_many_lines)] // one linear pass over the format, line type by line type
async fn produce(
    db: &PgPool,
    crypto: &Crypto,
    canvas_id: Uuid,
    schema: Schema,
    sent: &SharedSent,
    out: &tokio::sync::mpsc::Sender<anyhow::Result<Bytes>>,
) -> anyhow::Result<()> {
    let mut tx = db.begin().await.context("opening a read transaction")?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let canvas: CanvasRow = sqlx::query_as(
        "SELECT name, kind, owner_id, doc_key_wrapped, created_at, updated_at
         FROM canvases WHERE id = $1",
    )
    .bind(canvas_id)
    .fetch_optional(&mut *tx)
    .await?
    .context("the canvas disappeared from the legacy tables")?;
    let key = crypto
        .unwrap_key(&canvas.doc_key_wrapped)
        .context("unwrapping the canvas doc key (is this copper-cloud's master_key?)")?;
    let aad = doc_aad(canvas_id);

    let bytes = Line::Canvas {
        version: VERSION,
        id: canvas_id,
        name: &canvas.name,
        kind: &canvas.kind,
        owner: subject(canvas.owner_id),
        created_at: canvas.created_at,
        updated_at: canvas.updated_at,
    }
    .to_bytes()?;
    emit(out, bytes).await?;

    let members: Vec<MemberRow> = sqlx::query_as(
        "SELECT user_id, role, added_at FROM canvas_members WHERE canvas_id = $1
         ORDER BY (role = 'owner') DESC, added_at, user_id",
    )
    .bind(canvas_id)
    .fetch_all(&mut *tx)
    .await?;
    for m in &members {
        let bytes = Line::Member {
            subject: subject(m.user_id),
            role: &m.role,
            added_at: m.added_at,
        }
        .to_bytes()?;
        emit(out, bytes).await?;
    }
    with_sent(sent, |s| s.members = members.len() as u64);

    let invites: Vec<InviteRow> = sqlx::query_as(invites_sql(schema))
        .bind(canvas_id)
        .fetch_all(&mut *tx)
        .await?;
    for i in &invites {
        // `canvas_invites.token` already is the lower-case hex SHA-256 of the invite's
        // random secret (rest.rs: `ids::sha256_hex(random_token())`), so it is sent as is.
        let bytes = Line::Invite {
            id: i.id,
            email: &i.email,
            invited_by: i.invited_by.map(subject),
            status: &i.status,
            created_at: i.created_at,
            accepted_at: i.accepted_at,
            nudged_at: i.nudged_at,
            token_sha256_hex: &i.token,
        }
        .to_bytes()?;
        emit(out, bytes).await?;
    }
    with_sent(sent, |s| s.invites = invites.len() as u64);

    if schema.links {
        let links: Vec<LinkRow> = sqlx::query_as(
            "SELECT id, token_sha256, role, created_by, created_at, uses
             FROM canvas_share_links WHERE canvas_id = $1 ORDER BY created_at, id",
        )
        .bind(canvas_id)
        .fetch_all(&mut *tx)
        .await?;
        for l in &links {
            let bytes = Line::Link {
                id: l.id,
                token_sha256_hex: hex::encode(&l.token_sha256),
                role: &l.role,
                created_by: l.created_by.map(subject),
                created_at: l.created_at,
                uses: l.uses,
            }
            .to_bytes()?;
            emit(out, bytes).await?;
        }
        with_sent(sent, |s| s.links = links.len() as u64);
    }

    if schema.mentions {
        let mentions: Vec<MentionRow> = sqlx::query_as(
            "SELECT id, message_id, from_user, to_user, excerpt_sealed, created_at, read_at
             FROM canvas_mentions WHERE canvas_id = $1 ORDER BY created_at, id",
        )
        .bind(canvas_id)
        .fetch_all(&mut *tx)
        .await?;
        for m in &mentions {
            // The old server served an excerpt that no longer decrypts as "" (with a
            // warning); keep that behaviour rather than failing the whole canvas.
            let excerpt =
                if let Ok(bytes) = Crypto::open(&key, &excerpt_aad(m.id), &m.excerpt_sealed) {
                    String::from_utf8_lossy(&bytes).into_owned()
                } else {
                    with_sent(sent, |s| {
                        s.warnings.push(format!(
                            "mention {} excerpt does not decrypt; sent empty",
                            m.id
                        ));
                    });
                    String::new()
                };
            let bytes = Line::Mention {
                id: m.id,
                message_id: &m.message_id,
                from: subject(m.from_user),
                to: subject(m.to_user),
                excerpt: &excerpt,
                created_at: m.created_at,
                read_at: m.read_at,
            }
            .to_bytes()?;
            emit(out, bytes).await?;
        }
        with_sent(sent, |s| s.mentions = mentions.len() as u64);
    }

    let engine = base64::engine::general_purpose::STANDARD;
    let snapshot: Option<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT seq, state FROM canvas_snapshots WHERE canvas_id = $1")
            .bind(canvas_id)
            .fetch_optional(&mut *tx)
            .await?;
    let mut since = 0_i64;
    if let Some((seq, sealed)) = snapshot {
        let state = Crypto::open(&key, &aad, &sealed)
            .with_context(|| format!("the snapshot (seq {seq}) does not decrypt"))?;
        since = seq;
        with_sent(sent, |s| {
            s.snapshot = true;
            s.updates += 1;
            s.bytes += state.len() as u64;
        });
        let bytes = Line::Update {
            b64: engine.encode(&state),
        }
        .to_bytes()?;
        check_line(sent, "the snapshot", &bytes);
        emit(out, bytes).await?;
    }
    loop {
        let page: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            r#"SELECT seq, "update" FROM canvas_updates
               WHERE canvas_id = $1 AND seq > $2 ORDER BY seq LIMIT $3"#,
        )
        .bind(canvas_id)
        .bind(since)
        .bind(UPDATE_PAGE)
        .fetch_all(&mut *tx)
        .await?;
        if page.is_empty() {
            break;
        }
        for (seq, sealed) in page {
            let update = Crypto::open(&key, &aad, &sealed)
                .with_context(|| format!("update seq {seq} does not decrypt"))?;
            since = seq;
            with_sent(sent, |s| {
                s.updates += 1;
                s.bytes += update.len() as u64;
            });
            let bytes = Line::Update {
                b64: engine.encode(&update),
            }
            .to_bytes()?;
            check_line(sent, &format!("update seq {seq}"), &bytes);
            emit(out, bytes).await?;
        }
    }
    tx.rollback().await?;

    let end = with_sent(sent, |s| Line::End {
        updates: s.updates,
        members: s.members,
        invites: s.invites,
        links: s.links,
        mentions: s.mentions,
    });
    let bytes = end.to_bytes()?;
    emit(out, bytes).await?;
    Ok(())
}
