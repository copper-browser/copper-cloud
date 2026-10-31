//! Device agent relay (`device_agent`, 0.13.0): links, the frames mailbox, agent tokens,
//! `/v1/agent/*` outside the gate, and MCP calls relayed to a simulated device (an SSE reader
//! for `request` / `cancel` frames plus frames POSTs for replies and progress).

#![allow(clippy::too_many_lines)]

mod common;

use std::pin::Pin;
use std::time::{Duration, Instant};

use common::{new_device, session_from, start_with, Session, TestServer, PASSWORD};
use futures::{Stream, StreamExt as _};
use reqwest::Method;
use serde_json::{json, Value};

const DEVICE_NAME: &str = "Ada's iPhone";
const SSE_ACCEPT: &str = "application/json, text/event-stream";

/// Short relay timings so deadlines, keepalives and offline devices take about a second.
fn tune(cfg: &mut copper_cloud_core::config::Config) {
    cfg.limits.auth_per_minute = 1000;
    let t = &mut cfg.device_agent;
    t.relay_deadline = Duration::from_millis(1500);
    t.agent_keepalive = Duration::from_millis(200);
    t.device_ping = Duration::from_millis(400);
    t.offline_grace = Duration::from_millis(500);
}

async fn start() -> TestServer {
    start_with(tune).await
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

// ---------------------------------------------------------------------------------------------
// A minimal SSE reader

type Chunks = Pin<Box<dyn Stream<Item = reqwest::Result<Vec<u8>>> + Send>>;

struct SseReader {
    chunks: Chunks,
    buf: Vec<u8>,
    /// `:` comment lines seen so far (keepalives).
    comments: usize,
}

#[derive(Debug)]
struct Ev {
    name: String,
    data: String,
}

impl Ev {
    fn json(&self) -> Value {
        serde_json::from_str(&self.data).unwrap_or_else(|_| panic!("not JSON: {}", self.data))
    }
}

impl SseReader {
    fn new(r: reqwest::Response) -> Self {
        Self {
            chunks: Box::pin(r.bytes_stream().map(|c| c.map(|b| b.to_vec()))),
            buf: Vec::new(),
            comments: 0,
        }
    }

    /// The next event (comments are counted and skipped); `None` once the stream ended.
    async fn next(&mut self) -> Option<Ev> {
        tokio::time::timeout(Duration::from_secs(10), self.read())
            .await
            .expect("timed out waiting for an SSE event")
    }

    /// Like [`Self::next`], but `Err` if nothing arrives within `wait`.
    async fn next_within(&mut self, wait: Duration) -> Result<Option<Ev>, ()> {
        tokio::time::timeout(wait, self.read())
            .await
            .map_err(|_| ())
    }

    /// The next event that is not a `ping`.
    async fn next_event(&mut self) -> Option<Ev> {
        loop {
            let ev = self.next().await?;
            if ev.name != "ping" {
                return Some(ev);
            }
        }
    }

    async fn read(&mut self) -> Option<Ev> {
        loop {
            if let Some(pos) = self.buf.windows(2).position(|w| w == b"\n\n") {
                let block: Vec<u8> = self.buf.drain(..pos + 2).collect();
                let text = String::from_utf8(block).unwrap();
                let (mut name, mut data, mut comment) = (String::new(), Vec::new(), false);
                for line in text.lines() {
                    if line.starts_with(':') {
                        comment = true;
                    } else if let Some(v) = line.strip_prefix("event:") {
                        v.trim_start().clone_into(&mut name);
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data.push(v.strip_prefix(' ').unwrap_or(v).to_owned());
                    }
                }
                if name.is_empty() && data.is_empty() {
                    if comment {
                        self.comments += 1;
                    }
                    continue;
                }
                return Some(Ev {
                    name,
                    data: data.join("\n"),
                });
            }
            match self.chunks.next().await {
                Some(Ok(chunk)) => self.buf.extend_from_slice(&chunk),
                Some(Err(_)) | None => return None,
            }
        }
    }
}

fn assert_sse_headers(r: &reqwest::Response) {
    let h = r.headers();
    assert!(
        h["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream"),
        "{h:?}"
    );
    assert_eq!(h["cache-control"], "no-store");
    assert_eq!(h["x-accel-buffering"], "no");
}

// ---------------------------------------------------------------------------------------------
// Device side helpers

/// A signed-up account whose device is named [`DEVICE_NAME`] (the phone).
async fn phone(s: &TestServer, email: &str) -> Session {
    let p = s.signup(email, PASSWORD, &new_device()).await;
    let r = s
        .authed(Method::PATCH, &format!("/v1/devices/{}", p.device_id), &p)
        .json(&json!({ "name": DEVICE_NAME }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    p
}

/// Another device of the same account (`Other Mac`).
async fn second_device(s: &TestServer, email: &str) -> Session {
    let r = s.login(email, PASSWORD, &new_device()).await;
    assert_eq!(r.status(), 200);
    session_from(&r.json().await.unwrap())
}

fn tool(name: &str) -> Value {
    json!({
        "name": name,
        "description": format!("{name} on the phone"),
        "inputSchema": { "type": "object", "properties": { "goal": { "type": "string" } } },
    })
}

fn manifest() -> Value {
    json!([
        tool("jev_run"),
        tool("jev_observe"),
        tool("browser_snapshot")
    ])
}

async fn register(s: &TestServer, p: &Session, body: Value) -> reqwest::Response {
    s.authed(Method::POST, "/v1/me/links", p)
        .json(&body)
        .send()
        .await
        .unwrap()
}

/// Register the phone's link with [`manifest`]; returns the link object.
async fn register_ok(s: &TestServer, p: &Session) -> Value {
    let r = register(
        s,
        p,
        json!({ "label": DEVICE_NAME, "client_version": "ios-1.0.0", "tools": manifest() }),
    )
    .await;
    assert_eq!(r.status(), 200);
    json_of(r).await
}

async fn get_link(s: &TestServer, p: &Session, id: &str) -> Value {
    let r = s
        .authed(Method::GET, &format!("/v1/me/links/{id}"), p)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    json_of(r).await
}

async fn list_links(s: &TestServer, p: &Session) -> Vec<Value> {
    let r = s
        .authed(Method::GET, "/v1/me/links", p)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    json_of(r).await.as_array().unwrap().clone()
}

/// `POST /v1/me/agent-tokens`; returns the minted token object.
async fn mint(s: &TestServer, p: &Session, label: &str) -> Value {
    let r = s
        .authed(Method::POST, "/v1/me/agent-tokens", p)
        .json(&json!({ "label": label }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    json_of(r).await
}

fn token_of(minted: &Value) -> String {
    minted["token"].as_str().unwrap().to_owned()
}

/// Open the frames stream (asserting the SSE headers).
async fn frames(s: &TestServer, p: &Session, link: &str) -> SseReader {
    let r = s
        .authed(Method::GET, &format!("/v1/me/links/{link}/frames"), p)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_sse_headers(&r);
    SseReader::new(r)
}

/// Open the frames stream and read `hello`; returns the reader and the hello data.
async fn connect(s: &TestServer, p: &Session, link: &str) -> (SseReader, Value) {
    let mut dev = frames(s, p, link).await;
    let hello = dev.next().await.expect("hello");
    assert_eq!(hello.name, "hello");
    let data = hello.json();
    (dev, data)
}

async fn post_frames(s: &TestServer, p: &Session, link: &str, frames: Value) -> Value {
    let r = s
        .authed(Method::POST, &format!("/v1/me/links/{link}/frames"), p)
        .json(&json!({ "frames": frames }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body = json_of(r).await;
    assert_eq!(body["ok"], true);
    body
}

/// Read the next `request` frame on the device's stream.
async fn next_request(dev: &mut SseReader) -> Value {
    let ev = dev.next_event().await.expect("request frame");
    assert_eq!(ev.name, "request", "{ev:?}");
    ev.json()
}

// ---------------------------------------------------------------------------------------------
// Agent side helpers

/// A request to `/v1/agent/*` with the token and **no** gate header.
fn agent(s: &TestServer, method: Method, path: &str, token: &str) -> reqwest::RequestBuilder {
    s.http.request(method, s.url(path)).bearer_auth(token)
}

/// `POST /v1/agent/mcp`, JSON answer; returns the JSON-RPC response.
async fn rpc(s: &TestServer, token: &str, body: Value) -> Value {
    let r = agent(s, Method::POST, "/v1/agent/mcp", token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("application/json"));
    json_of(r).await
}

#[allow(clippy::needless_pass_by_value)]
fn call(id: i64, tool: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": tool, "arguments": arguments },
    })
}

/// A JSON-mode `tools/call` running in the background (its own client and connection).
fn spawn_call(s: &TestServer, token: &str, body: Value) -> tokio::task::JoinHandle<Value> {
    let url = s.url("/v1/agent/mcp");
    let token = token.to_owned();
    tokio::spawn(async move {
        let r = reqwest::Client::new()
            .post(url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        r.json::<Value>().await.unwrap()
    })
}

/// An SSE-mode `tools/call`: returns once the headers are in.
async fn sse_call(s: &TestServer, token: &str, body: Value) -> SseReader {
    let r = reqwest::Client::new()
        .post(s.url("/v1/agent/mcp"))
        .bearer_auth(token)
        .header("Accept", SSE_ACCEPT)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_sse_headers(&r);
    SseReader::new(r)
}

async fn status(s: &TestServer, token: &str) -> Value {
    let r = agent(s, Method::GET, "/v1/agent/status", token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    json_of(r).await
}

fn text_of(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text block: {result}"))
        .to_owned()
}

/// Poll `cond` for up to 5 s.
async fn eventually<F, Fut>(what: &str, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let until = Instant::now() + Duration::from_secs(5);
    while !cond().await {
        assert!(Instant::now() < until, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---------------------------------------------------------------------------------------------
// Links

#[tokio::test]
async fn links_register_upsert_revoke_and_reregister() {
    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let link = register_ok(&s, &p).await;
    let id = link["id"].as_str().unwrap().to_owned();
    assert_eq!(link["device_id"], p.device_id.as_str());
    assert_eq!(link["device_name"], DEVICE_NAME);
    assert_eq!(link["label"], DEVICE_NAME);
    assert_eq!(link["online"], false);
    assert_eq!(link["revoked"], false);
    assert_eq!(link["tools"], 3);
    assert!(link["last_seen_at"].is_null());
    assert!(link["created_at"].is_string());

    // Idempotent per device: same id; omitted fields keep their stored value.
    let again = json_of(
        register(
            &s,
            &p,
            json!({ "label": "Ada's phone", "tools": [tool("jev_run")] }),
        )
        .await,
    )
    .await;
    assert_eq!(again["id"], id.as_str());
    assert_eq!(again["label"], "Ada's phone");
    assert_eq!(again["tools"], 1);
    let kept = json_of(register(&s, &p, json!({})).await).await;
    assert_eq!(kept["id"], id.as_str());
    assert_eq!(kept["label"], "Ada's phone");
    assert_eq!(kept["tools"], 1);
    register_ok(&s, &p).await;

    let listed = list_links(&s, &p).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], id.as_str());
    assert_eq!(get_link(&s, &p, &id).await["tools"], 3);

    // Another user's link: 404; their list is their own.
    let bob = phone(&s, "bob@example.com").await;
    for m in [Method::GET, Method::DELETE] {
        let r = s
            .authed(m, &format!("/v1/me/links/{id}"), &bob)
            .send()
            .await
            .unwrap();
        expect_error(r, 404, "not_found").await;
    }
    assert_eq!(list_links(&s, &bob).await.len(), 0);

    // A link from a second device: its own id, label defaults to the device name.
    let mac = second_device(&s, "ada@example.com").await;
    let mac_link = json_of(register(&s, &mac, json!({})).await).await;
    assert_ne!(mac_link["id"], id.as_str());
    assert_eq!(mac_link["label"], "Other Mac");
    assert_eq!(mac_link["tools"], 0);
    assert_eq!(list_links(&s, &p).await.len(), 2);

    // Revoke (from any device of the user): gone from the list, still readable, idempotent.
    let r = s
        .authed(Method::DELETE, &format!("/v1/me/links/{id}"), &mac)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(json_of(r).await, json!({ "ok": true }));
    let listed = list_links(&s, &p).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], mac_link["id"]);
    let revoked = get_link(&s, &p, &id).await;
    assert_eq!(revoked["revoked"], true);
    assert_eq!(revoked["online"], false);
    let r = s
        .authed(Method::DELETE, &format!("/v1/me/links/{id}"), &p)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    // Registering again brings the same link back.
    let back = register_ok(&s, &p).await;
    assert_eq!(back["id"], id.as_str());
    assert_eq!(back["revoked"], false);
    assert_eq!(list_links(&s, &p).await.len(), 2);

    // Bad manifests: 400, and the stored one is untouched.
    let many: Vec<Value> = (0..101).map(|i| tool(&format!("t{i}"))).collect();
    for bad in [
        json!({ "tools": { "name": "jev_run" } }),
        json!({ "tools": ["jev_run"] }),
        json!({ "tools": [{ "description": "no name" }] }),
        json!({ "tools": [tool("jev_run"), tool("jev_run")] }),
        json!({ "tools": many }),
        json!({ "tools": [{ "name": "big", "description": "x".repeat(300 * 1024) }] }),
    ] {
        expect_error(register(&s, &p, bad).await, 400, "bad_request").await;
    }
    assert_eq!(get_link(&s, &p, &id).await["tools"], 3);
    // Small routes keep small body caps (only the frames upload takes 32 MiB).
    let r = s
        .authed(Method::POST, "/v1/me/agent-tokens", &p)
        .json(&json!({ "label": "x".repeat(20 * 1024) }))
        .send()
        .await
        .unwrap();
    expect_error(r, 413, "payload_too_large").await;

    // Gate + session like every user route.
    let r = s
        .http
        .get(s.url("/v1/me/links"))
        .bearer_auth(&p.token)
        .send()
        .await
        .unwrap();
    expect_error(r, 401, "instance_key").await;
    let r = s.req(Method::GET, "/v1/me/links").send().await.unwrap();
    expect_error(r, 401, "session").await;
}

#[tokio::test]
async fn frames_owner_only_hello_ping_and_superseded() {
    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let id = register_ok(&s, &p).await["id"].as_str().unwrap().to_owned();
    let path = format!("/v1/me/links/{id}/frames");

    // Only the link's own device: 403 for the user's other device, 404 for another user.
    let mac = second_device(&s, "ada@example.com").await;
    let r = s.authed(Method::GET, &path, &mac).send().await.unwrap();
    expect_error(r, 403, "forbidden").await;
    let r = s
        .authed(Method::POST, &path, &mac)
        .json(&json!({ "frames": [{ "type": "heartbeat" }] }))
        .send()
        .await
        .unwrap();
    expect_error(r, 403, "forbidden").await;
    let bob = phone(&s, "bob@example.com").await;
    let r = s.authed(Method::GET, &path, &bob).send().await.unwrap();
    expect_error(r, 404, "not_found").await;

    // `hello` first, with the link and the protocol.
    let (mut first, hello) = connect(&s, &p, &id).await;
    assert_eq!(hello["protocol"], 1);
    assert_eq!(hello["link"]["id"], id.as_str());
    assert_eq!(hello["link"]["online"], true);
    assert_eq!(hello["link"]["revoked"], false);
    assert_eq!(hello["link"]["tools"], 3);
    let link = get_link(&s, &p, &id).await;
    assert_eq!(link["online"], true);
    assert!(link["last_seen_at"].is_string());
    let ping = first.next().await.unwrap();
    assert_eq!((ping.name.as_str(), ping.data.as_str()), ("ping", "{}"));

    // A new stream supersedes the old one, which closes.
    let (mut second, _) = connect(&s, &p, &id).await;
    let ev = first.next_event().await.unwrap();
    assert_eq!((ev.name.as_str(), ev.data.as_str()), ("superseded", "{}"));
    assert!(first.next().await.is_none(), "superseded stream must end");
    assert_eq!(get_link(&s, &p, &id).await["online"], true);
    assert_eq!(second.next().await.unwrap().name, "ping");

    // Closing the current stream takes the link offline.
    drop(second);
    eventually("link offline after its stream closed", || {
        let (s, p, id) = (&s, &p, &id);
        async move { get_link(s, p, id).await["online"] == false }
    })
    .await;

    // Uploads: bad bodies are 400.
    for bad in ["{}", "[]", "{\"frames\": {}}", "nope"] {
        let r = s
            .authed(Method::POST, &path, &p)
            .body(bad)
            .send()
            .await
            .unwrap();
        expect_error(r, 400, "bad_request").await;
    }
}

// ---------------------------------------------------------------------------------------------
// Agent tokens and the gate

#[tokio::test]
async fn agent_tokens_mint_list_revoke() {
    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let minted = mint(&s, &p, "phi").await;
    let token = token_of(&minted);
    let token_id = minted["id"].as_str().unwrap().to_owned();
    assert!(
        token.starts_with("cat_") && token.len() == 4 + 43,
        "{token}"
    );
    assert_eq!(minted["label"], "phi");
    assert_eq!(minted["device_id"], p.device_id.as_str());
    assert!(minted["created_at"].is_string());
    assert_eq!(minted["mcp_url"], "https://localhost:8443/v1/agent/mcp");

    // No body: label "Agent".
    let r = s
        .authed(Method::POST, "/v1/me/agent-tokens", &p)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let other = json_of(r).await;
    assert_eq!(other["label"], "Agent");

    // The list: newest first, never the token.
    let r = s
        .authed(Method::GET, "/v1/me/agent-tokens", &p)
        .send()
        .await
        .unwrap();
    let list = json_of(r).await;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["id"], other["id"]);
    assert_eq!(list[1]["id"], token_id.as_str());
    assert!(list.iter().all(|t| t.get("token").is_none()));
    assert!(list[1]["last_used_at"].is_null());
    assert_eq!(list[1]["device_id"], p.device_id.as_str());

    // The token works without the gate header and names its device.
    let st = status(&s, &token).await;
    assert_eq!(st["device"]["id"], p.device_id.as_str());
    assert_eq!(st["device"]["name"], DEVICE_NAME);
    assert_eq!(st["online"], false);
    assert!(st["label"].is_null(), "never registered: {st}");
    assert_eq!(st["tools"], 0);
    assert!(st["last_seen_at"].is_null());
    let r = s
        .authed(Method::GET, "/v1/me/agent-tokens", &p)
        .send()
        .await
        .unwrap();
    let list = json_of(r).await;
    assert!(list[1]["last_used_at"].is_string(), "{list}");

    // Missing, malformed, unknown, or other credentials: 401 agent_token.
    let unknown = format!("cat_{}", copper_cloud_core::ids::random_token());
    for bad in [
        None,
        Some("cat_short"),
        Some(unknown.as_str()),
        Some(p.token.as_str()),
        Some(s.key()),
    ] {
        let mut rb = s.http.get(s.url("/v1/agent/status"));
        if let Some(t) = bad {
            rb = rb.bearer_auth(t);
        }
        let r = rb.send().await.unwrap();
        assert!(r.headers().contains_key("www-authenticate"));
        expect_error(r, 401, "agent_token").await;
    }

    // Another user cannot revoke it.
    let bob = phone(&s, "bob@example.com").await;
    let path = format!("/v1/me/agent-tokens/{token_id}");
    let r = s.authed(Method::DELETE, &path, &bob).send().await.unwrap();
    expect_error(r, 404, "not_found").await;
    status(&s, &token).await;

    // Revoked: 401 at once (status and MCP), idempotent, gone from the list.
    let r = s.authed(Method::DELETE, &path, &p).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(json_of(r).await, json!({ "ok": true }));
    let r = agent(&s, Method::GET, "/v1/agent/status", &token)
        .send()
        .await
        .unwrap();
    expect_error(r, 401, "agent_token").await;
    let r = agent(&s, Method::POST, "/v1/agent/mcp", &token)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
        .send()
        .await
        .unwrap();
    expect_error(r, 401, "agent_token").await;
    let r = s.authed(Method::DELETE, &path, &p).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let r = s
        .authed(Method::GET, "/v1/me/agent-tokens", &p)
        .send()
        .await
        .unwrap();
    assert_eq!(json_of(r).await.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn agent_routes_skip_the_gate_every_other_route_keeps_it() {
    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let id = register_ok(&s, &p).await["id"].as_str().unwrap().to_owned();
    let token = token_of(&mint(&s, &p, "phi").await);

    status(&s, &token).await;
    let pong = rpc(
        &s,
        &token,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
    )
    .await;
    assert_eq!(pong["result"], json!({}));
    // A wrong gate header does not matter here: the token is the credential.
    let r = agent(&s, Method::GET, "/v1/agent/status", &token)
        .header("X-Copper-Instance", "not-the-key")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = s
        .http
        .get(s.url("/v1/agent/nope"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    expect_error(r, 404, "not_found").await;

    // Every other /v1 route still needs the gate.
    let frames_path = format!("/v1/me/links/{id}/frames");
    for (m, path) in [
        (Method::GET, "/v1/me/links"),
        (Method::POST, "/v1/me/links"),
        (Method::GET, frames_path.as_str()),
        (Method::POST, frames_path.as_str()),
        (Method::GET, "/v1/me/agent-tokens"),
        (Method::POST, "/v1/me/agent-tokens"),
        (Method::GET, "/v1/info"),
        (Method::GET, "/v1/devices"),
        (Method::GET, "/v1/agents"),
        (Method::GET, "/v1/agentx"),
    ] {
        let r = s
            .http
            .request(m.clone(), s.url(path))
            .bearer_auth(&p.token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401, "{m} {path}");
        assert_eq!(json_of(r).await["error"], "instance_key", "{m} {path}");
    }
    // A cat_ token is not a session.
    let r = s
        .req(Method::GET, "/v1/me/links")
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    expect_error(r, 401, "session").await;

    let info = json_of(s.req(Method::GET, "/v1/info").send().await.unwrap()).await;
    assert!(
        info["features"]
            .as_array()
            .unwrap()
            .contains(&json!("device_agent")),
        "{info}"
    );
}

#[tokio::test]
async fn agent_rate_limit_is_per_token() {
    let s = start_with(|cfg| {
        tune(cfg);
        cfg.device_agent.agent_per_minute = 5;
    })
    .await;
    let p = phone(&s, "ada@example.com").await;
    let token_a = token_of(&mint(&s, &p, "a").await);
    let token_b = token_of(&mint(&s, &p, "b").await);
    for _ in 0..5 {
        status(&s, &token_a).await;
    }
    let resp = agent(&s, Method::GET, "/v1/agent/status", &token_a)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.headers()["retry-after"], "60");
    expect_error(resp, 429, "rate_limited").await;
    let resp = agent(&s, Method::POST, "/v1/agent/mcp", &token_a)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 429);
    status(&s, &token_b).await;
}

// ---------------------------------------------------------------------------------------------
// MCP

#[tokio::test]
async fn mcp_protocol_basics() {
    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let token = token_of(&mint(&s, &p, "phi").await);

    let init = rpc(
        &s,
        &token,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "phi", "version": "1.0" },
            },
        }),
    )
    .await;
    assert_eq!(init["jsonrpc"], "2.0");
    assert_eq!(init["id"], 1);
    let result = &init["result"];
    assert_eq!(result["protocolVersion"], "2025-03-26");
    assert_eq!(
        result["capabilities"],
        json!({ "tools": { "listChanged": false } })
    );
    assert_eq!(result["serverInfo"]["name"], "copper-mobile");
    assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    let instructions = result["instructions"].as_str().unwrap();
    for needle in [
        "iPhone",
        "jev_run",
        "DONE",
        "BLOCKED",
        "browser_snapshot",
        "open on their screen",
    ] {
        assert!(instructions.contains(needle), "{needle}: {instructions}");
    }
    let init = rpc(
        &s,
        &token,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize",
                "params": { "protocolVersion": "2099-01-01" } }),
    )
    .await;
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");

    // Notifications (and client responses): 202, empty body.
    for body in [
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 1 } }),
        json!({ "jsonrpc": "2.0", "id": 9, "result": {} }),
    ] {
        let r = agent(&s, Method::POST, "/v1/agent/mcp", &token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 202, "{body}");
        assert_eq!(r.text().await.unwrap(), "");
    }

    let pong = rpc(
        &s,
        &token,
        json!({ "jsonrpc": "2.0", "id": "a", "method": "ping" }),
    )
    .await;
    assert_eq!(pong, json!({ "jsonrpc": "2.0", "id": "a", "result": {} }));
    let unknown = rpc(
        &s,
        &token,
        json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/list" }),
    )
    .await;
    assert_eq!(unknown["id"], 3);
    assert_eq!(unknown["error"]["code"], -32601);
    let no_name = rpc(
        &s,
        &token,
        json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {} }),
    )
    .await;
    assert_eq!(no_name["error"]["code"], -32602);

    // Batches, bad JSON, non-objects: 400 with a JSON-RPC error.
    for (body, code) in [
        (r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#, -32600),
        ("not json", -32700),
        ("42", -32600),
        (r#"{"jsonrpc":"2.0","id":1}"#, -32600),
    ] {
        let r = agent(&s, Method::POST, "/v1/agent/mcp", &token)
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "{body}");
        assert_eq!(json_of(r).await["error"]["code"], code, "{body}");
    }

    // GET and DELETE: 405.
    for m in [Method::GET, Method::DELETE] {
        let r = agent(&s, m.clone(), "/v1/agent/mcp", &token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 405, "{m}");
        assert_eq!(r.headers()["allow"], "POST");
    }
    let r = s.http.get(s.url("/v1/agent/mcp")).send().await.unwrap();
    expect_error(r, 401, "agent_token").await;

    // tools/list: empty before the device registers, then its manifest even while offline.
    let list = rpc(
        &s,
        &token,
        json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list" }),
    )
    .await;
    assert_eq!(list["result"], json!({ "tools": [] }));
    register_ok(&s, &p).await;
    let list = rpc(
        &s,
        &token,
        json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/list", "params": {} }),
    )
    .await;
    assert_eq!(list["result"]["tools"], manifest());
    let st = status(&s, &token).await;
    assert_eq!(st["online"], false);
    assert_eq!(st["tools"], 3);
    assert_eq!(st["label"], DEVICE_NAME);
}

