//! The in-memory relay hub: which link has a frames stream open right now, and the agent calls
//! waiting for a device's reply. Single-process by design (one daemon per instance), like
//! [`crate::events`]. Every method takes the lock briefly and never awaits while holding it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use serde_json::value::RawValue;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// What the cloud sends down a device's frames stream (`GET /v1/me/links/{id}/frames`).
#[derive(Debug)]
pub enum Down {
    /// A `request` event; the data (one JSON object) is already serialized.
    Request(String),
    /// A `cancel` event for this request id.
    Cancel(String),
    /// A newer stream for the same link replaced this one: `superseded`, then close.
    Superseded,
    /// The link was turned off: `revoked`, then close.
    Revoked,
}

/// How a pending call ended.
#[derive(Debug)]
pub enum Reply {
    /// The device's `result`, exactly as it sent it.
    Result(Box<RawValue>),
    /// The device's `error.message`.
    Error(String),
    /// The link was revoked (or its device deleted) while the call was running.
    TurnedOff,
    /// The agent's token was revoked while the call was running.
    TokenRevoked,
}

/// One device `progress` frame, forwarded to the waiting call.
#[derive(Debug)]
pub struct Progress {
    pub seq: i64,
    pub message: String,
}

/// A call handed to a device: wait on `reply`, forward `progress`.
pub struct Call {
    pub reply: oneshot::Receiver<Reply>,
    pub progress: mpsc::UnboundedReceiver<Progress>,
}

struct Slot {
    stream: u64,
    tx: mpsc::UnboundedSender<Down>,
    last_heard: Instant,
}

struct Pending {
    link: Uuid,
    token: Uuid,
    reply: oneshot::Sender<Reply>,
    progress: mpsc::UnboundedSender<Progress>,
    last_seq: Option<i64>,
}

#[derive(Default)]
struct Inner {
    /// Link id → its current frames stream.
    slots: HashMap<Uuid, Slot>,
    /// Request id (`req_…`) → the call waiting for it.
    pending: HashMap<String, Pending>,
}

/// The relay hub (lives in [`crate::state::AppState`]).
pub struct Hub {
    inner: Mutex<Inner>,
    next_stream: AtomicU64,
    online_window: Duration,
    limiter: DefaultKeyedRateLimiter<Uuid>,
    checks: AtomicU64,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.lock();
        f.debug_struct("Hub")
            .field("streams", &inner.slots.len())
            .field("pending", &inner.pending.len())
            .finish_non_exhaustive()
    }
}

/// A fresh request id: `req_` + 22 base64url chars (16 random bytes).
pub fn new_request_id() -> String {
    format!(
        "req_{}",
        crate::ids::b64url(&crate::ids::random_bytes::<16>())
    )
}

