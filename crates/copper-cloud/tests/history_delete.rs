//! `DELETE /v1/sync/history` (0.8.0): delete by `seqs` (picked by the user on their client) or
//! wholesale by metadata (`since` / `until` / `device`), scoping, errors, the `history_deleted`
//! event, pull cursors after a delete, `features` in `/v1/info`, `copper-cloud admin history
//! delete` (library path and the real binary) — and that no delete path depends on (or opens)
//! history payloads.

// Scenario tests read best as one linear story with short names (s = server, a/b = sessions).
#![allow(clippy::too_many_lines, clippy::many_single_char_names)]

mod common;

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use common::{new_device, session_from, start, Session, TestServer, PASSWORD};
use copper_cloud_core::crypto::Crypto;
use copper_cloud_core::sync::{HISTORY_DELETE_BODY_LIMIT, MAX_HISTORY_DELETE_SEQS};
use futures::StreamExt as _;
use reqwest::Method;
use serde_json::{json, Value};
use uuid::Uuid;

/// No urls (`assert_eq!` against this shows what was left on failure).
const NONE: [&str; 0] = [];

const HOUR: [&str; 6] = [
    "2026-10-01T10:00:00Z",
    "2026-10-01T11:00:00Z",
    "2026-10-01T12:00:00Z",
    "2026-10-01T13:00:00Z",
    "2026-10-01T14:00:00Z",
    "2026-10-01T15:00:00Z",
];

/// Push `(url, visited_at)` visits; returns the highest `seq`.
async fn push(s: &TestServer, who: &Session, visits: &[(&str, &str)]) -> i64 {
    let entries: Vec<Value> = visits
        .iter()
        .map(|(url, at)| json!({ "url": url, "title": "t", "visited_at": at }))
        .collect();
    let r = s
        .authed(Method::POST, "/v1/sync/history", who)
        .json(&json!({ "entries": entries }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["inserted"], entries.len(), "{v}");
    v["seq"].as_i64().unwrap()
}

/// `DELETE /v1/sync/history?<query>` with an optional JSON body → (status, body).
async fn delete(s: &TestServer, who: &Session, query: &str, body: Option<Value>) -> (u16, Value) {
    let path = if query.is_empty() {
        "/v1/sync/history".to_owned()
    } else {
        format!("/v1/sync/history?{query}")
    };
    let mut req = s.authed(Method::DELETE, &path, who);
    if let Some(body) = body {
        req = req.json(&body);
    }
    let r = req.send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap())
}

/// A filter `DELETE` (no body) that must succeed; returns `deleted`.
async fn deleted(s: &TestServer, who: &Session, query: &str) -> i64 {
    let (status, v) = delete(s, who, query, None).await;
    assert_eq!(status, 200, "{query}: {v}");
    v["deleted"].as_i64().unwrap()
}

/// A `{"seqs": …}` `DELETE` that must succeed; returns `deleted`.
async fn deleted_seqs(s: &TestServer, who: &Session, seqs: &[i64]) -> i64 {
    let (status, v) = delete(s, who, "", Some(json!({ "seqs": seqs }))).await;
    assert_eq!(status, 200, "{seqs:?}: {v}");
    v["deleted"].as_i64().unwrap()
}