#[tokio::test]
async fn tools_call_answers_at_once_when_it_cannot_reach_the_device() {
    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let token = token_of(&mint(&s, &p, "phi").await);

    // Never registered: turned off.
    let r = rpc(&s, &token, call(1, "jev_run", json!({}))).await;
    assert_eq!(r["id"], 1);
    assert_eq!(r["result"]["isError"], true);
    let text = text_of(&r["result"]);
    assert!(
        text.contains("turned off") && text.contains(DEVICE_NAME),
        "{text}"
    );

    // Registered, Copper not open: immediate, names the device.
    let id = register_ok(&s, &p).await["id"].as_str().unwrap().to_owned();
    let started = Instant::now();
    let r = rpc(&s, &token, call(2, "jev_run", json!({ "goal": "x" }))).await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "offline must not wait"
    );
    assert_eq!(r["result"]["isError"], true);
    assert_eq!(
        text_of(&r["result"]),
        format!(
            "Copper isn't open on {DEVICE_NAME}. Ask the person to open Copper on their iPhone and \
             keep it on screen (Settings › Agents › Let agents use this iPhone must be on), then \
             try again."
        )
    );
    // The same as an SSE answer: one message, then the stream ends.
    let mut answer = sse_call(&s, &token, call(3, "jev_run", json!({}))).await;
    let ev = answer.next().await.unwrap();
    assert_eq!(ev.name, "message");
    let msg = ev.json();
    assert_eq!(msg["id"], 3);
    assert!(text_of(&msg["result"]).starts_with("Copper isn't open on"));
    assert!(answer.next().await.is_none());

    // A tool the device does not have: named, offline or online.
    let r = rpc(&s, &token, call(4, "nope_tool", json!({}))).await;
    assert_eq!(r["result"]["isError"], true);
    assert!(text_of(&r["result"]).contains("\"nope_tool\""));
    let (mut dev, _) = connect(&s, &p, &id).await;
    let r = rpc(&s, &token, call(5, "other_tool", json!({}))).await;
    assert!(text_of(&r["result"]).contains("\"other_tool\""));
    // Nothing reached the device.
    match dev.next_within(Duration::from_millis(300)).await {
        Err(()) => {}
        Ok(Some(ev)) => assert_eq!(ev.name, "ping", "{ev:?}"),
        Ok(None) => panic!("the device's stream ended"),
    }
    // Arguments that are not an object.
    let r = rpc(&s, &token, call(6, "jev_run", json!("go"))).await;
    assert_eq!(r["error"]["code"], -32602);

    // Revoked: turned off.
    let r = s
        .authed(Method::DELETE, &format!("/v1/me/links/{id}"), &p)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(dev.next_event().await.unwrap().name, "revoked");
    let r = rpc(&s, &token, call(7, "jev_run", json!({}))).await;
    assert!(text_of(&r["result"]).contains("turned off"));
}

