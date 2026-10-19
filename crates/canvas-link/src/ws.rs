//! The y-websocket pipe: `/v1/canvases/{id}/ws` from Copper ↔ the same path on Canvas Cloud.
//!
//! 1. Open the upstream WebSocket **first** (same path, query minus `token=`, app key +
//!    `X-Canvas-User`). If Canvas Cloud refuses the handshake with an HTTP status, answer the
//!    client with that status/body without upgrading — except 401 (`unknown_user` → provision
//!    + retry once; anything else → `502`, never a 401 that would sign Copper out).
//! 2. Upgrade the client (HTTP/1.1 `Upgrade` or HTTP/2 extended CONNECT) and pipe text/binary
//!    frames both ways, byte-exact. Each side keeps its own ping/pong; there is no idle
//!    timeout (rooms are long-lived). A close from either side is forwarded with the same
//!    code and reason (4403, 4404, 1001, …); a lost upstream closes the client with 1011;
//!    copper-cloud shutting down closes the client with 1001.

use std::time::Duration;

use axum::extract::ws::{
    CloseFrame as AxClose, Message as AxMessage, Utf8Bytes as AxUtf8, WebSocket, WebSocketUpgrade,
};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::{SinkExt as _, StreamExt as _};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame as TClose, WebSocketConfig};
use tokio_tungstenite::tungstenite::{self, Message as TMessage, Utf8Bytes as TUtf8};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::proxy::{jit_provision, rejected, Forward};
use crate::upstream::{error_fields, root_cause, CANVAS_USER_HEADER};
use crate::{upstream_failed, Active, CanvasLink};

/// Largest WebSocket message accepted on either side (images travel as `data:` URLs inside
/// Yjs updates; never cap below what Canvas Cloud accepts).
pub const MAX_MESSAGE_BYTES: usize = 64 << 20;
/// Largest single WebSocket frame accepted on either side.
pub const MAX_FRAME_BYTES: usize = 16 << 20;
/// copper-cloud → Copper keep-alive ping interval (no idle timeout is enforced).
pub const PING_EVERY: Duration = Duration::from_secs(30);
/// Upstream TCP + TLS + WebSocket handshake budget.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// A frame that cannot be written within this long ends the pipe (a wedged peer).
const SEND_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a forwarded close waits for the other side's close reply.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// Close codes copper-cloud itself sends on the pipe.
pub mod close {
    /// copper-cloud is shutting down.
    pub const GOING_AWAY: u16 = 1001;
    /// The upstream connection was lost; reconnect.
    pub const SERVER_ERROR: u16 = 1011;
}

type Upstream = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum ConnectError {
    Refused {
        status: StatusCode,
        content_type: Option<HeaderValue>,
        body: Vec<u8>,
    },
    Transport(String),
}

pub(crate) async fn proxy(
    link: &CanvasLink,
    active: &Active,
    fwd: &Forward,
    ws: WebSocketUpgrade,
) -> Response {
    let mut provisioned = false;
    let upstream = loop {
        match connect(active, fwd).await {
            Ok(stream) => break stream,
            Err(ConnectError::Refused {
                status,
                content_type,
                body,
            }) => {
                if status == StatusCode::UNAUTHORIZED {
                    let (code, message) = error_fields(&body);
                    if code.as_deref() == Some("unknown_user") && !provisioned {
                        provisioned = true;
                        if let Err(resp) = jit_provision(link, active, fwd.user_id).await {
                            return resp;
                        }
                        continue;
                    }
                    return rejected(code.as_deref(), message.as_deref(), fwd.user_id);
                }
                let mut resp = (status, body).into_response();
                if let Some(ct) = content_type {
                    resp.headers_mut().insert(header::CONTENT_TYPE, ct);
                }
                return resp;
            }
            Err(ConnectError::Transport(msg)) => {
                tracing::warn!(error = %msg, "canvas upstream WebSocket failed");
                return upstream_failed(msg);
            }
        }
    };
    let link = link.clone();
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| pipe(link, socket, upstream))
}

