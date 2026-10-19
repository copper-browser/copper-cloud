//! The canvas link end to end: the real copper-cloud app (native Postgres) in front of the
//! in-process mock Canvas Cloud — admin API, proxy rules, the WebSocket pipe, the event
//! bridge into `/v1/sync/events`, provisioning (JIT, batch, reconcile, outbox) and the
//! "never a 401 to Copper" rule.

#![allow(clippy::too_many_lines, clippy::many_single_char_names)]

mod common;
#[path = "../../canvas-link/tests/support/mock.rs"]
mod mock;

use std::time::Duration;

use common::*;
use futures::{SinkExt as _, StreamExt as _};
use reqwest::Method;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{self, Message};

async fn json_of(r: reqwest::Response) -> Value {
    r.json().await.unwrap()
}

/// Poll `f` (every 25 ms, up to 10 s).
async fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn put_link(s: &TestServer, c: &str, body: &Value) -> reqwest::Response {
    s.admin(Method::PUT, "canvas-link", Some(c))
        .json(body)
        .send()
        .await
        .unwrap()
}

/// Link `m` through the admin API and wait until the background provisioning settled.
async fn link(s: &TestServer, c: &str, m: &mock::Mock, users: &[&Session]) -> Value {
    let r = put_link(s, c, &json!({ "url": m.base, "app_key": mock::APP_KEY })).await;
    assert_eq!(r.status(), 200);
    let view = json_of(r).await;
    for u in users {
        wait_for("users provisioned", || m.st.has_user(&u.user_id)).await;
    }
    // Let the link-time sync and the first reconcile pass finish.
    tokio::time::sleep(Duration::from_millis(300)).await;
    view
}

/// Admin + mock + two users, linked.
async fn setup() -> (TestServer, String, mock::Mock, Session, Session) {
    let s = start().await;
    s.create_admin().await;
    let c = s.admin_login().await;
    let m = mock::start().await;
    let ana = s.signup("ana@example.com", PASSWORD, &new_device()).await;
    let bob = s.signup("bob@example.com", PASSWORD, &new_device()).await;
    link(&s, &c, &m, &[&ana, &bob]).await;
    (s, c, m, ana, bob)
}

fn assert_view_shape(v: &Value) {
    for key in [
        "linked",
        "url",
        "enabled",
        "tls_fingerprint",
        "linked_at",
        "updated_at",
        "updated_by",
        "status",
        "bridge",
        "migrations",
    ] {
        assert!(v.get(key).is_some(), "missing {key}: {v}");
    }
    for key in ["reachable", "version", "error", "checked_at"] {
        assert!(v["status"].get(key).is_some(), "status.{key}: {v}");
    }
    for key in ["connected", "last_event_at", "last_error"] {
        assert!(v["bridge"].get(key).is_some(), "bridge.{key}: {v}");
    }
    for key in ["migrated", "failed", "legacy_canvases"] {
        assert!(v["migrations"][key].is_i64(), "migrations.{key}: {v}");
    }
}

