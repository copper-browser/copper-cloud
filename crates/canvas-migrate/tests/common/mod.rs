//! Test harness: an isolated schema per test in the native PostgreSQL test database
//! (`COPPER_CLOUD_TEST_MIGRATE_DATABASE_URL`, default `postgres://localhost:5432/copper_cloud_test_migrate`) with
//! copper-cloud's real migrations applied, legacy canvas rows seeded through the same sealing
//! code paths the old `copper-cloud-canvas` crate used, and the mock Canvas Cloud.

#![allow(dead_code, clippy::missing_panics_doc)]

pub mod digest;
pub mod mock;

use std::path::Path;
use std::str::FromStr as _;
use std::time::Duration;

use copper_cloud_canvas_migrate::{doc_aad, excerpt_aad, run, MigrateOptions, MigrateReport};
use copper_cloud_core::crypto::Crypto;
use copper_cloud_core::ids;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use uuid::Uuid;
use yrs::updates::decoder::Decode as _;
use yrs::{Doc, Map as _, ReadTxn as _, StateVector, Transact as _, Update};

pub const DEFAULT_DATABASE_URL: &str = "postgres://localhost:5432/copper_cloud_test_migrate";

pub fn database_url() -> String {
    std::env::var("COPPER_CLOUD_TEST_MIGRATE_DATABASE_URL")
        .unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned())
}

/// The ledger table as cc-link's `0030_canvas_link.sql` defines it (created here only if the
/// migration is not in the tree yet).
pub const LEDGER_DDL: &str = "CREATE TABLE IF NOT EXISTS canvas_migrations (
    canvas_id   uuid PRIMARY KEY,
    status      text,
    digest      text,
    updates     int,
    migrated_at timestamptz,
    detail      jsonb
)";

/// One isolated schema with copper-cloud's migrations applied.
pub struct TestDb {
    pub pool: PgPool,
    admin: PgPool,
    schema: String,
}

impl TestDb {
    pub async fn new() -> Self {
        let base = PgConnectOptions::from_str(&database_url()).expect("database url");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(base.clone())
            .await
            .expect("connecting to the test database (createdb -w copper_cloud_test_migrate)");
        sweep_stale_schemas(&admin).await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let schema = format!("ccmig_{now}_{}", &Uuid::new_v4().simple().to_string()[..12]);
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(base.options([("search_path", schema.as_str())]))
            .await
            .unwrap();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../copper-cloud/migrations");
        let migrator = sqlx::migrate::Migrator::new(dir)
            .await
            .expect("reading copper-cloud migrations");
        migrator
            .run(&pool)
            .await
            .expect("applying copper-cloud migrations");
        sqlx::query(LEDGER_DDL).execute(&pool).await.unwrap();
        Self {
            pool,
            admin,
            schema,
        }
    }

    pub async fn cleanup(self) {
        self.pool.close().await;
        let _ = sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await;
    }
}

/// Drops `ccmig_<unix>_…` schemas older than two hours, left behind by panicked tests
/// (a passing test drops its own schema in [`TestDb::cleanup`]).
async fn sweep_stale_schemas(admin: &PgPool) {
    let Ok(names) = sqlx::query_scalar::<_, String>(
        r"SELECT nspname FROM pg_namespace WHERE nspname LIKE 'ccmig\_%'",
    )
    .fetch_all(admin)
    .await
    else {
        return;
    };
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(2 * 3600);
    for name in names {
        let born = name.split('_').nth(1).and_then(|t| t.parse::<u64>().ok());
        if born.is_some_and(|t| t < cutoff) {
            let _ = sqlx::query(&format!("DROP SCHEMA IF EXISTS {name} CASCADE"))
                .execute(admin)
                .await;
        }
    }
}

/// A seeded legacy canvas.
#[derive(Clone)]
pub struct Canvas {
    pub id: Uuid,
    pub key: [u8; 32],
    /// Every plaintext update ever appended (including the ones a compaction later folded
    /// into the snapshot): the ground truth of the document.
    pub log: Vec<Vec<u8>>,
}