#[tokio::test]
async fn relayed_call_json_round_trip() {
    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let id = register_ok(&s, &p).await["id"].as_str().unwrap().to_owned();
    let minted = mint(&s, &p, "phi").await;
    let token = token_of(&minted);
    let (mut dev, _) = connect(&s, &p, &id).await;
    assert_eq!(status(&s, &token).await["online"], true);

    let args =
        json!({ "goal": "Find flights to Lisbon on 2026-11-20", "url": "https://example.com" });
    let pending = spawn_call(&s, &token, call(7, "jev_run", args.clone()));
    let req = next_request(&mut dev).await;
    let rid = req["id"].as_str().unwrap().to_owned();
    assert!(rid.starts_with("req_"), "{rid}");
    assert_eq!(req["method"], "tools/call");
    assert_eq!(
        req["params"],
        json!({ "name": "jev_run", "arguments": args })
    );
    assert_eq!(
        req["caller"],
        json!({ "token_id": minted["id"], "label": "phi" })
    );
    assert_eq!(req["deadline_ms"], 1500);

    // Replies for unknown requests are accepted and ignored; unknown types are skipped.
    let ack = post_frames(
        &s,
        &p,
        &id,
        json!([
            { "type": "reply", "id": "req_unknown", "result": { "content": [] } },
            { "type": "heartbeat" },
            { "type": "log", "level": "info", "message": "starting" },
            { "type": "bogus" },
            { "no": "type" },
        ]),
    )
    .await;
    assert_eq!(ack["accepted"], 3);

    let result = json!({
        "content": [{ "type": "text", "text": "DONE: 3 flights" }],
        "isError": false,
        "structuredContent": { "flights": 3 },
    });
    let ack = post_frames(
        &s,
        &p,
        &id,
        json!([{ "type": "reply", "id": rid, "result": result }]),
    )
    .await;
    assert_eq!(ack["accepted"], 1);
    let answer = pending.await.unwrap();
    assert_eq!(
        answer,
        json!({ "jsonrpc": "2.0", "id": 7, "result": result })
    );
    // A late duplicate is ignored.
    post_frames(
        &s,
        &p,
        &id,
        json!([{ "type": "reply", "id": rid, "result": result }]),
    )
    .await;

    // A reply `error` becomes isError with its message.
    let pending = spawn_call(&s, &token, call(8, "browser_snapshot", json!({})));
    let rid = next_request(&mut dev).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    post_frames(
        &s,
        &p,
        &id,
        json!([{ "type": "reply", "id": rid,
                 "error": { "code": "tab_gone", "message": "The tab was closed" } }]),
    )
    .await;
    let answer = pending.await.unwrap();
    assert_eq!(answer["id"], 8);
    assert_eq!(answer["result"]["isError"], true);
    assert_eq!(text_of(&answer["result"]), "The tab was closed");

    // A progress token on a JSON answer: progress is not delivered, the call still completes.
    let mut body = call(9, "jev_observe", json!({}));
    body["params"]["_meta"] = json!({ "progressToken": 5 });
    let pending = spawn_call(&s, &token, body);
    let rid = next_request(&mut dev).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    post_frames(
        &s,
        &p,
        &id,
        json!([
            { "type": "progress", "id": rid, "seq": 1, "message": "1 · OBSERVE" },
            { "type": "reply", "id": rid, "result": { "content": [], "isError": false } },
        ]),
    )
    .await;
    assert_eq!(
        pending.await.unwrap(),
        json!({ "jsonrpc": "2.0", "id": 9, "result": { "content": [], "isError": false } })
    );
}