/// One pull page: ((seq, url) rows, next, more).
async fn pull(
    s: &TestServer,
    who: &Session,
    since: i64,
    limit: usize,
) -> (Vec<(i64, String)>, i64, bool) {
    let page: Value = s
        .authed(
            Method::GET,
            &format!("/v1/sync/history?since={since}&limit={limit}"),
            who,
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["seq"].as_i64().unwrap(),
                e["payload"]["url"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    (
        rows,
        page["next"].as_i64().unwrap(),
        page["more"].as_bool().unwrap(),
    )
}

/// Every (seq, url) the user has on the server, in `seq` order.
async fn history(s: &TestServer, who: &Session) -> Vec<(i64, String)> {
    let (rows, _, more) = pull(s, who, 0, 2000).await;
    assert!(!more);
    rows
}

/// Every url the user has on the server, in `seq` order.
async fn urls(s: &TestServer, who: &Session) -> Vec<String> {
    history(s, who).await.into_iter().map(|(_, u)| u).collect()
}

/// The client-side selection a Copper does: seqs of the rows whose url contains `needle`.
async fn pick(s: &TestServer, who: &Session, needle: &str) -> Vec<i64> {
    history(s, who)
        .await
        .into_iter()
        .filter(|(_, u)| u.contains(needle))
        .map(|(seq, _)| seq)
        .collect()
}

async fn rows(s: &TestServer, who: &Session) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM history WHERE user_id = $1::uuid")
        .bind(&who.user_id)
        .fetch_one(&s.state.db)
        .await
        .unwrap()
}

async fn second_device(s: &TestServer, email: &str, device: &str) -> Session {
    session_from(&s.login(email, PASSWORD, device).await.json().await.unwrap())
}

#[tokio::test]
async fn delete_by_seqs_only_touches_the_callers_rows() {
    let s = start().await;
    let (dev_a, dev_b) = (new_device(), new_device());
    let a = s.signup("del-seqs@example.com", PASSWORD, &dev_a).await;
    let a2 = second_device(&s, "del-seqs@example.com", &dev_b).await;
    let b = s
        .signup("del-seqs-b@example.com", PASSWORD, &new_device())
        .await;
    push(
        &s,
        &a,
        &[
            ("https://x.com/1", HOUR[0]),
            ("https://y.com/2", HOUR[1]),
            ("https://m.x.com/3", HOUR[2]),
        ],
    )
    .await;
    push(&s, &a2, &[("https://x.com/4", HOUR[3])]).await;
    push(
        &s,
        &b,
        &[("https://x.com/b1", HOUR[0]), ("https://x.com/b2", HOUR[1])],
    )
    .await;

    // The client picks the rows itself (here: everything on x.com, from any device) and sends
    // their seqs. Exact response shape.
    let mine = pick(&s, &a, "x.com").await;
    assert_eq!(mine.len(), 3);
    let (status, v) = delete(&s, &a, "", Some(json!({ "seqs": mine }))).await;
    assert_eq!(status, 200);
    assert_eq!(v, json!({ "deleted": 3 }));
    assert_eq!(urls(&s, &a).await, ["https://y.com/2"]);

    // Another user's seqs (and unknown / duplicate / non-positive ones) are silently ignored.
    let theirs = pick(&s, &b, "").await;
    assert_eq!(theirs.len(), 2);
    let mut mixed = theirs.clone();
    mixed.extend([theirs[0], 0, -1, i64::MAX]);
    assert_eq!(deleted_seqs(&s, &a, &mixed).await, 0);
    assert_eq!(urls(&s, &b).await, ["https://x.com/b1", "https://x.com/b2"]);
    // A list with one of mine among theirs deletes just mine; repeating it is harmless.
    let y = pick(&s, &a, "y.com").await;
    mixed.extend(&y);
    assert_eq!(deleted_seqs(&s, &a, &mixed).await, 1);
    assert_eq!(deleted_seqs(&s, &a, &mixed).await, 0);
    assert_eq!(rows(&s, &a).await, 0);
    assert_eq!(rows(&s, &b).await, 2);
    // An empty list deletes nothing (it never means "everything").
    assert_eq!(deleted_seqs(&s, &b, &[]).await, 0);
    assert_eq!(rows(&s, &b).await, 2);
    // Any device of the user may delete any of the user's rows.
    push(&s, &a, &[("https://z.com/5", HOUR[4])]).await;
    let z = pick(&s, &a2, "z.com").await;
    assert_eq!(deleted_seqs(&s, &a2, &z).await, 1);
    assert_eq!(rows(&s, &a).await, 0);
}

#[tokio::test]
async fn delete_seq_cap_and_body_errors() {
    let s = start().await;
    let a = s
        .signup("del-cap@example.com", PASSWORD, &new_device())
        .await;
    push(
        &s,
        &a,
        &[("https://x.com/1", HOUR[0]), ("https://y.com/1", HOUR[1])],
    )
    .await;
    let first = pick(&s, &a, "x.com").await;

    // Exactly the cap is fine; one more is a 400 and deletes nothing.
    let cap = i64::try_from(MAX_HISTORY_DELETE_SEQS).unwrap();
    // Seqs are positive, so the padding matches nothing.
    let mut at_cap: Vec<i64> = (-(cap - 1)..0).collect();
    at_cap.extend(&first);
    assert_eq!(at_cap.len(), MAX_HISTORY_DELETE_SEQS);
    let over: Vec<i64> = (1..=cap + 1).collect();
    let (status, v) = delete(&s, &a, "", Some(json!({ "seqs": over }))).await;
    assert_eq!(status, 400, "{v}");
    assert_eq!(v["error"], "bad_request");
    assert!(
        v["message"].as_str().unwrap().contains("at most 5000"),
        "{v}"
    );
    assert_eq!(rows(&s, &a).await, 2);
    assert_eq!(deleted_seqs(&s, &a, &at_cap).await, 1);
    assert_eq!(urls(&s, &a).await, ["https://y.com/1"]);

    // Seqs together with since/until/device: ambiguous, so 400 (nothing deleted).
    let y = pick(&s, &a, "y.com").await;
    for query in [
        format!("since={}", HOUR[0]),
        format!("until={}", HOUR[5]),
        format!("device={}", a.device_id),
    ] {
        let (status, v) = delete(&s, &a, &query, Some(json!({ "seqs": y }))).await;
        assert_eq!(status, 400, "{query}: {v}");
        assert_eq!(v["error"], "bad_request");
        assert!(v["message"].as_str().unwrap().contains("not both"), "{v}");
    }
    assert_eq!(rows(&s, &a).await, 1);

    // Malformed bodies are 400s, never "delete everything".
    for body in [
        "{}",
        "null",
        "[1]",
        r#"{"seqs":null}"#,
        r#"{"seqs":"1"}"#,
        r#"{"seqs":[1.5]}"#,
        r#"{"seq":[1]}"#,
        r#"{"seqs":[1],"host":"y.com"}"#,
        "seqs=1",
    ] {
        let r = s
            .authed(Method::DELETE, "/v1/sync/history", &a)
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "{body}");
        assert_eq!(r.json::<Value>().await.unwrap()["error"], "bad_request");
    }
    assert_eq!(rows(&s, &a).await, 1);

    // Body size cap: 413 (by Content-Length, and while streaming without one).
    let huge = format!(r#"{{"seqs":[1{}]}}"#, " ".repeat(HISTORY_DELETE_BODY_LIMIT));
    let r = s
        .authed(Method::DELETE, "/v1/sync/history", &a)
        .body(huge.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"],
        "payload_too_large"
    );
    let chunks = futures::stream::iter(
        huge.into_bytes()
            .chunks(64 * 1024)
            .map(|c| Ok::<_, std::io::Error>(c.to_vec()))
            .collect::<Vec<_>>(),
    );
    let r = s
        .authed(Method::DELETE, "/v1/sync/history", &a)
        .body(reqwest::Body::wrap_stream(chunks))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    assert_eq!(rows(&s, &a).await, 1);

    // A whitespace-only body is no body: delete everything.
    let r = s
        .authed(Method::DELETE, "/v1/sync/history", &a)
        .body(" \n")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap(), json!({ "deleted": 1 }));
}