impl Canvas {
    /// The §6.3 digest of the full update log.
    pub fn truth(&self) -> String {
        let doc = Doc::new();
        {
            let mut txn = doc.transact_mut();
            for u in &self.log {
                txn.apply_update(Update::decode_v1(u).unwrap()).unwrap();
            }
        }
        digest::digest(&doc)
    }
}

/// Writes legacy rows exactly like copper-cloud 0.8 did (`rest.rs`, `store.rs`,
/// `mentions.rs`).
#[derive(Clone)]
pub struct Legacy {
    pub db: PgPool,
    pub crypto: Crypto,
}

impl Legacy {
    pub fn new(db: &PgPool) -> Self {
        Self {
            db: db.clone(),
            crypto: Crypto::from_master_key(&[7u8; 32]),
        }
    }

    pub async fn user(&self, email: &str, display_name: &str, disabled: bool) -> Uuid {
        let id = ids::uuid_v7();
        sqlx::query(
            "INSERT INTO users (id, email, display_name, password_hash, data_key_wrapped, disabled)
             VALUES ($1, $2, $3, 'argon2-not-used', $4, $5)",
        )
        .bind(id)
        .bind(email)
        .bind(display_name)
        .bind(self.crypto.wrap_key(&Crypto::new_key()))
        .bind(disabled)
        .execute(&self.db)
        .await
        .unwrap();
        id
    }

    /// `rest.rs::insert_canvas`: canvas row, owner membership, initial `meta` update.
    pub async fn canvas(&self, owner: Uuid, name: &str, kind: &str) -> Canvas {
        let mut c = self.bare_canvas(owner, name, kind).await;
        let doc = Doc::new();
        let meta = doc.get_or_insert_map("meta");
        {
            let mut txn = doc.transact_mut();
            meta.insert(&mut txn, "name", name);
            meta.insert(&mut txn, "createdBy", owner.to_string());
        }
        let initial = doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        self.append(&mut c, &initial).await;
        c
    }

    /// A canvas row + owner membership but no document rows at all.
    pub async fn bare_canvas(&self, owner: Uuid, name: &str, kind: &str) -> Canvas {
        let id = ids::uuid_v7();
        let key = Crypto::new_key();
        sqlx::query(
            "INSERT INTO canvases (id, owner_id, name, kind, doc_key_wrapped)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(owner)
        .bind(name)
        .bind(kind)
        .bind(self.crypto.wrap_key(&key))
        .execute(&self.db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO canvas_members (canvas_id, user_id, role) VALUES ($1, $2, 'owner')",
        )
        .bind(id)
        .bind(owner)
        .execute(&self.db)
        .await
        .unwrap();
        Canvas {
            id,
            key,
            log: Vec::new(),
        }
    }

    /// `store.rs::append`.
    pub async fn append(&self, c: &mut Canvas, update: &[u8]) -> i64 {
        let sealed = Crypto::seal(&c.key, &doc_aad(c.id), update);
        let seq: i64 = sqlx::query_scalar(
            r#"INSERT INTO canvas_updates (canvas_id, "update") VALUES ($1, $2) RETURNING seq"#,
        )
        .bind(c.id)
        .bind(&sealed)
        .fetch_one(&self.db)
        .await
        .unwrap();
        c.log.push(update.to_vec());
        seq
    }

    /// `store.rs::compact`: the room's merged state of every update up to `upto_seq` becomes
    /// the snapshot and the covered rows go.
    pub async fn compact(&self, c: &Canvas, upto_seq: i64) {
        let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            r#"SELECT seq, "update" FROM canvas_updates WHERE canvas_id = $1 AND seq <= $2 ORDER BY seq"#,
        )
        .bind(c.id)
        .bind(upto_seq)
        .fetch_all(&self.db)
        .await
        .unwrap();
        let doc = Doc::new();
        {
            let mut txn = doc.transact_mut();
            if let Some(state) = self.snapshot_state(c).await {
                txn.apply_update(Update::decode_v1(&state).unwrap())
                    .unwrap();
            }
            for (_, sealed) in rows {
                let plain = Crypto::open(&c.key, &doc_aad(c.id), &sealed).unwrap();
                txn.apply_update(Update::decode_v1(&plain).unwrap())
                    .unwrap();
            }
        }
        let state = doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        let sealed = Crypto::seal(&c.key, &doc_aad(c.id), &state);
        sqlx::query(
            "INSERT INTO canvas_snapshots (canvas_id, seq, state, updated_at) VALUES ($1, $2, $3, now())
             ON CONFLICT (canvas_id) DO UPDATE SET seq = EXCLUDED.seq, state = EXCLUDED.state, updated_at = now()",
        )
        .bind(c.id)
        .bind(upto_seq)
        .bind(&sealed)
        .execute(&self.db)
        .await
        .unwrap();
        sqlx::query("DELETE FROM canvas_updates WHERE canvas_id = $1 AND seq <= $2")
            .bind(c.id)
            .bind(upto_seq)
            .execute(&self.db)
            .await
            .unwrap();
    }

