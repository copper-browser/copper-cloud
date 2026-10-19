//! The few canvas-cloud app endpoints the migration uses (`/v1/apps/self/*`, app key only —
//! never `X-Canvas-User`).

use std::time::Duration;

use anyhow::Context as _;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::legacy::LegacyUser;
use crate::wire::{subject, CONTENT_TYPE as NDJSON};

/// Users per `users:batch` call (canvas-cloud's limit).
pub(crate) const BATCH: usize = 1000;

const SHORT: Duration = Duration::from_secs(30);
const BATCH_TIMEOUT: Duration = Duration::from_secs(120);
/// Computing a digest reloads the whole document server-side.
const DIGEST_TIMEOUT: Duration = Duration::from_secs(120);

/// A non-2xx answer, decoded from canvas-cloud's `{"error","message"}` shape when possible.
#[derive(Debug, Clone)]
pub(crate) struct HttpFailure {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
    pub body: Value,
}

impl std::fmt::Display for HttpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {} {}", self.status.as_u16(), self.code)?;
        if !self.message.is_empty() {
            write!(f, ": {}", self.message)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) enum Failure {
    Http(HttpFailure),
    /// The request never produced a response (connect, TLS, timeout, aborted body…).
    Transport(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(h) => h.fmt(f),
            Self::Transport(e) => f.write_str(e),
        }
    }
}

fn transport(e: &reqwest::Error) -> Failure {
    // `{:#}`-style chain: reqwest's top-level message alone ("error sending request") hides
    // the cause.
    let mut msg = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        source = s.source();
    }
    Failure::Transport(msg)
}

async fn failure(resp: reqwest::Response) -> HttpFailure {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let code = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_else(|| status.canonical_reason().unwrap_or("error"))
        .to_owned();
    let message = body.get("message").and_then(Value::as_str).map_or_else(
        || text.chars().take(300).collect::<String>().trim().to_owned(),
        ToOwned::to_owned,
    );
    HttpFailure {
        status,
        code,
        message,
        body,
    }
}

/// `200` body of `POST /v1/apps/self/import`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Imported {
    #[serde(default)]
    pub status: Option<String>,
    pub updates_applied: u64,
    pub members: u64,
    pub invites: u64,
    pub links: u64,
    pub mentions: u64,
    pub digest: String,
}

#[derive(Serialize)]
struct BatchUser<'a> {
    subject: String,
    email: &'a str,
    display_name: &'a str,
    disabled: bool,
    id: Uuid,
}

/// Outcome of provisioning one user.
#[derive(Debug)]
pub(crate) struct Provisioned {
    pub created: bool,
    /// `Some(reason)` when canvas-cloud did not give this user its copper id.
    pub error: Option<String>,
}

pub(crate) struct Remote<'a> {
    client: &'a reqwest::Client,
    base: String,
    auth: String,
}

