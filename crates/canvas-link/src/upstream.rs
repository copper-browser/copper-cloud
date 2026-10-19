//! The HTTP(S) / WebSocket client for one linked Canvas Cloud.

use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderValue, AUTHORIZATION};
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use tokio_tungstenite::Connector;
use zeroize::Zeroizing;

use crate::settings::LinkSettings;

/// TCP + TLS connect budget.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Whole-request budget for ordinary calls.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Whole-request budget for `/ops` and imports.
pub const LONG_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Budget for admin/overview probes (the portal must stay responsive when Canvas Cloud is down).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Header naming the user an app request acts for.
pub const CANVAS_USER_HEADER: &str = "x-canvas-user";

/// A configured client for the linked Canvas Cloud. Cheap to clone.
#[derive(Clone)]
pub struct Upstream {
    client: reqwest::Client,
    base: String,
    app_key: Arc<Zeroizing<String>>,
    ws_tls: Option<Arc<rustls::ClientConfig>>,
}

impl std::fmt::Debug for Upstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upstream")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// Why a call to Canvas Cloud failed.
#[derive(Debug)]
pub enum UpstreamError {
    /// Could not connect / TLS failed / timed out / connection dropped.
    Transport(String),
    /// Canvas Cloud answered with an unexpected status.
    Status {
        status: StatusCode,
        code: Option<String>,
        message: Option<String>,
    },
    /// Canvas Cloud answered 2xx with a body we could not read.
    Body(String),
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(msg) => write!(f, "{msg}"),
            Self::Status {
                status,
                code,
                message,
            } => {
                write!(f, "Canvas Cloud answered {}", status.as_u16())?;
                if let Some(c) = code {
                    write!(f, " {c}")?;
                }
                if let Some(m) = message.as_deref().filter(|m| !m.is_empty()) {
                    write!(f, ": {m}")?;
                }
                Ok(())
            }
            Self::Body(msg) => write!(f, "unreadable Canvas Cloud response: {msg}"),
        }
    }
}

impl std::error::Error for UpstreamError {}

impl UpstreamError {
    /// Describe a reqwest failure without leaking the URL's query (none is secret, but keep
    /// messages short) — "unreachable", "timed out", TLS errors.
    pub fn from_reqwest(err: &reqwest::Error) -> Self {
        let what = if err.is_timeout() {
            "Canvas Cloud did not answer in time".to_owned()
        } else if err.is_connect() {
            format!("Canvas Cloud is unreachable ({})", root_cause(err))
        } else {
            format!("Canvas Cloud request failed ({})", root_cause(err))
        };
        Self::Transport(what)
    }