    async fn snapshot_state(&self, c: &Canvas) -> Option<Vec<u8>> {
        let sealed: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT state FROM canvas_snapshots WHERE canvas_id = $1")
                .bind(c.id)
                .fetch_optional(&self.db)
                .await
                .unwrap();
        sealed.map(|s| Crypto::open(&c.key, &doc_aad(c.id), &s).unwrap())
    }

    pub async fn member(&self, c: &Canvas, user: Uuid) {
        sqlx::query(
            "INSERT INTO canvas_members (canvas_id, user_id, role) VALUES ($1, $2, 'editor')",
        )
        .bind(c.id)
        .bind(user)
        .execute(&self.db)
        .await
        .unwrap();
    }

    /// `rest.rs` invite: `token` = hex SHA-256 of a random secret. Returns `(id, token)`.
    pub async fn invite(
        &self,
        c: &Canvas,
        email: &str,
        invited_by: Option<Uuid>,
        status: &str,
        nudged: bool,
    ) -> (Uuid, String) {
        let id = ids::uuid_v7();
        let token = ids::sha256_hex(ids::random_token().as_bytes());
        sqlx::query(
            "INSERT INTO canvas_invites (id, canvas_id, email, invited_by, token, status, accepted_at, nudged_at)
             VALUES ($1, $2, $3, $4, $5, $6,
                     CASE WHEN $6 = 'accepted' THEN now() END,
                     CASE WHEN $7 THEN now() END)",
        )
        .bind(id)
        .bind(c.id)
        .bind(email)
        .bind(invited_by)
        .bind(&token)
        .bind(status)
        .bind(nudged)
        .execute(&self.db)
        .await
        .unwrap();
        (id, token)
    }

    /// `rest.rs` share link: only the SHA-256 of the token is stored. Returns
    /// `(id, plaintext token)`.
    pub async fn share_link(&self, c: &Canvas, created_by: Uuid, uses: i64) -> (Uuid, String) {
        let id = ids::uuid_v7();
        let token = ids::random_token();
        sqlx::query(
            "INSERT INTO canvas_share_links (id, canvas_id, token_sha256, role, created_by, uses)
             VALUES ($1, $2, $3, 'editor', $4, $5)",
        )
        .bind(id)
        .bind(c.id)
        .bind(ids::sha256(token.as_bytes()).to_vec())
        .bind(created_by)
        .bind(uses)
        .execute(&self.db)
        .await
        .unwrap();
        (id, token)
    }

