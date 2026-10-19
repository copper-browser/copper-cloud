//! Single sign-on end to end against a fake identity provider (a local axum server standing
//! in for Google's token endpoint and an OIDC issuer): provider admin, `GET /v1/auth/sso`,
//! `POST /v1/auth/sso/{id}`, account linking/creation, password sign-in policy and `/start`.

#![allow(clippy::too_many_lines)]

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use common::{new_device, start_with, TestServer, PASSWORD};
use copper_cloud_core::access::AccessMode;
use reqwest::Method;
use serde_json::{json, Value};

const CLIENT_ID: &str = "123-abc.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "GOCSPX-test-secret-value";
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const REDIRECT: &str = "http://127.0.0.1:53682/";

// ---------------------------------------------------------------------------------------------
// Fake identity provider

#[derive(Default)]
struct IdpState {
    base: String,
    /// code → ID token claims.
    codes: HashMap<String, Value>,
    /// Every token request's form.
    requests: Vec<HashMap<String, String>>,
    /// The secret the token endpoint accepts (`None`: public client, no secret expected).
    secret: Option<String>,
}

#[derive(Clone)]
struct Idp {
    base: String,
    state: Arc<Mutex<IdpState>>,
}

fn unsigned_jwt(claims: &Value) -> String {
    let enc = |v: &[u8]| copper_cloud_core::ids::b64url(v);
    format!(
        "{}.{}.{}",
        enc(br#"{"alg":"RS256","kid":"test"}"#),
        enc(claims.to_string().as_bytes()),
        enc(b"not-a-signature")
    )
}

async fn discovery(State(idp): State<Arc<Mutex<IdpState>>>) -> Json<Value> {
    let base = idp.lock().unwrap().base.clone();
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "jwks_uri": format!("{base}/jwks"),
    }))
}

async fn token(
    State(idp): State<Arc<Mutex<IdpState>>>,
    Form(form): Form<HashMap<String, String>>,
) -> (axum::http::StatusCode, Json<Value>) {
    use axum::http::StatusCode;
    let mut st = idp.lock().unwrap();
    st.requests.push(form.clone());
    if form.get("client_secret") != st.secret.as_ref()
        || form.get("client_id").map(String::as_str) != Some(CLIENT_ID)
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid_client" })),
        );
    }
    let code = form.get("code").cloned().unwrap_or_default();
    match st.codes.remove(&code) {
        Some(claims)
            if form.get("grant_type").map(String::as_str) == Some("authorization_code") =>
        {
            (
                StatusCode::OK,
                Json(json!({
                    "access_token": "ya29.x",
                    "token_type": "Bearer",
                    "expires_in": 3599,
                    "id_token": unsigned_jwt(&claims),
                })),
            )
        }
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_grant" })),
        ),
    }
}

impl Idp {
    async fn start(secret: Option<&str>) -> Self {
        let state = Arc::new(Mutex::new(IdpState {
            secret: secret.map(str::to_owned),
            ..IdpState::default()
        }));
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/token", post(token))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = format!("http://{addr}");
        state.lock().unwrap().base.clone_from(&base);
        Self { base, state }
    }

    /// Register a one-time code that yields `claims`.
    fn code(&self, claims: Value) -> String {
        let code = format!("4/0A{}", uuid::Uuid::now_v7().simple());
        self.state
            .lock()
            .unwrap()
            .codes
            .insert(code.clone(), claims);
        code
    }

    fn last_request(&self) -> HashMap<String, String> {
        self.state.lock().unwrap().requests.last().cloned().unwrap()
    }
}

/// Google-shaped claims for `email` (Workspace `hd` = the email's domain).
fn google_claims(sub: &str, email: &str) -> Value {
    let domain = email.rsplit_once('@').unwrap().1;
    json!({
        "iss": "https://accounts.google.com",
        "azp": CLIENT_ID,
        "aud": CLIENT_ID,
        "sub": sub,
        "hd": domain,
        "email": email,
        "email_verified": true,
        "name": "Ana Example",
        "iat": time::OffsetDateTime::now_utc().unix_timestamp(),
        "exp": time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
    })
}