impl Hub {
    /// `per_minute` requests per agent token; a link is online while heard from within
    /// `online_window`.
    pub fn new(per_minute: u32, online_window: Duration) -> Self {
        let n = std::num::NonZeroU32::new(per_minute.max(1)).expect("non-zero");
        Self {
            inner: Mutex::new(Inner::default()),
            next_stream: AtomicU64::new(1),
            online_window,
            limiter: RateLimiter::keyed(Quota::per_minute(n)),
            checks: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `true` if this agent token may make another request now.
    pub fn allow(&self, token: Uuid) -> bool {
        if self.checks.fetch_add(1, Ordering::Relaxed) % 4096 == 4095 {
            self.limiter.retain_recent();
            self.limiter.shrink_to_fit();
        }
        self.limiter.check_key(&token).is_ok()
    }

    /// A device opened its frames stream: it becomes the link's stream (the previous one, if
    /// any, gets [`Down::Superseded`]). Returns the stream id (for [`Hub::detach`]) and the
    /// receiving end.
    pub fn attach(&self, link: Uuid) -> (u64, mpsc::UnboundedReceiver<Down>) {
        let stream = self.next_stream.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        let old = self.lock().slots.insert(
            link,
            Slot {
                stream,
                tx,
                last_heard: Instant::now(),
            },
        );
        if let Some(old) = old {
            let _ = old.tx.send(Down::Superseded);
        }
        (stream, rx)
    }

    /// A frames stream ended. No-op if a newer stream already replaced it. Calls in flight stay
    /// pending (the device may reconnect and still reply).
    pub fn detach(&self, link: Uuid, stream: u64) -> bool {
        let mut inner = self.lock();
        if inner.slots.get(&link).is_some_and(|s| s.stream == stream) {
            inner.slots.remove(&link);
            true
        } else {
            false
        }
    }

    /// The device was heard from (heartbeat or any frames POST).
    pub fn heard(&self, link: Uuid) {
        if let Some(slot) = self.lock().slots.get_mut(&link) {
            slot.last_heard = Instant::now();
        }
    }

    /// Online: a stream is open and the device was heard from within the online window.
    pub fn is_online(&self, link: Uuid) -> bool {
        self.lock()
            .slots
            .get(&link)
            .is_some_and(|s| s.last_heard.elapsed() < self.online_window)
    }

    /// Hand a call to the device: sends `frame` (the serialized `request` data carrying `id`)
    /// down the link's stream and registers the waiter. `None` when the link is offline (the
    /// caller answers at once).
    pub fn begin(&self, link: Uuid, token: Uuid, id: &str, frame: String) -> Option<Call> {
        let mut inner = self.lock();
        let slot = inner
            .slots
            .get(&link)
            .filter(|s| s.last_heard.elapsed() < self.online_window)?;
        slot.tx.send(Down::Request(frame)).ok()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        let (progress_tx, progress_rx) = mpsc::unbounded_channel();
        inner.pending.insert(
            id.to_owned(),
            Pending {
                link,
                token,
                reply: reply_tx,
                progress: progress_tx,
                last_seq: None,
            },
        );
        Some(Call {
            reply: reply_rx,
            progress: progress_rx,
        })
    }

    /// A device's reply. Only the link the request went to may answer it; a reply for an
    /// unknown or finished request is ignored (`false`).
    pub fn reply(&self, link: Uuid, id: &str, reply: Reply) -> bool {
        let pending = {
            let mut inner = self.lock();
            if inner.pending.get(id).is_none_or(|p| p.link != link) {
                return false;
            }
            inner.pending.remove(id)
        };
        pending.is_some_and(|p| p.reply.send(reply).is_ok())
    }

    /// A device's progress frame. `seq` must grow (a stale or repeated one is ignored); a frame
    /// without one counts as the next. `false` when ignored.
    pub fn progress(&self, link: Uuid, id: &str, seq: Option<i64>, message: String) -> bool {
        let mut inner = self.lock();
        let Some(p) = inner.pending.get_mut(id).filter(|p| p.link == link) else {
            return false;
        };
        let seq = seq.unwrap_or_else(|| p.last_seq.map_or(1, |s| s.saturating_add(1)));
        if p.last_seq.is_some_and(|last| seq <= last) {
            return false;
        }
        p.last_seq = Some(seq);
        p.progress.send(Progress { seq, message }).is_ok()
    }

    /// Stop waiting for a call: forget it and send the device a `cancel` if its stream is
    /// open. `false` if the call had already finished.
    pub fn cancel(&self, id: &str) -> bool {
        let mut inner = self.lock();
        let Some(p) = inner.pending.remove(id) else {
            return false;
        };
        if let Some(slot) = inner.slots.get(&p.link) {
            let _ = slot.tx.send(Down::Cancel(id.to_owned()));
        }
        true
    }

    /// The link was turned off (revoked, or its device deleted): close its stream with
    /// `revoked` and fail its calls in flight.
    pub fn revoke_link(&self, link: Uuid) {
        let (slot, failed) = {
            let mut inner = self.lock();
            let slot = inner.slots.remove(&link);
            let ids: Vec<String> = inner
                .pending
                .iter()
                .filter(|(_, p)| p.link == link)
                .map(|(id, _)| id.clone())
                .collect();
            let failed: Vec<Pending> = ids
                .iter()
                .filter_map(|id| inner.pending.remove(id))
                .collect();
            (slot, failed)
        };
        if let Some(slot) = slot {
            let _ = slot.tx.send(Down::Revoked);
        }
        for p in failed {
            let _ = p.reply.send(Reply::TurnedOff);
        }
    }

    /// An agent token was revoked: fail its calls in flight and tell the devices to stop them.
    pub fn revoke_token(&self, token: Uuid) {
        let failed = {
            let mut inner = self.lock();
            let ids: Vec<String> = inner
                .pending
                .iter()
                .filter(|(_, p)| p.token == token)
                .map(|(id, _)| id.clone())
                .collect();
            let mut failed = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(p) = inner.pending.remove(&id) {
                    if let Some(slot) = inner.slots.get(&p.link) {
                        let _ = slot.tx.send(Down::Cancel(id));
                    }
                    failed.push(p);
                }
            }
            failed
        };
        for p in failed {
            let _ = p.reply.send(Reply::TokenRevoked);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub() -> Hub {
        Hub::new(3, Duration::from_secs(45))
    }

    fn raw(s: &str) -> Box<RawValue> {
        RawValue::from_string(s.to_owned()).unwrap()
    }

    #[test]
    fn request_ids() {
        let id = new_request_id();
        assert!(id.starts_with("req_") && id.len() == 4 + 22, "{id}");
        assert_ne!(id, new_request_id());
    }

    #[tokio::test]
    async fn attach_supersedes_and_detach_only_removes_its_own_stream() {
        let h = hub();
        let link = Uuid::now_v7();
        assert!(!h.is_online(link));
        let (first, mut rx1) = h.attach(link);
        assert!(h.is_online(link));
        let (second, _rx2) = h.attach(link);
        assert!(matches!(rx1.recv().await, Some(Down::Superseded)));
        assert!(
            !h.detach(link, first),
            "the old stream must not detach the new one"
        );
        assert!(h.is_online(link));
        assert!(h.detach(link, second));
        assert!(!h.is_online(link));
    }

    #[tokio::test]
    async fn calls_go_to_the_link_and_only_it_may_answer() {
        let h = hub();
        let (link, other, token) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        assert!(
            h.begin(link, token, "req_a", "{}".into()).is_none(),
            "offline"
        );
        let (_, mut rx) = h.attach(link);
        let mut call = h
            .begin(link, token, "req_a", r#"{"id":"req_a"}"#.into())
            .unwrap();
        assert!(matches!(rx.recv().await, Some(Down::Request(f)) if f.contains("req_a")));
        assert!(!h.reply(other, "req_a", Reply::Error("x".into())));
        assert!(h.progress(link, "req_a", Some(2), "two".into()));
        assert!(
            !h.progress(link, "req_a", Some(2), "again".into()),
            "stale seq"
        );
        assert!(
            !h.progress(link, "req_a", Some(1), "older".into()),
            "stale seq"
        );
        assert!(h.progress(link, "req_a", None, "next".into()));
        assert_eq!(call.progress.recv().await.unwrap().seq, 2);
        assert_eq!(call.progress.recv().await.unwrap().seq, 3);
        assert!(h.reply(link, "req_a", Reply::Result(raw(r#"{"content":[]}"#))));
        assert!(matches!(call.reply.await, Ok(Reply::Result(r)) if r.get() == r#"{"content":[]}"#));
        assert!(
            !h.reply(link, "req_a", Reply::Error("late".into())),
            "finished"
        );
        assert!(!h.cancel("req_a"), "finished");
    }

    #[tokio::test]
    async fn cancel_revoke_and_token_revoke() {
        let h = hub();
        let (link, token, other_token) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let (_, mut rx) = h.attach(link);
        let _c = h.begin(link, token, "req_c", "{}".into()).unwrap();
        rx.recv().await.unwrap();
        assert!(h.cancel("req_c"));
        assert!(matches!(rx.recv().await, Some(Down::Cancel(id)) if id == "req_c"));

        let t = h.begin(link, token, "req_t", "{}".into()).unwrap();
        let o = h.begin(link, other_token, "req_o", "{}".into()).unwrap();
        rx.recv().await.unwrap();
        rx.recv().await.unwrap();
        h.revoke_token(token);
        assert!(matches!(t.reply.await, Ok(Reply::TokenRevoked)));
        assert!(matches!(rx.recv().await, Some(Down::Cancel(id)) if id == "req_t"));

        h.revoke_link(link);
        assert!(matches!(rx.recv().await, Some(Down::Revoked)));
        assert!(matches!(o.reply.await, Ok(Reply::TurnedOff)));
        assert!(!h.is_online(link));
    }

    #[test]
    fn online_needs_a_recent_heartbeat() {
        let h = Hub::new(3, Duration::from_millis(30));
        let link = Uuid::now_v7();
        let _stream = h.attach(link);
        assert!(h.is_online(link));
        std::thread::sleep(Duration::from_millis(40));
        assert!(!h.is_online(link));
        assert!(h
            .begin(link, Uuid::now_v7(), "req_x", "{}".into())
            .is_none());
        h.heard(link);
        assert!(h.is_online(link));
    }

    #[test]
    fn rate_limit_per_token() {
        let h = hub();
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        assert!(h.allow(a) && h.allow(a) && h.allow(a));
        assert!(!h.allow(a));
        assert!(h.allow(b));
    }
}
