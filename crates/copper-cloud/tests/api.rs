//! End-to-end API tests against a real Postgres and the composed app.

// Scenario tests read best as one linear story with short names (s = server, a/b = sessions).
#![allow(clippy::too_many_lines, clippy::many_single_char_names)]

mod common;

use std::time::Duration;

use common::{new_device, start, start_with, PASSWORD};
use futures::StreamExt as _;
use reqwest::Method;
use serde_json::{json, Value};

fn b64(s: &str) -> String {
    copper_cloud_core::ids::b64_std(s.as_bytes())
}

fn unb64(v: &Value) -> String {
    String::from_utf8(copper_cloud_core::ids::b64_decode_any(v.as_str().unwrap()).unwrap()).unwrap()
}

#[tokio::test]
async fn instance_gate() {
    let s = start().await;
    // healthz is the only open endpoint.
    let r = s.http.get(s.url("/healthz")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "ok");

    for path in [
        "/v1/auth/me",
        "/v1/info",
        "/v1/does-not-exist",
        "/elsewhere",
    ] {
        let r = s.http.get(s.url(path)).send().await.unwrap();
        assert_eq!(r.status(), 401, "{path}");
        let body: Value = r.json().await.unwrap();
        assert_eq!(body["error"], "instance_key", "{path}");

        let r = s
            .http
            .get(s.url(path))
            .header(
                "X-Copper-Instance",
                "wrong-key-wrong-key-wrong-key-wrong-key",
            )
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401, "{path} wrong key");
    }
    // POST bodies are not even read without the key.
    let r = s
        .http
        .post(s.url("/v1/auth/signup"))
        .json(&json!({"email":"a@b.co","password":PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    // With the key: info works, unknown routes are a JSON 404, auth is required on /me.
    let r = s.req(Method::GET, "/v1/info").send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["signup"], true);
    let r = s
        .req(Method::GET, "/v1/does-not-exist")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "not_found");
    let r = s.req(Method::GET, "/v1/auth/me").send().await.unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "session");

    // The canvas router is mounted under the same gate + auth.
    let r = s.http.get(s.url("/v1/canvases")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "instance_key");
    let r = s.req(Method::GET, "/v1/canvases").send().await.unwrap();
    assert_eq!(r.status(), 401);
    let a = s.signup("gate@example.com", PASSWORD, &new_device()).await;
    let r = s
        .authed(Method::GET, "/v1/canvases", &a)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn signup_login_me_logout() {
    let s = start_with(|c| c.limits.auth_per_minute = 100).await;
    let dev = new_device();
    let a = s.signup("Alice@Example.com", PASSWORD, &dev).await;
    assert_eq!(a.device_id, dev);
    assert_eq!(a.token.len(), 43);

    let me: Value = s
        .authed(Method::GET, "/v1/auth/me", &a)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["user"]["email"], "Alice@Example.com");
    assert_eq!(me["user"]["display_name"], "Test User");
    assert_eq!(me["device"]["id"], dev);
    assert_eq!(me["device"]["name"], "Test Mac");
    assert_eq!(me["sync_cursor"]["history_seq"], 0);

    // The token is stored hashed only.
    let raw: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE token_sha256 = $1")
        .bind(a.token.as_bytes())
        .fetch_one(&s.state.db)
        .await
        .unwrap();
    assert_eq!(raw, 0);

    // ?token= works too (websocket tooling).
    let r = s
        .req(Method::GET, &format!("/v1/auth/me?token={}", a.token))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    // Duplicate email (case-insensitive) → 409.
    let r = s
        .req(Method::POST, "/v1/auth/signup")
        .json(&json!({"email":"alice@EXAMPLE.com","password":PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "conflict");

    // Validation.
    let r = s
        .req(Method::POST, "/v1/auth/signup")
        .json(&json!({"email":"bob@example.com","password":"short"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = s
        .req(Method::POST, "/v1/auth/signup")
        .json(&json!({"email":"not-an-email","password":PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = s
        .req(Method::POST, "/v1/auth/signup")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // Wrong password / unknown user → 401 credentials (same answer for both).
    let r = s.login("alice@example.com", "wrong password!!", &dev).await;
    assert_eq!(r.status(), 401);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "credentials");
    let r = s.login("nobody@example.com", PASSWORD, &dev).await;
    assert_eq!(r.status(), 401);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "credentials");

    // Login on a second device.
    let dev2 = new_device();
    let r = s.login("ALICE@example.com", PASSWORD, &dev2).await;
    assert_eq!(r.status(), 200);
    let b = common::session_from(&r.json().await.unwrap());
    assert_eq!(b.user_id, a.user_id);
    assert_ne!(b.token, a.token);

    // Devices list marks the current one.
    let devices: Vec<Value> = s
        .authed(Method::GET, "/v1/devices", &b)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(devices.len(), 2);
    assert!(devices
        .iter()
        .any(|d| d["id"] == dev2.as_str() && d["current"] == true));
    let r = s
        .authed(Method::PATCH, &format!("/v1/devices/{dev}"), &b)
        .json(&json!({"name":"Renamed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["name"], "Renamed");

    // Logout kills only that session.
    let r = s
        .authed(Method::POST, "/v1/auth/logout", &a)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = s
        .authed(Method::GET, "/v1/auth/me", &a)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = s
        .authed(Method::GET, "/v1/auth/me", &b)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    // Deleting a device revokes its sessions.
    let r = s
        .authed(Method::DELETE, &format!("/v1/devices/{dev2}"), &b)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = s
        .authed(Method::GET, "/v1/auth/me", &b)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn password_change_revokes_other_sessions() {
    let s = start().await;
    let a = s.signup("pw@example.com", PASSWORD, &new_device()).await;
    let other = common::session_from(
        &s.login("pw@example.com", PASSWORD, &new_device())
            .await
            .json()
            .await
            .unwrap(),
    );
    let r = s
        .authed(Method::POST, "/v1/auth/password", &a)
        .json(&json!({"old":"not my password","new":"a brand new password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = s
        .authed(Method::POST, "/v1/auth/password", &a)
        .json(&json!({"old":PASSWORD,"new":"a brand new password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["revoked_sessions"], 1);
    assert_eq!(
        s.authed(Method::GET, "/v1/auth/me", &a)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        s.authed(Method::GET, "/v1/auth/me", &other)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        s.login("pw@example.com", PASSWORD, &new_device())
            .await
            .status(),
        401
    );
    assert_eq!(
        s.login("pw@example.com", "a brand new password", &new_device())
            .await
            .status(),
        200
    );
}

#[tokio::test]
async fn signup_policy() {
    let s = start_with(|c| c.allow_signup = false).await;
    let r = s.req(Method::GET, "/v1/info").send().await.unwrap();
    assert_eq!(
        r.json::<Value>().await.unwrap()["signup"],
        true,
        "first user always allowed"
    );
    s.signup("first@example.com", PASSWORD, &new_device()).await;
    let r = s
        .req(Method::POST, "/v1/auth/signup")
        .json(&json!({"email":"second@example.com","password":PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    // Admin override (what `admin enable-signup` writes) wins over the config.
    sqlx::query("INSERT INTO server_settings (key, value) VALUES ('allow_signup', 'true'::jsonb)")
        .execute(&s.state.db)
        .await
        .unwrap();
    s.signup("second@example.com", PASSWORD, &new_device())
        .await;
}

#[tokio::test]
async fn auth_rate_limit() {
    let s = start().await;
    assert_eq!(s.state.cfg.limits.auth_per_minute, 10);
    // Over-long passwords are rejected before hashing, so these 10 attempts are fast enough
    // that the GCRA bucket (one token per 6 s) cannot refill mid-test.
    let long = "x".repeat(2000);
    for i in 0..10 {
        let r = s.login("nobody@example.com", &long, &new_device()).await;
        assert_eq!(r.status(), 401, "attempt {i}");
    }
    let r = s.login("nobody@example.com", &long, &new_device()).await;
    assert_eq!(r.status(), 429);
    assert!(r.headers().contains_key("retry-after"));
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "rate_limited");
    // Non-auth routes and the status probe (GET /v1/auth/me) are not limited.
    for _ in 0..15 {
        assert_eq!(
            s.req(Method::GET, "/v1/info")
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        assert_eq!(
            s.req(Method::GET, "/v1/auth/me")
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    let r = s
        .req(Method::POST, "/v1/auth/signup")
        .json(&json!({"email":"x@example.com","password":PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429, "signup shares the bucket");
}

#[tokio::test]
async fn sync_docs_lww() {
    let s = start().await;
    let a = s.signup("docs@example.com", PASSWORD, &new_device()).await;
    let doc1 = r#"{"spaces":[{"id":"s1","name":"Work"}]}"#;

    let r = s
        .authed(Method::GET, "/v1/sync/docs/spaces", &a)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    let r = s
        .authed(Method::PUT, "/v1/sync/docs/spaces", &a)
        .json(&json!({"base_version":0,"payload":b64(doc1)}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["version"], 1);

    let got: Value = s
        .authed(Method::GET, "/v1/sync/docs/spaces", &a)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(got["version"], 1);
    assert_eq!(got["device_id"], a.device_id.as_str());
    assert_eq!(unb64(&got["payload"]), doc1);

    // Encrypted at rest.
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT payload FROM sync_docs WHERE domain = 'spaces'")
            .fetch_one(&s.state.db)
            .await
            .unwrap();
    assert_eq!(stored.len(), doc1.len() + 28);
    assert!(!stored.windows(6).any(|w| w == b"spaces"));

    // Stale base → 409 with the server copy.
    let r = s
        .authed(Method::PUT, "/v1/sync/docs/spaces", &a)
        .json(&json!({"base_version":0,"payload":b64("{}")}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);
    let c: Value = r.json().await.unwrap();
    assert_eq!(c["error"], "conflict");
    assert_eq!(c["version"], 1);
    assert_eq!(unb64(&c["payload"]), doc1);

    let doc2 = r#"{"spaces":[]}"#;
    let r = s
        .authed(Method::PUT, "/v1/sync/docs/spaces", &a)
        .json(&json!({"base_version":1,"payload":b64(doc2)}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["version"], 2);
    let r = s
        .authed(Method::PUT, "/v1/sync/docs/spaces", &a)
        .json(&json!({"base_version":1,"payload":b64("{}")}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);
    assert_eq!(r.json::<Value>().await.unwrap()["version"], 2);

    // A base for a doc that does not exist → 409 with version 0.
    let r = s
        .authed(Method::PUT, "/v1/sync/docs/bookmarks", &a)
        .json(&json!({"base_version":5,"payload":b64("[]")}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);
    let c: Value = r.json().await.unwrap();
    assert_eq!(c["version"], 0);
    assert!(c["payload"].is_null());

    s.authed(Method::PUT, "/v1/sync/docs/settings", &a)
        .json(&json!({"base_version":0,"payload":b64(r#"{"theme":"dark"}"#)}))
        .send()
        .await
        .unwrap();
    let list: Vec<Value> = s
        .authed(Method::GET, "/v1/sync/docs", &a)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["domain"], "settings");
    assert_eq!(list[1]["domain"], "spaces");
    assert_eq!(list[1]["version"], 2);
    assert_eq!(list[1]["bytes"], doc2.len());
    assert!(list[0].get("payload").is_none());

    // Unknown domain / bad payload.
    let r = s
        .authed(Method::PUT, "/v1/sync/docs/passwords", &a)
        .json(&json!({"base_version":0,"payload":b64("{}")}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = s
        .authed(Method::PUT, "/v1/sync/docs/settings", &a)
        .json(&json!({"base_version":1,"payload":"***not base64***"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // Another user sees none of it.
    let b = s.signup("other@example.com", PASSWORD, &new_device()).await;
    let list: Vec<Value> = s
        .authed(Method::GET, "/v1/sync/docs", &b)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.len(), 0, "list should be empty");
    assert_eq!(
        s.authed(Method::GET, "/v1/sync/docs/spaces", &b)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

#[tokio::test]
async fn sync_doc_size_limit() {
    let s = start_with(|c| c.limits.max_blob_bytes = 2048).await;
    let a = s.signup("big@example.com", PASSWORD, &new_device()).await;
    let ok = "x".repeat(2048);
    let r = s
        .authed(Method::PUT, "/v1/sync/docs/settings", &a)
        .json(&json!({"base_version":0,"payload":b64(&ok)}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let too_big = "x".repeat(2049);
    let r = s
        .authed(Method::PUT, "/v1/sync/docs/settings", &a)
        .json(&json!({"base_version":1,"payload":b64(&too_big)}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    let huge = "x".repeat(100_000);
    let r = s
        .authed(Method::PUT, "/v1/sync/docs/settings", &a)
        .json(&json!({"base_version":1,"payload":b64(&huge)}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"],
        "payload_too_large"
    );
}

#[tokio::test]
async fn tabs_domain_ownership() {
    let s = start().await;
    let (dev_a, dev_b) = (new_device(), new_device());
    let a = s.signup("tabs@example.com", PASSWORD, &dev_a).await;
    let b = common::session_from(
        &s.login("tabs@example.com", PASSWORD, &dev_b)
            .await
            .json()
            .await
            .unwrap(),
    );
    let tabs = r#"{"tabs":[{"url":"https://example.com"}]}"#;
    let path_a = format!("/v1/sync/docs/tabs:{dev_a}");
    let r = s
        .authed(Method::PUT, &path_a, &a)
        .json(&json!({"base_version":0,"payload":b64(tabs)}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // Device B may read A's tabs but not write them.
    let r = s
        .authed(Method::PUT, &path_a, &b)
        .json(&json!({"base_version":1,"payload":b64("{}")}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let got: Value = s
        .authed(Method::GET, &path_a, &b)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(unb64(&got["payload"]), tabs);
    // Percent-encoded colon and upper-case uuid normalize to the same domain.
    let upper = format!("/v1/sync/docs/tabs%3A{}", dev_a.to_uppercase());
    let got: Value = s
        .authed(Method::GET, &upper, &b)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(got["domain"], format!("tabs:{dev_a}"));
    // Removing device A drops its tabs doc.
    s.authed(Method::DELETE, &format!("/v1/devices/{dev_a}"), &b)
        .send()
        .await
        .unwrap();
    assert_eq!(
        s.authed(Method::GET, &path_a, &b)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

#[tokio::test]
async fn history_push_pull() {
    let s = start_with(|c| c.limits.max_history_batch = 50).await;
    let (dev_a, dev_b) = (new_device(), new_device());
    let a = s.signup("hist@example.com", PASSWORD, &dev_a).await;
    let b = common::session_from(
        &s.login("hist@example.com", PASSWORD, &dev_b)
            .await
            .json()
            .await
            .unwrap(),
    );
    let entries: Vec<Value> = (0..5)
        .map(|i| {
            json!({
                "url": format!("https://example.com/{i}"),
                "title": format!("Page \"{i}\" ünïcode"),
                "visited_at": if i % 2 == 0 { json!("2026-10-01T12:00:00Z") } else { json!(1_790_856_000 + i) },
                "transition": "link",
            })
        })
        .collect();
    let r = s
        .authed(Method::POST, "/v1/sync/history", &a)
        .json(&json!({ "entries": entries }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let pushed: Value = r.json().await.unwrap();
    assert_eq!(pushed["inserted"], 5);
    let last_seq = pushed["seq"].as_i64().unwrap();

    // B pulls A's entries, excluding its own (none).
    let page: Value = s
        .authed(
            Method::GET,
            "/v1/sync/history?since=0&exclude_device=me",
            &b,
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let got = page["entries"].as_array().unwrap();
    assert_eq!(got.len(), 5);
    assert_eq!(got[0]["payload"], entries[0]);
    assert_eq!(got[4]["payload"], entries[4]);
    assert_eq!(got[1]["visited_at"], "2026-10-01T12:00:01Z");
    assert_eq!(got[0]["device_id"], dev_a.as_str());
    assert_eq!(page["next"], last_seq);
    assert_eq!(page["more"], false);

    // A excluding itself sees nothing.
    let page: Value = s
        .authed(Method::GET, "/v1/sync/history?exclude_device=me", &a)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["entries"].as_array().unwrap().len(), 0);

    // Pagination.
    let page: Value = s
        .authed(Method::GET, "/v1/sync/history?since=0&limit=2", &b)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["entries"].as_array().unwrap().len(), 2);
    assert_eq!(page["more"], true);
    let next = page["next"].as_i64().unwrap();
    let page: Value = s
        .authed(
            Method::GET,
            &format!("/v1/sync/history?since={next}&limit=10"),
            &b,
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["entries"].as_array().unwrap().len(), 3);
    assert_eq!(page["more"], false);

    // /me exposes the cursor.
    let me: Value = s
        .authed(Method::GET, "/v1/auth/me", &b)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["sync_cursor"]["history_seq"], last_seq);

    // Limits and validation.
    let too_many: Vec<Value> = (0..51).map(|i| json!({ "url": format!("u{i}") })).collect();
    let r = s
        .authed(Method::POST, "/v1/sync/history", &a)
        .json(&json!({ "entries": too_many }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = s
        .authed(Method::POST, "/v1/sync/history", &a)
        .json(&json!({ "entries": ["not an object"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = s
        .authed(Method::POST, "/v1/sync/history", &a)
        .json(&json!({ "entries": [{ "url": "x", "visited_at": "last tuesday" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // Stored encrypted; other users see nothing.
    let stored: Vec<Vec<u8>> = sqlx::query_scalar("SELECT payload FROM history")
        .fetch_all(&s.state.db)
        .await
        .unwrap();
    assert!(stored
        .iter()
        .all(|p| !p.windows(7).any(|w| w == b"example")));
    let c = s.signup("hist2@example.com", PASSWORD, &new_device()).await;
    let page: Value = s
        .authed(Method::GET, "/v1/sync/history", &c)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["entries"].as_array().unwrap().len(), 0);
}

/// Read SSE frames until one with `event: <name>` arrives; returns its data JSON.
async fn next_event(
    stream: &mut (impl futures::Stream<Item = reqwest::Result<axum::body::Bytes>> + Unpin),
    buf: &mut String,
    name: &str,
) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        while let Some(end) = buf.find("\n\n") {
            let frame: String = buf.drain(..end + 2).collect();
            let mut event = None;
            let mut data = String::new();
            for line in frame.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    event = Some(v.trim().to_owned());
                } else if let Some(v) = line.strip_prefix("data:") {
                    data.push_str(v.trim());
                }
            }
            if event.as_deref() == Some(name) {
                return serde_json::from_str(&data).unwrap();
            }
        }
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for SSE event {name}"))
            .expect("stream ended")
            .unwrap();
        buf.push_str(std::str::from_utf8(&chunk).unwrap());
    }
}

#[tokio::test]
async fn sse_events_on_writes() {
    let s = start().await;
    let (dev_a, dev_b) = (new_device(), new_device());
    let a = s.signup("sse@example.com", PASSWORD, &dev_a).await;
    let b = common::session_from(
        &s.login("sse@example.com", PASSWORD, &dev_b)
            .await
            .json()
            .await
            .unwrap(),
    );
    let other = s
        .signup("sse-other@example.com", PASSWORD, &new_device())
        .await;

    let resp = s
        .authed(Method::GET, "/v1/sync/events", &b)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let mut stream = Box::pin(resp.bytes_stream());
    let mut buf = String::new();
    let ready = next_event(&mut stream, &mut buf, "ready").await;
    assert_eq!(ready["device_id"], dev_b.as_str());

    // Another user's writes are not delivered; ours are.
    s.authed(Method::PUT, "/v1/sync/docs/spaces", &other)
        .json(&json!({"base_version":0,"payload":b64("{}")}))
        .send()
        .await
        .unwrap();
    s.authed(Method::PUT, "/v1/sync/docs/spaces", &a)
        .json(&json!({"base_version":0,"payload":b64(r#"{"a":1}"#)}))
        .send()
        .await
        .unwrap();
    let ev = next_event(&mut stream, &mut buf, "doc").await;
    assert_eq!(ev["domain"], "spaces");
    assert_eq!(ev["version"], 1);
    assert_eq!(ev["device_id"], dev_a.as_str());

    let pushed: Value = s
        .authed(Method::POST, "/v1/sync/history", &a)
        .json(&json!({"entries":[{"url":"https://example.com"}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ev = next_event(&mut stream, &mut buf, "history").await;
    assert_eq!(ev["seq"], pushed["seq"]);

    // Canvas events published by other crates arrive on the same stream.
    let uid: uuid::Uuid = a.user_id.parse().unwrap();
    s.state.events.publish(
        uid,
        copper_cloud_core::events::Event::Canvas {
            canvas_id: uuid::Uuid::nil(),
            kind: "created".into(),
        },
    );
    let ev = next_event(&mut stream, &mut buf, "canvas").await;
    assert_eq!(ev["kind"], "created");
}

#[tokio::test]
async fn handler_panic_becomes_500() {
    let s = start().await;
    let extra = axum::Router::new().route(
        "/boom",
        axum::routing::get(|| async {
            panic!("boom");
            #[allow(unreachable_code)]
            ""
        }),
    );
    let app = copper_cloud_core::app(s.state.clone(), extra);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let r = s
        .http
        .get(format!("http://{addr}/v1/boom"))
        .header("X-Copper-Instance", s.key())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 500);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "internal");
    // The server keeps serving.
    let r = s
        .http
        .get(format!("http://{addr}/healthz"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}