#[tokio::test]
async fn relayed_call_sse_answer_with_progress() {
    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let id = register_ok(&s, &p).await["id"].as_str().unwrap().to_owned();
    let token = token_of(&mint(&s, &p, "phi").await);
    let (mut dev, _) = connect(&s, &p, &id).await;

    let mut body = call(10, "jev_run", json!({ "goal": "Search for lamps" }));
    body["params"]["_meta"] = json!({ "progressToken": "tok-1" });
    let mut answer = sse_call(&s, &token, body).await;
    let rid = next_request(&mut dev).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    post_frames(
        &s,
        &p,
        &id,
        json!([
            { "type": "progress", "id": rid, "seq": 1, "message": "1 · OBSERVE" },
            { "type": "progress", "id": rid, "seq": 2, "message": "2 · CLICK [12] button \"Search\"" },
        ]),
    )
    .await;
    // Stale or repeated seqs are ignored.
    post_frames(
        &s,
        &p,
        &id,
        json!([
            { "type": "progress", "id": rid, "seq": 2, "message": "dup" },
            { "type": "progress", "id": rid, "seq": 1, "message": "old" },
        ]),
    )
    .await;
    for (seq, message) in [(1, "1 · OBSERVE"), (2, "2 · CLICK [12] button \"Search\"")] {
        let ev = answer.next().await.unwrap();
        assert_eq!(ev.name, "message");
        assert_eq!(
            ev.json(),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": { "progressToken": "tok-1", "progress": seq, "message": message },
            })
        );
    }
    // Idle: keepalive comments (every 200 ms in this suite).
    tokio::time::sleep(Duration::from_millis(700)).await;
    post_frames(
        &s,
        &p,
        &id,
        json!([{ "type": "reply", "id": rid,
                 "result": { "content": [{ "type": "text", "text": "DONE" }], "isError": false } }]),
    )
    .await;
    let ev = answer.next().await.unwrap();
    assert_eq!(ev.name, "message");
    assert_eq!(
        ev.json(),
        json!({ "jsonrpc": "2.0", "id": 10,
                "result": { "content": [{ "type": "text", "text": "DONE" }], "isError": false } })
    );
    assert!(
        answer.next().await.is_none(),
        "the stream ends after the response"
    );
    assert!(
        answer.comments >= 1,
        "keepalive comments: {}",
        answer.comments
    );

    // Without a progress token, progress frames are not forwarded. A pretty-printed reply
    // still arrives as one `data:` line.
    let mut answer = sse_call(&s, &token, call(11, "jev_run", json!({}))).await;
    let rid = next_request(&mut dev).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    post_frames(
        &s,
        &p,
        &id,
        json!([{ "type": "progress", "id": rid, "seq": 1, "message": "1 · OBSERVE" }]),
    )
    .await;
    let pretty = format!(
        "{{\"frames\": [{{\"type\": \"reply\", \"id\": \"{rid}\", \"result\": {{\n  \"content\": [],\n  \"isError\": false\n}}}}]}}"
    );
    let r = s
        .authed(Method::POST, &format!("/v1/me/links/{id}/frames"), &p)
        .body(pretty)
        .send()
        .await
        .unwrap();
    assert_eq!(json_of(r).await["accepted"], 1);
    let ev = answer.next().await.unwrap();
    assert!(!ev.data.contains('\n'), "{:?}", ev.data);
    assert_eq!(
        ev.json(),
        json!({ "jsonrpc": "2.0", "id": 11, "result": { "content": [], "isError": false } })
    );
    assert!(answer.next().await.is_none());
}

