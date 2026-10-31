//! The in-memory relay hub: which link has a frames stream open right now, the agent calls
//! waiting for a device's reply, and each agent token's rate limit. Single-process by design
//! (one daemon per instance), like [`crate::events`]. Every method takes a lock briefly and never
//! awaits while holding it.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// Progress frames buffered for one call; a call that falls this far behind loses the newest
/// (the frames POST never waits).
pub const PROGRESS_BUFFER: usize = 32;

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
    /// The user was disabled or deleted while the call was running.
    UserRemoved,
}

/// One device `progress` frame, forwarded to the waiting call.
#[derive(Debug)]
pub struct Progress {
    pub seq: i64,
    pub message: String,
}

/// A call for a device (see [`Hub::begin`]).
#[derive(Debug)]
pub struct Request {
    pub link: Uuid,
    pub user: Uuid,
    pub token: Uuid,
    /// The request id (`req_…`), also in `frame`.
    pub id: String,
    /// The `request` frame without `deadline_ms`, which is added each time it is sent (the time
    /// left then).
    pub frame: Map<String, Value>,
    /// How long the call may run.
    pub timeout: Duration,
}

/// A call handed to a device: wait on `reply` until `deadline`, forward `progress`.
pub struct Call {
    pub reply: oneshot::Receiver<Reply>,
    pub progress: mpsc::Receiver<Progress>,
    pub deadline: Instant,
}

/// Why [`Hub::begin`] did not hand a call to the device.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// No open stream, or not heard from within the online window.
    Offline,
    /// The link already has the most calls in flight it may.
    Busy,
}

struct Slot {
    stream: u64,
    user: Uuid,
    tx: mpsc::UnboundedSender<Down>,
    last_heard: Instant,
}

struct Pending {
    link: Uuid,
    user: Uuid,
    token: Uuid,
    /// Begin order, so a re-delivery keeps it.
    order: u64,
    frame: Map<String, Value>,
    deadline: Instant,
    reply: oneshot::Sender<Reply>,
    progress: mpsc::Sender<Progress>,
    last_seq: Option<i64>,
}

#[derive(Default)]
struct Inner {
    /// Link id → its current frames stream.
    slots: HashMap<Uuid, Slot>,
    /// Request id (`req_…`) → the call waiting for it.
    pending: HashMap<String, Pending>,
}

/// One agent token's rate limit (GCRA: `tat` is the theoretical arrival time).
struct Bucket {
    user: Uuid,
    tat: Instant,
}

/// The relay hub (lives in [`crate::state::AppState`]).
pub struct Hub {
    inner: Mutex<Inner>,
    buckets: Mutex<HashMap<Uuid, Bucket>>,
    next_stream: AtomicU64,
    next_call: AtomicU64,
    online_window: Duration,
    max_pending: usize,
    /// One request every `interval`, bursts up to `burst` ahead (one minute's worth).
    interval: Duration,
    burst: Duration,
    checks: AtomicU64,
    sweeping: AtomicBool,
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

/// The `request` event data: `frame` plus `deadline_ms`.
fn render(frame: &Map<String, Value>, deadline_ms: u64) -> String {
    #[derive(Serialize)]
    struct Out<'a> {
        #[serde(flatten)]
        frame: &'a Map<String, Value>,
        deadline_ms: u64,
    }
    serde_json::to_string(&Out { frame, deadline_ms }).unwrap_or_default()
}

/// Whole milliseconds, rounded up.
fn ceil_ms(d: Duration) -> u64 {
    u64::try_from(d.as_micros().div_ceil(1000)).unwrap_or(u64::MAX)
}

impl Hub {
    /// `per_minute` requests per agent token; a link is online while heard from within
    /// `online_window`; at most `max_pending` calls in flight per link.
    pub fn new(per_minute: u32, online_window: Duration, max_pending: usize) -> Self {
        let per_minute = per_minute.max(1);
        let interval = Duration::from_secs(60) / per_minute;
        Self {
            inner: Mutex::new(Inner::default()),
            buckets: Mutex::new(HashMap::new()),
            next_stream: AtomicU64::new(1),
            next_call: AtomicU64::new(1),
            online_window,
            max_pending: max_pending.max(1),
            interval,
            burst: interval * per_minute,
            checks: AtomicU64::new(0),
            sweeping: AtomicBool::new(false),
        }
    }

