//! Per-user in-process event fan-out (SSE `/v1/sync/events`, canvas notifications).
//!
//! One `tokio::sync::broadcast` channel per user that currently has at least one subscriber.
//! Publishing to a user nobody is listening to is a cheap map lookup; empty channels are
//! dropped on the next publish. Single-process only (one daemon per instance).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::broadcast;
use uuid::Uuid;

/// Per-user channel capacity. A subscriber that falls this far behind gets `Lagged` and is
/// told to resync.
const CHANNEL_CAPACITY: usize = 256;

#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A sync doc changed. `device_id` is the writer.
    Doc {
        domain: String,
        version: i64,
        device_id: Option<Uuid>,
    },
    /// New history rows exist up to and including `seq`.
    History { seq: i64 },
    /// Something about a canvas changed (`kind` e.g. `created`, `renamed`, `deleted`,
    /// `invited` (new invite or a re-send reminder), `invite_revoked`, `invite_declined`,
    /// `member_added`, `member_removed`, `update`).
    Canvas { canvas_id: Uuid, kind: String },
}

impl Event {
    /// SSE `event:` name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Doc { .. } => "doc",
            Self::History { .. } => "history",
            Self::Canvas { .. } => "canvas",
        }
    }
}

#[derive(Clone, Default)]
pub struct Events {
    inner: Arc<Mutex<HashMap<Uuid, broadcast::Sender<Event>>>>,
}

impl std::fmt::Debug for Events {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Events")
            .field("users", &self.lock().len())
            .finish()
    }
}

impl Events {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, broadcast::Sender<Event>>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Deliver `ev` to every live subscriber of `user_id`. Never blocks.
    pub fn publish(&self, user_id: Uuid, ev: Event) {
        let mut map = self.lock();
        if let Some(tx) = map.get(&user_id) {
            if tx.send(ev).is_err() {
                // No receivers left.
                map.remove(&user_id);
            }
        }
    }

    /// Subscribe to `user_id`'s events.
    pub fn subscribe(&self, user_id: Uuid) -> broadcast::Receiver<Event> {
        let mut map = self.lock();
        if let Some(tx) = map.get(&user_id) {
            return tx.subscribe();
        }
        let (tx, rx) = broadcast::channel(CHANNEL_CAPACITY);
        map.insert(user_id, tx);
        rx
    }

    /// Number of live subscribers for `user_id` (diagnostics/tests).
    pub fn subscriber_count(&self, user_id: Uuid) -> usize {
        self.lock()
            .get(&user_id)
            .map_or(0, broadcast::Sender::receiver_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fanout_is_per_user() {
        let ev = Events::new();
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let mut ra1 = ev.subscribe(a);
        let mut ra2 = ev.subscribe(a);
        let mut rb = ev.subscribe(b);
        ev.publish(a, Event::History { seq: 7 });
        assert!(matches!(
            ra1.recv().await.unwrap(),
            Event::History { seq: 7 }
        ));
        assert!(matches!(
            ra2.recv().await.unwrap(),
            Event::History { seq: 7 }
        ));
        assert!(rb.try_recv().is_err());
        assert_eq!(ev.subscriber_count(a), 2);
    }

    #[test]
    fn publish_without_subscribers_cleans_up() {
        let ev = Events::new();
        let u = Uuid::now_v7();
        ev.publish(u, Event::History { seq: 1 });
        drop(ev.subscribe(u));
        ev.publish(u, Event::History { seq: 2 });
        assert_eq!(ev.lock().len(), 0);
    }

    #[test]
    fn serializes_with_type_tag() {
        let s = serde_json::to_string(&Event::Doc {
            domain: "spaces".into(),
            version: 3,
            device_id: None,
        })
        .unwrap();
        assert_eq!(
            s,
            r#"{"type":"doc","domain":"spaces","version":3,"device_id":null}"#
        );
    }
}
