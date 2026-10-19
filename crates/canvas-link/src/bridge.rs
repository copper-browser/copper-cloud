//! Event bridge (CONTRACT §7.3): while a Canvas Cloud is linked, one SSE client on
//! `GET {url}/v1/apps/self/events` (`Last-Event-ID` resume, exponential backoff 1 s → 30 s).
//! Each `canvas` event is re-published to the Copper user whose id is the event's `subject`
//! as `Event::Canvas {canvas_id, kind}`, so Copper's `/v1/sync/events` stream carries the same
//! `event: canvas` frames as before the split. A `resync` (Canvas Cloud could not resume from
//! our `Last-Event-ID`, e.g. after it restarted, so canvas events may have been missed) is
//! passed on to every connected Copper user as `event: resync`, which Copper clients answer by
//! refetching; keep-alive comments only feed the idle watchdog.

use std::time::Duration;

use copper_cloud_core::events::Event;
use futures::StreamExt as _;
use reqwest::header::{ACCEPT, AUTHORIZATION};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::upstream::{error_fields, root_cause, CONNECT_TIMEOUT};
use crate::{Active, CanvasLink};

/// Reconnect backoff bounds.
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Canvas Cloud sends a keep-alive every 15 s; reconnect after this much silence.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(45);
/// How often a live session re-checks the settings (CLI edits from another process).
const SETTINGS_POLL: Duration = Duration::from_secs(5);
/// Largest single SSE event we buffer.
const MAX_EVENT_BYTES: usize = 256 * 1024;

/// One dispatched SSE event.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
    pub id: Option<String>,
}

/// Incremental `text/event-stream` parser (WHATWG rules: `event`, `data` (joined by `\n`),
/// `id`; comments and `retry` ignored; CR, LF and CRLF line ends).
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: String,
    data: Vec<String>,
    id: Option<String>,
    has_fields: bool,
}

impl SseParser {
    /// Feed bytes; returns the events completed by them.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n' || *b == b'\r') {
            // A lone trailing `\r` might be the first half of `\r\n`: wait for more.
            if self.buf[pos] == b'\r' && pos + 1 == self.buf.len() {
                break;
            }
            let skip = if self.buf[pos] == b'\r' && self.buf.get(pos + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
            let line: Vec<u8> = self.buf.drain(..pos + skip).take(pos).collect();
            let line = String::from_utf8_lossy(&line);
            if let Some(ev) = self.line(&line) {
                out.push(ev);
            }
        }
        if self.buf.len() > MAX_EVENT_BYTES {
            self.buf.clear();
        }
        out
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            if !self.has_fields {
                return None;
            }
            self.has_fields = false;
            let ev = SseEvent {
                event: if self.event.is_empty() {
                    "message".to_owned()
                } else {
                    std::mem::take(&mut self.event)
                },
                data: std::mem::take(&mut self.data).join("\n"),
                id: self.id.clone(),
            };
            return Some(ev);
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => {
                value.clone_into(&mut self.event);
                self.has_fields = true;
            }
            "data" => {
                if self.data.iter().map(String::len).sum::<usize>() < MAX_EVENT_BYTES {
                    self.data.push(value.to_owned());
                }
                self.has_fields = true;
            }
            "id" if !value.contains('\0') => {
                self.id = Some(value.to_owned());
                self.has_fields = true;
            }
            _ => {}
        }
        None
    }
}

#[derive(Deserialize)]
struct CanvasEventData {
    subject: Option<String>,
    user_id: Option<Uuid>,
    canvas_id: Uuid,
    kind: String,
}

/// Map one SSE event to `(copper user id, Event::Canvas)`. The subject is the Copper user id
/// (provisioning uses it as both subject and id hint); `user_id` is the fallback.
pub fn canvas_event(ev: &SseEvent) -> Option<(Uuid, Event)> {
    if ev.event != "canvas" {
        return None;
    }
    let data: CanvasEventData = serde_json::from_str(&ev.data).ok()?;
    let user = data
        .subject
        .as_deref()
        .and_then(|s| s.parse::<Uuid>().ok())
        .or(data.user_id)?;
    Some((
        user,
        Event::Canvas {
            canvas_id: data.canvas_id,
            kind: data.kind,
        },
    ))
}

