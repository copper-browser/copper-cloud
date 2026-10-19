//! Addons end to end on real Postgres: admin API (upload rules, PATCH, delete, audit, guard),
//! per-person resolution and `GET /v1/addons` (`ETag` / 304), bundle authorization, the SSE
//! `addons` event, and the `copper-cloud addons …` CLI. Zip fixtures are built in-test.

#![allow(clippy::too_many_lines)]

mod common;

use std::io::Write as _;
use std::time::Duration;

use common::*;
use copper_cloud::admin_addons_cli::{
    AddonIdArg, AddonsCommand, AddonsEnableArgs, AddonsSettingsArgs, AddonsUploadArgs,
};
use futures::StreamExt as _;
use reqwest::Method;
use serde_json::{json, Value};

const ID: &str = "com.example.notes";
const OTHER: &str = "com.example.other";
/// No addons resolved.
const NONE: [&str; 0] = [];

fn manifest(id: &str, version: &str) -> Value {
    json!({
        "schema": "copper-addon/1",
        "id": id,
        "name": format!("Addon {id}"),
        "version": version,
        "slot": "agent-sidebar",
        "entry": "index.html",
        "description": "Test addon.",
        "author": "Example",
        "bridge": 1,
        "permissions": {
            "hosts": ["api.example.com", "*.example.org"],
            "capabilities": ["identity", "storage", "fetch"]
        },
        "settings": {
            "apiUrl": { "type": "string", "default": "https://api.example.com", "title": "API" },
            "limit": { "type": "number", "default": 5 },
            "beta": { "type": "boolean" }
        },
        "platforms": ["mac", "linux"],
        "x-vendor": { "kept": true }
    })
}

