//! Access model (spec §7.1) and pairing codes (§7.2): open vs directory gate, access keys
//! (expiry, revocation, `max_uses`, email binding) and single-use pairing.

// Scenario tests read best as one linear story with short names (s = server, c = cookie).
#![allow(clippy::too_many_lines, clippy::many_single_char_names)]

mod common;

use common::*;
use copper_cloud_core::access::AccessMode;
use reqwest::Method;
use serde_json::{json, Value};

async fn status_of(s: &TestServer, path: &str, key: &str) -> u16 {
    s.gated(Method::GET, path, key)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test]
async fn open_mode_accepts_instance_key_and_access_keys() {
    let s = start().await;
    let r = s.req(Method::GET, "/v1/info").send().await.unwrap();
    assert_eq!(r.status(), 200);
    let info: Value = r.json().await.unwrap();
    assert_eq!(info["access_mode"], "open", "no row → open");
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));

    let (_, key) = s.mint_key("Ana", None, None).await;
    assert_eq!(status_of(&s, "/v1/info", &key).await, 200);
    // A well-shaped but unknown access key and garbage are both 401 instance_key.
    let unknown = copper_cloud_core::access::new_access_key();
    let r = s
        .gated(Method::GET, "/v1/info", &unknown)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "instance_key");
    assert_eq!(status_of(&s, "/v1/info", "ck_nope").await, 401);

    // allow_signup=false blocks the shared key but an access key implies permission.
    sqlx::query("INSERT INTO server_settings (key, value) VALUES ('allow_signup', 'false')")
        .execute(&s.state.db)
        .await
        .unwrap();
    s.signup("first@example.com", PASSWORD, &new_device()).await; // first user always may
    let r = s
        .signup_with(s.key(), "second@example.com", &new_device())
        .await;
    assert_eq!(r.status(), 403);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "forbidden");
    let r = s
        .signup_with(&key, "second@example.com", &new_device())
        .await;
    assert_eq!(r.status(), 200);
    let info: Value = s
        .gated(Method::GET, "/v1/info", &key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["signup"], true, "access key may sign up");
    let info: Value = s
        .req(Method::GET, "/v1/info")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["signup"], false, "instance key may not");
}