/// The bridge supervisor: runs for the life of the process.
pub async fn run(link: CanvasLink) {
    let mut generation = link.subscribe();
    let mut shutdown = copper_cloud_core::shutdown::subscribe();
    let mut last_id: Option<String> = None;
    let mut last_base: Option<String> = None;
    let mut backoff = BACKOFF_MIN;
    loop {
        if copper_cloud_core::shutdown::is_shutting_down() {
            break;
        }
        let active = match link.enabled().await {
            Ok(a) => a,
            Err(err) => {
                tracing::warn!(error = %err, "canvas link settings unavailable");
                None
            }
        };
        let Some(active) = active else {
            link.update_bridge(|b| {
                b.connected = false;
                b.last_error = None;
            });
            tokio::select! {
                _ = generation.changed() => {}
                () = tokio::time::sleep(SETTINGS_POLL) => {}
                _ = shutdown.changed() => {}
            }
            continue;
        };
        if last_base.as_deref() != Some(active.upstream.base()) {
            last_id = None;
            last_base = Some(active.upstream.base().to_owned());
        }
        let result = tokio::select! {
            r = session(&link, &active, &mut last_id, &mut backoff) => Some(r),
            () = settings_changed(&link, &active) => None,
            _ = shutdown.changed() => None,
        };
        link.update_bridge(|b| b.connected = false);
        match result {
            None => backoff = BACKOFF_MIN,
            Some(Err(err)) => {
                tracing::warn!(error = %err, retry_in_s = backoff.as_secs(), "canvas event bridge disconnected");
                link.update_bridge(|b| b.last_error = Some(err));
                tokio::select! {
                    () = tokio::time::sleep(backoff) => {}
                    () = settings_changed(&link, &active) => {}
                    _ = shutdown.changed() => {}
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
            Some(Ok(())) => {}
        }
    }
    link.update_bridge(|b| b.connected = false);
}

/// Resolves when the effective link is no longer `active` (unlinked, re-keyed, …).
async fn settings_changed(link: &CanvasLink, active: &std::sync::Arc<Active>) {
    loop {
        tokio::time::sleep(SETTINGS_POLL).await;
        match link.enabled().await {
            Ok(Some(now)) if std::sync::Arc::ptr_eq(&now, active) => {}
            Ok(_) => return,
            Err(_) => {}
        }
    }
}

/// One SSE connection until it ends. `Ok(())` = the server closed the stream cleanly.
async fn session(
    link: &CanvasLink,
    active: &Active,
    last_id: &mut Option<String>,
    backoff: &mut Duration,
) -> Result<(), String> {
    let up = &active.upstream;
    let mut rb = up
        .client()
        .get(up.url("/v1/apps/self/events"))
        .header(AUTHORIZATION, up.auth_header())
        .header(ACCEPT, "text/event-stream");
    if let Some(id) = last_id.as_deref() {
        rb = rb.header("last-event-id", id);
    }
    let resp = tokio::time::timeout(CONNECT_TIMEOUT * 3, rb.send())
        .await
        .map_err(|_| "Canvas Cloud event stream did not answer in time".to_owned())?
        .map_err(|e| format!("Canvas Cloud event stream unreachable ({})", root_cause(&e)))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.bytes().await.unwrap_or_default();
        let (code, message) = error_fields(&body);
        return Err(format!(
            "Canvas Cloud event stream answered {}{}{}",
            status.as_u16(),
            code.map(|c| format!(" {c}")).unwrap_or_default(),
            message.map(|m| format!(": {m}")).unwrap_or_default()
        ));
    }
    *backoff = BACKOFF_MIN;
    link.update_bridge(|b| {
        b.connected = true;
        b.last_error = None;
    });
    tracing::info!(url = up.base(), "canvas event bridge connected");
    let state = link.state();
    let mut parser = SseParser::default();
    let mut stream = resp.bytes_stream();
    loop {
        let chunk = match tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await {
            Err(_) => return Err("Canvas Cloud event stream went silent".into()),
            Ok(None) => return Ok(()),
            Ok(Some(Err(e))) => {
                return Err(format!(
                    "Canvas Cloud event stream broke ({})",
                    root_cause(&e)
                ))
            }
            Ok(Some(Ok(c))) => c,
        };
        for ev in parser.feed(&chunk) {
            if ev.id.is_some() {
                last_id.clone_from(&ev.id);
            }
            match ev.event.as_str() {
                "canvas" => {
                    if let Some((user, event)) = canvas_event(&ev) {
                        state.events.publish(user, event);
                        metrics::counter!("canvas_link_events_total").increment(1);
                    } else {
                        tracing::debug!(data = %ev.data, "ignoring undecodable canvas event");
                    }
                    link.update_bridge(|b| b.last_event_at = Some(OffsetDateTime::now_utc()));
                }
                "resync" => {
                    tracing::info!("canvas event bridge: resync (events may have been missed); telling clients to refetch");
                    state.events.publish_all(&Event::Resync {
                        reason: "canvas".into(),
                    });
                    link.update_bridge(|b| b.last_event_at = Some(OffsetDateTime::now_utc()));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sse() {
        let mut p = SseParser::default();
        let evs = p.feed(b": keepalive\n\nevent: canvas\nid: 7\ndata: {\"a\":1}\n\nevent: resync\r\ndata\r\n\r\n");
        assert_eq!(
            evs,
            vec![
                SseEvent {
                    event: "canvas".into(),
                    data: "{\"a\":1}".into(),
                    id: Some("7".into())
                },
                SseEvent {
                    event: "resync".into(),
                    data: String::new(),
                    id: Some("7".into())
                },
            ]
        );
        // Split across chunks, including inside a CRLF.
        let mut p = SseParser::default();
        assert_eq!(p.feed(b"event: can"), Vec::<SseEvent>::new());
        assert_eq!(p.feed(b"vas\r"), Vec::<SseEvent>::new());
        assert_eq!(p.feed(b"\ndata: x\ndata: y\r"), Vec::<SseEvent>::new());
        let evs = p.feed(b"\n\r\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "x\ny");
        assert_eq!(evs[0].id, None);
    }

    #[test]
    fn maps_canvas_events() {
        let user = Uuid::now_v7();
        let canvas = Uuid::now_v7();
        let ev = SseEvent {
            event: "canvas".into(),
            data: format!(
                r#"{{"subject":"{user}","user_id":"{}","canvas_id":"{canvas}","kind":"renamed"}}"#,
                Uuid::nil()
            ),
            id: Some("1".into()),
        };
        let (u, e) = canvas_event(&ev).unwrap();
        assert_eq!(u, user);
        assert!(
            matches!(e, Event::Canvas { canvas_id, ref kind } if canvas_id == canvas && kind == "renamed")
        );
        let ev = SseEvent {
            event: "canvas".into(),
            data: format!(
                r#"{{"subject":"x","user_id":"{user}","canvas_id":"{canvas}","kind":"update"}}"#
            ),
            id: None,
        };
        assert_eq!(canvas_event(&ev).unwrap().0, user);
        assert!(canvas_event(&SseEvent {
            event: "resync".into(),
            ..SseEvent::default()
        })
        .is_none());
    }
}