async fn connect(active: &Active, fwd: &Forward) -> Result<Upstream, ConnectError> {
    let up = &active.upstream;
    let mut req = up
        .ws_url(&fwd.path_and_query)
        .into_client_request()
        .map_err(|e| ConnectError::Transport(format!("invalid Canvas Cloud URL ({e})")))?;
    let headers = req.headers_mut();
    headers.insert(header::AUTHORIZATION, up.auth_header());
    if let Ok(v) = HeaderValue::from_str(&fwd.user_id.to_string()) {
        headers.insert(CANVAS_USER_HEADER, v);
    }
    if let Some(ip) = fwd.client_ip {
        if let Ok(v) = HeaderValue::from_str(&ip.to_string()) {
            headers.insert("x-forwarded-for", v);
        }
    }
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES));
    let attempt = tokio_tungstenite::connect_async_tls_with_config(
        req,
        Some(config),
        true,
        Some(up.ws_connector()),
    );
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, attempt).await {
        Err(_) => Err(ConnectError::Transport(
            "Canvas Cloud did not answer the WebSocket handshake in time".into(),
        )),
        Ok(Ok((stream, _))) => Ok(stream),
        Ok(Err(tungstenite::Error::Http(resp))) => {
            let status = resp.status();
            let content_type = resp.headers().get(header::CONTENT_TYPE).cloned();
            let body = resp.into_body().unwrap_or_default();
            Err(ConnectError::Refused {
                status,
                content_type,
                body,
            })
        }
        Ok(Err(err)) => Err(ConnectError::Transport(format!(
            "Canvas Cloud WebSocket is unreachable ({})",
            root_cause(&err)
        ))),
    }
}

/// Resolves once `rx` turns `true`; never if its sender is gone.
async fn flagged(rx: &mut watch::Receiver<bool>) {
    if rx.wait_for(|v| *v).await.is_err() {
        std::future::pending::<()>().await;
    }
}

fn to_upstream_close(frame: Option<AxClose>) -> Option<TClose> {
    let f = frame?;
    sendable(f.code).then(|| TClose {
        code: CloseCode::from(f.code),
        reason: TUtf8::try_from(Bytes::from(f.reason)).unwrap_or_default(),
    })
}

fn to_client_close(frame: Option<TClose>) -> Option<AxClose> {
    let f = frame?;
    let code = u16::from(f.code);
    sendable(code).then(|| AxClose {
        code,
        reason: AxUtf8::try_from(Bytes::from(f.reason)).unwrap_or_default(),
    })
}

/// Codes that may appear on the wire (1005/1006/1015 are local-only).
fn sendable(code: u16) -> bool {
    !matches!(code, 1005 | 1006 | 1015)
}

fn own_close(code: u16, reason: &'static str) -> AxClose {
    AxClose {
        code,
        reason: AxUtf8::from_static(reason),
    }
}

async fn timed<F: std::future::Future<Output = Result<(), E>>, E>(f: F) -> bool {
    matches!(tokio::time::timeout(SEND_TIMEOUT, f).await, Ok(Ok(())))
}