#[tokio::test]
async fn delete_all_leaves_other_users_alone() {
    let s = start().await;
    let (dev_a, dev_b) = (new_device(), new_device());
    let a = s.signup("del-all@example.com", PASSWORD, &dev_a).await;
    let a2 = second_device(&s, "del-all@example.com", &dev_b).await;
    let b = s
        .signup("del-other@example.com", PASSWORD, &new_device())
        .await;
    push(
        &s,
        &a,
        &[("https://a.com/1", HOUR[0]), ("https://a.com/2", HOUR[1])],
    )
    .await;
    push(&s, &a2, &[("https://a.com/3", HOUR[2])]).await;
    push(
        &s,
        &b,
        &[("https://b.com/1", HOUR[0]), ("https://a.com/1", HOUR[0])],
    )
    .await;

    // Nothing given: everything of the caller's, from every device. Exact response shape.
    let (status, v) = delete(&s, &a, "", None).await;
    assert_eq!(status, 200);
    assert_eq!(v, json!({ "deleted": 3 }));
    assert_eq!(urls(&s, &a).await, NONE);
    assert_eq!(urls(&s, &a2).await, NONE);
    assert_eq!(rows(&s, &a).await, 0);
    // The other user's rows (same urls, same times) are untouched.
    assert_eq!(urls(&s, &b).await, ["https://b.com/1", "https://a.com/1"]);
    // Idempotent.
    assert_eq!(deleted(&s, &a, "").await, 0);
    // b's window / delete-all never reach a's rows either.
    push(&s, &a, &[("https://a.com/4", HOUR[0])]).await;
    assert_eq!(deleted(&s, &b, &format!("until={}", HOUR[1])).await, 2);
    assert_eq!(deleted(&s, &b, "").await, 0);
    assert_eq!(urls(&s, &a).await, ["https://a.com/4"]);
}