#[tokio::test]
async fn deadline_cancels_the_call_on_the_device() {
    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let id = register_ok(&s, &p).await["id"].as_str().unwrap().to_owned();
    let token = token_of(&mint(&s, &p, "phi").await);
    let (mut dev, _) = connect(&s, &p, &id).await;

    // JSON answer: the device gets `cancel` at the deadline, the agent "timed out".
    let started = Instant::now();
    let pending = spawn_call(&s, &token, call(12, "jev_run", json!({})));
    let rid = next_request(&mut dev).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Heartbeats keep the device online; nothing answers.
    post_frames(&s, &p, &id, json!([{ "type": "heartbeat" }])).await;
    let ev = dev.next_event().await.unwrap();
    assert_eq!(ev.name, "cancel");
    assert_eq!(ev.json(), json!({ "id": rid }));
    let answer = pending.await.unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(1400) && elapsed < Duration::from_secs(5),
        "{elapsed:?}"
    );
    assert_eq!(answer["id"], 12);
    assert_eq!(answer["result"]["isError"], true);
    assert!(text_of(&answer["result"]).contains("timed out"));
    // A reply after the deadline is ignored.
    post_frames(
        &s,
        &p,
        &id,
        json!([{ "type": "reply", "id": rid, "result": { "content": [] } }]),
    )
    .await;

    // SSE answer: keepalives while waiting, then the timed-out response.
    let mut answer = sse_call(&s, &token, call(13, "jev_run", json!({}))).await;
    let rid = next_request(&mut dev).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ev = dev.next_event().await.unwrap();
    assert_eq!(
        (ev.name.as_str(), ev.json()),
        ("cancel", json!({ "id": rid }))
    );
    let msg = answer.next().await.unwrap().json();
    assert_eq!(msg["id"], 13);
    assert!(text_of(&msg["result"]).contains("timed out"));
    assert!(answer.comments >= 3, "keepalives: {}", answer.comments);
    assert!(answer.next().await.is_none());
}