// ---------------------------------------------------------------------------------------------
// Harness

/// Server whose Google token endpoint is the fake identity provider's (plain HTTP allowed).
async fn start(idp: &Idp) -> TestServer {
    let token = format!("{}/token", idp.base);
    start_with(|cfg| {
        cfg.sso_test.google_token_endpoint = Some(token);
        cfg.sso_test.allow_http = true;
        cfg.limits.auth_per_minute = 1000;
    })
    .await
}

async fn json_of(r: reqwest::Response) -> Value {
    let status = r.status();
    let text = r.text().await.unwrap();
    serde_json::from_str(&text).unwrap_or_else(|_| panic!("{status}: {text}"))
}

async fn put_google(s: &TestServer, cookie: &str, body: Value) -> reqwest::Response {
    s.admin(Method::PUT, "sso/providers/google", Some(cookie))
        .json(&body)
        .send()
        .await
        .unwrap()
}

fn google_body(domains: &[&str]) -> Value {
    json!({
        "kind": "google",
        "name": "Google",
        "client_id": CLIENT_ID,
        "client_secret": CLIENT_SECRET,
        "domains": domains,
        "auto_create": true,
        "enabled": true,
    })
}

/// Admin + Google provider for `example.com`; returns the admin cookie.
async fn setup_google(s: &TestServer) -> String {
    s.create_admin().await;
    let cookie = s.admin_login().await;
    let r = put_google(s, &cookie, google_body(&["example.com"])).await;
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    cookie
}

/// `POST /v1/auth/sso/{provider}` WITHOUT the gate header.
async fn sign_in(s: &TestServer, provider: &str, code: &str, device: &str) -> reqwest::Response {
    sign_in_with(
        s,
        provider,
        json!({
            "code": code,
            "code_verifier": VERIFIER,
            "redirect_uri": REDIRECT,
            "device": { "id": device, "name": "Ana's MacBook" },
        }),
    )
    .await
}