#[tokio::test]
async fn admin_link_lifecycle() {
    let s = start().await;
    s.create_admin().await;
    let c = s.admin_login().await;
    let m = mock::start().await;
    let ana = s.signup("ana@example.com", PASSWORD, &new_device()).await;

    // Unlinked: the view says so, canvas routes answer 503, /v1/info says linked: false.
    let v = json_of(
        s.admin(Method::GET, "canvas-link", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_view_shape(&v);
    assert_eq!(v["linked"], false);
    assert_eq!(v["url"], Value::Null);
    assert_eq!(v["status"]["reachable"], Value::Null);
    assert_eq!(v["bridge"]["connected"], false);
    assert_eq!(
        v["migrations"],
        json!({ "migrated": 0, "failed": 0, "legacy_canvases": 0 })
    );
    let r = s
        .authed(Method::GET, "/v1/canvases", &ana)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    assert_eq!(json_of(r).await["error"], "canvas_unavailable");
    let info = json_of(s.req(Method::GET, "/v1/info").send().await.unwrap()).await;
    assert_eq!(info["canvas"], json!({ "linked": false }));
    assert!(info["features"]
        .as_array()
        .unwrap()
        .contains(&json!("canvas_link")));

    // Validation: bad URLs, a wrong key, an unreachable server → 400 with a clear message.
    for (body, needle) in [
        (
            json!({ "url": "http://10.1.2.3:8480", "app_key": mock::APP_KEY }),
            "localhost",
        ),
        (
            json!({ "url": "ftp://x.example", "app_key": mock::APP_KEY }),
            "https://",
        ),
        (json!({ "url": m.base, "app_key": "x" }), "app_key"),
        (
            json!({ "url": m.base, "app_key": mock::APP_KEY, "tls_fingerprint": "abc" }),
            "tls_fingerprint",
        ),
        (
            json!({ "url": m.base, "app_key": "cva_definitely_wrong_key" }),
            "rejected the app key",
        ),
        (
            json!({ "url": "http://127.0.0.1:9", "app_key": mock::APP_KEY }),
            "could not reach",
        ),
    ] {
        let r = put_link(&s, &c, &body).await;
        assert_eq!(r.status(), 400, "{body}");
        let e = json_of(r).await;
        assert_eq!(e["error"], "bad_request");
        assert!(
            e["message"].as_str().unwrap().contains(needle),
            "{body}: {e}"
        );
    }
    // CSRF still applies.
    let r = s
        .http
        .put(s.url("/admin/api/canvas-link"))
        .header("Cookie", &c)
        .json(&json!({ "url": m.base, "app_key": mock::APP_KEY }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // Link: tested, stored sealed, users provisioned in bulk.
    let v = link(&s, &c, &m, &[&ana]).await;
    assert_view_shape(&v);
    assert_eq!(v["linked"], true);
    assert_eq!(v["url"], m.base.as_str());
    assert_eq!(v["enabled"], true);
    assert_eq!(v["updated_by"], ADMIN_EMAIL);
    assert_eq!(v["status"]["reachable"], true);
    assert_eq!(v["status"]["version"], "0.0.0-mock");
    assert!(v["linked_at"].is_string());
    assert!(m.st.batch_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    let u = m.st.user(&ana.user_id).unwrap();
    assert_eq!(u.email, "ana@example.com");
    assert_eq!(
        u.user_id, ana.user_id,
        "Canvas Cloud user id == Copper user id"
    );
    assert_eq!(u.display_name.as_deref(), Some("Test User"));
    let sealed: Vec<u8> = sqlx::query_scalar("SELECT app_key_sealed FROM canvas_link")
        .fetch_one(&s.state.db)
        .await
        .unwrap();
    assert!(!sealed
        .windows(mock::APP_KEY.len())
        .any(|w| w == mock::APP_KEY.as_bytes()));
    let info = json_of(s.req(Method::GET, "/v1/info").send().await.unwrap()).await;
    assert_eq!(info["canvas"], json!({ "linked": true }));
    // The key never comes back.
    let text = s
        .admin(Method::GET, "canvas-link", Some(&c))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!text.contains(mock::APP_KEY));

    // The event bridge connects.
    wait_for("bridge connected", || m.st.event_clients() >= 1).await;
    let mut connected = false;
    for _ in 0..100 {
        let v = json_of(
            s.admin(Method::GET, "canvas-link", Some(&c))
                .send()
                .await
                .unwrap(),
        )
        .await;
        if v["bridge"]["connected"] == true {
            connected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(connected, "bridge.connected");

    // Test.
    let t = json_of(
        s.admin(Method::POST, "canvas-link/test", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(t["ok"], true);
    assert_eq!(t["version"], "0.0.0-mock");
    assert!(t["latency_ms"].is_u64());
    assert_eq!(t["error"], Value::Null);
    m.st.set_app_key("cva_rotated_elsewhere_000");
    let t = json_of(
        s.admin(Method::POST, "canvas-link/test", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(t["ok"], false);
    assert_eq!(t["latency_ms"], Value::Null);
    assert!(t["error"].as_str().unwrap().contains("401"), "{t}");
    m.st.set_app_key(mock::APP_KEY);

    // Unlink (idempotent), audited once.
    for _ in 0..2 {
        let r = s
            .admin(Method::DELETE, "canvas-link", Some(&c))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(json_of(r).await, json!({ "ok": true }));
    }
    let v = json_of(
        s.admin(Method::GET, "canvas-link", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(v["linked"], false);
    let r = s
        .authed(Method::GET, "/v1/canvases", &ana)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    let t = json_of(
        s.admin(Method::POST, "canvas-link/test", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(t["ok"], false);

    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM admin_audit WHERE action LIKE 'canvas_link.%' ORDER BY id",
    )
    .fetch_all(&s.state.db)
    .await
    .unwrap();
    assert_eq!(
        actions,
        [
            "canvas_link.set",
            "canvas_link.test",
            "canvas_link.test",
            "canvas_link.remove"
        ]
    );
    let detail: Value =
        sqlx::query_scalar("SELECT detail FROM admin_audit WHERE action = 'canvas_link.set'")
            .fetch_one(&s.state.db)
            .await
            .unwrap();
    assert_eq!(detail["url"], m.base.as_str());
    assert!(!detail.to_string().contains(mock::APP_KEY));
}

#[tokio::test]
async fn proxy_forwards_requests_and_responses() {
    let (s, _c, _m, ana, _bob) = setup().await;

    // Header and query rules.
    let r = s
        .authed(
            Method::GET,
            "/v1/canvases/c1/echo?token=SECRET&full=true&token=again",
            &ana,
        )
        .header("Cookie", "copper=1")
        .header("If-None-Match", "\"v1\"")
        .header("Accept", "application/json")
        .header("X-Custom", "nope")
        .header("X-Canvas-User", "spoofed")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "private, max-age=5");
    assert_eq!(r.headers()["etag"], "\"mock-etag\"");
    assert!(r.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("application/json"));
    assert!(r.headers().get("x-mock-internal").is_none());
    assert!(r.headers().get("set-cookie").is_none());
    let e = json_of(r).await;
    assert_eq!(e["path"], "/v1/canvases/c1/echo");
    assert_eq!(e["query"], "full=true");
    assert_eq!(e["user"], ana.user_id.as_str());
    let h = &e["headers"];
    assert_eq!(h["authorization"], format!("Bearer {}", mock::APP_KEY));
    assert_eq!(h["x-canvas-user"], ana.user_id.as_str());
    assert_eq!(h["if-none-match"], "\"v1\"");
    assert_eq!(h["accept"], "application/json");
    assert_eq!(h["x-forwarded-for"], "127.0.0.1");
    for gone in ["cookie", "x-copper-instance", "x-custom"] {
        assert!(h.get(gone).is_none(), "{gone} forwarded: {h}");
    }

    // Method + body + content type; the other mounted prefixes.
    let r = s
        .authed(Method::PATCH, "/v1/invites/i1/accept", &ana)
        .json(&json!({ "x": 1 }))
        .send()
        .await
        .unwrap();
    let e = json_of(r).await;
    assert_eq!(e["method"], "PATCH");
    assert_eq!(e["path"], "/v1/invites/i1/accept");
    assert_eq!(e["body"], "{\"x\":1}");
    assert_eq!(e["headers"]["content-type"], "application/json");
    for path in [
        "/v1/people",
        "/v1/invites",
        "/v1/mentions",
        "/v1/mentions/read",
        "/v1/canvas-links/tok",
    ] {
        let r = s.authed(Method::GET, path, &ana).send().await.unwrap();
        assert_eq!(r.status(), 200, "{path}");
        assert_eq!(json_of(r).await["path"], path);
    }
    // Real list/create through the proxy.
    let created = json_of(
        s.authed(Method::POST, "/v1/canvases", &ana)
            .json(&json!({ "name": "Roadmap" }))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(created["name"], "Roadmap");
    let list = json_of(
        s.authed(Method::GET, "/v1/canvases", &ana)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(list["canvases"][0]["id"], created["id"]);

    // Client errors pass through verbatim (clients rely on the exact codes)…
    for code in [403_u16, 404, 409, 422, 429, 500] {
        let r = s
            .authed(Method::GET, &format!("/v1/canvases/c1/status/{code}"), &ana)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), code);
        if code == 429 {
            assert_eq!(r.headers()["retry-after"], "7");
        }
        assert_eq!(json_of(r).await["error"], format!("mock_{code}"));
    }
    // … but never a 401 (Copper would sign the user out).
    let r = s
        .authed(Method::GET, "/v1/canvases/c1/status/401", &ana)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    let e = json_of(r).await;
    assert_eq!(e["error"], "canvas_upstream");
    assert!(e["message"]
        .as_str()
        .unwrap()
        .contains("Canvas Cloud rejected this Copper Cloud's app key/user"));

    // Copper auth still applies first.
    let r = s.req(Method::GET, "/v1/canvases").send().await.unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(json_of(r).await["error"], "session");

    // Body limits: 64 KiB in general, 4 MiB for ops.
    let r = s
        .authed(Method::POST, "/v1/canvases/c1/ops", &ana)
        .header("content-type", "application/json")
        .body(vec![b' '; 3 << 20])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(json_of(r).await["received"], 3 << 20);
    let r = s
        .authed(Method::POST, "/v1/canvases/c1/ops", &ana)
        .body(vec![b' '; 5 << 20])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    let r = s
        .authed(Method::POST, "/v1/canvases/c1/chat", &ana)
        .body(vec![b' '; 100 << 10])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
}

#[tokio::test]
async fn upstream_401_is_never_forwarded() {
    let (s, _c, m, ana, _bob) = setup().await;
    let puts = || m.st.user_puts.load(std::sync::atomic::Ordering::SeqCst);

    // unknown_user → provision just in time, retry once, succeed.
    m.st.users.lock().unwrap().remove(&ana.user_id);
    let before = puts();
    let r = s
        .authed(Method::GET, "/v1/canvases", &ana)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(puts(), before + 1);
    assert_eq!(m.st.user(&ana.user_id).unwrap().email, "ana@example.com");

    // A second unknown_user after provisioning → 502, one provisioning attempt only.
    m.st.users.lock().unwrap().remove(&ana.user_id);
    m.st.sticky_unknown
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let before = puts();
    let r = s
        .authed(Method::GET, "/v1/canvases", &ana)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    assert_eq!(json_of(r).await["error"], "canvas_upstream");
    assert_eq!(puts(), before + 1);
    m.st.sticky_unknown
        .store(false, std::sync::atomic::Ordering::SeqCst);

    // Provisioning itself fails → 502.
    m.st.fail_provision
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let r = s
        .authed(Method::GET, "/v1/canvases", &ana)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    assert_eq!(json_of(r).await["error"], "canvas_upstream");
    m.st.fail_provision
        .store(false, std::sync::atomic::Ordering::SeqCst);

    // The app key is refused (revoked/rotated on Canvas Cloud) → 502, not 401.
    m.st.add_user(&ana.user_id, "ana@example.com");
    m.st.set_app_key("cva_rotated_elsewhere_000");
    let r = s
        .authed(Method::GET, "/v1/canvases", &ana)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    let e = json_of(r).await;
    assert_eq!(e["error"], "canvas_upstream");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .starts_with("Canvas Cloud rejected this Copper Cloud's app key/user ("),
        "{e}"
    );

    // The same for a refused WebSocket handshake.
    let err = ws_connect(&s, &ana, "c1").await.unwrap_err();
    let (status, body) = http_error(err);
    assert_eq!(status, 502);
    assert_eq!(body["error"], "canvas_upstream");
    m.st.set_app_key(mock::APP_KEY);
    // …and an unknown user on the WebSocket is provisioned and let through.
    m.st.users.lock().unwrap().remove(&ana.user_id);
    let (mut ws, _) = ws_connect(&s, &ana, "c1").await.unwrap();
    ws.send(Message::Text("hi".into())).await.unwrap();
    assert_eq!(next_data(&mut ws).await, Message::Text("hi".into()));
    m.st.users.lock().unwrap().remove(&ana.user_id);
    m.st.sticky_unknown
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, body) = http_error(ws_connect(&s, &ana, "c2").await.unwrap_err());
    assert_eq!(status, 502);
    assert_eq!(body["error"], "canvas_upstream");
    m.st.sticky_unknown
        .store(false, std::sync::atomic::Ordering::SeqCst);

    // Canvas Cloud unreachable (the stored URL changed, e.g. by the CLI) → 502.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    sqlx::query("UPDATE canvas_link SET url = $1, updated_at = now()")
        .bind(&dead)
        .execute(&s.state.db)
        .await
        .unwrap();
    s.link.reload().await.unwrap();
    let r = s
        .authed(Method::GET, "/v1/canvases", &ana)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    let e = json_of(r).await;
    assert_eq!(e["error"], "canvas_upstream");
    assert!(
        e["message"].as_str().unwrap().contains("unreachable"),
        "{e}"
    );
    let (status, body) = http_error(ws_connect(&s, &ana, "c1").await.unwrap_err());
    assert_eq!(status, 502);
    assert_eq!(body["error"], "canvas_upstream");
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Open `/v1/canvases/{id}/ws` the way Copper (Mac) does: instance key header, bearer
/// header and `?token=`.
async fn ws_connect(
    s: &TestServer,
    who: &Session,
    id: &str,
) -> Result<(Ws, tungstenite::handshake::client::Response), tungstenite::Error> {
    let mut req = format!("ws://{}/v1/canvases/{id}/ws?token={}", s.addr, who.token)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("x-copper-instance", s.key().parse().unwrap());
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {}", who.token).parse().unwrap(),
    );
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(64 << 20))
        .max_frame_size(Some(16 << 20));
    tokio_tungstenite::connect_async_with_config(req, Some(config), true).await
}

fn http_error(err: tungstenite::Error) -> (u16, Value) {
    match err {
        tungstenite::Error::Http(resp) => {
            let status = resp.status().as_u16();
            let body = resp.into_body().unwrap_or_default();
            (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
        }
        other => panic!("expected an HTTP refusal, got {other:?}"),
    }
}

/// Next text/binary/close message (pings skipped), 10 s budget.
async fn next_data(ws: &mut Ws) -> Message {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("timed out waiting for a WebSocket message")
            .expect("WebSocket ended")
            .unwrap();
        if !matches!(m, Message::Ping(_) | Message::Pong(_)) {
            return m;
        }
    }
}

#[tokio::test]
async fn websocket_pipe() {
    let (s, _c, m, ana, _bob) = setup().await;

    let (mut ws, _) = ws_connect(&s, &ana, "c1").await.unwrap();
    // The upstream handshake carried the app key + user, not Copper's token.
    let seen = m.st.requests_for("/v1/canvases/c1/ws");
    let last = seen.last().unwrap();
    assert_eq!(last.query, None, "token= stripped");
    assert_eq!(
        last.headers["authorization"],
        format!("Bearer {}", mock::APP_KEY)
    );
    assert_eq!(last.headers["x-canvas-user"], ana.user_id);
    assert!(!last.headers.contains_key("x-copper-instance"));

    // Frames are piped byte-exact, both kinds, including large ones.
    let bin: Vec<u8> = (0..=255_u8).cycle().take(1000).collect();
    ws.send(Message::Binary(bin.clone().into())).await.unwrap();
    assert_eq!(next_data(&mut ws).await, Message::Binary(bin.into()));
    ws.send(Message::Text("hello ✓".into())).await.unwrap();
    assert_eq!(next_data(&mut ws).await, Message::Text("hello ✓".into()));
    let big: Vec<u8> = (0..(12_u32 << 20)).map(|i| (i % 251) as u8).collect();
    ws.send(Message::Binary(big.clone().into())).await.unwrap();
    match next_data(&mut ws).await {
        Message::Binary(b) => assert!(b.as_ref() == big.as_slice(), "12 MiB frame intact"),
        other => panic!("{other:?}"),
    }
    assert_eq!(m.st.live_peers.load(std::sync::atomic::Ordering::SeqCst), 1);

    // Canvas Cloud closes with 4404 → the client sees 4404 with the reason.
    ws.send(Message::Text("close 4404 canvas deleted".into()))
        .await
        .unwrap();
    match next_data(&mut ws).await {
        Message::Close(Some(f)) => {
            assert_eq!(u16::from(f.code), 4404);
            assert_eq!(f.reason.as_str(), "canvas deleted");
        }
        other => panic!("{other:?}"),
    }
    drop(ws);

    // The client closes with 4403 → Canvas Cloud sees 4403 with the reason.
    let (mut ws, _) = ws_connect(&s, &ana, "c1").await.unwrap();
    ws.close(Some(CloseFrame {
        code: CloseCode::from(4403),
        reason: "bye".into(),
    }))
    .await
    .unwrap();
    wait_for("client close forwarded", || {
        m.st.ws_closes
            .lock()
            .unwrap()
            .contains(&(4403, "bye".to_owned()))
    })
    .await;

    // Handshake refusals are returned as-is, without upgrading.
    let (status, body) = http_error(ws_connect(&s, &ana, "forbidden").await.unwrap_err());
    assert_eq!(status, 403);
    assert_eq!(body["error"], "forbidden");
    let (status, body) = http_error(ws_connect(&s, &ana, "missing").await.unwrap_err());
    assert_eq!(status, 404);
    assert_eq!(body["error"], "not_found");

    // copper-cloud shutting down → 1001 to Copper, a close to Canvas Cloud.
    let (mut ws, _) = ws_connect(&s, &ana, "c1").await.unwrap();
    ws.send(Message::Text("ping".into())).await.unwrap();
    assert_eq!(next_data(&mut ws).await, Message::Text("ping".into()));
    s.link.close_all();
    match next_data(&mut ws).await {
        Message::Close(Some(f)) => {
            assert_eq!(u16::from(f.code), 1001);
            assert_eq!(f.reason.as_str(), "server shutting down");
        }
        other => panic!("{other:?}"),
    }
    wait_for("upstream told", || {
        m.st.ws_closes
            .lock()
            .unwrap()
            .iter()
            .any(|(code, _)| *code == 1001)
    })
    .await;
}

/// Read SSE frames until one with `event: <name>` arrives; returns its data JSON.
async fn next_event(
    stream: &mut (impl futures::Stream<Item = reqwest::Result<axum::body::Bytes>> + Unpin),
    buf: &mut String,
    name: &str,
) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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
async fn event_bridge_feeds_sync_events() {
    let (s, c, m, ana, bob) = setup().await;
    wait_for("bridge connected", || m.st.event_clients() >= 1).await;

    let resp = s
        .authed(Method::GET, "/v1/sync/events", &ana)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();

    let canvas_id = uuid::Uuid::now_v7().to_string();
    // Bob's event does not reach Ana; hers does, in copper-cloud's own `canvas` shape.
    m.st.publish_canvas(&bob.user_id, &uuid::Uuid::now_v7().to_string(), "renamed");
    let id =
        m.st.publish_canvas(&ana.user_id, &canvas_id, "member_added");
    let ev = next_event(&mut stream, &mut buf, "canvas").await;
    assert_eq!(ev["type"], "canvas");
    assert_eq!(ev["canvas_id"], canvas_id.as_str());
    assert_eq!(ev["kind"], "member_added");

    let mut last_event_at = Value::Null;
    for _ in 0..100 {
        let v = json_of(
            s.admin(Method::GET, "canvas-link", Some(&c))
                .send()
                .await
                .unwrap(),
        )
        .await;
        if v["bridge"]["last_event_at"].is_string() {
            last_event_at = v["bridge"]["last_event_at"].clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(last_event_at.is_string());

    // A changed link (re-saved settings) restarts the stream, resuming after the last id.
    sqlx::query("UPDATE canvas_link SET updated_at = now()")
        .execute(&s.state.db)
        .await
        .unwrap();
    s.link.reload().await.unwrap();
    let want = Some(id.to_string());
    wait_for("resumed with Last-Event-ID", || {
        m.st.last_event_ids.lock().unwrap().contains(&want)
    })
    .await;
    // (The new stream subscribes right after recording the id.)
    tokio::time::sleep(Duration::from_millis(300)).await;
    let canvas_id2 = uuid::Uuid::now_v7().to_string();
    m.st.publish_canvas(&ana.user_id, &canvas_id2, "deleted");
    let ev = next_event(&mut stream, &mut buf, "canvas").await;
    assert_eq!(ev["canvas_id"], canvas_id2.as_str());
    assert_eq!(ev["kind"], "deleted");

    // Canvas Cloud could not resume (it restarted): every connected user is told to refetch.
    let resp = s
        .authed(Method::GET, "/v1/sync/events", &bob)
        .send()
        .await
        .unwrap();
    let mut bob_stream = resp.bytes_stream();
    let mut bob_buf = String::new();
    next_event(&mut bob_stream, &mut bob_buf, "ready").await;
    m.st.publish_resync();
    for (stream, buf) in [(&mut stream, &mut buf), (&mut bob_stream, &mut bob_buf)] {
        let ev = next_event(stream, buf, "resync").await;
        assert_eq!(ev, json!({ "type": "resync", "reason": "canvas" }));
    }
}

/// Every new account is provisioned on Canvas Cloud within seconds, whichever path created
/// it — here a direct insert, as `admin create-user` (another process) does.
#[tokio::test]
async fn new_users_are_provisioned_right_away() {
    let (s, _c, m, _ana, _bob) = setup().await;
    let mut conn = s.state.db.acquire().await.unwrap();
    let email = format!("late-{}@example.com", uuid::Uuid::now_v7().simple());
    let user =
        copper_cloud_core::auth::insert_user(&mut conn, &s.state.crypto, &email, "Late", None)
            .await
            .unwrap();
    drop(conn);
    let id = user.id.to_string();
    wait_for("new user provisioned", || {
        m.st.user(&id).is_some_and(|u| u.email == email)
    })
    .await;
}

#[tokio::test]
async fn admin_canvases_overview_people_and_user_hooks() {
    let (s, c, m, ana, bob) = setup().await;
    let roadmap = m.st.add_canvas(&ana.user_id, "Roadmap", "shared");
    m.st.canvases.lock().unwrap()[0]
        .members
        .push(bob.user_id.clone());
    m.st.add_canvas(&ana.user_id, "Personal", "personal");

    // Overview numbers come from Canvas Cloud.
    let ov = json_of(
        s.admin(Method::GET, "overview", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ov["counts"]["canvases"], 2);
    assert_eq!(ov["counts"]["live_rooms"], 0);
    assert_eq!(ov["counts"]["live_peers"], 0);
    assert_eq!(ov["counts"]["users"], 2);

    // The canvas list keeps its shape (+ linked).
    let cs = json_of(
        s.admin(Method::GET, "canvases?kind=shared&limit=10", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(cs["linked"], true);
    assert_eq!(cs["total"], 1);
    assert_eq!(cs["limit"], 10);
    assert_eq!(cs["offset"], 0);
    let row = &cs["items"][0];
    assert_eq!(row["id"], roadmap.as_str());
    assert_eq!(row["name"], "Roadmap");
    assert_eq!(row["kind"], "shared");
    assert_eq!(row["owner_id"], ana.user_id.as_str());
    assert_eq!(row["owner_email"], "ana@example.com");
    assert_eq!(row["member_count"], 2);
    assert_eq!(row["share_link_count"], 0);
    assert!(row["created_at"].is_string() && row["updated_at"].is_string());
    let r = s
        .admin(Method::GET, "canvases?kind=bogus", Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // People: canvas_count from Canvas Cloud.
    let users = json_of(
        s.admin(Method::GET, "users", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let count = |email: &str| {
        users["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["email"] == email)
            .unwrap()["canvas_count"]
            .clone()
    };
    assert_eq!(count("ana@example.com"), 2);
    assert_eq!(count("bob@example.com"), 1);

    // Delete a canvas (audited); again → 404.
    let r = s
        .admin(Method::DELETE, &format!("canvases/{roadmap}"), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(m.st.canvas_count(), 1);
    let r = s
        .admin(Method::DELETE, &format!("canvases/{roadmap}"), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM admin_audit WHERE action = 'canvas.delete' AND target = $1",
    )
    .bind(&roadmap)
    .fetch_one(&s.state.db)
    .await
    .unwrap();
    assert_eq!(n, 1);

    // Canvas Cloud refusing us: the list says 502 canvas_upstream, the overview goes null.
    m.st.set_app_key("cva_rotated_elsewhere_000");
    let r = s
        .admin(Method::GET, "canvases", Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    assert_eq!(json_of(r).await["error"], "canvas_upstream");
    let r = s
        .admin(Method::DELETE, &format!("canvases/{roadmap}"), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    let ov = json_of(
        s.admin(Method::GET, "overview", Some(&c))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ov["counts"]["canvases"], Value::Null);
    assert_eq!(ov["counts"]["live_rooms"], Value::Null);
    assert_eq!(ov["counts"]["live_peers"], Value::Null);
    m.st.set_app_key(mock::APP_KEY);

    // Disable Bob → pushed as disabled and disconnected on Canvas Cloud.
    let r = s
        .admin(Method::PATCH, &format!("users/{}", bob.user_id), Some(&c))
        .json(&json!({ "disabled": true, "display_name": "Robert" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    wait_for("bob disabled upstream", || {
        m.st.user(&bob.user_id)
            .is_some_and(|u| u.disabled && u.display_name.as_deref() == Some("Robert"))
    })
    .await;
    wait_for("bob disconnected", || {
        m.st.disconnects.lock().unwrap().contains(&bob.user_id)
    })
    .await;

    // Delete Ana → deleted on Canvas Cloud (with her canvases).
    let r = s
        .admin(Method::DELETE, &format!("users/{}", ana.user_id), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    wait_for("ana deleted upstream", || {
        m.st.deleted_users.lock().unwrap().contains(&ana.user_id)
    })
    .await;
    assert_eq!(m.st.canvas_count(), 0);

    // Canvas Cloud down while deleting Bob: kept in the outbox, retried later.
    m.st.set_app_key("cva_rotated_elsewhere_000");
    let r = s
        .admin(Method::DELETE, &format!("users/{}", bob.user_id), Some(&c))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let mut row: Option<(i32, Option<String>)> = None;
    for _ in 0..200 {
        row = sqlx::query_as(
            "SELECT attempts, last_error FROM canvas_link_outbox
             WHERE kind = 'delete_user' AND subject = $1 AND attempts > 0",
        )
        .bind(bob.user_id.parse::<uuid::Uuid>().unwrap())
        .fetch_optional(&s.state.db)
        .await
        .unwrap();
        if row.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (attempts, last_error) = row.expect("failed delete kept in the outbox");
    assert_eq!(attempts, 1);
    assert!(last_error.unwrap().contains("401"));
    assert!(m.st.has_user(&bob.user_id));
    m.st.set_app_key(mock::APP_KEY);
    sqlx::query("UPDATE canvas_link_outbox SET next_attempt_at = now()")
        .execute(&s.state.db)
        .await
        .unwrap();
    s.link.kick_outbox();
    wait_for("bob deleted upstream", || {
        m.st.deleted_users.lock().unwrap().contains(&bob.user_id)
    })
    .await;
    let mut left = 1_i64;
    for _ in 0..100 {
        left = sqlx::query_scalar("SELECT count(*) FROM canvas_link_outbox")
            .fetch_one(&s.state.db)
            .await
            .unwrap();
        if left == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(left, 0, "outbox drained");
}

#[tokio::test]
async fn reconcile_pushes_changes_and_only_reports_deleted_users() {
    let (s, _c, m, ana, bob) = setup().await;
    let active = s.link.enabled().await.unwrap().unwrap();

    // Nothing changed: nothing pushed.
    let (pushed, disconnected, deleted) =
        copper_cloud_canvas_link::provision::reconcile_once(&s.link, &active)
            .await
            .unwrap();
    assert_eq!((pushed, disconnected, deleted), (0, 0, 0));

    // Changed outside the admin API (e.g. the CLI): email change, disable, delete.
    sqlx::query("UPDATE users SET email = 'ana2@example.com' WHERE id = $1")
        .bind(ana.user_id.parse::<uuid::Uuid>().unwrap())
        .execute(&s.state.db)
        .await
        .unwrap();
    let carol = s.signup("carol@example.com", PASSWORD, &new_device()).await;
    sqlx::query("UPDATE users SET disabled = true WHERE id = $1")
        .bind(carol.user_id.parse::<uuid::Uuid>().unwrap())
        .execute(&s.state.db)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(bob.user_id.parse::<uuid::Uuid>().unwrap())
        .execute(&s.state.db)
        .await
        .unwrap();
    let (pushed, disconnected, orphaned) =
        copper_cloud_canvas_link::provision::reconcile_once(&s.link, &active)
            .await
            .unwrap();
    assert_eq!((pushed, disconnected, orphaned), (2, 1, 1));
    assert_eq!(m.st.user(&ana.user_id).unwrap().email, "ana2@example.com");
    assert!(m.st.user(&carol.user_id).unwrap().disabled);
    assert!(m.st.disconnects.lock().unwrap().contains(&carol.user_id));
    // Reconcile never deletes: bob (gone from Copper) is still on Canvas Cloud...
    assert!(m.st.has_user(&bob.user_id));
    // ...until an operator prunes explicitly.
    let (deleted, failed) = copper_cloud_canvas_link::provision::prune_orphans(&s.link, &active)
        .await
        .unwrap();
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(deleted.len(), 1);
    assert!(!m.st.has_user(&bob.user_id));
}

#[tokio::test]
async fn join_landing_page_is_public() {
    let s = start().await;
    let token = copper_cloud_core::ids::random_token();
    // No instance key: the landing page is outside /v1 and needs no database.
    let r = s
        .http
        .get(s.url(&format!("/join/{token}")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "no-store");
    let html = r.text().await.unwrap();
    assert!(html.contains(&format!("copper://canvas/join/{token}")));
}
