//! Agent integrations: admin CRUD, `POST /v1/agents/pair` (key, domain, account creation,
//! device binding, re-pair revocation) and redeeming the bound pairing code.

#![allow(clippy::too_many_lines)]

mod common;

use common::{new_device, start_with, TestServer, PASSWORD};
use copper_cloud_core::access::AccessMode;
use reqwest::Method;
use serde_json::{json, Value};

async fn start() -> TestServer {
    start_with(|cfg| cfg.limits.auth_per_minute = 1000).await
}

async fn json_of(r: reqwest::Response) -> Value {
    let status = r.status();
    let text = r.text().await.unwrap();
    serde_json::from_str(&text).unwrap_or_else(|_| panic!("{status}: {text}"))
}

async fn expect_error(r: reqwest::Response, status: u16, code: &str) -> Value {
    assert_eq!(r.status(), status);
    let body = json_of(r).await;
    assert_eq!(body["error"], code, "{body}");
    body
}

/// Admin login + a new integration; returns `(cookie, integration id, key)`.
async fn integration(s: &TestServer, body: Value) -> (String, String, String) {
    s.create_admin().await;
    let cookie = s.admin_login().await;
    let r = s
        .admin(Method::POST, "integrations", Some(&cookie))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let created = json_of(r).await;
    let key = created["key"].as_str().unwrap().to_owned();
    assert!(key.starts_with("cai_") && key.len() == 47, "{key}");
    let id = created["integration"]["id"].as_str().unwrap().to_owned();
    (cookie, id, key)
}

/// `POST /v1/agents/pair` without the gate header.
async fn agent_pair(s: &TestServer, key: Option<&str>, body: Value) -> reqwest::Response {
    let mut rb = s.http.post(s.url("/v1/agents/pair")).json(&body);
    if let Some(k) = key {
        rb = rb.bearer_auth(k);
    }
    rb.send().await.unwrap()
}

fn pair_body(email: &str, device: &str) -> Value {
    json!({
        "email": email,
        "device": { "id": device, "name": "Bot · Vera" },
        "display_name": "Ana Example",
    })
}