#[tokio::test]
async fn delete_since_until_window() {
    let s = start().await;
    let a = s
        .signup("del-window@example.com", PASSWORD, &new_device())
        .await;
    let b = s
        .signup("del-window-b@example.com", PASSWORD, &new_device())
        .await;
    let visits: Vec<(String, &str)> = HOUR
        .iter()
        .enumerate()
        .map(|(i, at)| (format!("https://w.com/{i}"), *at))
        .collect();
    let visits: Vec<(&str, &str)> = visits.iter().map(|(u, at)| (u.as_str(), *at)).collect();
    push(&s, &a, &visits).await;
    push(&s, &b, &visits).await;
    let left = |list: &[usize]| -> Vec<String> {
        list.iter().map(|i| format!("https://w.com/{i}")).collect()
    };

    // since is inclusive, until exclusive.
    assert_eq!(
        deleted(&s, &a, &format!("since={}&until={}", HOUR[1], HOUR[3])).await,
        2
    );
    assert_eq!(urls(&s, &a).await, left(&[0, 3, 4, 5]));
    // Only an upper bound.
    assert_eq!(deleted(&s, &a, "until=2026-10-01T10:30:00Z").await, 1);
    assert_eq!(urls(&s, &a).await, left(&[3, 4, 5]));
    // Offsets: `%2B02:00` and a raw `+02:00` (form-decoded to a space, repaired) both work.
    // [15:00+02:00, 16:30+02:00) = [13:00Z, 14:30Z).
    assert_eq!(
        deleted(
            &s,
            &a,
            "since=2026-10-01T15:00:00%2B02:00&until=2026-10-01T16:30:00+02:00"
        )
        .await,
        2
    );
    assert_eq!(urls(&s, &a).await, left(&[5]));
    // Only a lower bound.
    assert_eq!(deleted(&s, &a, &format!("since={}", HOUR[5])).await, 1);
    assert_eq!(urls(&s, &a).await, NONE);
    // The other user's identical visits are all still there.
    assert_eq!(urls(&s, &b).await, left(&[0, 1, 2, 3, 4, 5]));
}

#[tokio::test]
async fn delete_by_device() {
    let s = start().await;
    let (dev_a, dev_b) = (new_device(), new_device());
    let a = s.signup("del-dev@example.com", PASSWORD, &dev_a).await;
    let b = second_device(&s, "del-dev@example.com", &dev_b).await;
    push(
        &s,
        &a,
        &[("https://a.com/1", HOUR[0]), ("https://a.com/2", HOUR[1])],
    )
    .await;
    push(
        &s,
        &b,
        &[("https://b.com/1", HOUR[0]), ("https://b.com/2", HOUR[1])],
    )
    .await;

    assert_eq!(
        deleted(&s, &a, &format!("device={}", dev_b.to_uppercase())).await,
        2
    );
    assert_eq!(urls(&s, &b).await, ["https://a.com/1", "https://a.com/2"]);
    assert_eq!(
        deleted(&s, &a, &format!("device={}", new_device())).await,
        0
    );
    // Combined with a window.
    assert_eq!(
        deleted(&s, &b, &format!("device={dev_a}&until={}", HOUR[1])).await,
        1
    );
    assert_eq!(urls(&s, &a).await, ["https://a.com/2"]);
}