    pub fn status(&self) -> Option<StatusCode> {
        match self {
            Self::Status { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// The upstream JSON `error` code, if any.
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Status { code, .. } => code.as_deref(),
            _ => None,
        }
    }
}

/// The innermost error message of a chain (e.g. the rustls error under reqwest's wrapper).
pub fn root_cause(err: &(dyn std::error::Error + 'static)) -> String {
    let mut cur: &(dyn std::error::Error + 'static) = err;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur.to_string()
}

/// `{"error": …, "message": …}` of an error body, if it is one.
pub fn error_fields(body: &[u8]) -> (Option<String>, Option<String>) {
    #[derive(Deserialize)]
    struct E {
        error: Option<String>,
        message: Option<String>,
    }
    serde_json::from_slice::<E>(body).map_or((None, None), |e| (e.error, e.message))
}

/// `GET /v1/apps/self/stats`.
#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct Stats {
    #[serde(default)]
    pub users: i64,
    #[serde(default)]
    pub canvases: i64,
    #[serde(default)]
    pub rooms: Rooms,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct Rooms {
    #[serde(default)]
    pub rooms: i64,
    #[serde(default)]
    pub peers: i64,
}

impl Upstream {
    /// Build a client for `settings` (TLS pin, timeouts).
    pub fn new(settings: &LinkSettings) -> anyhow::Result<Self> {
        let https = settings.url.scheme() == "https";
        let pin = settings.tls_fingerprint.as_deref();
        let mut builder = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .pool_idle_timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("copper-cloud/", env!("CARGO_PKG_VERSION")));
        let ws_tls = if https {
            builder = builder.use_preconfigured_tls(crate::tls::client_config(pin)?);
            Some(Arc::new(crate::tls::client_config(pin)?))
        } else {
            None
        };
        Ok(Self {
            client: builder.build()?,
            base: settings.base(),
            app_key: Arc::new(settings.app_key.clone()),
            ws_tls,
        })
    }

    /// The underlying client (TLS pin applied; no default auth header, no global timeout —
    /// set one per request).
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Base URL without a trailing `/`.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// `Bearer <app key>` header value.
    pub(crate) fn auth_header(&self) -> HeaderValue {
        let mut v = HeaderValue::from_str(&format!("Bearer {}", self.app_key.as_str()))
            .unwrap_or_else(|_| HeaderValue::from_static("Bearer invalid"));
        v.set_sensitive(true);
        v
    }

    /// `{base}{path}` where `path` starts with `/` (e.g. `/v1/info`).
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// The WebSocket URL for an absolute `/v1/...` path (`ws(s)://`).
    pub fn ws_url(&self, path_and_query: &str) -> String {
        let base = self
            .base
            .strip_prefix("https://")
            .map(|r| format!("wss://{r}"))
            .or_else(|| {
                self.base
                    .strip_prefix("http://")
                    .map(|r| format!("ws://{r}"))
            })
            .unwrap_or_else(|| self.base.clone());
        format!("{base}{path_and_query}")
    }

    /// The tokio-tungstenite connector matching the HTTP client's TLS policy.
    pub fn ws_connector(&self) -> Connector {
        match &self.ws_tls {
            Some(cfg) => Connector::Rustls(cfg.clone()),
            None => Connector::Plain,
        }
    }

    /// An app-authenticated request (`Authorization: Bearer <app key>`, 30 s budget).
    pub fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(method, self.url(path))
            .header(AUTHORIZATION, self.auth_header())
            .timeout(REQUEST_TIMEOUT)
    }

    /// Send `rb` and decode a 2xx JSON body; any other status is an [`UpstreamError::Status`].
    pub async fn json<T: serde::de::DeserializeOwned>(
        rb: RequestBuilder,
    ) -> Result<T, UpstreamError> {
        let resp = rb
            .send()
            .await
            .map_err(|e| UpstreamError::from_reqwest(&e))?;
        let status = resp.status();
        let mut body = resp
            .bytes()
            .await
            .map_err(|e| UpstreamError::from_reqwest(&e))?;
        if !status.is_success() {
            let (code, message) = error_fields(&body);
            return Err(UpstreamError::Status {
                status,
                code,
                message,
            });
        }
        if body.iter().all(u8::is_ascii_whitespace) {
            // `204 No Content` and friends: decode as JSON `null`.
            body = bytes::Bytes::from_static(b"null");
        }
        serde_json::from_slice(&body).map_err(|e| UpstreamError::Body(e.to_string()))
    }

    /// `GET /v1/info` (unauthenticated on Canvas Cloud; the app key is sent anyway).
    pub async fn info(&self, timeout: Duration) -> Result<Value, UpstreamError> {
        Self::json(self.request(Method::GET, "/v1/info").timeout(timeout)).await
    }

    /// `GET /v1/apps/self/stats` — also proves the app key is accepted.
    pub async fn stats(&self, timeout: Duration) -> Result<Stats, UpstreamError> {
        Self::json(
            self.request(Method::GET, "/v1/apps/self/stats")
                .timeout(timeout),
        )
        .await
    }

    /// Probe: `/v1/info` + `/v1/apps/self/stats`. Returns the server version and latency.
    pub async fn check(&self, timeout: Duration) -> Result<Probe, UpstreamError> {
        let started = Instant::now();
        let info = self.info(timeout).await?;
        let stats = self.stats(timeout).await?;
        Ok(Probe {
            version: info
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_owned),
            name: info.get("name").and_then(Value::as_str).map(str::to_owned),
            latency: started.elapsed(),
            stats,
        })
    }
}

/// Result of [`Upstream::check`].
#[derive(Clone, Debug)]
pub struct Probe {
    pub version: Option<String>,
    pub name: Option<String>,
    pub latency: Duration,
    pub stats: Stats,
}
