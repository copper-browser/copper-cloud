//! API error type. Every handler returns `Result<_, ApiError>`; the response body is always
//! JSON `{"error": <code>, "message": <human text>}` (409 conflicts carry extra fields).

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// 400 — malformed or invalid request. The string is shown to the client.
    #[error("bad request: {0}")]
    BadRequest(String),
    /// 401 — the `&'static str` is the machine-readable error code
    /// (e.g. `"instance_key"`, `"session"`, `"credentials"`).
    #[error("unauthorized: {0}")]
    Unauthorized(&'static str),
    /// 403
    #[error("forbidden")]
    Forbidden,
    /// 403 with a specific machine-readable code (e.g. `"access_key_email"`, `"csrf"`).
    #[error("forbidden: {code}")]
    Denied {
        code: &'static str,
        message: &'static str,
    },
    /// 404
    #[error("not found")]
    NotFound,
    /// 404 with a route-specific machine-readable code.
    #[error("not found: {code}")]
    NotFoundCode {
        code: &'static str,
        message: &'static str,
    },
    /// 409 — the value is merged into the body (objects) so the client gets the server copy.
    #[error("conflict")]
    Conflict(Value),
    /// 413
    #[error("payload too large")]
    PayloadTooLarge,
    /// 429
    #[error("rate limited")]
    RateLimited,
    /// 500 — logged at error level with the request span; never shown to the client.
    #[error("internal error: {0:#}")]
    Internal(anyhow::Error),
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }

    pub fn internal(msg: impl std::fmt::Display) -> Self {
        Self::Internal(anyhow::anyhow!("{msg}"))
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden | Self::Denied { .. } => StatusCode::FORBIDDEN,
            Self::NotFound | Self::NotFoundCode { .. } => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Machine-readable error code used in the `error` field.
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::Unauthorized(code)
            | Self::Denied { code, .. }
            | Self::NotFoundCode { code, .. } => code,
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict(_) => "conflict",
            Self::PayloadTooLarge => "payload_too_large",
            Self::RateLimited => "rate_limited",
            Self::Internal(_) => "internal",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let code = self.code();
        let body = match self {
            Self::BadRequest(msg) => json!({ "error": code, "message": msg }),
            Self::Unauthorized(_) => json!({ "error": code, "message": "unauthorized" }),
            Self::Forbidden => json!({ "error": code, "message": "forbidden" }),
            Self::Denied { message, .. } | Self::NotFoundCode { message, .. } => {
                json!({ "error": code, "message": message })
            }
            Self::NotFound => json!({ "error": code, "message": "not found" }),
            Self::Conflict(Value::Object(mut map)) => {
                map.insert("error".into(), Value::from(code));
                map.entry("message")
                    .or_insert_with(|| Value::from("conflict"));
                Value::Object(map)
            }
            Self::Conflict(other) => {
                json!({ "error": code, "message": "conflict", "detail": other })
            }
            Self::PayloadTooLarge => json!({ "error": code, "message": "payload too large" }),
            Self::RateLimited => json!({ "error": code, "message": "too many requests" }),
            Self::Internal(err) => {
                // The request span (method/path/user_id) is the current span here.
                tracing::error!(error = format!("{err:#}"), "internal error");
                json!({ "error": code, "message": "internal error" })
            }
        };
        let mut resp = (status, axum::Json(body)).into_response();
        if status == StatusCode::TOO_MANY_REQUESTS {
            resp.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
        }
        if status == StatusCode::UNAUTHORIZED {
            resp.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"copper-cloud\""),
            );
        }
        resp
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::RowNotFound => Self::NotFound,
            other => Self::Internal(anyhow::Error::new(other)),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal(err)
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(err: tokio::task::JoinError) -> Self {
        Self::Internal(anyhow::Error::new(err))
    }
}

/// Convenience alias.
pub type ApiResult<T> = Result<T, ApiError>;

/// True when `err` is a Postgres unique-constraint violation.
pub fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
}