impl<'a> Remote<'a> {
    pub fn new(
        client: &'a reqwest::Client,
        base: &url::Url,
        app_key: &str,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            matches!(base.scheme(), "http" | "https"),
            "canvas-cloud URL must be http(s), got {base}"
        );
        anyhow::ensure!(
            !app_key.trim().is_empty(),
            "the canvas-cloud app key is empty"
        );
        let mut base = base.clone();
        base.set_query(None);
        base.set_fragment(None);
        Ok(Self {
            client,
            base: base.as_str().trim_end_matches('/').to_owned(),
            auth: format!("Bearer {}", app_key.trim()),
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn get(&self, path: &str, timeout: Duration) -> reqwest::RequestBuilder {
        self.client
            .get(self.url(path))
            .header(AUTHORIZATION, &self.auth)
            .header(ACCEPT, "application/json")
            .timeout(timeout)
    }

    /// `GET /v1/apps/self/stats` must answer 200 with JSON.
    pub async fn preflight(&self) -> anyhow::Result<Value> {
        let path = "/v1/apps/self/stats";
        let resp = self
            .get(path, SHORT)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("{}", transport(&e)))
            .with_context(|| format!("canvas-cloud preflight: GET {} failed", self.url(path)))?;
        if resp.status() != StatusCode::OK {
            let f = failure(resp).await;
            let hint = match f.status {
                StatusCode::UNAUTHORIZED => " (the app key was rejected — revoked or wrong key?)",
                StatusCode::NOT_FOUND => " (is this URL a Canvas Cloud? /v1/apps/self is missing)",
                _ => "",
            };
            anyhow::bail!(
                "canvas-cloud preflight: GET {} answered {f}{hint}",
                self.url(path)
            );
        }
        let stats: Value = resp
            .json()
            .await
            .context("canvas-cloud preflight: /v1/apps/self/stats did not return JSON")?;
        Ok(stats)
    }

    /// Provisions `users` (subject = id hint = copper user id) through `users:batch`.
    /// Returns one outcome per input user, in order.
    pub async fn provision(&self, users: &[LegacyUser]) -> anyhow::Result<Vec<Provisioned>> {
        let path = "/v1/apps/self/users:batch";
        let mut out = Vec::with_capacity(users.len());
        for chunk in users.chunks(BATCH) {
            let body = serde_json::json!({
                "users": chunk.iter().map(|u| BatchUser {
                    subject: subject(u.id),
                    email: &u.email,
                    display_name: &u.display_name,
                    disabled: u.disabled,
                    id: u.id,
                }).collect::<Vec<_>>(),
            });
            let resp = self
                .client
                .post(self.url(path))
                .header(AUTHORIZATION, &self.auth)
                .timeout(BATCH_TIMEOUT)
                .json(&body)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("{}", transport(&e)))
                .with_context(|| format!("provisioning users: POST {}", self.url(path)))?;
            if !resp.status().is_success() {
                let f = failure(resp).await;
                anyhow::bail!("provisioning users: POST {} answered {f}", self.url(path));
            }
            let body: Value = resp
                .json()
                .await
                .context("provisioning users: users:batch did not return JSON")?;
            out.extend(match_results(chunk, &body));
        }
        Ok(out)
    }

    /// `GET /v1/apps/self/canvases/{id}/digest`: `Ok(None)` when the canvas does not exist.
    pub async fn digest(&self, canvas_id: Uuid) -> Result<Option<String>, Failure> {
        let resp = self
            .get(
                &format!("/v1/apps/self/canvases/{canvas_id}/digest"),
                DIGEST_TIMEOUT,
            )
            .send()
            .await
            .map_err(|e| transport(&e))?;
        match resp.status() {
            StatusCode::OK => {
                let text = resp.text().await.map_err(|e| transport(&e))?;
                parse_digest(&text).map(Some).ok_or_else(|| {
                    Failure::Transport(format!(
                        "digest endpoint returned no digest: {}",
                        text.chars().take(200).collect::<String>()
                    ))
                })
            }
            StatusCode::NOT_FOUND => Ok(None),
            _ => Err(Failure::Http(failure(resp).await)),
        }
    }

    /// `POST /v1/apps/self/import[?mode=replace]` with a streamed NDJSON body.
    pub async fn import(
        &self,
        body: reqwest::Body,
        replace: bool,
        timeout: Duration,
    ) -> Result<Imported, Failure> {
        let mut url = self.url("/v1/apps/self/import");
        if replace {
            url.push_str("?mode=replace");
        }
        let resp = self
            .client
            .post(url)
            .header(AUTHORIZATION, &self.auth)
            .header(CONTENT_TYPE, NDJSON)
            .header(ACCEPT, "application/json")
            .timeout(timeout)
            .body(body)
            .send()
            .await
            .map_err(|e| transport(&e))?;
        if resp.status() != StatusCode::OK {
            return Err(Failure::Http(failure(resp).await));
        }
        let text = resp.text().await.map_err(|e| transport(&e))?;
        serde_json::from_str(&text).map_err(|e| {
            Failure::Transport(format!(
                "import answered 200 with an unexpected body ({e}): {}",
                text.chars().take(200).collect::<String>()
            ))
        })
    }
}

/// A digest from a digest response: `{"digest": "…"}`, a JSON string, or bare hex.
pub(crate) fn parse_digest(text: &str) -> Option<String> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => m.get("digest").and_then(Value::as_str).and_then(normalize),
        Ok(Value::String(s)) => normalize(&s),
        _ => normalize(text),
    }
}

/// The digest a `409 canvas_exists` body carries, if any.
pub(crate) fn conflict_digest(body: &Value) -> Option<String> {
    body.get("digest")
        .and_then(Value::as_str)
        .and_then(normalize)
}