#[tokio::test]
async fn directory_mode_gate() {
    let s = start().await;
    // An account created while open keeps working through any valid key later.
    let early = s.signup("early@example.com", PASSWORD, &new_device()).await;
    let (key_id, key) = s.mint_key("Team", None, None).await;
    s.set_access_mode(AccessMode::Directory).await;

    // The shared instance key is rejected everywhere under /v1 …
    for path in ["/v1/info", "/v1/auth/me", "/v1/canvases"] {
        let r = s.req(Method::GET, path).send().await.unwrap();
        assert_eq!(r.status(), 401, "{path}");
        assert_eq!(r.json::<Value>().await.unwrap()["error"], "instance_key");
    }
    let r = s.signup_with(s.key(), "x@example.com", &new_device()).await;
    assert_eq!(r.status(), 401);
    // … while an access key passes and /v1/info says so.
    let r = s.gated(Method::GET, "/v1/info", &key).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let info: Value = r.json().await.unwrap();
    assert_eq!(info["access_mode"], "directory");
    assert_eq!(info["signup"], true);
    let r = s
        .gated(Method::GET, "/v1/auth/me", &key)
        .bearer_auth(&early.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "existing session through an access key");
    let r = s
        .gated(Method::POST, "/v1/auth/login", &key)
        .json(
            &json!({"email":"early@example.com","password":PASSWORD,"device":{"id":new_device()}}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "login works with any valid key");

    // Signup with an unbound key: allowed, counts one use, touches last_used_at.
    let r = s.signup_with(&key, "new@example.com", &new_device()).await;
    assert_eq!(r.status(), 200);
    let (uses, touched): (i32, bool) =
        sqlx::query_as("SELECT uses, last_used_at IS NOT NULL FROM access_keys WHERE id = $1")
            .bind(key_id)
            .fetch_one(&s.state.db)
            .await
            .unwrap();
    assert_eq!((uses, touched), (1, true));

    // Expired keys are rejected at the gate.
    let (exp_id, expired) = s.mint_key("Expired", None, None).await;
    assert_eq!(status_of(&s, "/v1/info", &expired).await, 200);
    sqlx::query("UPDATE access_keys SET expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(exp_id)
        .execute(&s.state.db)
        .await
        .unwrap();
    assert_eq!(status_of(&s, "/v1/info", &expired).await, 401);

    // Revoked keys too (immediately: keys are not cached).
    let (rev_id, revoked) = s.mint_key("Revoked", None, None).await;
    assert_eq!(status_of(&s, "/v1/info", &revoked).await, 200);
    sqlx::query("UPDATE access_keys SET revoked_at = now() WHERE id = $1")
        .bind(rev_id)
        .execute(&s.state.db)
        .await
        .unwrap();
    assert_eq!(status_of(&s, "/v1/info", &revoked).await, 401);
}

#[tokio::test]
async fn max_uses_and_email_binding() {
    let s = start().await;
    s.set_access_mode(AccessMode::Directory).await;

    // max_uses = 1: one account, then the key stops creating accounts but keeps working
    // for the person who used it.
    let (_, once) = s.mint_key("Invite", None, Some(1)).await;
    let r = s.signup_with(&once, "ana@example.com", &new_device()).await;
    assert_eq!(r.status(), 200);
    let ana = session_from(&r.json().await.unwrap());
    let r = s.signup_with(&once, "bob@example.com", &new_device()).await;
    assert_eq!(r.status(), 403);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"],
        "access_key_exhausted"
    );
    assert!(
        copper_cloud_core::auth::user_id_by_email(&s.state.db, "bob@example.com")
            .await
            .unwrap()
            .is_none()
    );
    let r = s
        .gated(Method::GET, "/v1/auth/me", &once)
        .bearer_auth(&ana.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    // A failed signup (email taken) does not consume a use.
    let (_, two) = s.mint_key("Two", None, Some(1)).await;
    let r = s.signup_with(&two, "ana@example.com", &new_device()).await;
    assert_eq!(r.status(), 409);
    let r = s
        .signup_with(&two, "carol@example.com", &new_device())
        .await;
    assert_eq!(r.status(), 200, "the use was not burnt by the 409");

    // Email-bound key: only that email (case-insensitive).
    let (_, bound) = s.mint_key("Dana", Some("Dana@Example.com"), None).await;
    let r = s
        .signup_with(&bound, "eve@example.com", &new_device())
        .await;
    assert_eq!(r.status(), 403);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"],
        "access_key_email"
    );
    let r = s
        .signup_with(&bound, "dana@EXAMPLE.com", &new_device())
        .await;
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn access_mode_is_cached_for_the_ttl() {
    let s = start().await;
    assert_eq!(status_of(&s, "/v1/info", s.key()).await, 200); // caches "open"
                                                               // Another process flips the mode: this one keeps the cached value until the TTL …
    copper_cloud_core::access::set_access_mode(&s.state.db, AccessMode::Directory)
        .await
        .unwrap();
    assert_eq!(status_of(&s, "/v1/info", s.key()).await, 200, "cached");
    // … (test hook: expire it now) then follows.
    s.state.access_mode.clear();
    assert_eq!(status_of(&s, "/v1/info", s.key()).await, 401);
}

// ---------------------------------------------------------------------------------------------
// Pairing

async fn mint_code(s: &TestServer, gate: &str, sess: &Session, name: Option<&str>) -> Value {
    let mut rb = s
        .gated(Method::POST, "/v1/auth/pairing", gate)
        .bearer_auth(&sess.token);
    if let Some(n) = name {
        rb = rb.json(&json!({ "device_name": n }));
    }
    let r = rb.send().await.unwrap();
    assert_eq!(r.status(), 200);
    r.json().await.unwrap()
}

async fn pair(s: &TestServer, code: &str, device: &str, name: Option<&str>) -> reqwest::Response {
    // No X-Copper-Instance header: the code is the credential.
    s.http
        .post(s.url("/v1/auth/pair"))
        .json(&json!({ "code": code, "device": { "id": device, "name": name } }))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn pairing_open_mode() {
    let s = start().await;
    let a = s.signup("ana@example.com", PASSWORD, &new_device()).await;

    let minted = mint_code(&s, s.key(), &a, Some("Ana's Mac mini")).await;
    let code = minted["code"].as_str().unwrap();
    assert!(code.starts_with("cp_") && code.len() == 35, "{code}");
    assert_eq!(
        minted["link"].as_str().unwrap(),
        format!("copper-cloud://localhost:8443/#p={code}")
    );
    assert_eq!(minted["device_name"], "Ana's Mac mini");
    let r = s
        .authed(Method::GET, "/v1/auth/pairing", &a)
        .send()
        .await
        .unwrap();
    let listed: Vec<Value> = r.json().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], minted["id"]);
    assert!(listed[0].get("code").is_none(), "codes are shown once");

    // Pair a new device (no gate header) — name falls back to the code's device_name.
    let d2 = new_device();
    let r = pair(&s, code, &d2, None).await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["user"]["id"].as_str().unwrap(), a.user_id);
    assert_eq!(body["device"]["id"].as_str().unwrap(), d2);
    assert_eq!(body["device"]["name"], "Ana's Mac mini");
    assert_eq!(
        body["gate_key"].as_str().unwrap(),
        s.key(),
        "open mode → instance key"
    );
    let token = body["token"].as_str().unwrap();
    let r = s
        .gated(
            Method::GET,
            "/v1/auth/me",
            body["gate_key"].as_str().unwrap(),
        )
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.json::<Value>().await.unwrap()["device"]["id"]
            .as_str()
            .unwrap(),
        d2
    );

    // Single use.
    let r = pair(&s, code, &new_device(), Some("Thief")).await;
    assert_eq!(r.status(), 401);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "pairing_code");
    let listed: Vec<Value> = s
        .authed(Method::GET, "/v1/auth/pairing", &a)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed, Vec::<Value>::new());

    // Expired codes are refused.
    let minted = mint_code(&s, s.key(), &a, None).await;
    sqlx::query("UPDATE pairing_codes SET expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(
            minted["id"]
                .as_str()
                .unwrap()
                .parse::<uuid::Uuid>()
                .unwrap(),
        )
        .execute(&s.state.db)
        .await
        .unwrap();
    let r = pair(&s, minted["code"].as_str().unwrap(), &new_device(), None).await;
    assert_eq!(r.status(), 401);

    // Revoked codes too; revoking twice is a 404; garbage is 401.
    let minted = mint_code(&s, s.key(), &a, None).await;
    let path = format!("/v1/auth/pairing/{}", minted["id"].as_str().unwrap());
    let r = s.authed(Method::DELETE, &path, &a).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let r = s.authed(Method::DELETE, &path, &a).send().await.unwrap();
    assert_eq!(r.status(), 404);
    let r = pair(&s, minted["code"].as_str().unwrap(), &new_device(), None).await;
    assert_eq!(r.status(), 401);
    let r = pair(
        &s,
        "cp_definitely-not-a-real-pairing-cd",
        &new_device(),
        None,
    )
    .await;
    assert_eq!(r.status(), 401);

    // Another user cannot revoke my code; minting needs a session.
    let b = s.signup("bob@example.com", PASSWORD, &new_device()).await;
    let minted = mint_code(&s, s.key(), &a, None).await;
    let path = format!("/v1/auth/pairing/{}", minted["id"].as_str().unwrap());
    let r = s.authed(Method::DELETE, &path, &b).send().await.unwrap();
    assert_eq!(r.status(), 404);
    let r = s
        .req(Method::POST, "/v1/auth/pairing")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    // A disabled account cannot be paired into.
    sqlx::query("UPDATE users SET disabled = true WHERE email = 'ana@example.com'")
        .execute(&s.state.db)
        .await
        .unwrap();
    let r = pair(&s, minted["code"].as_str().unwrap(), &new_device(), None).await;
    assert_eq!(r.status(), 401);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"],
        "account_disabled"
    );
}

