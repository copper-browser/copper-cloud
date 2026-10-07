//! Cloud-wide intelligence keys: admin API (masked), `/v1/intelligence` (gate + session,
//! exact contract), encryption at rest, sharing toggle, audit.

#![allow(clippy::too_many_lines)]

mod common;

use common::*;
use reqwest::Method;
use serde_json::{json, Value};

const JEV_KEY: &str = "jev-test-key-0123456789-JEVK";
const ROUTER_KEY: &str = "sk-router-test-key-9876543210-RTRK";
const ROUTER_URL: &str = "https://router.example.com";

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
        json!({ "jev": null, "router": null, "updated_at": null, "agent": null })
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

    // Validation: a new block needs a key (and a router a URL: there is no built-in default);
    // URLs must be http(s); unknown fields rejected.
    for bad in [
        json!({ "jev": { "endpoint": "https://x.example.com" } }),
        json!({ "router": { "key": ROUTER_KEY } }),
        json!({ "router": { "key": ROUTER_KEY, "url": "  " } }),
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

    // Set both; defaults fill the Jev endpoint/model, the router URL is explicit. The admin
    // only sees the last 4 chars.
    let r = s
        .admin(Method::PUT, "intelligence", Some(&c))
        .json(&json!({
            "jev": { "key": JEV_KEY },
            "router": { "key": ROUTER_KEY, "url": ROUTER_URL },
        }))
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
    assert_eq!(v["router"]["url"], ROUTER_URL);
    assert_eq!(v["updated_by"], ADMIN_EMAIL);
    assert!(v["defaults"].get("router_url").is_none(), "{v}");
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
    assert_eq!(obj.len(), 4, "{v}");
    assert_eq!(v["agent"], Value::Null);
    assert_eq!(
        v["jev"],
        json!({ "key": JEV_KEY, "endpoint": "https://api.typesafe.ai/v1/systemone", "model": "jev-latest" })
    );
    assert_eq!(v["router"], json!({ "key": ROUTER_KEY, "url": ROUTER_URL }));
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
        json!({ "jev": null, "router": null, "updated_at": null, "agent": null })
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
        json!({ "jev": null, "router": null, "updated_at": null, "agent": null })
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
                    url: Some(ROUTER_URL.into()),
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

#[tokio::test]
async fn agent_max_turns_is_org_wide_and_independent_of_keys() {
    let s = start().await;
    s.create_admin().await;
    let c = s.admin_login().await;
    let ana = s.signup("ana@example.com", PASSWORD, &new_device()).await;
    let user_view = || async {
        s.authed(Method::GET, "/v1/intelligence", &ana)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()
    };
    let put = |body: Value| {
        let req = s.admin(Method::PUT, "intelligence", Some(&c)).json(&body);
        async move { req.send().await.unwrap() }
    };

    // Unset: null for users, null + the client default for the admin.
    assert_eq!(user_view().await["agent"], Value::Null);
    let v = json_of(
        s.admin(Method::GET, "intelligence", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(v["agent"], Value::Null);
    assert_eq!(v["defaults"]["agent_max_turns"], 60);

    // Validation: integers 1..=500 in an object; nothing is stored on a 400.
    for bad in [
        json!({ "agent": { "max_turns": 0 } }),
        json!({ "agent": { "max_turns": 501 } }),
        json!({ "agent": { "max_turns": -1 } }),
        json!({ "agent": { "max_turns": 1.5 } }),
        json!({ "agent": { "max_turns": "100" } }),
        json!({ "agent": { "max_turns": 100_000_000_000_i64 } }),
        json!({ "agent": { "max_turns": null } }),
        json!({ "agent": {} }),
        json!({ "agent": { "max_turns": 100, "extra": 1 } }),
        json!({ "agent": 100 }),
    ] {
        let r = put(bad.clone()).await;
        assert_eq!(r.status(), 400, "{bad}");
        assert!(json_of(r).await["message"].is_string(), "{bad}");
    }
    assert_eq!(user_view().await["agent"], Value::Null);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM intelligence_settings")
        .fetch_one(&s.state.db)
        .await
        .unwrap();
    assert_eq!(rows, 0);

    // Bounds are inclusive. With no keys at all the budget is still served.
    for n in [1, 500, 100] {
        let r = put(json!({ "agent": { "max_turns": n } })).await;
        assert_eq!(r.status(), 200);
        let v = json_of(r).await;
        assert_eq!(v["agent"], json!({ "max_turns": n }));
        assert_eq!(v["jev"], Value::Null);
    }
    assert_eq!(
        user_view().await,
        json!({ "jev": null, "router": null, "updated_at": null, "agent": { "max_turns": 100 } })
    );

    // Keys set alongside; an absent "agent" keeps the budget.
    let r = put(json!({ "router": { "key": ROUTER_KEY, "url": ROUTER_URL } })).await;
    assert_eq!(json_of(r).await["agent"]["max_turns"], 100);
    let v = user_view().await;
    assert_eq!(v["router"]["key"], ROUTER_KEY);
    assert_eq!(v["agent"], json!({ "max_turns": 100 }));

    // Sharing off withholds the keys but not the budget.
    put(json!({ "enabled": false })).await;
    assert_eq!(
        user_view().await,
        json!({ "jev": null, "router": null, "updated_at": null, "agent": { "max_turns": 100 } })
    );
    put(json!({ "enabled": true })).await;

    // DELETE clears the keys only: the budget is not a key.
    let v = json_of(
        s.admin(Method::DELETE, "intelligence", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(v["router"], Value::Null);
    assert_eq!(v["agent"], json!({ "max_turns": 100 }));
    assert_eq!(
        user_view().await,
        json!({ "jev": null, "router": null, "updated_at": null, "agent": { "max_turns": 100 } })
    );

    // The database refuses out-of-range values too.
    assert!(
        sqlx::query("UPDATE intelligence_settings SET agent_max_turns = 501 WHERE id = 1")
            .execute(&s.state.db)
            .await
            .is_err()
    );

    // `null` clears; with nothing else stored the row goes away.
    let v = json_of(put(json!({ "agent": null })).await).await;
    assert_eq!(v["agent"], Value::Null);
    assert_eq!(v["updated_at"], Value::Null);
    assert_eq!(user_view().await["agent"], Value::Null);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM intelligence_settings")
        .fetch_one(&s.state.db)
        .await
        .unwrap();
    assert_eq!(rows, 0);

    // Audited like the keys: set and clear, with the admin and the value.
    let details: Vec<(Option<uuid::Uuid>, Value)> = sqlx::query_as(
        "SELECT admin_id, detail FROM admin_audit
         WHERE action = 'intelligence.update' AND detail ? 'agent' ORDER BY at, id",
    )
    .fetch_all(&s.state.db)
    .await
    .unwrap();
    let agents: Vec<&Value> = details.iter().map(|(_, d)| &d["agent"]).collect();
    assert_eq!(
        agents,
        [
            &json!({ "max_turns": 1 }),
            &json!({ "max_turns": 500 }),
            &json!({ "max_turns": 100 }),
            &Value::Null,
        ]
    );
    assert!(details
        .iter()
        .all(|(a, d)| a.is_some() && d["via"] == "admin_api"));

    // CLI: `intelligence set --agent-max-turns` / `clear --agent`, audited as the CLI.
    let cfg = (*s.state.cfg).clone();
    copper_cloud::admin::intelligence(
        cfg.clone(),
        copper_cloud::cli::IntelligenceCommand::Set(copper_cloud::cli::IntelligenceSetArgs {
            agent_max_turns: Some(250),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(user_view().await["agent"], json!({ "max_turns": 250 }));
    copper_cloud::admin::intelligence(cfg.clone(), copper_cloud::cli::IntelligenceCommand::Show)
        .await
        .unwrap();
    // A plain `clear` keeps the budget; `clear --agent` drops it.
    for (agent, expect) in [(false, json!({ "max_turns": 250 })), (true, Value::Null)] {
        copper_cloud::admin::intelligence(
            cfg.clone(),
            copper_cloud::cli::IntelligenceCommand::Clear {
                jev: false,
                router: false,
                agent,
            },
        )
        .await
        .unwrap();
        assert_eq!(user_view().await["agent"], expect);
    }
    // Out of range through the library path (clap already refuses it on argv).
    assert!(copper_cloud::admin::intelligence(
        cfg,
        copper_cloud::cli::IntelligenceCommand::Set(copper_cloud::cli::IntelligenceSetArgs {
            agent_max_turns: Some(0),
            ..Default::default()
        }),
    )
    .await
    .is_err());
    let cli_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM admin_audit
         WHERE action = 'intelligence.update' AND detail ? 'agent' AND detail->>'via' = 'cli'",
    )
    .fetch_one(&s.state.db)
    .await
    .unwrap();
    assert_eq!(cli_rows, 2);
}

/// Upgrades must not touch a router URL an instance already stores (including one that used
/// to come from a built-in default): rotating the key or toggling sharing keeps it, and a new
/// router block without a URL is refused instead of silently pointing at someone's gateway.
#[tokio::test]
async fn configured_router_url_is_preserved_and_never_defaulted() {
    use copper_cloud_core::intelligence::{self as intel, Actor, Change, RouterInput, Update};
    let s = start().await;
    let ana = s.signup("ana@example.com", PASSWORD, &new_device()).await;
    let set_router = |key: Option<&str>, url: Option<&str>| Update {
        router: Change::Set(RouterInput {
            key: key.map(str::to_owned),
            url: url.map(str::to_owned),
        }),
        ..Default::default()
    };

    // Nothing stored + no URL → 400, nothing written.
    let Err(err) = intel::apply(
        &s.state.db,
        &s.state.crypto,
        &Actor::Cli,
        &set_router(Some(ROUTER_KEY), None),
    )
    .await
    else {
        panic!("a router key without a URL is refused");
    };
    assert_eq!(err.status(), 400);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM intelligence_settings")
        .fetch_one(&s.state.db)
        .await
        .unwrap();
    assert_eq!(rows, 0);

    // An instance configured with its own gateway (as any pre-0.8.1 instance is: the URL lives
    // in the row, not in the binary).
    let configured = "https://gateway.example.org";
    intel::apply(
        &s.state.db,
        &s.state.crypto,
        &Actor::Cli,
        &set_router(Some(ROUTER_KEY), Some(configured)),
    )
    .await
    .unwrap();

    // Key rotation with no URL keeps the stored one; so does a blank URL and a sharing toggle.
    let rotated = "sk-router-rotated-key-1111111111-ROT2";
    for update in [
        set_router(Some(rotated), None),
        set_router(None, Some("")),
        Update {
            enabled: Some(false),
            ..Default::default()
        },
        Update {
            enabled: Some(true),
            ..Default::default()
        },
    ] {
        let got = intel::apply(&s.state.db, &s.state.crypto, &Actor::Cli, &update)
            .await
            .unwrap();
        assert_eq!(got.router.as_ref().unwrap().url, configured);
    }
    let v: Value = s
        .authed(Method::GET, "/v1/intelligence", &ana)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["router"], json!({ "key": rotated, "url": configured }));
}
