//! Cloud-wide intelligence keys: admin API (masked), `/v1/intelligence` (gate + session,
//! exact contract), encryption at rest, sharing toggle, audit.

#![allow(clippy::too_many_lines)]

mod common;

use common::*;
use reqwest::Method;
use serde_json::{json, Value};

const JEV_KEY: &str = "jev-test-key-0123456789-JEVK";
const ROUTER_KEY: &str = "sk-router-test-key-9876543210-RTRK";

async fn json_of(r: reqwest::Response) -> Value {
    r.json().await.unwrap()
}

#[tokio::test]
async fn intelligence_keys_end_to_end() {
    let s = start().await;
    s.create_admin().await;
    let c = s.admin_login().await;
    let ana = s.signup("ana@example.com", PASSWORD, &new_device()).await;

    // Gate + session: no instance key → 401 instance_key; no session → 401 session.
    let r = s
        .http
        .get(s.url("/v1/intelligence"))
        .bearer_auth(&ana.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(json_of(r).await["error"], "instance_key");
    let r = s.req(Method::GET, "/v1/intelligence").send().await.unwrap();
    assert_eq!(r.status(), 401);

    // Unset → exact null contract.
    let r = s
        .authed(Method::GET, "/v1/intelligence", &ana)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(
        json_of(r).await,
        json!({ "jev": null, "router": null, "updated_at": null })
    );

    // Admin API needs a session and the CSRF header.
    let r = s
        .admin(Method::GET, "intelligence", None)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = s
        .http
        .put(s.url("/admin/api/intelligence"))
        .header("Cookie", &c)
        .json(&json!({ "jev": { "key": JEV_KEY } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // Validation: a new block needs a key; URLs must be http(s); unknown fields rejected.
    for bad in [
        json!({ "jev": { "endpoint": "https://x.example.com" } }),
        json!({ "router": { "key": ROUTER_KEY, "url": "ftp://x" } }),
        json!({ "router": { "key": "short" } }),
        json!({ "jev": { "key": JEV_KEY, "nope": 1 } }),
        json!({ "jevv": null }),
    ] {
        let r = s
            .admin(Method::PUT, "intelligence", Some(&c))
            .json(&bad)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "{bad}");
    }

    // Set both; defaults fill endpoint/model/url. The admin only sees the last 4 chars.
    let r = s
        .admin(Method::PUT, "intelligence", Some(&c))
        .json(&json!({ "jev": { "key": JEV_KEY }, "router": { "key": ROUTER_KEY } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let text = r.text().await.unwrap();
    assert!(
        !text.contains(JEV_KEY) && !text.contains(ROUTER_KEY),
        "{text}"
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["enabled"], true);
    assert_eq!(v["jev"]["key_last4"], "JEVK");
    assert_eq!(v["jev"]["endpoint"], "https://api.typesafe.ai/v1/systemone");
    assert_eq!(v["jev"]["model"], "jev-latest");
    assert_eq!(v["router"]["key_last4"], "RTRK");
    assert_eq!(v["router"]["url"], "https://llm.example.com");
    assert_eq!(v["updated_by"], ADMIN_EMAIL);
    let r = s
        .admin(Method::GET, "intelligence", Some(&c))
        .send()
        .await
        .unwrap();
    let text = r.text().await.unwrap();
    assert!(!text.contains(JEV_KEY) && !text.contains(ROUTER_KEY));

    // Encrypted at rest: the plaintext is nowhere in the row.
    let (jev_sealed, router_sealed): (Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT jev_key_sealed, router_key_sealed FROM intelligence_settings WHERE id = 1",
    )
    .fetch_one(&s.state.db)
    .await
    .unwrap();
    for (blob, key) in [(&jev_sealed, JEV_KEY), (&router_sealed, ROUTER_KEY)] {
        assert!(!blob.windows(key.len()).any(|w| w == key.as_bytes()));
    }

    // A signed-in user gets the exact contract with the keys in clear.
    let r = s
        .authed(Method::GET, "/v1/intelligence", &ana)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v = json_of(r).await;
    let obj = v.as_object().unwrap();
    assert_eq!(obj.len(), 3, "{v}");
    assert_eq!(
        v["jev"],
        json!({ "key": JEV_KEY, "endpoint": "https://api.typesafe.ai/v1/systemone", "model": "jev-latest" })
    );
    assert_eq!(
        v["router"],
        json!({ "key": ROUTER_KEY, "url": "https://llm.example.com" })
    );
    let at = v["updated_at"].as_str().unwrap();
    assert!(
        time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339).is_ok()
    );

    // Partial update keeps the key; a URL change alone works.
    let r = s
        .admin(Method::PUT, "intelligence", Some(&c))
        .json(&json!({ "router": { "url": "https://llm.example.com/" }, "jev": { "model": "jev-2" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v = s
        .authed(Method::GET, "/v1/intelligence", &ana)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(v["router"]["key"], ROUTER_KEY);
    assert_eq!(v["router"]["url"], "https://llm.example.com");
    assert_eq!(v["jev"]["key"], JEV_KEY);
    assert_eq!(v["jev"]["model"], "jev-2");

    // Sharing off → nulls for users; keys kept for the admin.
    let r = s
        .admin(Method::PUT, "intelligence", Some(&c))
        .json(&json!({ "enabled": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(json_of(r).await["router"]["key_last4"], "RTRK");
    let v = s
        .authed(Method::GET, "/v1/intelligence", &ana)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(
        v,
        json!({ "jev": null, "router": null, "updated_at": null })
    );
    s.admin(Method::PUT, "intelligence", Some(&c))
        .json(&json!({ "enabled": true }))
        .send()
        .await
        .unwrap();

    // Clearing one block (null) leaves the other.
    let r = s
        .admin(Method::PUT, "intelligence", Some(&c))
        .json(&json!({ "jev": null }))
        .send()
        .await
        .unwrap();
    let v = json_of(r).await;
    assert_eq!(v["jev"], Value::Null);
    assert_eq!(v["router"]["key_last4"], "RTRK");
    let v = s
        .authed(Method::GET, "/v1/intelligence", &ana)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(v["jev"], Value::Null);
    assert_eq!(v["router"]["key"], ROUTER_KEY);
    assert!(v["updated_at"].is_string());

    // Audit: changes + (throttled) reads, never key material.
    let r = s
        .admin(Method::GET, "audit?limit=100", Some(&c))
        .send()
        .await
        .unwrap();
    let text = r.text().await.unwrap();
    assert!(!text.contains(JEV_KEY) && !text.contains(ROUTER_KEY));
    let audit: Value = serde_json::from_str(&text).unwrap();
    let actions: Vec<&str> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions
            .iter()
            .filter(|a| **a == "intelligence.update")
            .count(),
        5,
        "{actions:?}"
    );
    assert_eq!(
        actions
            .iter()
            .filter(|a| **a == "intelligence.read")
            .count(),
        1,
        "reads are audited once per user per hour: {actions:?}"
    );

    // DELETE clears everything.
    let r = s
        .admin(Method::DELETE, "intelligence", Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v = json_of(r).await;
    assert_eq!(v["jev"], Value::Null);
    assert_eq!(v["router"], Value::Null);
    assert_eq!(v["updated_at"], Value::Null);
    let v = s
        .authed(Method::GET, "/v1/intelligence", &ana)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(
        v,
        json!({ "jev": null, "router": null, "updated_at": null })
    );
}

#[tokio::test]
async fn intelligence_works_with_access_keys_and_survives_wrong_master_key() {
    let s = start().await;
    s.set_access_mode(copper_cloud_core::access::AccessMode::Directory)
        .await;
    let (_, key) = s.mint_key("Bo", None, None).await;
    let r = s.signup_with(&key, "bo@example.com", &new_device()).await;
    assert_eq!(r.status(), 200);
    let bo = session_from(&r.json().await.unwrap());

    copper_cloud_core::intelligence::apply(
        &s.state.db,
        &s.state.crypto,
        &copper_cloud_core::intelligence::Actor::Cli,
        &copper_cloud_core::intelligence::Update {
            router: copper_cloud_core::intelligence::Change::Set(
                copper_cloud_core::intelligence::RouterInput {
                    key: Some(ROUTER_KEY.into()),
                    url: None,
                },
            ),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let v: Value = s
        .gated(Method::GET, "/v1/intelligence", &key)
        .bearer_auth(&bo.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["jev"], Value::Null);
    assert_eq!(v["router"]["key"], ROUTER_KEY);
    // The instance key is refused in directory mode.
    let r = s
        .authed(Method::GET, "/v1/intelligence", &bo)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    // Another master key cannot open the stored keys (fails closed, no plaintext).
    let other = copper_cloud_core::crypto::Crypto::from_master_key(&[7u8; 32]);
    assert!(copper_cloud_core::intelligence::load(&s.state.db, &other)
        .await
        .is_err());

    // CLI-authored change is audited without an admin.
    let (admin_id, detail): (Option<uuid::Uuid>, Value) = sqlx::query_as(
        "SELECT admin_id, detail FROM admin_audit WHERE action = 'intelligence.update'",
    )
    .fetch_one(&s.state.db)
    .await
    .unwrap();
    assert!(admin_id.is_none());
    assert_eq!(detail["via"], "cli");
    assert!(!detail.to_string().contains(ROUTER_KEY));
}