/// Lower-case 64-hex-digit SHA-256, or `None`.
fn normalize(raw: &str) -> Option<String> {
    let d = raw.trim().to_ascii_lowercase();
    (d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit())).then_some(d)
}

/// Matches `users:batch` per-item results to the users sent. Accepts `{"results":[…]}`,
/// `{"users":[…]}` or a bare array; items carry `subject` and `user_id` (or `id`), or an
/// `error`/`message`. Items without a `subject` are matched by position.
fn match_results(chunk: &[LegacyUser], body: &Value) -> Vec<Provisioned> {
    let items: &[Value] = body
        .get("results")
        .or_else(|| body.get("users"))
        .unwrap_or(body)
        .as_array()
        .map_or(&[], Vec::as_slice);
    let by_subject: std::collections::HashMap<&str, &Value> = items
        .iter()
        .filter_map(|v| Some((v.get("subject")?.as_str()?, v)))
        .collect();
    chunk
        .iter()
        .enumerate()
        .map(|(i, u)| {
            let wanted = subject(u.id);
            let item = by_subject
                .get(wanted.as_str())
                .copied()
                .or_else(|| items.get(i).filter(|v| v.get("subject").is_none()));
            let Some(item) = item else {
                return Provisioned {
                    created: false,
                    error: Some("missing from the users:batch response".into()),
                };
            };
            if let Some(code) = item.get("error").and_then(Value::as_str) {
                let message = item.get("message").and_then(Value::as_str).unwrap_or("");
                return Provisioned {
                    created: false,
                    error: Some(format!("{code}: {message}").trim_end_matches(": ").to_owned()),
                };
            }
            let got = item
                .get("user_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok());
            let created = item.get("created").and_then(Value::as_bool).unwrap_or(false);
            match got {
                Some(id) if id == u.id => Provisioned {
                    created,
                    error: None,
                },
                Some(id) => Provisioned {
                    created,
                    error: Some(format!(
                        "canvas-cloud mapped it to user {id} instead of {} (email already used by another canvas-cloud account?)",
                        u.id
                    )),
                },
                None => Provisioned {
                    created,
                    error: Some("users:batch result has no user_id".into()),
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(n: u128) -> LegacyUser {
        LegacyUser {
            id: Uuid::from_u128(n),
            email: format!("u{n}@example.com"),
            display_name: String::new(),
            disabled: false,
        }
    }

    #[test]
    fn digests_parse_leniently_but_strictly_hex() {
        let d = "AB".repeat(32);
        assert_eq!(
            parse_digest(&format!(r#"{{"digest":"{d}"}}"#)),
            Some("ab".repeat(32))
        );
        assert_eq!(parse_digest(&format!(r#""{d}""#)), Some("ab".repeat(32)));
        assert_eq!(parse_digest(&format!("{d}\n")), Some("ab".repeat(32)));
        assert_eq!(parse_digest(r#"{"digest":"xyz"}"#), None);
        assert_eq!(parse_digest("{}"), None);
        assert_eq!(
            conflict_digest(&serde_json::json!({"error":"canvas_exists","digest": d})),
            Some("ab".repeat(32))
        );
        assert_eq!(
            conflict_digest(&serde_json::json!({"error":"canvas_exists"})),
            None
        );
    }

    #[test]
    fn batch_results_match_by_subject_or_position() {
        let users = [user(1), user(2), user(3), user(4)];
        let body = serde_json::json!({"results": [
            {"subject": subject(users[1].id), "user_id": users[1].id, "created": true},
            {"subject": subject(users[0].id), "user_id": users[0].id, "created": false},
            {"subject": subject(users[2].id), "user_id": Uuid::from_u128(99)},
        ]});
        let out = match_results(&users, &body);
        assert!(out[0].error.is_none() && !out[0].created);
        assert!(out[1].error.is_none() && out[1].created);
        assert!(out[2].error.as_deref().unwrap().contains("instead of"));
        assert!(out[3].error.as_deref().unwrap().contains("missing"));

        let positional = serde_json::json!([
            {"user_id": users[0].id},
            {"error": "email_taken", "message": "taken"},
        ]);
        let out = match_results(&users[..2], &positional);
        assert!(out[0].error.is_none());
        assert_eq!(out[1].error.as_deref(), Some("email_taken: taken"));
    }
}