async fn sign_in_with(s: &TestServer, provider: &str, body: Value) -> reqwest::Response {
    s.http
        .post(s.url(&format!("/v1/auth/sso/{provider}")))
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn expect_error(r: reqwest::Response, status: u16, code: &str) -> Value {
    assert_eq!(r.status(), status);
    let body = json_of(r).await;
    assert_eq!(body["error"], code, "{body}");
    body
}

async fn me(s: &TestServer, gate: &str, token: &str) -> u16 {
    s.gated(Method::GET, "/v1/auth/me", gate)
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

// ---------------------------------------------------------------------------------------------
// Tests

#[tokio::test]
async fn provider_list_is_public_and_follows_admin_config() {
    let idp = Idp::start(Some(CLIENT_SECRET)).await;
    let s = start(&idp).await;
    let off = json_of(s.http.get(s.url("/v1/auth/sso")).send().await.unwrap()).await;
    assert_eq!(
        off,
        json!({ "enabled": false, "password_signin": true, "providers": [] })
    );

    let cookie = setup_google(&s).await;
    let on = json_of(s.http.get(s.url("/v1/auth/sso")).send().await.unwrap()).await;
    assert_eq!(on["enabled"], true);
    assert_eq!(
        on["providers"],
        json!([{
            "id": "google", "kind": "google", "name": "Google", "client_id": CLIENT_ID,
            "authorization_endpoint": "https://accounts.google.com/o/oauth2/v2/auth",
            "scopes": ["openid", "email", "profile"], "domains": ["example.com"],
            "hd": "example.com",
        }])
    );

    // Two domains: no `hd` hint.
    let r = put_google(&s, &cookie, google_body(&["example.com", "@Example.ORG"])).await;
    assert_eq!(r.status(), 200);
    let on = json_of(s.http.get(s.url("/v1/auth/sso")).send().await.unwrap()).await;
    assert_eq!(
        on["providers"][0]["domains"],
        json!(["example.com", "example.org"])
    );
    assert_eq!(on["providers"][0]["hd"], Value::Null);

    // An OIDC provider gets its authorization endpoint from discovery.
    let r = s
        .admin(Method::PUT, "sso/providers/okta", Some(&cookie))
        .json(&json!({
            "kind": "oidc", "name": "Okta", "client_id": CLIENT_ID,
            "issuer": format!("{}/", idp.base), "domains": ["example.com"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    let on = json_of(s.http.get(s.url("/v1/auth/sso")).send().await.unwrap()).await;
    let okta = &on["providers"][1];
    assert_eq!(okta["id"], "okta");
    assert_eq!(okta["kind"], "oidc");
    assert_eq!(
        okta["authorization_endpoint"],
        format!("{}/authorize", idp.base)
    );
    assert_eq!(okta["hd"], Value::Null);

    // Disabled providers disappear.
    copper_cloud_core::sso::set_providers_enabled(
        &s.state.db,
        &copper_cloud_core::intelligence::Actor::Cli,
        Some("google"),
        false,
    )
    .await
    .unwrap();
    let on = json_of(s.http.get(s.url("/v1/auth/sso")).send().await.unwrap()).await;
    assert_eq!(on["providers"].as_array().unwrap().len(), 1);
    assert_eq!(on["providers"][0]["id"], "okta");

    let info = json_of(s.req(Method::GET, "/v1/info").send().await.unwrap()).await;
    let features = info["features"].as_array().unwrap();
    assert!(features.contains(&json!("sso")) && features.contains(&json!("agent_pairing")));
}

#[tokio::test]
async fn google_sign_in_creates_then_reuses_the_account() {
    let idp = Idp::start(Some(CLIENT_SECRET)).await;
    let s = start(&idp).await;
    setup_google(&s).await;
    let device = new_device();

    let code = idp.code(google_claims("1001", "Ana@Example.com"));
    let r = sign_in(&s, "google", &code, &device).await;
    assert_eq!(r.status(), 200);
    let body = json_of(r).await;
    assert_eq!(body["created"], true);
    assert_eq!(body["user"]["email"], "Ana@Example.com");
    assert_eq!(body["user"]["display_name"], "Ana Example");
    assert_eq!(body["device"]["id"], device);
    assert_eq!(body["device"]["name"], "Ana's MacBook");
    assert_eq!(body["gate_key"], s.key(), "open mode: the instance key");
    let token = body["token"].as_str().unwrap();
    assert_eq!(me(&s, s.key(), token).await, 200);

    // The server did the exchange with the secret and the client's PKCE verifier.
    let sent = idp.last_request();
    assert_eq!(sent["grant_type"], "authorization_code");
    assert_eq!(sent["code"], code);
    assert_eq!(sent["code_verifier"], VERIFIER);
    assert_eq!(sent["redirect_uri"], REDIRECT);
    assert_eq!(sent["client_id"], CLIENT_ID);
    assert_eq!(sent["client_secret"], CLIENT_SECRET);

    // No password; the identity is linked.
    let (hash, uid): (Option<String>, uuid::Uuid) = sqlx::query_as(
        "SELECT password_hash, id FROM users WHERE lower(email) = 'ana@example.com'",
    )
    .fetch_one(&s.state.db)
    .await
    .unwrap();
    assert!(hash.is_none());
    let linked: uuid::Uuid = sqlx::query_scalar(
        "SELECT user_id FROM user_identities WHERE provider_id = 'google' AND subject = '1001'",
    )
    .fetch_one(&s.state.db)
    .await
    .unwrap();
    assert_eq!(linked, uid);

    // Same Google account again (new device): same user, not created.
    let code = idp.code(google_claims("1001", "ana@example.com"));
    let body = json_of(sign_in(&s, "google", &code, &new_device()).await).await;
    assert_eq!(body["created"], false);
    assert_eq!(body["user"]["id"], uid.to_string());

    // A used code is rejected by the provider.
    expect_error(
        sign_in(&s, "google", &code, &device).await,
        401,
        "sso_failed",
    )
    .await;

    // Password login for the SSO-only account says to use SSO.
    let r = s.login("ana@example.com", PASSWORD, &new_device()).await;
    let err = expect_error(r, 401, "sso_only").await;
    assert!(err["message"].as_str().unwrap().contains("single sign-on"));

    // Created + audited.
    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM admin_audit WHERE action LIKE 'sso.%' ORDER BY id")
            .fetch_all(&s.state.db)
            .await
            .unwrap();
    assert!(
        actions.contains(&"sso.user_create".to_owned()),
        "{actions:?}"
    );
}

#[tokio::test]
async fn existing_password_account_is_linked_by_email() {
    let idp = Idp::start(Some(CLIENT_SECRET)).await;
    let s = start(&idp).await;
    let password_user = s.signup("bo@example.com", PASSWORD, &new_device()).await;
    setup_google(&s).await;

    let code = idp.code(google_claims("2002", "BO@example.com"));
    let body = json_of(sign_in(&s, "google", &code, &new_device()).await).await;
    assert_eq!(body["created"], false);
    assert_eq!(body["user"]["id"], password_user.user_id);
    let linked: i64 =
        sqlx::query_scalar("SELECT count(*) FROM user_identities WHERE subject = '2002'")
            .fetch_one(&s.state.db)
            .await
            .unwrap();
    assert_eq!(linked, 1);
    // The password keeps working.
    assert_eq!(
        s.login("bo@example.com", PASSWORD, &new_device())
            .await
            .status(),
        200
    );
}

#[tokio::test]
async fn sign_in_rejections() {
    let idp = Idp::start(Some(CLIENT_SECRET)).await;
    let s = start(&idp).await;
    let cookie = setup_google(&s).await;
    let dev = new_device();

    // Wrong domain (a consumer account) → 403 naming the allowed domain.
    let mut claims = google_claims("3", "ana@gmail.com");
    claims["hd"] = Value::Null;
    let err = expect_error(
        sign_in(&s, "google", &idp.code(claims), &dev).await,
        403,
        "sso_domain",
    )
    .await;
    assert!(
        err["message"].as_str().unwrap().contains("@example.com"),
        "{err}"
    );
    // An allowed address on a non-Workspace Google account (no `hd`) → 403 too.
    let mut claims = google_claims("4", "ana@example.com");
    claims["hd"] = Value::Null;
    expect_error(
        sign_in(&s, "google", &idp.code(claims), &dev).await,
        403,
        "sso_domain",
    )
    .await;
    // Unverified email.
    let mut claims = google_claims("5", "ana@example.com");
    claims["email_verified"] = json!(false);
    expect_error(
        sign_in(&s, "google", &idp.code(claims), &dev).await,
        401,
        "sso_failed",
    )
    .await;
    // Token for another client / issuer / expired.
    for (k, v) in [
        ("aud", json!("someone-else")),
        ("iss", json!("https://evil.example")),
        ("exp", json!(1_000_000_000)),
    ] {
        let mut claims = google_claims("6", "ana@example.com");
        claims[k] = v;
        expect_error(
            sign_in(&s, "google", &idp.code(claims), &dev).await,
            401,
            "sso_failed",
        )
        .await;
    }
    // Code the provider does not know.
    expect_error(
        sign_in(&s, "google", "4/0Aunknown", &dev).await,
        401,
        "sso_failed",
    )
    .await;

    // Bad redirect URIs never reach the provider.
    let before = idp.state.lock().unwrap().requests.len();
    for redirect in [
        "https://127.0.0.1:53682/",
        "http://evil.example:80/",
        "http://127.0.0.1/",
        "copper://cb",
    ] {
        let r = sign_in_with(
            &s,
            "google",
            json!({
                "code": idp.code(google_claims("7", "ana@example.com")),
                "code_verifier": VERIFIER, "redirect_uri": redirect,
            }),
        )
        .await;
        expect_error(r, 400, "bad_redirect").await;
    }
    let r = sign_in_with(
        &s,
        "google",
        json!({
            "code": "x", "code_verifier": "short", "redirect_uri": REDIRECT,
        }),
    )
    .await;
    expect_error(r, 400, "bad_request").await;
    assert_eq!(idp.state.lock().unwrap().requests.len(), before);

    // Unknown and disabled providers.
    expect_error(sign_in(&s, "okta", "x", &dev).await, 404, "sso_disabled").await;
    let mut body = google_body(&["example.com"]);
    body["enabled"] = json!(false);
    assert_eq!(put_google(&s, &cookie, body).await.status(), 200);
    let code = idp.code(google_claims("8", "ana@example.com"));
    expect_error(
        sign_in(&s, "google", &code, &dev).await,
        404,
        "sso_disabled",
    )
    .await;

    // auto_create off: only existing accounts.
    let mut body = google_body(&["example.com"]);
    body["auto_create"] = json!(false);
    assert_eq!(put_google(&s, &cookie, body).await.status(), 200);
    let code = idp.code(google_claims("9", "new@example.com"));
    expect_error(
        sign_in(&s, "google", &code, &dev).await,
        403,
        "sso_no_account",
    )
    .await;
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&s.state.db)
        .await
        .unwrap();
    assert_eq!(users, 0);

    // Disabled account.
    s.signup("off@example.com", PASSWORD, &new_device()).await;
    sqlx::query("UPDATE users SET disabled = true WHERE email = 'off@example.com'")
        .execute(&s.state.db)
        .await
        .unwrap();
    let code = idp.code(google_claims("10", "off@example.com"));
    expect_error(
        sign_in(&s, "google", &code, &dev).await,
        403,
        "account_disabled",
    )
    .await;
}

#[tokio::test]
async fn directory_mode_signs_in_with_a_minted_gate_key() {
    let idp = Idp::start(Some(CLIENT_SECRET)).await;
    let s = start(&idp).await;
    setup_google(&s).await;
    s.set_access_mode(AccessMode::Directory).await;

    let device = new_device();
    let code = idp.code(google_claims("11", "cy@example.com"));
    let body = json_of(sign_in(&s, "google", &code, &device).await).await;
    let gate = body["gate_key"].as_str().unwrap();
    assert!(gate.starts_with("ck_"), "{gate}");
    let token = body["token"].as_str().unwrap();
    assert_eq!(me(&s, gate, token).await, 200);
    assert_eq!(
        me(&s, s.key(), token).await,
        401,
        "instance key no longer passes the gate"
    );
    let (label, bound): (String, Option<uuid::Uuid>) =
        sqlx::query_as("SELECT label, device_id FROM access_keys")
            .fetch_one(&s.state.db)
            .await
            .unwrap();
    assert_eq!(label, "Ana's MacBook via sso");
    assert_eq!(bound.unwrap().to_string(), device);
}

#[tokio::test]
async fn password_signin_can_be_turned_off() {
    let idp = Idp::start(Some(CLIENT_SECRET)).await;
    let s = start(&idp).await;
    s.signup("pw@example.com", PASSWORD, &new_device()).await;
    let cookie = setup_google(&s).await;

    let r = s
        .admin(Method::PATCH, "sso", Some(&cookie))
        .json(&json!({ "password_signin": false }))
        .send()
        .await
        .unwrap();
    let view = json_of(r).await;
    assert_eq!(view["password_signin"], false);
    let public = json_of(s.http.get(s.url("/v1/auth/sso")).send().await.unwrap()).await;
    assert_eq!(public["password_signin"], false);

    let r = s.login("pw@example.com", PASSWORD, &new_device()).await;
    expect_error(r, 403, "password_signin_disabled").await;
    let r = s
        .req(Method::POST, "/v1/auth/signup")
        .json(&json!({ "email": "new@example.com", "password": PASSWORD }))
        .send()
        .await
        .unwrap();
    expect_error(r, 403, "password_signin_disabled").await;
    // The password user signs in with SSO (email match); admins are unaffected.
    let code = idp.code(google_claims("12", "pw@example.com"));
    assert_eq!(
        sign_in(&s, "google", &code, &new_device()).await.status(),
        200
    );
    s.admin_login().await;

    let audit: i64 =
        sqlx::query_scalar("SELECT count(*) FROM admin_audit WHERE action = 'sso.password_signin'")
            .fetch_one(&s.state.db)
            .await
            .unwrap();
    assert_eq!(audit, 1);
}

#[tokio::test]
async fn oidc_provider_by_discovery() {
    // Public client: no secret configured or sent.
    let idp = Idp::start(None).await;
    let s = start(&idp).await;
    s.create_admin().await;
    let cookie = s.admin_login().await;
    let r = s
        .admin(Method::PUT, "sso/providers/okta", Some(&cookie))
        .json(&json!({
            "kind": "oidc", "client_id": CLIENT_ID, "issuer": idp.base,
            "domains": ["example.com"],
        }))
        .send()
        .await
        .unwrap();
    let view = json_of(r).await;
    assert_eq!(view["providers"][0]["has_secret"], false);
    assert_eq!(view["providers"][0]["name"], "okta");

    let mut claims = google_claims("13", "dee@example.com");
    claims["iss"] = json!(idp.base);
    claims["hd"] = Value::Null;
    claims["aud"] = json!([CLIENT_ID]);
    let body = json_of(sign_in(&s, "okta", &idp.code(claims), &new_device()).await).await;
    assert_eq!(body["created"], true);
    assert!(!idp.last_request().contains_key("client_secret"));

    // Google's issuer is not accepted for this provider.
    let mut claims = google_claims("14", "dee@example.com");
    claims["hd"] = Value::Null;
    expect_error(
        sign_in(&s, "okta", &idp.code(claims), &new_device()).await,
        401,
        "sso_failed",
    )
    .await;
}

#[tokio::test]
async fn admin_sso_api_never_returns_the_secret() {
    let idp = Idp::start(Some(CLIENT_SECRET)).await;
    let s = start(&idp).await;
    s.create_admin().await;
    let cookie = s.admin_login().await;

    assert_eq!(
        s.admin(Method::GET, "sso", None)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let empty = json_of(
        s.admin(Method::GET, "sso", Some(&cookie))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        empty,
        json!({ "enabled": false, "password_signin": true, "providers": [], "join_link": null, "start_url": null })
    );

    // Google needs a secret the first time; kind and id must agree.
    let mut body = google_body(&["example.com"]);
    body.as_object_mut().unwrap().remove("client_secret");
    expect_error(
        put_google(&s, &cookie, body.clone()).await,
        400,
        "bad_request",
    )
    .await;
    let r = s
        .admin(Method::PUT, "sso/providers/google", Some(&cookie))
        .json(&json!({ "kind": "oidc", "client_id": "x", "issuer": "https://x.example", "domains": ["a.com"] }))
        .send()
        .await
        .unwrap();
    expect_error(r, 400, "bad_request").await;
    expect_error(
        put_google(&s, &cookie, google_body(&[])).await,
        400,
        "bad_request",
    )
    .await;

    let r = put_google(&s, &cookie, google_body(&["example.com"])).await;
    let text = r.text().await.unwrap();
    assert!(!text.contains(CLIENT_SECRET));
    let view: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(view["enabled"], true);
    let p = &view["providers"][0];
    assert_eq!(p["has_secret"], true);
    assert_eq!(p["client_id"], CLIENT_ID);
    assert_eq!(p["issuer"], Value::Null);
    assert!(p["updated_at"].is_string());
    assert_eq!(
        view["join_link"],
        "copper-cloud://localhost:8443/#sso=google"
    );
    assert_eq!(view["start_url"], "https://localhost:8443/start");

    // Omitted secret = keep it.
    let r = put_google(&s, &cookie, body).await;
    assert_eq!(json_of(r).await["providers"][0]["has_secret"], true);

    // Self-test: the dummy code is rejected with invalid_grant = credentials OK.
    let t = json_of(
        s.admin(Method::POST, "sso/providers/google/test", Some(&cookie))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(t["ok"], true, "{t}");
    assert!(t["checks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["name"].is_string()));
    // Wrong secret → invalid_client.
    let mut wrong = google_body(&["example.com"]);
    wrong["client_secret"] = json!("GOCSPX-wrong");
    assert_eq!(put_google(&s, &cookie, wrong).await.status(), 200);
    let t = json_of(
        s.admin(Method::POST, "sso/providers/google/test", Some(&cookie))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(t["ok"], false);
    assert!(t.to_string().contains("invalid_client"), "{t}");
    let r = s
        .admin(Method::POST, "sso/providers/nope/test", Some(&cookie))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    // The secret is sealed at rest and absent from the audit trail.
    let sealed: Vec<u8> = sqlx::query_scalar("SELECT client_secret_sealed FROM sso_providers")
        .fetch_one(&s.state.db)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains("GOCSPX"));
    let audit: Vec<(String, Option<Value>)> = sqlx::query_as(
        "SELECT action, detail FROM admin_audit WHERE action LIKE 'sso.%' ORDER BY id",
    )
    .fetch_all(&s.state.db)
    .await
    .unwrap();
    assert!(audit.len() >= 3);
    assert!(audit.iter().all(
        |(a, d)| a == "sso.provider.put" && !d.as_ref().unwrap().to_string().contains("GOCSPX")
    ));

    // Delete.
    let r = s
        .admin(Method::DELETE, "sso/providers/google", Some(&cookie))
        .send()
        .await
        .unwrap();
    let view = json_of(r).await;
    assert_eq!(view["providers"], json!([]));
    assert_eq!(view["join_link"], Value::Null);
    let r = s
        .admin(Method::DELETE, "sso/providers/google", Some(&cookie))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    // instance_name lives in the settings.
    let r = s
        .admin(Method::PATCH, "settings", Some(&cookie))
        .json(&json!({ "instance_name": "Acme" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let settings = json_of(
        s.admin(Method::GET, "settings", Some(&cookie))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(settings["instance_name"], "Acme");
}

#[tokio::test]
async fn start_page_renders_both_states() {
    let idp = Idp::start(Some(CLIENT_SECRET)).await;
    let s = start(&idp).await;

    let r = s.http.get(s.url("/start")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "no-store");
    let html = r.text().await.unwrap();
    assert!(html.contains("isn't ready for sign-in yet"), "{html}");
    assert!(!html.contains("Open in Copper"));

    let cookie = setup_google(&s).await;
    s.admin(Method::PATCH, "settings", Some(&cookie))
        .json(&json!({ "instance_name": "Acme & Co" }))
        .send()
        .await
        .unwrap();
    let r = s.http.get(s.url("/start")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let csp = r.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(csp.contains("default-src 'none'") && csp.contains("script-src 'sha256-"));
    assert!(!csp.contains("unsafe-eval") && !csp.contains("script-src 'unsafe-inline'"));
    assert_eq!(r.headers()["referrer-policy"], "no-referrer");
    let html = r.text().await.unwrap();
    assert!(html.contains("Join Acme &amp; Co on Copper Cloud"));
    assert!(html.contains("value=\"copper-cloud://localhost:8443/#sso=google\""));
    assert!(html.contains(
        "href=\"copper://cloud/join?code=copper-cloud%3A%2F%2Flocalhost%3A8443%2F%23sso%3Dgoogle\""
    ));
    assert!(html.contains("brew install --cask copper-browser/copper/copper"));
    assert!(html.contains("Continue with Google"));
    assert!(html.contains("@example.com"));
    assert!(
        !html.contains("http://") && !html.contains("<link"),
        "no external assets"
    );
}

#[tokio::test]
async fn sign_in_is_rate_limited_but_the_provider_list_is_not() {
    let idp = Idp::start(Some(CLIENT_SECRET)).await;
    let token = format!("{}/token", idp.base);
    let s = start_with(|cfg| {
        cfg.sso_test.google_token_endpoint = Some(token);
        cfg.sso_test.allow_http = true;
        cfg.limits.auth_per_minute = 2;
    })
    .await;
    for _ in 0..5 {
        assert_eq!(
            s.http
                .get(s.url("/v1/auth/sso"))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    let statuses = {
        let mut out = Vec::new();
        for _ in 0..3 {
            out.push(
                sign_in(&s, "google", "x", &new_device())
                    .await
                    .status()
                    .as_u16(),
            );
        }
        out
    };
    assert_eq!(statuses, [404, 404, 429]);
}