/// Redeem a pairing code (no gate header), optionally claiming another device id.
async fn redeem(s: &TestServer, code: &str, device: &str) -> reqwest::Response {
    s.http
        .post(s.url("/v1/auth/pair"))
        .json(&json!({ "code": code, "device": { "id": device, "name": "spoofed" } }))
        .send()
        .await
        .unwrap()
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

#[tokio::test]
async fn agent_pairing_end_to_end() {
    let s = start().await;
    let (cookie, integration_id, key) = integration(
        &s,
        json!({ "name": "Bots", "domains": ["example.com"], "create_users": true }),
    )
    .await;
    let device = new_device();

    // Bad / missing / malformed keys.
    expect_error(
        agent_pair(&s, None, pair_body("a@example.com", &device)).await,
        401,
        "agent_key",
    )
    .await;
    let fake = format!("cai_{}", copper_cloud_core::ids::random_token());
    expect_error(
        agent_pair(&s, Some(&fake), pair_body("a@example.com", &device)).await,
        401,
        "agent_key",
    )
    .await;
    expect_error(
        agent_pair(&s, Some("cai_x"), pair_body("a@example.com", &device)).await,
        401,
        "agent_key",
    )
    .await;
    // Domain.
    let err = expect_error(
        agent_pair(&s, Some(&key), pair_body("a@gmail.com", &device)).await,
        403,
        "agent_domain",
    )
    .await;
    assert!(err["message"].as_str().unwrap().contains("@example.com"));
    // A device id is required.
    let r = agent_pair(
        &s,
        Some(&key),
        json!({ "email": "a@example.com", "device": {} }),
    )
    .await;
    expect_error(r, 400, "bad_request").await;

    // New user created (no password).
    let r = agent_pair(&s, Some(&key), pair_body("Bot.Owner@Example.com", &device)).await;
    assert_eq!(r.status(), 200);
    let paired = json_of(r).await;
    assert_eq!(paired["created_user"], true);
    assert_eq!(
        paired["user"]["email"], "Bot.Owner@Example.com",
        "as given (matched case-insensitively)"
    );
    assert_eq!(paired["user"]["display_name"], "Ana Example");
    assert_eq!(
        paired["device"],
        json!({ "id": device, "name": "Bot · Vera" })
    );
    let code = paired["code"].as_str().unwrap();
    assert!(code.starts_with("cp_"));
    assert_eq!(
        paired["link"],
        format!("copper-cloud://localhost:8443/#p={code}")
    );
    assert!(paired["expires_at"].is_string());
    let hash: Option<String> = sqlx::query_scalar(
        "SELECT password_hash FROM users WHERE lower(email) = 'bot.owner@example.com'",
    )
    .fetch_one(&s.state.db)
    .await
    .unwrap();
    assert!(hash.is_none());

    // Redeem: the bound device id and name win over the request's.
    let r = redeem(&s, code, &new_device()).await;
    assert_eq!(r.status(), 200);
    let first = json_of(r).await;
    assert_eq!(first["device"]["id"], device);
    assert_eq!(first["device"]["name"], "Bot · Vera");
    let first_token = first["token"].as_str().unwrap().to_owned();
    assert_eq!(me(&s, s.key(), &first_token).await, 200);
    // Single use.
    expect_error(redeem(&s, code, &device).await, 401, "pairing_code").await;

    // Flagged as the integration's agent device.
    let devices = json_of(
        s.admin(
            Method::GET,
            &format!("integrations/{integration_id}/devices"),
            Some(&cookie),
        )
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(
        devices
            .as_array()
            .unwrap()
            .iter()
            .map(|d| (d["user_email"].clone(), d["device_id"].clone()))
            .collect::<Vec<_>>(),
        [(json!("Bot.Owner@Example.com"), json!(device))]
    );

    // Re-pair the same device: existing user, the earlier session is revoked.
    let again =
        json_of(agent_pair(&s, Some(&key), pair_body("bot.owner@example.com", &device)).await)
            .await;
    assert_eq!(again["created_user"], false);
    assert_eq!(
        me(&s, s.key(), &first_token).await,
        401,
        "old session revoked on re-pair"
    );
    let second = json_of(redeem(&s, again["code"].as_str().unwrap(), &device).await).await;
    assert_eq!(
        me(&s, s.key(), second["token"].as_str().unwrap()).await,
        200
    );

    // List: no key, counts and last use.
    let r = s
        .admin(Method::GET, "integrations", Some(&cookie))
        .send()
        .await
        .unwrap();
    let text = r.text().await.unwrap();
    assert!(!text.contains(&key));
    let list: Value = serde_json::from_str(&text).unwrap();
    let item = &list.as_array().unwrap()[0];
    assert_eq!(item["id"], integration_id);
    assert_eq!(item["name"], "Bots");
    assert_eq!(item["domains"], json!(["example.com"]));
    assert_eq!(item["create_users"], true);
    assert_eq!(item["device_count"], 1);
    assert!(item["last_used_at"].is_string());
    assert_eq!(item["revoked_at"], Value::Null);

    // Audited.
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM admin_audit WHERE action LIKE 'agent.%' OR action LIKE 'integration.%' ORDER BY id",
    )
    .fetch_all(&s.state.db)
    .await
    .unwrap();
    assert_eq!(actions, ["integration.create", "agent.pair", "agent.pair"]);
    let stored: Vec<u8> = sqlx::query_scalar("SELECT key_sha256 FROM agent_integrations")
        .fetch_one(&s.state.db)
        .await
        .unwrap();
    assert_eq!(stored, copper_cloud_core::ids::sha256(key.as_bytes()));
}

#[tokio::test]
async fn create_users_off_and_disabled_accounts() {
    let s = start().await;
    let (_, _, key) = integration(
        &s,
        json!({ "name": "Strict", "domains": ["example.com"], "create_users": false }),
    )
    .await;
    let r = agent_pair(
        &s,
        Some(&key),
        pair_body("nobody@example.com", &new_device()),
    )
    .await;
    expect_error(r, 404, "no_account").await;
    s.signup("real@example.com", PASSWORD, &new_device()).await;
    let r = agent_pair(&s, Some(&key), pair_body("real@example.com", &new_device())).await;
    assert_eq!(json_of(r).await["created_user"], false);
    sqlx::query("UPDATE users SET disabled = true WHERE email = 'real@example.com'")
        .execute(&s.state.db)
        .await
        .unwrap();
    let r = agent_pair(&s, Some(&key), pair_body("real@example.com", &new_device())).await;
    expect_error(r, 403, "account_disabled").await;
}

#[tokio::test]
async fn domains_default_to_the_sso_providers() {
    let s = start().await;
    let (cookie, _, key) = integration(&s, json!({ "name": "Default domains" })).await;
    // No SSO provider and no own domains: nothing is allowed.
    let r = agent_pair(&s, Some(&key), pair_body("a@example.com", &new_device())).await;
    expect_error(r, 403, "agent_domain").await;
    let r = s
        .admin(Method::PUT, "sso/providers/google", Some(&cookie))
        .json(&json!({
            "kind": "google", "client_id": "x.apps.googleusercontent.com",
            "client_secret": "GOCSPX-x", "domains": ["example.com"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = agent_pair(&s, Some(&key), pair_body("a@example.com", &new_device())).await;
    assert_eq!(
        json_of(r).await["created_user"],
        true,
        "create_users defaults to true"
    );
    let r = agent_pair(&s, Some(&key), pair_body("a@other.com", &new_device())).await;
    expect_error(r, 403, "agent_domain").await;
}

#[tokio::test]
async fn directory_mode_repair_revokes_the_gate_key() {
    let s = start().await;
    let (_, _, key) = integration(&s, json!({ "name": "Bots", "domains": ["example.com"] })).await;
    s.set_access_mode(AccessMode::Directory).await;
    let device = new_device();

    let code =
        json_of(agent_pair(&s, Some(&key), pair_body("bot@example.com", &device)).await).await;
    let first = json_of(redeem(&s, code["code"].as_str().unwrap(), &device).await).await;
    let gate = first["gate_key"].as_str().unwrap().to_owned();
    assert!(gate.starts_with("ck_"));
    assert_eq!(me(&s, &gate, first["token"].as_str().unwrap()).await, 200);

    let code =
        json_of(agent_pair(&s, Some(&key), pair_body("bot@example.com", &device)).await).await;
    let second = json_of(redeem(&s, code["code"].as_str().unwrap(), &device).await).await;
    let new_gate = second["gate_key"].as_str().unwrap();
    assert_ne!(new_gate, gate);
    assert_eq!(
        me(&s, new_gate, second["token"].as_str().unwrap()).await,
        200
    );
    // The first gate key was revoked with its session.
    let r = s
        .gated(Method::GET, "/v1/info", &gate)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn rotate_and_revoke() {
    let s = start().await;
    let (cookie, id, key) =
        integration(&s, json!({ "name": "Bots", "domains": ["example.com"] })).await;
    let device = new_device();
    let code = json_of(agent_pair(&s, Some(&key), pair_body("z@example.com", &device)).await).await;
    let session = json_of(redeem(&s, code["code"].as_str().unwrap(), &device).await).await;
    let token = session["token"].as_str().unwrap().to_owned();

    // Rotate: the old key stops working, the new one works.
    let r = s
        .admin(
            Method::POST,
            &format!("integrations/{id}/rotate"),
            Some(&cookie),
        )
        .send()
        .await
        .unwrap();
    let rotated = json_of(r).await;
    let new_key = rotated["key"].as_str().unwrap().to_owned();
    assert_ne!(new_key, key);
    expect_error(
        agent_pair(&s, Some(&key), pair_body("z@example.com", &device)).await,
        401,
        "agent_key",
    )
    .await;
    let code =
        json_of(agent_pair(&s, Some(&new_key), pair_body("z@example.com", &device)).await).await;
    let session = json_of(redeem(&s, code["code"].as_str().unwrap(), &device).await).await;
    let token2 = session["token"].as_str().unwrap().to_owned();
    assert_eq!(me(&s, s.key(), &token).await, 401);
    assert_eq!(me(&s, s.key(), &token2).await, 200);

    // Revoke: key dead, agent devices signed out; idempotent.
    let r = s
        .admin(Method::DELETE, &format!("integrations/{id}"), Some(&cookie))
        .send()
        .await
        .unwrap();
    let view = json_of(r).await;
    assert!(view["revoked_at"].is_string());
    expect_error(
        agent_pair(&s, Some(&new_key), pair_body("z@example.com", &device)).await,
        401,
        "agent_key",
    )
    .await;
    assert_eq!(me(&s, s.key(), &token2).await, 401);
    let r = s
        .admin(Method::DELETE, &format!("integrations/{id}"), Some(&cookie))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // Rotating a revoked integration / unknown ids.
    let r = s
        .admin(
            Method::POST,
            &format!("integrations/{id}/rotate"),
            Some(&cookie),
        )
        .send()
        .await
        .unwrap();
    expect_error(r, 400, "bad_request").await;
    let unknown = uuid::Uuid::now_v7();
    for (m, path) in [
        (Method::POST, format!("integrations/{unknown}/rotate")),
        (Method::DELETE, format!("integrations/{unknown}")),
        (Method::GET, format!("integrations/{unknown}/devices")),
    ] {
        let r = s.admin(m, &path, Some(&cookie)).send().await.unwrap();
        assert_eq!(r.status(), 404, "{path}");
    }
    let r = s
        .admin(Method::POST, "integrations", Some(&cookie))
        .json(&json!({ "name": "  " }))
        .send()
        .await
        .unwrap();
    expect_error(r, 400, "bad_request").await;
    assert_eq!(
        s.admin(Method::GET, "integrations", None)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );

    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM admin_audit WHERE action LIKE 'integration.%' ORDER BY id",
    )
    .fetch_all(&s.state.db)
    .await
    .unwrap();
    assert_eq!(
        actions,
        [
            "integration.create",
            "integration.rotate",
            "integration.revoke",
            "integration.revoke"
        ]
    );
}