#[tokio::test]
async fn agent_going_away_cancels_the_call_on_the_device() {
    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let id = register_ok(&s, &p).await["id"].as_str().unwrap().to_owned();
    let token = token_of(&mint(&s, &p, "phi").await);
    let (mut dev, _) = connect(&s, &p, &id).await;

    // JSON answer: the agent's request is dropped mid-call.
    let pending = spawn_call(&s, &token, call(14, "jev_run", json!({})));
    let rid = next_request(&mut dev).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    pending.abort();
    let ev = dev.next_event().await.unwrap();
    assert_eq!(
        (ev.name.as_str(), ev.json()),
        ("cancel", json!({ "id": rid }))
    );

    // SSE answer: the agent closes the stream.
    let answer = sse_call(&s, &token, call(15, "jev_run", json!({}))).await;
    let rid = next_request(&mut dev).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    drop(answer);
    let ev = dev.next_event().await.unwrap();
    assert_eq!(
        (ev.name.as_str(), ev.json()),
        ("cancel", json!({ "id": rid }))
    );
    // The device's late reply is ignored.
    post_frames(
        &s,
        &p,
        &id,
        json!([{ "type": "reply", "id": rid, "result": { "content": [] } }]),
    )
    .await;
}

#[tokio::test]
async fn revoking_the_link_closes_its_stream_and_fails_the_call() {
    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let id = register_ok(&s, &p).await["id"].as_str().unwrap().to_owned();
    let token = token_of(&mint(&s, &p, "phi").await);
    let (mut dev, _) = connect(&s, &p, &id).await;

    let pending = spawn_call(&s, &token, call(16, "jev_run", json!({})));
    next_request(&mut dev).await;
    let mac = second_device(&s, "ada@example.com").await;
    let r = s
        .authed(Method::DELETE, &format!("/v1/me/links/{id}"), &mac)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let ev = dev.next_event().await.unwrap();
    assert_eq!((ev.name.as_str(), ev.data.as_str()), ("revoked", "{}"));
    assert!(dev.next().await.is_none(), "revoked stream must end");
    let answer = pending.await.unwrap();
    assert_eq!(answer["result"]["isError"], true);
    assert!(text_of(&answer["result"]).contains("turned off"));
    let r = rpc(&s, &token, call(17, "jev_run", json!({}))).await;
    assert!(text_of(&r["result"]).contains("turned off"));
    assert_eq!(status(&s, &token).await["online"], false);

    // Opening the revoked link's stream: hello (revoked), then revoked, then the end.
    let (mut dev, hello) = connect(&s, &p, &id).await;
    assert_eq!(hello["link"]["revoked"], true);
    assert_eq!(hello["link"]["online"], false);
    assert_eq!(dev.next_event().await.unwrap().name, "revoked");
    assert!(dev.next().await.is_none());

    // Registering again turns it back on.
    register_ok(&s, &p).await;
    let (_dev, hello) = connect(&s, &p, &id).await;
    assert_eq!(hello["link"]["online"], true);
    assert_eq!(status(&s, &token).await["online"], true);
}