#[tokio::test]
async fn delete_auth_and_bad_params() {
    let s = start().await;
    let a = s
        .signup("del-bad@example.com", PASSWORD, &new_device())
        .await;
    push(
        &s,
        &a,
        &[("https://x.com/1", HOUR[0]), ("https://y.com/1", HOUR[1])],
    )
    .await;
    let seqs = pick(&s, &a, "").await;

    // Session required (after the gate), with or without a body.
    let r = s
        .req(Method::DELETE, "/v1/sync/history")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "session");
    let r = s
        .req(Method::DELETE, "/v1/sync/history")
        .bearer_auth("x".repeat(43))
        .json(&json!({ "seqs": seqs }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "session");
    // Gate required.
    let r = s
        .http
        .delete(s.url("/v1/sync/history"))
        .bearer_auth(&a.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "instance_key");
    assert_eq!(rows(&s, &a).await, 2);

    for query in [
        "since=yesterday",
        "until=1790856000",
        "since=",
        "since=2026-10-01T12:00:00Z&until=2026-10-01T12:00:00Z",
        "since=2026-10-01T13:00:00Z&until=2026-10-01T12:00:00Z",
        "since=2026-10-01T12:00:00Z&since=2026-10-01T13:00:00Z",
        "device=me",
        "device=",
        // There are no content filters: picking by site is the client's job (seqs).
        "host=x.com",
        "url=https://x.com",
        "q=x",
        "seqs=1",
        "x.com",
    ] {
        let (status, v) = delete(&s, &a, query, None).await;
        assert_eq!(status, 400, "{query}: {v}");
        assert_eq!(v["error"], "bad_request", "{query}");
        assert!(!v["message"].as_str().unwrap().is_empty(), "{query}");
    }
    // Nothing was deleted by any of them.
    assert_eq!(rows(&s, &a).await, 2);

    // The `?token=` session form is accepted (not an unknown parameter).
    let r = s
        .req(
            Method::DELETE,
            &format!("/v1/sync/history?token={}", a.token),
        )
        .json(&json!({ "seqs": [seqs[1]] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap(), json!({ "deleted": 1 }));
    assert_eq!(urls(&s, &a).await, ["https://x.com/1"]);
}

/// The server never needs to read a payload to delete: rows whose payload is garbage (not
/// sealed, sealed with another key, not JSON) are deleted by seq and by window like any other,
/// and the route still works when the user's data key cannot even be unwrapped (it never
/// unwraps it) — while a pull, which does open payloads, cannot serve that user.
#[tokio::test]
async fn delete_never_depends_on_payload_content() {
    let s = start().await;
    let dev = new_device();
    let a = s.signup("del-opaque@example.com", PASSWORD, &dev).await;
    let b = s
        .signup("del-opaque-b@example.com", PASSWORD, &new_device())
        .await;
    push(&s, &b, &[("https://b.com/", HOUR[0])]).await;
    let uid: Uuid = a.user_id.parse().unwrap();
    let aad = Crypto::user_aad(uid, "history");
    let garbage: Vec<Vec<u8>> = vec![
        b"\x00\x01not sealed at all".to_vec(),
        Vec::new(),
        Crypto::seal(&Crypto::new_key(), &aad, br#"{"url":"https://x.com/"}"#),
        vec![0xff; 4096],
    ];
    let times = [HOUR[0], HOUR[1], HOUR[2], HOUR[4]];
    let insert = |payloads: Vec<Vec<u8>>, at: Vec<&str>| {
        let db = s.state.db.clone();
        let dev = dev.clone();
        let at: Vec<time::OffsetDateTime> = at
            .iter()
            .map(|t| copper_cloud_core::sync::parse_history_time("t", t).unwrap())
            .collect();
        async move {
            sqlx::query_scalar::<_, i64>(
                "INSERT INTO history (user_id, device_id, visited_at, payload)
                 SELECT $1, $2::uuid, v, p FROM UNNEST($3::timestamptz[], $4::bytea[]) AS t(v, p)
                 RETURNING seq",
            )
            .bind(uid)
            .bind(&dev)
            .bind(at)
            .bind(payloads)
            .fetch_all(&db)
            .await
            .unwrap()
        }
    };
    let seqs = insert(garbage.clone(), times.to_vec()).await;
    assert_eq!(seqs.len(), 4);

    // By seq.
    assert_eq!(deleted_seqs(&s, &a, &seqs[..2]).await, 2);
    // By window: [11:00, 13:00) holds one of the remaining garbage rows.
    assert_eq!(
        deleted(&s, &a, &format!("since={}&until={}", HOUR[1], HOUR[3])).await,
        1
    );
    // By device, then delete-all.
    assert_eq!(deleted(&s, &a, &format!("device={dev}")).await, 1);
    insert(garbage.clone(), times.to_vec()).await;
    assert_eq!(deleted(&s, &a, "").await, 4);
    assert_eq!(rows(&s, &a).await, 0);

    // Make the user's data key impossible to unwrap: deletes still work (they never touch it).
    sqlx::query("UPDATE users SET data_key_wrapped = $2 WHERE id = $1")
        .bind(uid)
        .bind(vec![7_u8; 60])
        .execute(&s.state.db)
        .await
        .unwrap();
    let seqs = insert(garbage, times.to_vec()).await;
    let r = s
        .authed(Method::GET, "/v1/sync/history", &a)
        .send()
        .await
        .unwrap();
    assert!(!r.status().is_success(), "a pull has to open payloads");
    assert_eq!(deleted_seqs(&s, &a, &seqs[..1]).await, 1);
    assert_eq!(deleted(&s, &a, &format!("until={}", HOUR[2])).await, 1);
    assert_eq!(deleted(&s, &a, "").await, 2);
    assert_eq!(rows(&s, &a).await, 0);
    assert_eq!(urls(&s, &b).await, ["https://b.com/"]);
}

#[tokio::test]
async fn delete_is_rate_limited_per_user() {
    let s = start().await;
    let a = s
        .signup("del-rate@example.com", PASSWORD, &new_device())
        .await;
    let b = s
        .signup("del-rate-b@example.com", PASSWORD, &new_device())
        .await;
    let mut ok = 0_u32;
    let limited = loop {
        let r = s
            .authed(Method::DELETE, "/v1/sync/history", &a)
            .send()
            .await
            .unwrap();
        if r.status() == 429 {
            break r;
        }
        assert_eq!(r.status(), 200);
        ok += 1;
        assert!(ok <= 30, "never rate limited");
    };
    assert!(ok >= copper_cloud_core::sync::HISTORY_DELETES_PER_MINUTE);
    assert_eq!(limited.headers()["retry-after"], "60");
    assert_eq!(
        limited.json::<Value>().await.unwrap()["error"],
        "rate_limited"
    );
    // Per user: someone else is unaffected.
    assert_eq!(deleted(&s, &b, "").await, 0);
}

#[tokio::test]
async fn info_lists_features() {
    let s = start().await;
    let info: Value = s
        .req(Method::GET, "/v1/info")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["features"], json!(["history_delete"]));
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
}

/// Deleting rows before and after a client's cursor only means it sees fewer rows; `next`
/// never moves backwards and new rows still arrive after a delete-everything.
#[tokio::test]
async fn pull_cursor_survives_deletes() {
    let s = start().await;
    let (dev_a, dev_b) = (new_device(), new_device());
    let a = s.signup("del-cursor@example.com", PASSWORD, &dev_a).await;
    let b = second_device(&s, "del-cursor@example.com", &dev_b).await;
    let visits: Vec<String> = (0..6)
        .map(|i| {
            let host = if i % 2 == 0 { "keep" } else { "drop" };
            format!("https://{host}.com/{i}")
        })
        .collect();
    let visits: Vec<(&str, &str)> = visits.iter().map(|u| (u.as_str(), HOUR[0])).collect();
    push(&s, &a, &visits).await;

    let (got, cursor, more) = pull(&s, &b, 0, 2).await;
    let got: Vec<&str> = got.iter().map(|(_, u)| u.as_str()).collect();
    assert_eq!(got, ["https://keep.com/0", "https://drop.com/1"]);
    assert!(more);

    // Rows on both sides of b's cursor go away (picked by seq on a's side).
    let drop = pick(&s, &a, "drop.com").await;
    assert_eq!(deleted_seqs(&s, &a, &drop).await, 3);
    let (got, cursor2, more) = pull(&s, &b, cursor, 2).await;
    let got: Vec<&str> = got.iter().map(|(_, u)| u.as_str()).collect();
    assert_eq!(got, ["https://keep.com/2", "https://keep.com/4"]);
    assert!(!more);
    assert!(cursor2 > cursor);
    let (got, cursor3, more) = pull(&s, &b, cursor2, 2).await;
    assert!(got.is_empty() && !more);
    assert_eq!(cursor3, cursor2);

    // Everything goes; the cursor stays put and new rows still arrive after it.
    assert_eq!(deleted(&s, &a, "").await, 3);
    let (got, cursor4, more) = pull(&s, &b, cursor3, 2).await;
    assert!(got.is_empty() && !more);
    assert_eq!(cursor4, cursor3);
    let seq = push(&s, &a, &[("https://new.com/", HOUR[1])]).await;
    assert!(seq > cursor4, "seq is never reused");
    let (got, cursor5, _) = pull(&s, &b, cursor4, 2).await;
    assert_eq!(got, [(seq, "https://new.com/".to_owned())]);
    assert_eq!(cursor5, seq);
    assert_eq!(urls(&s, &b).await, ["https://new.com/"]);
}

/// Read SSE frames until one with `event: <name>` arrives; returns its raw data.
async fn next_event(
    stream: &mut (impl futures::Stream<Item = reqwest::Result<axum::body::Bytes>> + Unpin),
    buf: &mut String,
    name: &str,
) -> String {
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
                return data;
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
async fn delete_publishes_history_deleted() {
    let s = start().await;
    let (dev_a, dev_b) = (new_device(), new_device());
    let a = s.signup("del-sse@example.com", PASSWORD, &dev_a).await;
    let b = second_device(&s, "del-sse@example.com", &dev_b).await;
    let other = s
        .signup("del-sse-other@example.com", PASSWORD, &new_device())
        .await;

    let resp = s
        .authed(Method::GET, "/v1/sync/events", &b)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut stream = Box::pin(resp.bytes_stream());
    let mut buf = String::new();
    next_event(&mut stream, &mut buf, "ready").await;

    // Another user's delete is not delivered (it would arrive first and fail the asserts).
    push(&s, &other, &[("https://x.com/", HOUR[0])]).await;
    assert_eq!(deleted(&s, &other, "").await, 1);

    push(
        &s,
        &a,
        &[
            ("https://x.com/1", HOUR[1]),
            ("https://m.x.com/2", HOUR[2]),
            ("https://y.com/3", HOUR[2]),
            ("https://x.com/4", HOUR[5]),
        ],
    )
    .await;

    // A delete by seq: a count of seqs, no filter, and never anything about the rows.
    let mut picked = pick(&s, &a, "x.com").await;
    picked.push(picked[0]);
    assert_eq!(deleted_seqs(&s, &a, &picked).await, 3);
    let raw = next_event(&mut stream, &mut buf, "history_deleted").await;
    assert!(!raw.contains("x.com") && !raw.contains("http"), "{raw}");
    let ev: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        ev,
        json!({
            "type": "history_deleted",
            "deleted": 3,
            "since": null,
            "until": null,
            "device": null,
            "seqs": 3,
            "device_id": dev_a,
        })
    );

    // A filter delete echoes the (metadata) filter; `seqs` is omitted.
    assert_eq!(
        deleted(
            &s,
            &a,
            &format!("since=2026-10-01T12:00:00%2B01:00&until=2026-10-01T15:00:00Z&device={dev_a}")
        )
        .await,
        1
    );
    let ev: Value =
        serde_json::from_str(&next_event(&mut stream, &mut buf, "history_deleted").await).unwrap();
    assert_eq!(
        ev,
        json!({
            "type": "history_deleted",
            "deleted": 1,
            "since": "2026-10-01T11:00:00Z",
            "until": "2026-10-01T15:00:00Z",
            "device": dev_a,
            "device_id": dev_a,
        })
    );
    // Sent even when nothing matched, so other devices can still apply the filter locally.
    assert_eq!(deleted(&s, &a, &format!("device={dev_b}")).await, 0);
    let ev: Value =
        serde_json::from_str(&next_event(&mut stream, &mut buf, "history_deleted").await).unwrap();
    assert_eq!(
        ev,
        json!({
            "type": "history_deleted",
            "deleted": 0,
            "since": null,
            "until": null,
            "device": dev_b,
            "device_id": dev_a,
        })
    );
}

// ---------------------------------------------------------------------------------------------
// `copper-cloud admin history delete`

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_copper-cloud"));
    // Never let the developer's environment leak into the CLI under test.
    for (k, _) in std::env::vars() {
        if k.starts_with("COPPER_CLOUD_") {
            c.env_remove(k);
        }
    }
    c
}

/// A config file pointing at the test database with `master_key`.
fn write_config(master_key: &[u8; 32]) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "copper-cloud-test-histdel-{}",
        copper_cloud_core::ids::random_token()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("copper-cloud.toml");
    let out = bin()
        .args(["init-config", "--tls-mode", "off", "--write"])
        .arg(&path)
        .args(["--database-url", &common::database_url()])
        .args(["--master-key", &copper_cloud_core::ids::b64url(master_key)])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    (dir, path)
}

