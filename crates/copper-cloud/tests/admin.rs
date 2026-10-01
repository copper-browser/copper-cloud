//! Admin API (spec §7.3): cookie sessions, CSRF header, rate limit, access keys, people,
//! devices, canvases, pairing codes, settings, overview and audit.

// Scenario tests read best as one linear story with short names (s = server, c = cookie).
#![allow(clippy::too_many_lines, clippy::many_single_char_names)]

mod common;

use common::*;
use reqwest::Method;
use serde_json::{json, Value};

async fn json_of(r: reqwest::Response) -> Value {
    r.json().await.unwrap()
}

#[tokio::test]
async fn login_logout_me_and_csrf() {
    let s = start().await;
    s.create_admin().await;

    let r = s.admin(Method::GET, "me", None).send().await.unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(json_of(r).await["error"], "admin_session");

    // CSRF header required on every non-GET (login included).
    let r = s
        .http
        .post(s.url("/admin/api/login"))
        .json(&json!({ "email": ADMIN_EMAIL, "password": ADMIN_PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    assert_eq!(json_of(r).await["error"], "csrf");

    let r = s
        .admin(Method::POST, "login", None)
        .json(&json!({ "email": ADMIN_EMAIL, "password": "wrong password!!" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(json_of(r).await["error"], "credentials");
    let r = s
        .admin(Method::POST, "login", None)
        .json(&json!({ "email": "nobody@example.com", "password": ADMIN_PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    let r = s
        .admin(Method::POST, "login", None)
        .json(&json!({ "email": "ADMIN@example.com", "password": ADMIN_PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let set = r.headers()["set-cookie"].to_str().unwrap().to_owned();
    assert!(set.starts_with("cc_admin="), "{set}");
    for attr in [
        "HttpOnly",
        "SameSite=Strict",
        "Path=/admin",
        "Max-Age=604800",
    ] {
        assert!(set.contains(attr), "{attr} in {set}");
    }
    assert!(!set.contains("Secure"), "tls off → no Secure: {set}");
    assert_eq!(r.headers()["cache-control"], "no-store");
    let cookie = cookie_from(&r);
    let body = json_of(r).await;
    assert_eq!(body["admin"]["email"], ADMIN_EMAIL);
    assert!(body["admin"]["last_login_at"].is_string());
    assert!(body.get("token").is_none());

    // GET needs no CSRF header.
    let r = s
        .http
        .get(s.url("/admin/api/me"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let me = json_of(r).await;
    assert_eq!(me["admin"]["email"], ADMIN_EMAIL);
    assert!(me["session"]["expires_at"].is_string());
    // A mutation with a valid cookie but no CSRF header is refused.
    let r = s
        .http
        .patch(s.url("/admin/api/settings"))
        .header("Cookie", &cookie)
        .json(&json!({ "allow_signup": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    // The instance gate does not apply to the admin API (and the instance key is no login).
    let r = s
        .http
        .get(s.url("/admin/api/me"))
        .header("X-Copper-Instance", s.key())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    // Unknown admin routes are a JSON 404, not the portal.
    let r = s
        .admin(Method::GET, "nope", Some(&cookie))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    assert_eq!(json_of(r).await["error"], "not_found");

    let r = s
        .admin(Method::POST, "logout", Some(&cookie))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    let r = s
        .admin(Method::GET, "me", Some(&cookie))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    // Logout without a session is still fine.
    let r = s.admin(Method::POST, "logout", None).send().await.unwrap();
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn secure_cookie_unless_tls_off() {
    let s = start_with(|c| c.tls.mode = copper_cloud_core::config::TlsMode::SelfSigned).await;
    s.create_admin().await;
    let r = s
        .admin(Method::POST, "login", None)
        .json(&json!({ "email": ADMIN_EMAIL, "password": ADMIN_PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .contains("; Secure"));
}

#[tokio::test]
async fn admin_login_is_rate_limited_separately() {
    let s = start_with(|c| c.limits.auth_per_minute = 3).await;
    s.create_admin().await;
    let mut statuses = Vec::new();
    for _ in 0..4 {
        let r = s
            .admin(Method::POST, "login", None)
            .json(&json!({ "email": ADMIN_EMAIL, "password": "wrong password!!" }))
            .send()
            .await
            .unwrap();
        statuses.push(r.status().as_u16());
    }
    assert_eq!(statuses, [401, 401, 401, 429]);
    // /v1/auth has its own bucket.
    let r = s.login("x@example.com", PASSWORD, &new_device()).await;
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn access_keys_crud_and_settings() {
    let s = start().await;
    s.create_admin().await;
    let c = s.admin_login().await;

    // Validation.
    for bad in [
        json!({ "label": "" }),
        json!({ "label": "x", "max_uses": 0 }),
        json!({ "label": "x", "expires_in_days": 0 }),
        json!({ "label": "x", "email": "not-an-email" }),
    ] {
        let r = s
            .admin(Method::POST, "access-keys", Some(&c))
            .json(&bad)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "{bad}");
    }

    let r = s
        .admin(Method::POST, "access-keys", Some(&c))
        .json(
            &json!({ "label": "Ana's MacBook", "email": "ana@example.com",
                       "expires_in_days": 30, "max_uses": 1 }),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let created = json_of(r).await;
    let key = created["key"].as_str().unwrap().to_owned();
    assert!(copper_cloud_core::access::plausible_access_key(&key));
    assert_eq!(
        created["link_code"].as_str().unwrap(),
        format!("copper-cloud://localhost:8443/#k={key}")
    );
    assert_eq!(created["status"], "active");
    assert_eq!(created["created_by"], ADMIN_EMAIL);
    assert_eq!(created["uses"], 0);
    assert_eq!(created["max_uses"], 1);
    assert!(created["expires_at"].is_string());
    let id = created["id"].as_str().unwrap().to_owned();
    s.admin(Method::POST, "access-keys", Some(&c))
        .json(&json!({ "label": "Team" }))
        .send()
        .await
        .unwrap();

    // The secret is never returned again.
    let r = s
        .admin(Method::GET, "access-keys", Some(&c))
        .send()
        .await
        .unwrap();
    let text = r.text().await.unwrap();
    assert!(!text.contains(&key));
    let list: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(list["total"], 2);
    assert_eq!(list["limit"], 100);
    assert_eq!(list["items"][0]["label"], "Team", "newest first");
    assert!(list["items"][0].get("key").is_none());
    assert!(list["items"][0].get("link_code").is_none());

    // The key works at the gate, then signup consumes its single use.
    s.set_access_mode(copper_cloud_core::access::AccessMode::Directory)
        .await;
    let r = s.signup_with(&key, "ana@example.com", &new_device()).await;
    assert_eq!(r.status(), 200);
    let list = json_of(
        s.admin(Method::GET, "access-keys?status=exhausted", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"].as_str().unwrap(), id);
    assert_eq!(list["items"][0]["uses"], 1);
    assert!(list["items"][0]["last_used_at"].is_string());

    // Revoke → gate refuses it; revoking again is idempotent; unknown ids 404.
    let r = s
        .admin(Method::DELETE, &format!("access-keys/{id}"), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v = json_of(r).await;
    assert_eq!(v["status"], "revoked");
    assert!(v.get("key").is_none());
    let r = s.gated(Method::GET, "/v1/info", &key).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = s
        .admin(Method::DELETE, &format!("access-keys/{id}"), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    for bad in [
        "access-keys/not-a-uuid",
        &format!("access-keys/{}", uuid::Uuid::now_v7()),
    ] {
        let r = s.admin(Method::DELETE, bad, Some(&c)).send().await.unwrap();
        assert_eq!(r.status(), 404, "{bad}");
    }
    let r = s
        .admin(Method::GET, "access-keys?status=bogus", Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // Settings: PATCH flips access_mode and the gate follows (this process at once).
    let r = s
        .admin(Method::GET, "settings", Some(&c))
        .send()
        .await
        .unwrap();
    let st = json_of(r).await;
    assert_eq!(st["access_mode"], "directory");
    assert_eq!(st["allow_signup"], true);
    assert!(st.get("instance_link_code").is_some());
    assert_eq!(
        s.req(Method::GET, "/v1/info")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let r = s
        .admin(Method::PATCH, "settings", Some(&c))
        .json(&json!({ "access_mode": "open", "allow_signup": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let st = json_of(r).await;
    assert_eq!(st["access_mode"], "open");
    assert_eq!(st["allow_signup"], false);
    let r = s.req(Method::GET, "/v1/info").send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(json_of(r).await["access_mode"], "open");
    let r = s
        .admin(Method::PATCH, "settings", Some(&c))
        .json(&json!({ "access_mode": "closed" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = s
        .admin(Method::PATCH, "settings", Some(&c))
        .json(&json!({ "access_mode": "directory" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        s.req(Method::GET, "/v1/info")
            .send()
            .await
            .unwrap()
            .status(),
        401,
        "gate follows the PATCH"
    );

    // Audit: newest first, paginated.
    let r = s
        .admin(Method::GET, "audit?limit=2", Some(&c))
        .send()
        .await
        .unwrap();
    let audit = json_of(r).await;
    assert_eq!(audit["items"].as_array().unwrap().len(), 2);
    assert_eq!(audit["limit"], 2);
    assert_eq!(audit["items"][0]["action"], "settings.update");
    assert_eq!(audit["items"][0]["detail"]["access_mode"], "directory");
    assert_eq!(audit["items"][0]["admin_email"], ADMIN_EMAIL);
    let all = json_of(
        s.admin(Method::GET, "audit?limit=500", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let actions: Vec<&str> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions,
        [
            "settings.update",
            "settings.update",
            "access_key.revoke",
            "access_key.revoke",
            "access_key.create",
            "access_key.create",
            "login"
        ]
    );
    assert_eq!(all["total"], 7);
    let r = s
        .admin(Method::GET, "audit?limit=x", Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
}

#[tokio::test]
async fn people_devices_canvases_pairing_overview() {
    let s = start().await;
    s.create_admin().await;
    let c = s.admin_login().await;
    let ana = s.signup("ana@example.com", PASSWORD, &new_device()).await;
    let bob = s.signup("bob@example.com", PASSWORD, &new_device()).await;

    // Ana: a second device, a sync doc, history, a pairing code, a shared canvas with Bob as
    // member, a pending invite to someone else, an access key bound to her email.
    let ana_d2 = new_device();
    assert_eq!(
        s.login("ana@example.com", PASSWORD, &ana_d2).await.status(),
        200
    );
    let r = s
        .authed(Method::PUT, "/v1/sync/docs/settings", &ana)
        .json(&json!({ "base_version": 0, "payload": copper_cloud_core::ids::b64_std(b"{}") }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = s
        .authed(Method::POST, "/v1/sync/history", &ana)
        .json(&json!({ "entries": [{ "url": "https://a.example", "visited_at": "2026-10-01T00:00:00Z" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = s
        .authed(Method::POST, "/v1/auth/pairing", &ana)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let canvas = json_of(
        s.authed(Method::POST, "/v1/canvases", &ana)
            .json(&json!({ "name": "Roadmap" }))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let canvas_id = canvas["id"].as_str().unwrap().to_owned();
    let inv = json_of(
        s.authed(
            Method::POST,
            &format!("/v1/canvases/{canvas_id}/invites"),
            &ana,
        )
        .json(&json!({ "email": "bob@example.com" }))
        .send()
        .await
        .unwrap(),
    )
    .await;
    let r = s
        .authed(
            Method::POST,
            &format!("/v1/invites/{}/accept", inv["id"].as_str().unwrap()),
            &bob,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    s.authed(
        Method::POST,
        &format!("/v1/canvases/{canvas_id}/invites"),
        &ana,
    )
    .json(&json!({ "email": "carol@example.com" }))
    .send()
    .await
    .unwrap();
    s.mint_key("Ana", Some("ana@example.com"), None).await;
    let (_, team_key) = s.mint_key("Team", None, None).await;

    // Overview.
    let ov = json_of(
        s.admin(Method::GET, "overview", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ov["counts"]["users"], 2);
    assert_eq!(ov["counts"]["devices"], 3);
    assert_eq!(ov["counts"]["access_keys"], 2);
    assert!(ov["counts"]["canvases"].as_i64().unwrap() >= 1);
    assert!(ov["counts"]["live_rooms"].is_number());
    assert!(ov["counts"]["live_peers"].is_number());
    assert_eq!(ov["access_mode"], "open");
    assert_eq!(ov["public_url"], "localhost:8443");
    assert_eq!(ov["tls"]["mode"], "off");
    assert!(ov["version"].is_string());
    assert!(ov["uptime_s"].is_number());

    // People.
    let users = json_of(
        s.admin(Method::GET, "users", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(users["total"], 2);
    assert_eq!(
        users["items"][0]["email"], "bob@example.com",
        "newest first"
    );
    let ana_row = &users["items"][1];
    assert_eq!(ana_row["device_count"], 2);
    assert!(ana_row["canvas_count"].as_i64().unwrap() >= 1);
    assert!(ana_row["last_seen_at"].is_string());
    assert_eq!(ana_row["disabled"], false);
    let q = json_of(
        s.admin(Method::GET, "users?q=ANA", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(q["total"], 1);
    let q = json_of(
        s.admin(Method::GET, "users?q=%25", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(q["total"], 0, "wildcards are literal");
    let page = json_of(
        s.admin(Method::GET, "users?limit=1&offset=1", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["email"], "ana@example.com");
    assert_eq!(page["total"], 2);

    // Disable Bob → sessions revoked, login refused; rename; enable.
    let r = s
        .admin(Method::PATCH, &format!("users/{}", bob.user_id), Some(&c))
        .json(&json!({ "disabled": true, "display_name": "Robert" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let row = json_of(r).await;
    assert_eq!(row["disabled"], true);
    assert_eq!(row["display_name"], "Robert");
    let r = s
        .authed(Method::GET, "/v1/auth/me", &bob)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = s.login("bob@example.com", PASSWORD, &new_device()).await;
    assert_eq!(json_of(r).await["error"], "account_disabled");
    let r = s
        .admin(Method::PATCH, &format!("users/{}", bob.user_id), Some(&c))
        .json(&json!({ "disabled": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // Reset Bob's password.
    let r = s
        .admin(
            Method::POST,
            &format!("users/{}/reset-password", bob.user_id),
            Some(&c),
        )
        .json(&json!({ "password": "short" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = s
        .admin(
            Method::POST,
            &format!("users/{}/reset-password", bob.user_id),
            Some(&c),
        )
        .json(&json!({ "password": "a brand new password" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        s.login("bob@example.com", PASSWORD, &new_device())
            .await
            .status(),
        401
    );
    let r = s
        .login("bob@example.com", "a brand new password", &new_device())
        .await;
    assert_eq!(r.status(), 200);
    let bob = session_from(&json_of(r).await);

    // Devices: filter, delete one row (with user_id) → its sessions die.
    let devs = json_of(
        s.admin(
            Method::GET,
            &format!("devices?user_id={}", ana.user_id),
            Some(&c),
        )
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(devs["total"], 2);
    assert_eq!(devs["items"][0]["user_email"], "ana@example.com");
    let r = s
        .admin(
            Method::DELETE,
            &format!("devices/{ana_d2}?user_id={}", ana.user_id),
            Some(&c),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(json_of(r).await["deleted"], 1);
    let r = s
        .admin(Method::DELETE, &format!("devices/{ana_d2}"), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    // Canvases: listed without content; pairing codes listed.
    let cs = json_of(
        s.admin(Method::GET, "canvases?kind=shared", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(cs["total"], 1);
    assert_eq!(cs["items"][0]["name"], "Roadmap");
    assert_eq!(cs["items"][0]["owner_email"], "ana@example.com");
    assert_eq!(cs["items"][0]["member_count"], 2);
    assert!(cs["items"][0].get("state").is_none());
    let pc = json_of(
        s.admin(Method::GET, "pairing-codes", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(pc["total"], 1);
    assert_eq!(pc["items"][0]["user_email"], "ana@example.com");

    // Delete Ana: everything of hers goes; Bob and the team key stay.
    let r = s
        .admin(Method::DELETE, &format!("users/{}", ana.user_id), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let ana_id: uuid::Uuid = ana.user_id.parse().unwrap();
    for (table, col) in [
        ("sessions", "user_id"),
        ("devices", "user_id"),
        ("sync_docs", "user_id"),
        ("history", "user_id"),
        ("pairing_codes", "user_id"),
        ("canvases", "owner_id"),
        ("canvas_members", "user_id"),
    ] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE {col} = $1"))
            .bind(ana_id)
            .fetch_one(&s.state.db)
            .await
            .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM canvas_members WHERE canvas_id = $1")
        .bind(canvas_id.parse::<uuid::Uuid>().unwrap())
        .fetch_one(&s.state.db)
        .await
        .unwrap();
    assert_eq!(n, 0, "Bob's membership of Ana's canvas");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM canvas_invites")
        .fetch_one(&s.state.db)
        .await
        .unwrap();
    assert_eq!(n, 0, "invites of deleted canvases");
    let keys: Vec<String> = sqlx::query_scalar("SELECT label FROM access_keys")
        .fetch_all(&s.state.db)
        .await
        .unwrap();
    assert_eq!(keys, ["Team"]);
    assert_eq!(
        s.gated(Method::GET, "/v1/info", &team_key)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let r = s
        .authed(Method::GET, "/v1/canvases", &bob)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = s
        .admin(Method::DELETE, &format!("users/{}", ana.user_id), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    // Delete Bob's Personal canvas: allowed for admins; it comes back empty on next use.
    let personal = json_of(
        s.admin(Method::GET, "canvases?kind=personal", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let pid = personal["items"][0]["id"].as_str().unwrap().to_owned();
    let r = s
        .admin(Method::DELETE, &format!("canvases/{pid}"), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = s
        .admin(Method::DELETE, &format!("canvases/{pid}"), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    // Pairing code revoke.
    let minted = json_of(
        s.authed(Method::POST, "/v1/auth/pairing", &bob)
            .send()
            .await
            .unwrap(),
    )
    .await;
    let r = s
        .admin(
            Method::DELETE,
            &format!("pairing-codes/{}", minted["id"].as_str().unwrap()),
            Some(&c),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = s
        .http
        .post(s.url("/v1/auth/pair"))
        .json(&json!({ "code": minted["code"], "device": { "id": new_device() } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM admin_audit ORDER BY id")
        .fetch_all(&s.state.db)
        .await
        .unwrap();
    for a in [
        "user.update",
        "user.reset_password",
        "device.delete",
        "user.delete",
        "canvas.delete",
        "pairing_code.revoke",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} audited: {actions:?}");
    }
}

#[tokio::test]
async fn admin_password_change() {
    let s = start().await;
    s.create_admin().await;
    let c1 = s.admin_login().await;
    let c2 = s.admin_login().await;
    let r = s
        .admin(Method::POST, "password", Some(&c1))
        .json(&json!({ "old": "not my password", "new": "a new admin password" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(json_of(r).await["error"], "credentials");
    let r = s
        .admin(Method::POST, "password", Some(&c1))
        .json(&json!({ "old": ADMIN_PASSWORD, "new": "short" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let r = s
        .admin(Method::POST, "password", Some(&c1))
        .json(&json!({ "old": ADMIN_PASSWORD, "new": "a new admin password" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(json_of(r).await["revoked_sessions"], 1);
    assert_eq!(
        s.admin(Method::GET, "me", Some(&c1))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        s.admin(Method::GET, "me", Some(&c2))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let r = s
        .admin(Method::POST, "login", None)
        .json(&json!({ "email": ADMIN_EMAIL, "password": "a new admin password" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    // Expired sessions are refused.
    sqlx::query("UPDATE admin_sessions SET expires_at = now() - interval '1 second'")
        .execute(&s.state.db)
        .await
        .unwrap();
    assert_eq!(
        s.admin(Method::GET, "me", Some(&c1))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
}

#[tokio::test]
async fn cli_admin_accounts() {
    let s = start().await;
    let db = &s.state.db;
    let a = copper_cloud::admin_api::create_admin(db, " Root@Example.com ", ADMIN_PASSWORD.into())
        .await
        .unwrap();
    assert_eq!(a.email, "root@example.com");
    let dup = copper_cloud::admin_api::create_admin(db, "ROOT@example.com", ADMIN_PASSWORD.into())
        .await
        .unwrap_err();
    assert_eq!(dup.status(), 409);
    assert!(
        copper_cloud::admin_api::create_admin(db, "x@example.com", "short".into())
            .await
            .is_err()
    );
    let found = copper_cloud::admin_api::find_admin(db, "root@EXAMPLE.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, a.id);
    assert_eq!(
        copper_cloud::admin_api::list_admins(db)
            .await
            .unwrap()
            .len(),
        1
    );
}