    /// `true` the first time only: whoever gets it runs the user sweep.
    pub(crate) fn claim_sweep(&self) -> bool {
        !self.sweeping.swap(true, Ordering::SeqCst)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_buckets(&self) -> MutexGuard<'_, HashMap<Uuid, Bucket>> {
        self.buckets.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `true` if agent token `token` (of `user`) may make another request now.
    pub fn allow(&self, user: Uuid, token: Uuid) -> bool {
        let now = Instant::now();
        let mut buckets = self.lock_buckets();
        if self.checks.fetch_add(1, Ordering::Relaxed) % 4096 == 4095 {
            // A bucket whose arrival time has passed is as good as a fresh one.
            buckets.retain(|_, b| b.tat > now);
            buckets.shrink_to_fit();
        }
        let bucket = buckets.entry(token).or_insert(Bucket { user, tat: now });
        let tat = bucket.tat.max(now) + self.interval;
        if tat > now + self.burst {
            return false;
        }
        bucket.tat = tat;
        true
    }

    /// A device opened its frames stream: it becomes the link's stream (the previous one, if
    /// any, gets [`Down::Superseded`]), and every call still waiting on the link is sent again
    /// on it, oldest first, with the same request id and the time it has left (a device answers
    /// a request it already answered with its stored reply). Returns the stream id (for
    /// [`Hub::detach`]) and the receiving end.
    pub fn attach(&self, link: Uuid, user: Uuid) -> (u64, mpsc::UnboundedReceiver<Down>) {
        let stream = self.next_stream.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        let old = {
            let mut inner = self.lock();
            let now = Instant::now();
            let mut waiting: Vec<&Pending> = inner
                .pending
                .values()
                .filter(|p| p.link == link && p.deadline > now)
                .collect();
            waiting.sort_unstable_by_key(|p| p.order);
            for p in waiting {
                let frame = render(&p.frame, ceil_ms(p.deadline - now));
                let _ = tx.send(Down::Request(frame));
            }
            inner.slots.insert(
                link,
                Slot {
                    stream,
                    user,
                    tx,
                    last_heard: now,
                },
            )
        };
        if let Some(old) = old {
            let _ = old.tx.send(Down::Superseded);
        }
        (stream, rx)
    }

    /// A frames stream ended. No-op if a newer stream already replaced it. Calls in flight stay
    /// pending: a new stream gets them again, or they fail once the device has been offline for
    /// the offline grace (see [`crate::device_agent`]'s relay).
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

    /// Hand a call to the device: sends the `request` frame down the link's stream and registers
    /// the waiter. Refused when the link is offline or already has the most calls in flight
    /// (the caller answers at once).
    pub fn begin(&self, req: Request) -> Result<Call, Refused> {
        let mut guard = self.lock();
        let inner = &mut *guard;
        let slot = inner
            .slots
            .get(&req.link)
            .filter(|s| s.last_heard.elapsed() < self.online_window)
            .ok_or(Refused::Offline)?;
        let running = inner
            .pending
            .values()
            .filter(|p| p.link == req.link)
            .count();
        if running >= self.max_pending {
            return Err(Refused::Busy);
        }
        let deadline = Instant::now() + req.timeout;
        slot.tx
            .send(Down::Request(render(&req.frame, ceil_ms(req.timeout))))
            .map_err(|_| Refused::Offline)?;
        let (reply_tx, reply_rx) = oneshot::channel();
        let (progress_tx, progress_rx) = mpsc::channel(PROGRESS_BUFFER);
        inner.pending.insert(
            req.id,
            Pending {
                link: req.link,
                user: req.user,
                token: req.token,
                order: self.next_call.fetch_add(1, Ordering::Relaxed),
                frame: req.frame,
                deadline,
                reply: reply_tx,
                progress: progress_tx,
                last_seq: None,
            },
        );
        Ok(Call {
            reply: reply_rx,
            progress: progress_rx,
            deadline,
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
    /// without one counts as the next. Never waits: a call whose buffer is full loses it.
    /// `false` when ignored or dropped.
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
        match p.progress.try_send(Progress { seq, message }) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Closed(_)) => false,
        }
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

    /// The calls in flight, across every link.
    pub fn in_flight(&self) -> usize {
        self.lock().pending.len()
    }

    /// The link was turned off (revoked, or its device deleted): close its stream with
    /// `revoked` and fail its calls in flight.
    pub fn revoke_link(&self, link: Uuid) {
        let (slot, failed) = {
            let mut inner = self.lock();
            let slot = inner.slots.remove(&link);
            (slot, take_pending(&mut inner, |p| p.link == link))
        };
        if let Some(slot) = slot {
            let _ = slot.tx.send(Down::Revoked);
        }
        for (_, p) in failed {
            let _ = p.reply.send(Reply::TurnedOff);
        }
    }

    /// An agent token was revoked: fail its calls in flight, tell the devices to stop them and
    /// forget its rate limit.
    pub fn revoke_token(&self, token: Uuid) {
        let failed = {
            let mut inner = self.lock();
            let failed = take_pending(&mut inner, |p| p.token == token);
            for (id, p) in &failed {
                if let Some(slot) = inner.slots.get(&p.link) {
                    let _ = slot.tx.send(Down::Cancel(id.clone()));
                }
            }
            failed
        };
        self.lock_buckets().remove(&token);
        for (_, p) in failed {
            let _ = p.reply.send(Reply::TokenRevoked);
        }
    }

    /// A user was disabled or deleted: close their streams with `revoked`, fail their calls in
    /// flight and forget their tokens' rate limits. `true` if the hub held anything of theirs.
    pub fn revoke_user(&self, user: Uuid) -> bool {
        let (slots, failed) = {
            let mut inner = self.lock();
            let links: Vec<Uuid> = inner
                .slots
                .iter()
                .filter(|(_, s)| s.user == user)
                .map(|(link, _)| *link)
                .collect();
            let slots: Vec<Slot> = links
                .iter()
                .filter_map(|link| inner.slots.remove(link))
                .collect();
            (slots, take_pending(&mut inner, |p| p.user == user))
        };
        let buckets = {
            let mut buckets = self.lock_buckets();
            let before = buckets.len();
            buckets.retain(|_, b| b.user != user);
            before - buckets.len()
        };
        let touched = !slots.is_empty() || !failed.is_empty() || buckets > 0;
        for slot in slots {
            let _ = slot.tx.send(Down::Revoked);
        }
        for (_, p) in failed {
            let _ = p.reply.send(Reply::UserRemoved);
        }
        touched
    }

    /// Every user with an open stream, a call in flight or a live rate limit here.
    pub fn users(&self) -> Vec<Uuid> {
        let mut users: HashSet<Uuid> = {
            let inner = self.lock();
            inner
                .slots
                .values()
                .map(|s| s.user)
                .chain(inner.pending.values().map(|p| p.user))
                .collect()
        };
        users.extend(self.lock_buckets().values().map(|b| b.user));
        users.into_iter().collect()
    }
}

/// Remove and return the calls matching `pred`.
fn take_pending(inner: &mut Inner, pred: impl Fn(&Pending) -> bool) -> Vec<(String, Pending)> {
    let ids: Vec<String> = inner
        .pending
        .iter()
        .filter(|(_, p)| pred(p))
        .map(|(id, _)| id.clone())
        .collect();
    ids.into_iter()
        .filter_map(|id| inner.pending.remove(&id).map(|p| (id, p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hub() -> Hub {
        Hub::new(3, Duration::from_secs(45), 8)
    }

    fn raw(s: &str) -> Box<RawValue> {
        RawValue::from_string(s.to_owned()).unwrap()
    }

    fn req(link: Uuid, user: Uuid, token: Uuid, id: &str) -> Request {
        let Value::Object(frame) = json!({ "id": id, "method": "tools/call" }) else {
            unreachable!()
        };
        Request {
            link,
            user,
            token,
            id: id.to_owned(),
            frame,
            timeout: Duration::from_secs(200),
        }
    }

    fn request_of(down: Option<Down>) -> Value {
        match down {
            Some(Down::Request(f)) => serde_json::from_str(&f).unwrap(),
            other => panic!("not a request: {other:?}"),
        }
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
        let (link, user) = (Uuid::now_v7(), Uuid::now_v7());
        assert!(!h.is_online(link));
        let (first, mut rx1) = h.attach(link, user);
        assert!(h.is_online(link));
        let (second, _rx2) = h.attach(link, user);
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
        let (link, other, user, token) = (
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
        );
        assert_eq!(
            h.begin(req(link, user, token, "req_a")).err(),
            Some(Refused::Offline)
        );
        let (_, mut rx) = h.attach(link, user);
        let mut call = h.begin(req(link, user, token, "req_a")).unwrap();
        let sent = request_of(rx.recv().await);
        assert_eq!(sent["id"], "req_a");
        assert_eq!(sent["deadline_ms"], 200_000);
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
        assert_eq!(h.in_flight(), 1);
        assert!(h.reply(link, "req_a", Reply::Result(raw(r#"{"content":[]}"#))));
        assert!(matches!(call.reply.await, Ok(Reply::Result(r)) if r.get() == r#"{"content":[]}"#));
        assert!(
            !h.reply(link, "req_a", Reply::Error("late".into())),
            "finished"
        );
        assert!(!h.cancel("req_a"), "finished");
        assert_eq!(h.in_flight(), 0);
    }

    #[tokio::test]
    async fn a_new_stream_gets_the_calls_still_waiting() {
        let h = hub();
        let (link, user, token) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let (first, mut rx1) = h.attach(link, user);
        let _a = h.begin(req(link, user, token, "req_1")).unwrap();
        let _b = h.begin(req(link, user, token, "req_2")).unwrap();
        let _c = h.begin(req(link, user, token, "req_3")).unwrap();
        for id in ["req_1", "req_2", "req_3"] {
            assert_eq!(request_of(rx1.recv().await)["id"], id);
        }
        assert!(h.cancel("req_2"));
        // The stream dies without a reply; the calls stay pending.
        assert!(h.detach(link, first));
        assert_eq!(h.in_flight(), 2);
        let (_, mut rx2) = h.attach(link, user);
        for id in ["req_1", "req_3"] {
            let sent = request_of(rx2.recv().await);
            assert_eq!(sent["id"], id);
            assert_eq!(sent["method"], "tools/call");
            let left = sent["deadline_ms"].as_u64().unwrap();
            assert!(left > 190_000 && left <= 200_000, "{left}");
        }
        assert!(h.reply(link, "req_1", Reply::Error("done".into())));
        // Superseded while still open: the new stream gets what is left.
        let (_, mut rx3) = h.attach(link, user);
        assert!(matches!(rx2.recv().await, Some(Down::Superseded)));
        assert_eq!(request_of(rx3.recv().await)["id"], "req_3");
        assert!(rx3.try_recv().is_err());
    }

    #[tokio::test]
    async fn busy_past_the_per_link_cap() {
        let h = Hub::new(600, Duration::from_secs(45), 2);
        let (link, other, user, token) = (
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
        );
        let (_, _rx) = h.attach(link, user);
        let (_, _rx_other) = h.attach(other, user);
        let _a = h.begin(req(link, user, token, "req_a")).unwrap();
        let _b = h.begin(req(link, user, token, "req_b")).unwrap();
        assert_eq!(
            h.begin(req(link, user, token, "req_c")).err(),
            Some(Refused::Busy)
        );
        assert!(
            h.begin(req(other, user, token, "req_o")).is_ok(),
            "per link"
        );
        assert!(h.cancel("req_a"));
        assert!(h.begin(req(link, user, token, "req_c")).is_ok());
    }

    #[tokio::test]
    async fn progress_never_waits_for_a_slow_call() {
        let h = hub();
        let (link, user, token) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let (_, _rx) = h.attach(link, user);
        let mut call = h.begin(req(link, user, token, "req_p")).unwrap();
        let mut sent = 0;
        for seq in 1..=i64::try_from(PROGRESS_BUFFER + 10).unwrap() {
            sent += usize::from(h.progress(link, "req_p", Some(seq), "x".into()));
        }
        assert_eq!(sent, PROGRESS_BUFFER, "the overflow is dropped");
        assert_eq!(call.progress.recv().await.unwrap().seq, 1);
        assert!(h.progress(link, "req_p", None, "room again".into()));
    }

    #[tokio::test]
    async fn cancel_revoke_and_token_revoke() {
        let h = hub();
        let (link, user, token, other_token) = (
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
        );
        let (_, mut rx) = h.attach(link, user);
        let _c = h.begin(req(link, user, token, "req_c")).unwrap();
        rx.recv().await.unwrap();
        assert!(h.cancel("req_c"));
        assert!(matches!(rx.recv().await, Some(Down::Cancel(id)) if id == "req_c"));

        let t = h.begin(req(link, user, token, "req_t")).unwrap();
        let o = h.begin(req(link, user, other_token, "req_o")).unwrap();
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

    #[tokio::test]
    async fn revoke_user_closes_streams_fails_calls_and_forgets_rate_limits() {
        let h = hub();
        let (ada, bob) = (Uuid::now_v7(), Uuid::now_v7());
        let (ada_phone, ada_pad, bob_phone) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let (ada_token, bob_token) = (Uuid::now_v7(), Uuid::now_v7());
        let (_, mut rx_phone) = h.attach(ada_phone, ada);
        let (_, mut rx_pad) = h.attach(ada_pad, ada);
        let (_, mut rx_bob) = h.attach(bob_phone, bob);
        assert!(h.allow(ada, ada_token) && h.allow(bob, bob_token));
        let a = h.begin(req(ada_phone, ada, ada_token, "req_a")).unwrap();
        let b = h.begin(req(bob_phone, bob, bob_token, "req_b")).unwrap();
        rx_phone.recv().await.unwrap();
        rx_bob.recv().await.unwrap();
        let mut users = h.users();
        users.sort_unstable();
        let mut both = vec![ada, bob];
        both.sort_unstable();
        assert_eq!(users, both);

        assert!(h.revoke_user(ada));
        assert!(matches!(rx_phone.recv().await, Some(Down::Revoked)));
        assert!(matches!(rx_pad.recv().await, Some(Down::Revoked)));
        assert!(matches!(a.reply.await, Ok(Reply::UserRemoved)));
        assert!(!h.is_online(ada_phone) && !h.is_online(ada_pad));
        assert_eq!(h.users(), vec![bob]);
        assert!(!h.revoke_user(ada), "nothing left");
        // Bob is untouched.
        assert!(h.is_online(bob_phone));
        assert!(h.reply(bob_phone, "req_b", Reply::Error("fine".into())));
        assert!(matches!(b.reply.await, Ok(Reply::Error(_))));
    }

    #[test]
    fn online_needs_a_recent_heartbeat() {
        let h = Hub::new(3, Duration::from_millis(30), 8);
        let (link, user) = (Uuid::now_v7(), Uuid::now_v7());
        let _stream = h.attach(link, user);
        assert!(h.is_online(link));
        std::thread::sleep(Duration::from_millis(40));
        assert!(!h.is_online(link));
        assert_eq!(
            h.begin(req(link, user, Uuid::now_v7(), "req_x")).err(),
            Some(Refused::Offline)
        );
        h.heard(link);
        assert!(h.is_online(link));
    }

    #[test]
    fn rate_limit_per_token() {
        let h = hub();
        let (user, a, b) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        assert!(h.allow(user, a) && h.allow(user, a) && h.allow(user, a));
        assert!(!h.allow(user, a));
        assert!(h.allow(user, b));
        h.revoke_token(a);
        assert!(h.allow(user, a), "a revoked token's bucket is forgotten");
    }

    #[test]
    fn rate_limit_refills() {
        let h = Hub::new(600, Duration::from_secs(45), 8);
        let (user, token) = (Uuid::now_v7(), Uuid::now_v7());
        assert_eq!((0..700).filter(|_| h.allow(user, token)).count(), 600);
        std::thread::sleep(Duration::from_millis(250));
        assert!(h.allow(user, token), "one request every 100 ms comes back");
    }
}