/// Run `copper-cloud --config <cfg> admin history delete <args>` → (success, stdout, stderr).
fn cli_delete(cfg: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let out = bin()
        .arg("--config")
        .arg(cfg)
        .args(["admin", "history", "delete"])
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test]
async fn admin_cli_dry_run_then_delete() {
    let s = start().await;
    let (dev_a, dev_a2) = (new_device(), new_device());
    let a = s.signup("Hist-CLI@example.com", PASSWORD, &dev_a).await;
    let a2 = second_device(&s, "Hist-CLI@example.com", &dev_a2).await;
    let b = s
        .signup("hist-cli-b@example.com", PASSWORD, &new_device())
        .await;
    push(
        &s,
        &a,
        &[
            ("https://x.com/1", HOUR[0]),
            ("https://www.x.com/2", HOUR[1]),
            ("https://y.com/3", HOUR[2]),
            ("https://x.com/4", HOUR[4]),
        ],
    )
    .await;
    push(&s, &a2, &[("https://z.com/5", HOUR[1])]).await;
    push(&s, &b, &[("https://x.com/b", HOUR[0])]).await;

    // Library path (what the binary runs): a dry run only counts.
    let args = copper_cloud::cli::HistoryDeleteArgs {
        user: "hist-cli@EXAMPLE.com".into(),
        since: None,
        until: Some(copper_cloud_core::sync::parse_history_time("until", HOUR[3]).unwrap()),
        device: None,
        dry_run: true,
    };
    let (email, n) = copper_cloud::admin::history_delete(&s.state.db, &args)
        .await
        .unwrap();
    assert_eq!(email, "Hist-CLI@example.com");
    assert_eq!(n, 4);
    assert_eq!(rows(&s, &a).await, 5);

    // The real binary. Its config has a different master key on purpose: deleting history
    // never needs to unwrap a data key or open a payload.
    let (dir, path) = write_config(&Crypto::new_key());
    let (ok, stdout, stderr) = cli_delete(
        &path,
        &[
            "--user",
            "hist-cli@example.com",
            "--device",
            &dev_a2,
            "--dry-run",
        ],
    );
    assert!(ok, "{stderr}");
    assert_eq!(
        stdout.trim(),
        "dry run: would delete 1 history entry of Hist-CLI@example.com"
    );
    assert_eq!(rows(&s, &a).await, 5);
    let (ok, stdout, stderr) = cli_delete(
        &path,
        &[
            "--user",
            "hist-cli@example.com",
            "--since",
            HOUR[1],
            "--until",
            HOUR[3],
            "--dry-run",
        ],
    );
    assert!(ok, "{stderr}");
    assert_eq!(
        stdout.trim(),
        "dry run: would delete 3 history entries of Hist-CLI@example.com"
    );
    assert_eq!(rows(&s, &a).await, 5);
    // For real: the window, then everything left (by user id), with counts only on stdout.
    let (ok, stdout, stderr) = cli_delete(
        &path,
        &[
            "--user",
            "hist-cli@example.com",
            "--since",
            HOUR[1],
            "--until",
            HOUR[3],
        ],
    );
    assert!(ok, "{stderr}");
    assert_eq!(
        stdout.trim(),
        "deleted 3 history entries of Hist-CLI@example.com"
    );
    assert_eq!(urls(&s, &a).await, ["https://x.com/1", "https://x.com/4"]);
    let (ok, stdout, _) = cli_delete(&path, &["--user", &a.user_id.to_uppercase(), "--dry-run"]);
    assert!(ok);
    assert_eq!(
        stdout.trim(),
        "dry run: would delete 2 history entries of Hist-CLI@example.com"
    );
    let (ok, stdout, stderr) = cli_delete(&path, &["--user", &a.user_id]);
    assert!(ok);
    assert_eq!(
        stdout.trim(),
        "deleted 2 history entries of Hist-CLI@example.com"
    );
    for out in [&stdout, &stderr] {
        assert!(!out.contains("x.com") && !out.contains("http"), "{out}");
    }
    assert_eq!(rows(&s, &a).await, 0);
    assert_eq!(rows(&s, &b).await, 1);

    // Errors: unknown user, bad flags, content filters (there are none).
    let (ok, _, stderr) = cli_delete(&path, &["--user", "nobody@example.com", "--dry-run"]);
    assert!(!ok);
    assert!(stderr.contains("no user nobody@example.com"), "{stderr}");
    for bad in [
        &["--since", "yesterday"][..],
        &["--since", HOUR[2], "--until", HOUR[1]],
        &["--host", "x.com"],
        &["--seqs", "1"],
    ] {
        let mut args = vec!["--user", b.user_id.as_str()];
        args.extend_from_slice(bad);
        let (ok, _, _) = cli_delete(&path, &args);
        assert!(!ok, "{bad:?}");
    }
    assert_eq!(rows(&s, &b).await, 1);

    std::fs::remove_dir_all(dir).unwrap();
}