/// A `.copperaddon` zip: `manifest.json` + the given files.
fn zip_with(manifest: &Value, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    w.start_file("manifest.json", opts).unwrap();
    w.write_all(&serde_json::to_vec_pretty(manifest).unwrap())
        .unwrap();
    for (name, data) in files {
        w.start_file(*name, opts).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn addon_zip(id: &str, version: &str, body: &str) -> Vec<u8> {
    zip_with(
        &manifest(id, version),
        &[
            (
                "index.html",
                format!("<!doctype html><p>{body}</p>").as_bytes(),
            ),
            ("app.js", b"copper.ready();"),
        ],
    )
}

async fn json_of(r: reqwest::Response) -> Value {
    r.json().await.unwrap()
}

/// `POST /admin/api/addons?slot=<slot>` with a raw zip body.
async fn upload(s: &TestServer, cookie: &str, slot: &str, zip: Vec<u8>) -> reqwest::Response {
    s.admin(Method::POST, &format!("addons?slot={slot}"), Some(cookie))
        .header("Content-Type", "application/zip")
        .body(zip)
        .send()
        .await
        .unwrap()
}

async fn patch(s: &TestServer, cookie: &str, id: &str, body: Value) -> reqwest::Response {
    s.admin(Method::PATCH, &format!("addons/{id}"), Some(cookie))
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn client_list(s: &TestServer, who: &Session) -> Value {
    let r = s
        .authed(Method::GET, "/v1/addons", who)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    json_of(r).await
}

fn ids(list: &Value) -> Vec<String> {
    list["addons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_owned())
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    copper_cloud_core::ids::sha256_hex(bytes)
}

#[tokio::test]
async fn admin_upload_rules_patch_delete_and_audit() {
    let s = start_with(|cfg| cfg.limits.max_addon_bytes = 256 * 1024).await;
    s.create_admin().await;
    let c = s.admin_login().await;

    // Slots.
    let r = s
        .admin(Method::GET, "addons/slots", Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let slots = json_of(r).await;
    assert_eq!(slots[0]["id"], "agent-sidebar");
    assert_eq!(slots[0]["name"], "Agent sidebar");
    assert!(slots[0]["description"].as_str().unwrap().contains("Ctrl+E"));
    assert_eq!(slots.as_array().unwrap().len(), 1);

    // Empty list is a plain array.
    let r = s
        .admin(Method::GET, "addons", Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(json_of(r).await, json!([]));

    // Refusals.
    let good = addon_zip(ID, "1.0.0", "v1");
    let r = s
        .admin(Method::POST, "addons", Some(&c))
        .header("Content-Type", "application/zip")
        .body(good.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400, "?slot= is required");
    assert!(json_of(r).await["message"]
        .as_str()
        .unwrap()
        .contains("slot"));
    let r = upload(&s, &c, "toolbar", good.clone()).await;
    assert_eq!(r.status(), 400);
    assert!(json_of(r).await["message"]
        .as_str()
        .unwrap()
        .contains("unknown slot"));
    let r = s
        .admin(Method::POST, "addons?slot=agent-sidebar", Some(&c))
        .header("Content-Type", "application/json")
        .body(good.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 415);
    assert_eq!(json_of(r).await["error"], "unsupported_media_type");
    let mut toolbar = manifest(ID, "1.0.0");
    toolbar["slot"] = json!("toolbar");
    let mut bad_id = manifest(ID, "1.0.0");
    bad_id["id"] = json!("nodot");
    let mut bad_entry = manifest(ID, "1.0.0");
    bad_entry["entry"] = json!("missing.html");
    let html: &[u8] = b"<p>x</p>";
    for (what, zip) in [
        (
            "manifest slot unknown",
            zip_with(&toolbar, &[("index.html", html)]),
        ),
        ("bad id", zip_with(&bad_id, &[("index.html", html)])),
        (
            "missing entry",
            zip_with(&bad_entry, &[("index.html", html)]),
        ),
        (
            "traversal",
            zip_with(
                &manifest(ID, "1.0.0"),
                &[("index.html", html), ("../escape.js", html)],
            ),
        ),
        ("not a zip", b"definitely not a zip".to_vec()),
    ] {
        let r = upload(&s, &c, "agent-sidebar", zip).await;
        assert_eq!(r.status(), 400, "{what}");
        assert_eq!(json_of(r).await["error"], "bad_request", "{what}");
    }
    // Oversize (incompressible, over limits.max_addon_bytes): 413 before the zip is read.
    let mut seed = 0x9e37_79b9_u32;
    let noise: Vec<u8> = (0..300_000)
        .map(|_| {
            // xorshift32: incompressible enough for deflate.
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed.to_le_bytes()[0]
        })
        .collect();
    let r = upload(
        &s,
        &c,
        "agent-sidebar",
        zip_with(
            &manifest(ID, "1.0.0"),
            &[("index.html", html), ("big.bin", &noise)],
        ),
    )
    .await;
    assert_eq!(r.status(), 413);
    let nothing: i64 = sqlx::query_scalar("SELECT count(*) FROM addons")
        .fetch_one(&s.state.db)
        .await
        .unwrap();
    assert_eq!(nothing, 0, "refused uploads store nothing");

    // Create: off, audience people with nobody.
    let r = upload(&s, &c, "agent-sidebar", good.clone()).await;
    assert_eq!(r.status(), 201);
    let a = json_of(r).await;
    assert_eq!(a["upload"], "created");
    assert_eq!(a["id"], ID);
    assert_eq!(a["slot"], "agent-sidebar");
    assert_eq!(a["name"], format!("Addon {ID}"));
    assert_eq!(a["enabled"], false);
    assert_eq!(a["audience"], "people");
    assert_eq!(a["people"], json!([]));
    assert_eq!(a["settings"], json!({}));
    assert_eq!(a["priority"], 0);
    assert_eq!(a["revision"], 1);
    assert_eq!(a["manifest"], manifest(ID, "1.0.0"));
    assert_eq!(a["current"]["version"], "1.0.0");
    assert_eq!(a["current"]["uploaded_by"], ADMIN_EMAIL);
    assert_eq!(a["current"]["current"], true);
    assert_eq!(a["versions"].as_array().unwrap().len(), 1);
    let v1_sha = a["current"]["sha256"].as_str().unwrap().to_owned();

    // The stored bundle is the canonical served document; its sha256 is the bytes' digest.
    let r = s
        .admin(
            Method::GET,
            &format!("addons/{ID}/versions/1.0.0/bundle"),
            Some(&c),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "application/json");
    assert_eq!(r.headers()["cache-control"], "no-store");
    let bytes = r.bytes().await.unwrap();
    assert_eq!(sha256_hex(&bytes), v1_sha);
    assert_eq!(a["current"]["size"], bytes.len());
    let doc: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(doc["format"], "copper-addon-bundle/1");
    assert_eq!(doc["manifest"]["x-vendor"]["kept"], true);
    let paths: Vec<&str> = doc["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["app.js", "index.html", "manifest.json"]);

    // Same version, same bytes: idempotent. Same version, other bytes: 409 version_exists.
    let r = upload(&s, &c, "agent-sidebar", good.clone()).await;
    assert_eq!(r.status(), 200);
    let same = json_of(r).await;
    assert_eq!(same["upload"], "unchanged");
    assert_eq!(same["revision"], 1);
    let r = upload(&s, &c, "agent-sidebar", addon_zip(ID, "1.0.0", "changed")).await;
    assert_eq!(r.status(), 409);
    let conflict = json_of(r).await;
    assert_eq!(conflict["error"], "version_exists");
    assert_eq!(conflict["version"], "1.0.0");
    assert_eq!(conflict["sha256"], v1_sha.as_str());

    // New version becomes current.
    let r = upload(&s, &c, "agent-sidebar", addon_zip(ID, "1.1.0", "v1.1")).await;
    assert_eq!(r.status(), 200);
    let a = json_of(r).await;
    assert_eq!(a["upload"], "new_version");
    assert_eq!(a["current"]["version"], "1.1.0");
    assert_eq!(a["revision"], 2);
    let versions: Vec<(&str, bool)> = a["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["version"].as_str().unwrap(),
                v["current"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(versions, [("1.1.0", true), ("1.0.0", false)]);

    // PATCH: validation.
    for (body, what) in [
        (json!({}), "empty"),
        (json!({ "slot": "x" }), "unknown field"),
        (json!({ "audience": "some" }), "bad audience"),
        (json!({ "priority": 5000 }), "priority range"),
        (json!({ "people": ["not an email"] }), "bad email"),
        (json!({ "settings": { "nope": 1 } }), "unknown setting"),
        (json!({ "settings": { "limit": "five" } }), "setting type"),
        (json!({ "settings": [1] }), "settings not an object"),
        (json!({ "current_version": "9.9.9" }), "unknown version"),
    ] {
        let r = patch(&s, &c, ID, body).await;
        assert_eq!(r.status(), 400, "{what}");
    }
    let r = patch(&s, &c, "com.example.missing", json!({ "enabled": true })).await;
    assert_eq!(r.status(), 404);
    let r = patch(&s, &c, "NOT-AN-ID", json!({ "enabled": true })).await;
    assert_eq!(r.status(), 404);

    // PATCH: everything at once.
    let r = patch(
        &s,
        &c,
        ID,
        json!({
            "enabled": true,
            "audience": "people",
            "people": ["Ana@Example.com", "ana@example.com", "later@example.com"],
            "settings": { "limit": 9, "beta": true, "apiUrl": null },
            "priority": 3,
        }),
    )
    .await;
    assert_eq!(r.status(), 200);
    let a = json_of(r).await;
    assert_eq!(a["enabled"], true);
    assert_eq!(
        a["people"],
        json!([
            { "id": null, "email": "ana@example.com", "name": null },
            { "id": null, "email": "later@example.com", "name": null },
        ])
    );
    assert_eq!(a["settings"], json!({ "limit": 9, "beta": true }));
    assert_eq!(a["priority"], 3);
    assert_eq!(a["revision"], 3);
    assert_eq!(a["updated_by"], ADMIN_EMAIL);

    // Rollback to 1.0.0, then GET one / list.
    let r = patch(&s, &c, ID, json!({ "current_version": "1.0.0" })).await;
    assert_eq!(r.status(), 200);
    assert_eq!(json_of(r).await["current"]["sha256"], v1_sha.as_str());
    let r = s
        .admin(Method::GET, &format!("addons/{ID}"), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let one = json_of(r).await;
    assert_eq!(one["current"]["version"], "1.0.0");
    let r = s
        .admin(Method::GET, "addons", Some(&c))
        .send()
        .await
        .unwrap();
    let list = json_of(r).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0], one);
    let r = s
        .admin(Method::GET, "addons/com.example.missing", Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = s
        .admin(
            Method::GET,
            &format!("addons/{ID}/versions/7.0.0/bundle"),
            Some(&c),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    // Delete.
    let r = s
        .admin(Method::DELETE, &format!("addons/{ID}"), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(json_of(r).await, json!({ "ok": true }));
    let r = s
        .admin(Method::DELETE, &format!("addons/{ID}"), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let left: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM addon_versions), (SELECT count(*) FROM addon_audience)",
    )
    .fetch_one(&s.state.db)
    .await
    .unwrap();
    assert_eq!(left, (0, 0), "versions and audience cascade");

    // Audit: one row per change, by the admin, via the admin API; settings values never logged.
    let rows: Vec<(String, String, bool, Value)> = sqlx::query_as(
        "SELECT action, target, admin_id IS NOT NULL, detail FROM admin_audit
         WHERE action LIKE 'addon.%' ORDER BY id",
    )
    .fetch_all(&s.state.db)
    .await
    .unwrap();
    let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        actions,
        [
            "addon.create",
            "addon.upload",
            "addon.update",
            "addon.update",
            "addon.delete"
        ]
    );
    assert!(rows
        .iter()
        .all(|(_, target, admin, d)| target == ID && *admin && d["via"] == "admin_api"));
    assert_eq!(rows[0].3["version"], "1.0.0");
    assert_eq!(rows[0].3["sha256"], v1_sha.as_str());
    assert_eq!(rows[2].3["settings"], json!(["beta", "limit"]));
    assert_eq!(rows[2].3["people"], 2);
    assert_eq!(rows[3].3["current_version"], "1.0.0");
    assert_eq!(rows[4].3["versions"], 2);

    // /v1/info advertises the feature.
    let info = json_of(s.req(Method::GET, "/v1/info").send().await.unwrap()).await;
    assert!(info["features"]
        .as_array()
        .unwrap()
        .contains(&json!("addons")));
}

#[tokio::test]
async fn admin_addon_routes_require_session_and_csrf() {
    let s = start().await;
    s.create_admin().await;
    let c = s.admin_login().await;
    let routes = [
        (Method::GET, "addons/slots"),
        (Method::GET, "addons"),
        (Method::POST, "addons?slot=agent-sidebar"),
        (Method::GET, "addons/com.example.x"),
        (Method::PATCH, "addons/com.example.x"),
        (Method::DELETE, "addons/com.example.x"),
        (Method::GET, "addons/com.example.x/versions/1.0.0/bundle"),
    ];
    for (method, path) in &routes {
        // No cookie (CSRF header present): 401.
        let r = s
            .admin(method.clone(), path, None)
            .header("Content-Type", "application/zip")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401, "{method} {path}");
        // Cookie, no CSRF header: unsafe methods refused before the handler.
        let r = s
            .http
            .request(method.clone(), s.url(&format!("/admin/api/{path}")))
            .header("Cookie", &c)
            .header("Content-Type", "application/json")
            .body("{\"enabled\":true}")
            .send()
            .await
            .unwrap();
        if *method == Method::GET {
            assert_ne!(r.status(), 401, "{method} {path}");
            assert_ne!(r.status(), 403, "{method} {path}");
        } else {
            assert_eq!(r.status(), 403, "{method} {path}");
            assert_eq!(json_of(r).await["error"], "csrf", "{method} {path}");
        }
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM admin_audit WHERE action LIKE 'addon.%'")
        .fetch_one(&s.state.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn resolution_etag_and_bundle_authz() {
    let s = start().await;
    s.create_admin().await;
    let c = s.admin_login().await;
    let ana = s.signup("Ana@Example.com", PASSWORD, &new_device()).await;
    let bob = s.signup("bob@example.com", PASSWORD, &new_device()).await;

    // Session required.
    let r = s.req(Method::GET, "/v1/addons").send().await.unwrap();
    assert_eq!(r.status(), 401);

    // Nothing yet: empty list with a revision and ETag.
    let r = s
        .authed(Method::GET, "/v1/addons", &ana)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "no-store");
    let etag = r.headers()["etag"].to_str().unwrap().to_owned();
    let empty = json_of(r).await;
    assert_eq!(empty["addons"], json!([]));
    let rev = empty["revision"].as_str().unwrap().to_owned();
    assert!(rev.starts_with("r_") && rev.len() == 18, "{rev}");
    assert_eq!(etag, format!("\"{rev}\""));

    // Upload: off by default, nobody gets it.
    assert_eq!(
        upload(&s, &c, "agent-sidebar", addon_zip(ID, "1.0.0", "a"))
            .await
            .status(),
        201
    );
    assert_eq!(client_list(&s, &ana).await["revision"], rev.as_str());
    // On, audience people with nobody: still nobody.
    assert_eq!(
        patch(&s, &c, ID, json!({ "enabled": true })).await.status(),
        200
    );
    assert_eq!(ids(&client_list(&s, &ana).await), NONE);

    // Everyone.
    let r = patch(
        &s,
        &c,
        ID,
        json!({ "audience": "everyone", "settings": { "limit": 7 } }),
    )
    .await;
    assert_eq!(r.status(), 200);
    let sha_a = json_of(r).await["current"]["sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let list = client_list(&s, &ana).await;
    assert_eq!(ids(&list), [ID]);
    let entry = &list["addons"][0];
    let updated_at = entry["updated_at"].as_str().unwrap().to_owned();
    assert!(updated_at.contains('T'), "{updated_at}");
    let size = entry["size"].as_i64().unwrap();
    assert!(size > 0);
    assert_eq!(
        *entry,
        json!({
            "id": ID,
            "slot": "agent-sidebar",
            "name": format!("Addon {ID}"),
            "version": "1.0.0",
            "sha256": sha_a,
            "size": size,
            "entry": "index.html",
            "bridge": 1,
            "platforms": ["mac", "linux"],
            "permissions": {
                "hosts": ["api.example.com", "*.example.org"],
                "capabilities": ["identity", "storage", "fetch"]
            },
            "settings": { "apiUrl": "https://api.example.com", "limit": 7 },
            "bundle": format!("/v1/addons/{ID}/bundle/{sha_a}"),
            "updated_at": updated_at,
        })
    );
    assert_eq!(ids(&client_list(&s, &bob).await), [ID]);

    // ETag / If-None-Match: quoted, bare revision, stale.
    let rev = list["revision"].as_str().unwrap().to_owned();
    for inm in [format!("\"{rev}\""), rev.clone(), format!("W/\"{rev}\"")] {
        let r = s
            .authed(Method::GET, "/v1/addons", &ana)
            .header("If-None-Match", &inm)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 304, "{inm}");
        assert_eq!(r.headers()["etag"].to_str().unwrap(), format!("\"{rev}\""));
        assert!(r.bytes().await.unwrap().is_empty());
    }
    let r = s
        .authed(Method::GET, "/v1/addons", &ana)
        .header("If-None-Match", "\"r_0000000000000000\"")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    // Bundle: served while resolved, bytes match the advertised sha256, immutable.
    let r = s
        .authed(
            Method::GET,
            &format!("/v1/addons/{ID}/bundle/{sha_a}"),
            &ana,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "application/json");
    assert_eq!(
        r.headers()["cache-control"],
        "private, max-age=31536000, immutable"
    );
    let bytes = r.bytes().await.unwrap();
    assert_eq!(sha256_hex(&bytes), sha_a);
    assert_eq!(i64::try_from(bytes.len()).unwrap(), size);
    for bad in [
        format!("/v1/addons/{ID}/bundle/{}", "0".repeat(64)),
        format!("/v1/addons/{ID}/bundle/nothex"),
        format!("/v1/addons/com.example.none/bundle/{sha_a}"),
    ] {
        let r = s.authed(Method::GET, &bad, &ana).send().await.unwrap();
        assert_eq!(r.status(), 404, "{bad}");
    }
    let r = s
        .http
        .get(s.url(&format!("/v1/addons/{ID}/bundle/{sha_a}")))
        .header("X-Copper-Instance", s.key())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "bundle needs a session");

    // A second addon for listed people, higher priority: one addon per slot.
    assert_eq!(
        upload(&s, &c, "agent-sidebar", addon_zip(OTHER, "2.0.0", "b"))
            .await
            .status(),
        201
    );
    let r = patch(
        &s,
        &c,
        OTHER,
        json!({ "enabled": true, "people": ["ANA@example.COM", "carol@example.com"], "priority": 10 }),
    )
    .await;
    assert_eq!(r.status(), 200);
    let sha_b = json_of(r).await["current"]["sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        ids(&client_list(&s, &ana).await),
        [OTHER],
        "listed (any case), priority wins"
    );
    assert_eq!(ids(&client_list(&s, &bob).await), [ID], "not listed");
    // Bundle authz follows resolution.
    let r = s
        .authed(
            Method::GET,
            &format!("/v1/addons/{OTHER}/bundle/{sha_b}"),
            &bob,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404, "not resolved for bob");
    let r = s
        .authed(
            Method::GET,
            &format!("/v1/addons/{ID}/bundle/{sha_a}"),
            &ana,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        404,
        "ana's slot now resolves to the other addon"
    );

    // Someone listed before signing up gets it once they do.
    let carol = s.signup("carol@example.com", PASSWORD, &new_device()).await;
    assert_eq!(ids(&client_list(&s, &carol).await), [OTHER]);
    let people = json_of(
        s.admin(Method::GET, &format!("addons/{OTHER}"), Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await["people"]
        .clone();
    assert_eq!(people[0]["email"], "ana@example.com");
    assert_eq!(people[0]["id"], ana.user_id.as_str());
    assert_eq!(people[1]["email"], "carol@example.com");
    assert_eq!(people[1]["id"], carol.user_id.as_str());

    // Equal priority: the most recently changed addon wins.
    assert_eq!(
        patch(&s, &c, OTHER, json!({ "priority": 0 }))
            .await
            .status(),
        200
    );
    assert_eq!(ids(&client_list(&s, &ana).await), [OTHER]);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        patch(&s, &c, ID, json!({ "settings": { "limit": 8 } }))
            .await
            .status(),
        200
    );
    assert_eq!(ids(&client_list(&s, &ana).await), [ID]);

    // Disabled: never listed.
    assert_eq!(
        patch(&s, &c, ID, json!({ "enabled": false }))
            .await
            .status(),
        200
    );
    assert_eq!(ids(&client_list(&s, &ana).await), [OTHER]);
    assert_eq!(ids(&client_list(&s, &bob).await), NONE);

    // A new version replaces the old one; the old bundle is no longer served.
    let r = upload(&s, &c, "agent-sidebar", addon_zip(OTHER, "2.1.0", "b2")).await;
    assert_eq!(r.status(), 200);
    let list = client_list(&s, &ana).await;
    assert_eq!(list["addons"][0]["version"], "2.1.0");
    let r = s
        .authed(
            Method::GET,
            &format!("/v1/addons/{OTHER}/bundle/{sha_b}"),
            &ana,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
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
async fn addons_event_reaches_every_connected_user() {
    let s = start().await;
    s.create_admin().await;
    let c = s.admin_login().await;
    let ana = s.signup("ana@example.com", PASSWORD, &new_device()).await;
    let bob = s.signup("bob@example.com", PASSWORD, &new_device()).await;

    let mut streams = Vec::new();
    for who in [&ana, &bob] {
        let resp = s
            .authed(Method::GET, "/v1/sync/events", who)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let mut stream = Box::pin(resp.bytes_stream());
        let mut buf = String::new();
        next_event(&mut stream, &mut buf, "ready").await;
        streams.push((stream, buf));
    }

    // Upload (even of a disabled addon) and every PATCH notify everyone.
    assert_eq!(
        upload(&s, &c, "agent-sidebar", addon_zip(ID, "1.0.0", "a"))
            .await
            .status(),
        201
    );
    for (stream, buf) in &mut streams {
        assert_eq!(
            next_event(stream, buf, "addons").await,
            json!({ "type": "addons" })
        );
    }
    assert_eq!(
        patch(
            &s,
            &c,
            ID,
            json!({ "enabled": true, "audience": "everyone" })
        )
        .await
        .status(),
        200
    );
    for (stream, buf) in &mut streams {
        next_event(stream, buf, "addons").await;
    }
    let r = s
        .admin(Method::DELETE, &format!("addons/{ID}"), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    for (stream, buf) in &mut streams {
        next_event(stream, buf, "addons").await;
    }
}

fn id_arg(id: &str) -> AddonIdArg {
    AddonIdArg {
        id: Some(id.to_owned()),
        id_flag: None,
    }
}

#[tokio::test]
async fn cli_round_trip() {
    let s = start().await;
    let ana = s.signup("ana@example.com", PASSWORD, &new_device()).await;
    let bob = s.signup("bob@example.com", PASSWORD, &new_device()).await;
    let cfg = (*s.state.cfg).clone();
    let dir = std::env::temp_dir().join(format!("copper-addons-cli-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("notes.copperaddon");
    // Single top-level folder layout (stripped).
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    w.add_directory("notes/", opts).unwrap();
    w.start_file("notes/manifest.json", opts).unwrap();
    w.write_all(&serde_json::to_vec(&manifest(ID, "1.0.0")).unwrap())
        .unwrap();
    w.start_file("notes/index.html", opts).unwrap();
    w.write_all(b"<p>cli</p>").unwrap();
    w.start_file("notes/.DS_Store", opts).unwrap();
    w.write_all(b"junk").unwrap();
    std::fs::write(&file, w.finish().unwrap().into_inner()).unwrap();

    let run = |cmd: AddonsCommand| copper_cloud::admin_addons_cli::run(cfg.clone(), cmd);
    run(AddonsCommand::Slots).await.unwrap();
    // Wrong slot name is refused before touching the database.
    assert!(run(AddonsCommand::Upload(AddonsUploadArgs {
        file: Some(file.clone()),
        file_flag: None,
        slot: "toolbar".into(),
    }))
    .await
    .is_err());
    run(AddonsCommand::Upload(AddonsUploadArgs {
        file: None,
        file_flag: Some(file.clone()),
        slot: "agent-sidebar".into(),
    }))
    .await
    .unwrap();
    // Re-upload of the same file: unchanged, no error.
    run(AddonsCommand::Upload(AddonsUploadArgs {
        file: Some(file.clone()),
        file_flag: None,
        slot: "agent-sidebar".into(),
    }))
    .await
    .unwrap();
    assert!(
        ids(&client_list(&s, &ana).await).is_empty(),
        "off after upload"
    );

    run(AddonsCommand::Enable(AddonsEnableArgs {
        id: id_arg(ID),
        everyone: false,
        people: Some(vec!["ANA@example.com".into()]),
        priority: None,
    }))
    .await
    .unwrap();
    assert_eq!(ids(&client_list(&s, &ana).await), [ID]);
    assert_eq!(ids(&client_list(&s, &bob).await), NONE);

    let settings_err = run(AddonsCommand::Settings(AddonsSettingsArgs {
        id: id_arg(ID),
        json: Some("{\"nope\":1}".into()),
        file: None,
    }))
    .await
    .unwrap_err();
    assert!(
        settings_err.to_string().contains("unknown setting"),
        "{settings_err}"
    );
    let settings_file = dir.join("settings.json");
    std::fs::write(&settings_file, r#"{"apiUrl":"https://other.example.com"}"#).unwrap();
    run(AddonsCommand::Settings(AddonsSettingsArgs {
        id: id_arg(ID),
        json: None,
        file: Some(settings_file),
    }))
    .await
    .unwrap();
    assert_eq!(
        client_list(&s, &ana).await["addons"][0]["settings"],
        json!({ "apiUrl": "https://other.example.com", "limit": 5 })
    );

    run(AddonsCommand::Enable(AddonsEnableArgs {
        id: AddonIdArg {
            id: None,
            id_flag: Some(ID.into()),
        },
        everyone: true,
        people: None,
        priority: Some(2),
    }))
    .await
    .unwrap();
    assert_eq!(ids(&client_list(&s, &bob).await), [ID]);
    // `admin addons …` reaches the same code.
    copper_cloud::admin::run(
        cfg.clone(),
        copper_cloud::cli::AdminCommand::Addons(AddonsCommand::List { json: false }),
    )
    .await
    .unwrap();
    run(AddonsCommand::List { json: true }).await.unwrap();

    run(AddonsCommand::Disable(id_arg(ID))).await.unwrap();
    assert_eq!(ids(&client_list(&s, &bob).await), NONE);
    assert!(run(AddonsCommand::Disable(id_arg("com.example.missing")))
        .await
        .is_err());

    run(AddonsCommand::Delete {
        id: id_arg(ID),
        yes: true,
    })
    .await
    .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM addons")
        .fetch_one(&s.state.db)
        .await
        .unwrap();
    assert_eq!(n, 0);

    // Audited as the CLI (no admin id), one row per change.
    let rows: Vec<(String, bool, Value)> = sqlx::query_as(
        "SELECT action, admin_id IS NULL, detail FROM admin_audit
         WHERE action LIKE 'addon.%' ORDER BY id",
    )
    .fetch_all(&s.state.db)
    .await
    .unwrap();
    let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        actions,
        [
            "addon.create",
            "addon.update",
            "addon.update",
            "addon.update",
            "addon.update",
            "addon.delete"
        ]
    );
    assert!(rows.iter().all(|(_, cli, d)| *cli && d["via"] == "cli"));
    assert_eq!(rows[0].2["files"], 2, "junk dropped, folder stripped");
    let _ = std::fs::remove_dir_all(&dir);
}