#[tokio::test]
async fn pairing_directory_mode_mints_an_access_key() {
    let s = start().await;
    s.set_access_mode(AccessMode::Directory).await;
    let (_, key) = s.mint_key("Ana", Some("ana@example.com"), Some(1)).await;
    let r = s.signup_with(&key, "ana@example.com", &new_device()).await;
    assert_eq!(r.status(), 200);
    let a = session_from(&r.json().await.unwrap());

    let minted = mint_code(&s, &key, &a, None).await;
    let d2 = new_device();
    let r = pair(&s, minted["code"].as_str().unwrap(), &d2, Some("New Mac")).await;
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    let gate_key = body["gate_key"].as_str().unwrap();
    assert!(copper_cloud_core::access::plausible_access_key(gate_key));
    assert_ne!(gate_key, key);
    assert_ne!(gate_key, s.key());
    // The fresh key passes the gate (the instance key would not).
    let token = body["token"].as_str().unwrap();
    let r = s
        .gated(Method::GET, "/v1/auth/me", gate_key)
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = s
        .req(Method::GET, "/v1/auth/me")
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let (label, email, by_user): (String, Option<String>, Option<uuid::Uuid>) = sqlx::query_as(
        "SELECT label, email, created_by_user FROM access_keys WHERE created_by_user IS NOT NULL",
    )
    .fetch_one(&s.state.db)
    .await
    .unwrap();
    assert_eq!(label, "New Mac via pairing");
    assert_eq!(email.as_deref(), Some("ana@example.com"));
    assert_eq!(by_user.unwrap().to_string(), a.user_id);
}

#[tokio::test]
async fn pair_is_rate_limited() {
    let s = start_with(|c| c.limits.auth_per_minute = 3).await;
    let mut statuses = Vec::new();
    for _ in 0..5 {
        statuses.push(
            pair(
                &s,
                "cp_definitely-not-a-real-pairing-cd",
                &new_device(),
                None,
            )
            .await
            .status()
            .as_u16(),
        );
    }
    assert_eq!(statuses, [401, 401, 401, 429, 429]);
}