#[allow(clippy::too_many_lines)]
async fn pipe(link: CanvasLink, client: WebSocket, upstream: Upstream) {
    metrics::gauge!("canvas_link_ws_pipes").increment(1.0);
    let mut shutdown = copper_cloud_core::shutdown::subscribe();
    let mut closing = link.closing();
    let (mut c_tx, mut c_rx) = client.split();
    let (mut u_tx, mut u_rx) = upstream.split();
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING_EVERY, PING_EVERY);
    loop {
        tokio::select! {
            m = c_rx.next() => match m {
                Some(Ok(AxMessage::Binary(b))) => {
                    if !timed(u_tx.send(TMessage::Binary(b))).await {
                        let _ = c_tx.send(AxMessage::Close(Some(own_close(
                            close::SERVER_ERROR, "canvas server connection lost")))).await;
                        break;
                    }
                }
                Some(Ok(AxMessage::Text(t))) => {
                    let t = TUtf8::try_from(Bytes::from(t)).unwrap_or_default();
                    if !timed(u_tx.send(TMessage::Text(t))).await {
                        let _ = c_tx.send(AxMessage::Close(Some(own_close(
                            close::SERVER_ERROR, "canvas server connection lost")))).await;
                        break;
                    }
                }
                Some(Ok(AxMessage::Ping(_) | AxMessage::Pong(_))) => {}
                Some(Ok(AxMessage::Close(frame))) => {
                    let _ = timed(u_tx.send(TMessage::Close(to_upstream_close(frame)))).await;
                    // Wait (briefly) for Canvas Cloud's close reply so it sees a clean close.
                    let _ = tokio::time::timeout(CLOSE_GRACE, async {
                        while let Some(Ok(m)) = u_rx.next().await {
                            if matches!(m, TMessage::Close(_)) {
                                break;
                            }
                        }
                    })
                    .await;
                    let _ = tokio::time::timeout(CLOSE_GRACE, c_tx.close()).await;
                    break;
                }
                Some(Err(_)) | None => {
                    let _ = timed(u_tx.send(TMessage::Close(Some(TClose {
                        code: CloseCode::Away,
                        reason: "client went away".into(),
                    }))))
                    .await;
                    break;
                }
            },
            m = u_rx.next() => match m {
                Some(Ok(TMessage::Binary(b))) => {
                    if !timed(c_tx.send(AxMessage::Binary(b))).await {
                        let _ = timed(u_tx.send(TMessage::Close(Some(TClose {
                            code: CloseCode::Away,
                            reason: "client went away".into(),
                        })))).await;
                        break;
                    }
                }
                Some(Ok(TMessage::Text(t))) => {
                    let t = AxUtf8::try_from(Bytes::from(t)).unwrap_or_default();
                    if !timed(c_tx.send(AxMessage::Text(t))).await {
                        let _ = timed(u_tx.send(TMessage::Close(Some(TClose {
                            code: CloseCode::Away,
                            reason: "client went away".into(),
                        })))).await;
                        break;
                    }
                }
                Some(Ok(TMessage::Close(frame))) => {
                    let _ = timed(c_tx.send(AxMessage::Close(to_client_close(frame)))).await;
                    // Complete the upstream close handshake (tungstenite queued the reply).
                    let _ = tokio::time::timeout(CLOSE_GRACE, u_tx.close()).await;
                    // Give the client a moment to answer our close.
                    let _ = tokio::time::timeout(CLOSE_GRACE, async {
                        while let Some(Ok(m)) = c_rx.next().await {
                            if matches!(m, AxMessage::Close(_)) {
                                break;
                            }
                        }
                    })
                    .await;
                    break;
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => {
                    let _ = timed(c_tx.send(AxMessage::Close(Some(own_close(
                        close::SERVER_ERROR, "canvas server connection lost"))))).await;
                    break;
                }
            },
            _ = ping.tick() => {
                if !timed(c_tx.send(AxMessage::Ping(Bytes::new()))).await {
                    let _ = timed(u_tx.send(TMessage::Close(Some(TClose {
                        code: CloseCode::Away,
                        reason: "client went away".into(),
                    })))).await;
                    break;
                }
            }
            () = flagged(&mut shutdown) => {
                going_away(&mut c_tx, &mut u_tx).await;
                break;
            }
            () = flagged(&mut closing) => {
                going_away(&mut c_tx, &mut u_tx).await;
                break;
            }
        }
    }
    metrics::gauge!("canvas_link_ws_pipes").decrement(1.0);
}

async fn going_away<C, U>(c_tx: &mut C, u_tx: &mut U)
where
    C: futures::Sink<AxMessage> + Unpin,
    U: futures::Sink<TMessage> + Unpin,
{
    let _ = tokio::time::timeout(
        CLOSE_GRACE,
        c_tx.send(AxMessage::Close(Some(own_close(
            close::GOING_AWAY,
            "server shutting down",
        )))),
    )
    .await;
    let _ = tokio::time::timeout(
        CLOSE_GRACE,
        u_tx.send(TMessage::Close(Some(TClose {
            code: CloseCode::Away,
            reason: "server shutting down".into(),
        }))),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_frames_round_trip() {
        let up = to_upstream_close(Some(AxClose {
            code: 4403,
            reason: "removed".into(),
        }))
        .unwrap();
        assert_eq!(u16::from(up.code), 4403);
        assert_eq!(up.reason.as_str(), "removed");
        let down = to_client_close(Some(TClose {
            code: CloseCode::from(4404),
            reason: "deleted".into(),
        }))
        .unwrap();
        assert_eq!(down.code, 4404);
        assert_eq!(down.reason.as_str(), "deleted");
        assert!(to_client_close(Some(TClose {
            code: CloseCode::from(1005),
            reason: "".into(),
        }))
        .is_none());
        assert!(to_upstream_close(None).is_none());
    }
}