    /// `mentions.rs::post_mentions`: excerpt sealed under the doc key, AAD
    /// `"copper-cloud/v1/mention:" ‖ row id`.
    pub async fn mention(
        &self,
        c: &Canvas,
        from: Uuid,
        to: Uuid,
        message_id: &str,
        excerpt: &str,
        read: bool,
    ) -> Uuid {
        let id = ids::uuid_v7();
        let sealed = Crypto::seal(&c.key, &excerpt_aad(id), excerpt.as_bytes());
        sqlx::query(
            "INSERT INTO canvas_mentions (id, canvas_id, message_id, from_user, to_user, excerpt_sealed, read_at)
             VALUES ($1, $2, $3, $4, $5, $6, CASE WHEN $7 THEN now() END)",
        )
        .bind(id)
        .bind(c.id)
        .bind(message_id)
        .bind(from)
        .bind(to)
        .bind(&sealed)
        .bind(read)
        .execute(&self.db)
        .await
        .unwrap();
        id
    }
}

/// A Yjs client editing a canvas: every edit returns the incremental update it produced.
pub struct Client {
    pub doc: Doc,
}

impl Client {
    pub fn new(client_id: u64, base: &[Vec<u8>]) -> Self {
        let doc = Doc::with_client_id(client_id);
        {
            let mut txn = doc.transact_mut();
            for u in base {
                txn.apply_update(Update::decode_v1(u).unwrap()).unwrap();
            }
        }
        Self { doc }
    }

    pub fn edit(&self, f: impl FnOnce(&Doc)) -> Vec<u8> {
        let before = self.doc.transact().state_vector();
        f(&self.doc);
        self.doc.transact().encode_state_as_update_v1(&before)
    }
}

/// Everything a migration test needs.
pub struct Env {
    pub db: TestDb,
    pub legacy: Legacy,
    pub mock: mock::Mock,
    pub client: reqwest::Client,
}

impl Env {
    pub async fn new() -> Self {
        let db = TestDb::new().await;
        let legacy = Legacy::new(&db.pool);
        let mock = mock::Mock::start().await;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap();
        Self {
            db,
            legacy,
            mock,
            client,
        }
    }

    pub async fn migrate(&self, opts: MigrateOptions) -> anyhow::Result<MigrateReport> {
        self.migrate_with_key(mock::APP_KEY, opts).await
    }

    pub async fn migrate_with_key(
        &self,
        key: &str,
        opts: MigrateOptions,
    ) -> anyhow::Result<MigrateReport> {
        let report = run(
            &self.db.pool,
            &self.legacy.crypto,
            &self.client,
            &self.mock.url,
            key,
            opts,
        )
        .await?;
        println!("{report}");
        Ok(report)
    }

    pub async fn ledger(&self) -> Vec<(Uuid, String, Option<String>, Option<i32>)> {
        sqlx::query_as(
            "SELECT canvas_id, status, digest, updates FROM canvas_migrations ORDER BY canvas_id",
        )
        .fetch_all(&self.db.pool)
        .await
        .unwrap()
    }

    pub async fn ledger_row(
        &self,
        id: Uuid,
    ) -> Option<(String, Option<String>, serde_json::Value)> {
        sqlx::query_as("SELECT status, digest, detail FROM canvas_migrations WHERE canvas_id = $1")
            .bind(id)
            .fetch_optional(&self.db.pool)
            .await
            .unwrap()
    }

    /// A fingerprint of every legacy table, to prove the tool never writes them.
    pub async fn legacy_fingerprint(&self) -> String {
        let mut out = String::new();
        for table in [
            "users",
            "canvases",
            "canvas_members",
            "canvas_invites",
            "canvas_updates",
            "canvas_snapshots",
            "canvas_share_links",
            "canvas_mentions",
        ] {
            let rows: Vec<(String,)> =
                sqlx::query_as(&format!("SELECT t::text FROM {table} t ORDER BY t::text"))
                    .fetch_all(&self.db.pool)
                    .await
                    .unwrap();
            out.push_str(table);
            for (r,) in rows {
                out.push_str(&r);
                out.push('\n');
            }
        }
        out
    }

    pub async fn finish(self) {
        self.db.cleanup().await;
    }
}

/// Default options for tests (`concurrency` 2).
pub fn opts() -> MigrateOptions {
    MigrateOptions::default()
}
