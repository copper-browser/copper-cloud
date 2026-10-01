//! Shared harness: real Postgres (`COPPER_CLOUD_TEST_DATABASE_URL`, default
//! `postgres://localhost:5432/copper_cloud_test_core`), migrations, a truncated database per
//! test (tests are serialized by a process-wide lock), and the real app served on an
//! ephemeral port.

#![allow(dead_code, clippy::missing_panics_doc)]

use std::net::SocketAddr;

use copper_cloud_core::config::Config;
use copper_cloud_core::state::{AppState, SharedState};
use serde_json::{json, Value};
use tokio::sync::{Mutex, MutexGuard};

pub const DEFAULT_DB: &str = "postgres://localhost:5432/copper_cloud_test_core";

static DB_LOCK: Mutex<()> = Mutex::const_new(());

pub fn database_url() -> String {
    std::env::var("COPPER_CLOUD_TEST_DATABASE_URL").unwrap_or_else(|_| DEFAULT_DB.to_owned())
}

pub struct TestServer {
    pub addr: SocketAddr,
    pub base: String,
    pub state: SharedState,
    pub http: reqwest::Client,
    _guard: MutexGuard<'static, ()>,
}

/// Fresh, migrated, truncated database + running server. Tweak config with `f`.
pub async fn start_with(f: impl FnOnce(&mut Config)) -> TestServer {
    let guard = DB_LOCK.lock().await;
    let mut cfg = Config::for_tests(&database_url());
    f(&mut cfg);
    let pool = copper_cloud_core::db::connect(&cfg)
        .await
        .expect("test database reachable (createdb copper_cloud_test_core)");
    if copper_cloud_core::db::migrate(&pool).await.is_err() {
        // A migration changed under us (development churn): rebuild the test schema.
        sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
            .execute(&pool)
            .await
            .expect("reset test schema");
        copper_cloud_core::db::migrate(&pool)
            .await
            .expect("migrations apply cleanly");
    }
    truncate_all(&pool).await;
    let state = AppState::new(pool, cfg);
    let app = copper_cloud::build_app(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    TestServer {
        addr,
        base: format!("http://{addr}"),
        state,
        http: reqwest::Client::new(),
        _guard: guard,
    }
}

pub async fn start() -> TestServer {
    start_with(|_| {}).await
}

/// Empty every application table (core + canvas) without dropping the schema.
pub async fn truncate_all(pool: &sqlx::PgPool) {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT quote_ident(tablename) FROM pg_tables
         WHERE schemaname = 'public' AND tablename <> '_sqlx_migrations'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    if !tables.is_empty() {
        sqlx::raw_sql(&format!(
            "TRUNCATE {} RESTART IDENTITY CASCADE",
            tables.join(", ")
        ))
        .execute(pool)
        .await
        .unwrap();
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    pub token: String,
    pub user_id: String,
    pub device_id: String,
}

impl TestServer {
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub fn key(&self) -> &str {
        &self.state.cfg.instance_key
    }

    /// Request with the instance key.
    pub fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.url(path))
            .header("X-Copper-Instance", self.key())
    }

    /// Request with the instance key and a bearer token.
    pub fn authed(
        &self,
        method: reqwest::Method,
        path: &str,
        s: &Session,
    ) -> reqwest::RequestBuilder {
        self.req(method, path).bearer_auth(&s.token)
    }

    pub async fn signup(&self, email: &str, password: &str, device: &str) -> Session {
        let resp = self
            .req(reqwest::Method::POST, "/v1/auth/signup")
            .json(&json!({
                "email": email,
                "password": password,
                "display_name": "Test User",
                "device": { "id": device, "name": "Test Mac" },
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            200,
            "signup {email}: {}",
            resp.text().await.unwrap()
        );
        session_from(&resp.json().await.unwrap())
    }

    pub async fn login(&self, email: &str, password: &str, device: &str) -> reqwest::Response {
        self.req(reqwest::Method::POST, "/v1/auth/login")
            .json(&json!({
                "email": email,
                "password": password,
                "device": { "id": device, "name": "Other Mac" },
            }))
            .send()
            .await
            .unwrap()
    }
}

pub fn session_from(v: &Value) -> Session {
    Session {
        token: v["token"].as_str().unwrap().to_owned(),
        user_id: v["user"]["id"].as_str().unwrap().to_owned(),
        device_id: v["device"]["id"].as_str().unwrap().to_owned(),
    }
}

pub fn new_device() -> String {
    uuid::Uuid::now_v7().to_string()
}

pub const PASSWORD: &str = "correct horse battery staple";

// ---------------------------------------------------------------------------------------------
// Access keys, access mode, admin API

pub const ADMIN_EMAIL: &str = "admin@example.com";
pub const ADMIN_PASSWORD: &str = "admin password 0123";

impl TestServer {
    /// Request with an explicit gate credential (`X-Copper-Instance`).
    pub fn gated(&self, method: reqwest::Method, path: &str, key: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.url(path))
            .header("X-Copper-Instance", key)
    }

    /// Set `access_mode` in the database and drop this server's 5 s cache (test hook).
    pub async fn set_access_mode(&self, mode: copper_cloud_core::access::AccessMode) {
        copper_cloud_core::access::set_access_mode(&self.state.db, mode)
            .await
            .unwrap();
        self.state.access_mode.clear();
    }

    /// Mint an access key straight in the database; returns `(id, key)`.
    pub async fn mint_key(
        &self,
        label: &str,
        email: Option<&str>,
        max_uses: Option<i32>,
    ) -> (uuid::Uuid, String) {
        let mut conn = self.state.db.acquire().await.unwrap();
        copper_cloud_core::access::mint_access_key(
            &mut conn,
            &copper_cloud_core::access::NewAccessKey {
                label,
                email,
                max_uses,
                ..Default::default()
            },
        )
        .await
        .unwrap()
    }

    /// `POST /v1/auth/signup` with gate credential `key`.
    pub async fn signup_with(&self, key: &str, email: &str, device: &str) -> reqwest::Response {
        self.gated(reqwest::Method::POST, "/v1/auth/signup", key)
            .json(&json!({
                "email": email,
                "password": PASSWORD,
                "device": { "id": device, "name": "Test Mac" },
            }))
            .send()
            .await
            .unwrap()
    }

    pub async fn create_admin(&self) {
        copper_cloud::admin_api::create_admin(&self.state.db, ADMIN_EMAIL, ADMIN_PASSWORD.into())
            .await
            .unwrap();
    }

    /// Admin API request with the CSRF header and (optionally) the session cookie.
    pub fn admin(
        &self,
        method: reqwest::Method,
        path: &str,
        cookie: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut rb = self
            .http
            .request(method, self.url(&format!("/admin/api/{path}")))
            .header("X-Requested-With", "copper-cloud-portal");
        if let Some(c) = cookie {
            rb = rb.header("Cookie", c);
        }
        rb
    }

    /// Log in as [`ADMIN_EMAIL`]; returns the `Cookie` header value (`cc_admin=…`).
    pub async fn admin_login(&self) -> String {
        let r = self
            .admin(reqwest::Method::POST, "login", None)
            .json(&json!({ "email": ADMIN_EMAIL, "password": ADMIN_PASSWORD }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "admin login");
        cookie_from(&r)
    }
}

/// `cc_admin=<token>` from a response's `Set-Cookie`.
pub fn cookie_from(r: &reqwest::Response) -> String {
    let set = r
        .headers()
        .get("set-cookie")
        .expect("set-cookie")
        .to_str()
        .unwrap();
    set.split(';').next().unwrap().trim().to_owned()
}