#[tokio::test]
async fn revoking_a_token_fails_its_call_in_flight() {
    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let id = register_ok(&s, &p).await["id"].as_str().unwrap().to_owned();
    let minted = mint(&s, &p, "phi").await;
    let token = token_of(&minted);
    let (mut dev, _) = connect(&s, &p, &id).await;

    let pending = spawn_call(&s, &token, call(18, "jev_run", json!({})));
    let rid = next_request(&mut dev).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = s
        .authed(
            Method::DELETE,
            &format!("/v1/me/agent-tokens/{}", minted["id"].as_str().unwrap()),
            &p,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let ev = dev.next_event().await.unwrap();
    assert_eq!(
        (ev.name.as_str(), ev.json()),
        ("cancel", json!({ "id": rid }))
    );
    let answer = pending.await.unwrap();
    assert_eq!(answer["result"]["isError"], true);
    assert!(text_of(&answer["result"]).contains("revoked"));
}

#[tokio::test]
async fn deleting_the_device_revokes_its_link_and_tokens() {
    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let id = register_ok(&s, &p).await["id"].as_str().unwrap().to_owned();
    let token = token_of(&mint(&s, &p, "phi").await);
    let (mut dev, _) = connect(&s, &p, &id).await;

    let mac = second_device(&s, "ada@example.com").await;
    let r = s
        .authed(
            Method::DELETE,
            &format!("/v1/devices/{}", p.device_id),
            &mac,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(dev.next_event().await.unwrap().name, "revoked");
    let r = agent(&s, Method::GET, "/v1/agent/status", &token)
        .send()
        .await
        .unwrap();
    expect_error(r, 401, "agent_token").await;
    let link = get_link(&s, &mac, &id).await;
    assert_eq!(link["revoked"], true);
    assert_eq!(link["device_name"], DEVICE_NAME, "falls back to the label");
    assert_eq!(list_links(&s, &mac).await.len(), 0);
    let r = s
        .authed(Method::GET, "/v1/me/agent-tokens", &mac)
        .send()
        .await
        .unwrap();
    assert_eq!(json_of(r).await, json!([]));
}

#[tokio::test]
async fn large_frames_bodies_are_accepted() {
    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let id = register_ok(&s, &p).await["id"].as_str().unwrap().to_owned();
    let token = token_of(&mint(&s, &p, "phi").await);
    let (mut dev, _) = connect(&s, &p, &id).await;

    let pending = spawn_call(&s, &token, call(19, "browser_snapshot", json!({})));
    let rid = next_request(&mut dev).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let screenshot = "A".repeat(20 * 1024 * 1024);
    let ack = post_frames(
        &s,
        &p,
        &id,
        json!([{ "type": "reply", "id": rid, "result": {
            "content": [{ "type": "image", "mimeType": "image/png", "data": screenshot }],
            "isError": false,
        } }]),
    )
    .await;
    assert_eq!(ack["accepted"], 1);
    let answer = pending.await.unwrap();
    assert_eq!(
        answer["result"]["content"][0]["data"]
            .as_str()
            .unwrap()
            .len(),
        20 * 1024 * 1024
    );
}

#[tokio::test]
async fn online_needs_heartbeats_and_a_closed_stream_fails_the_call() {
    let s = start_with(|cfg| {
        tune(cfg);
        cfg.device_agent.online_window = Duration::from_millis(800);
    })
    .await;
    let p = phone(&s, "ada@example.com").await;
    let id = register_ok(&s, &p).await["id"].as_str().unwrap().to_owned();
    let token = token_of(&mint(&s, &p, "phi").await);
    let (dev, _) = connect(&s, &p, &id).await;
    assert_eq!(status(&s, &token).await["online"], true);

    // Stream open but silent past the window: offline (and calls answer at once).
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(status(&s, &token).await["online"], false);
    let r = rpc(&s, &token, call(20, "jev_run", json!({}))).await;
    assert!(text_of(&r["result"]).starts_with("Copper isn't open on"));
    // A heartbeat brings it back.
    let ack = post_frames(&s, &p, &id, json!([{ "type": "heartbeat" }])).await;
    assert_eq!(ack["accepted"], 1);
    assert_eq!(status(&s, &token).await["online"], true);
    assert!(status(&s, &token).await["last_seen_at"].is_string());

    // Copper leaves the screen mid-call: the call fails after the offline grace, not the
    // whole deadline.
    let mut dev = dev;
    let pending = spawn_call(&s, &token, call(21, "jev_run", json!({})));
    next_request(&mut dev).await;
    let started = Instant::now();
    drop(dev);
    let answer = pending.await.unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(1400),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(answer["result"]["isError"], true);
    assert!(
        text_of(&answer["result"]).contains("Copper closed on"),
        "{answer}"
    );
}

/// Collects everything a tracing subscriber writes.
#[derive(Clone, Default)]
struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[tokio::test]
async fn one_log_line_per_call_never_arguments_or_results() {
    // With a single registered dispatcher, tracing caches a callsite's interest from whichever
    // thread hits it first — another test's thread, with no subscriber, caches "never". A
    // second live dispatcher makes interest the union of both, so this thread sees everything.
    let _union = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_writer(std::io::sink)
            .finish(),
    );
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_current_span(false)
        .with_span_list(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(captured.clone())
        .finish();
    // `#[tokio::test]` runs the server's tasks on this thread, so a thread-local subscriber
    // sees them.
    let _default = tracing::subscriber::set_default(subscriber);

    let s = start().await;
    let p = phone(&s, "ada@example.com").await;
    let id = register_ok(&s, &p).await["id"].as_str().unwrap().to_owned();
    let minted = mint(&s, &p, "phi").await;
    let token = token_of(&minted);
    // Offline first, then a relayed call.
    rpc(
        &s,
        &token,
        call(1, "jev_run", json!({ "goal": "secret-argument-1" })),
    )
    .await;
    let (mut dev, _) = connect(&s, &p, &id).await;
    let pending = spawn_call(
        &s,
        &token,
        call(2, "jev_run", json!({ "goal": "secret-argument-2" })),
    );
    let rid = next_request(&mut dev).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    post_frames(
        &s,
        &p,
        &id,
        json!([
            { "type": "progress", "id": rid, "seq": 1, "message": "secret-progress" },
            { "type": "reply", "id": rid,
              "result": { "content": [{ "type": "text", "text": "secret-result" }], "isError": false } },
        ]),
    )
    .await;
    pending.await.unwrap();

    let out = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    for secret in [
        "secret-argument",
        "secret-progress",
        "secret-result",
        token.as_str(),
    ] {
        assert!(!out.contains(secret), "{secret} in the log:\n{out}");
    }
    let calls: Vec<Value> = out
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["message"] == "agent call" && v["token_id"] == minted["id"])
        .collect();
    assert_eq!(calls.len(), 2, "{out}");
    for (line, status) in calls.iter().zip(["offline", "ok"]) {
        assert_eq!(line["status"], status, "{line}");
        assert_eq!(line["token_id"], minted["id"], "{line}");
        assert_eq!(line["device_id"], p.device_id.as_str(), "{line}");
        assert_eq!(line["tool"], "jev_run", "{line}");
        assert!(line["ms"].is_u64(), "{line}");
    }
}
