//! `GET /v1/info` gains `"canvas": {"linked": bool}` (additive) without touching core: an outer
//! middleware rewrites the JSON body of a successful `/v1/info` response.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use serde_json::{json, Value};

use crate::CanvasLink;

const MAX_INFO_BYTES: usize = 64 * 1024;

/// Middleware (`axum::middleware::from_fn_with_state(link, info_layer)`) for the whole app.
pub async fn info_layer(State(link): State<CanvasLink>, req: Request, next: Next) -> Response {
    let is_info = req.method() == Method::GET && req.uri().path() == "/v1/info";
    let resp = next.run(req).await;
    if !is_info || resp.status() != StatusCode::OK {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_INFO_BYTES).await else {
        return Response::from_parts(parts, Body::empty());
    };
    let Ok(Value::Object(mut map)) = serde_json::from_slice::<Value>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    let linked = matches!(link.enabled().await, Ok(Some(_)));
    map.insert("canvas".into(), json!({ "linked": linked }));
    let out = serde_json::to_vec(&Value::Object(map)).unwrap_or_else(|_| bytes.to_vec());
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(out))
}
